use std::{fs,path::Path,process::Command};
use serde_json::{Value,json};
use volumetrail::{cleanup,model::*,store::Store};

fn invoke(exe:&Path,args:&[&str],expected:i32)->Value {
    let output=Command::new(exe).args(args).output().unwrap();
    assert_eq!(output.status.code(),Some(expected),"args={args:?}\nstdout={}\nstderr={}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
    let text=String::from_utf8(output.stdout).unwrap();
    let value:Value=serde_json::from_str(&text).unwrap_or_else(|e|panic!("Single JSON contract: {e}: {text}"));
    assert_eq!(value["schema_version"],1);value
}
fn checkpoint(volume:&str,usn:i64)->ScanOutcome {
    ScanOutcome {checkpoint:Checkpoint {volume_id:volume.into(),journal_id:1,next_usn:usn,root_id:5},
        mode:if usn==10 {ScanMode::Full} else {ScanMode::Incremental},total:100000,free:50000,
        sampled_at:format!("2026-10-21T12:{usn:02}:00+00:00"),pending:vec![],warnings:vec![]}
}
fn entry(id:u64,name:&str,directory:bool,size:u64)->Mutation {
    Mutation::Upsert(Entry {id,parent:5,name:name.into(),is_dir:directory,logical:size,allocated:size,modified:0,attributes:0,links:vec![]})
}

#[test]
fn real_cli_json_pagination_rules_preview_execute_retry_and_failure() {
    // One portable copy with a fresh database and dedicated sibling sample directory.
    let temp=tempfile::tempdir().unwrap();let bin=temp.path().join("portable");let scope=temp.path().join("中文 samples with spaces");
    fs::create_dir_all(bin.join("data")).unwrap();fs::create_dir(&scope).unwrap();
    let exe=bin.join("VolumeTrail-cli.exe");fs::copy(env!("CARGO_BIN_EXE_volumetrail"),&exe).unwrap();
    let caps=invoke(&exe,&["capabilities","--json"],0);assert_eq!(caps["transport"],"local_cli");
    let volume=cleanup::windows::volume_id(scope.to_str().unwrap()).unwrap();let root=&scope.to_str().unwrap()[..3];let drive=&root[..1];
    let mut store=Store::open(&bin.join("data/history.db")).unwrap();
    let first=store.publish(root,&[entry(20,"one",true,0),entry(21,"two",true,0),entry(30,"a.bin",false,10)],&checkpoint(&volume,10)).unwrap();
    let second=store.publish(root,&[entry(30,"a.bin",false,30)],&checkpoint(&volume,20)).unwrap();
    let folder=invoke(&exe,&["query","folder","--drive",drive,"--limit","1","--offset","1","--from",&first.to_string(),"--to",&second.to_string(),"--json"],0);
    assert_eq!(folder["total"],3);assert_eq!(folder["items"].as_array().unwrap().len(),1);assert_eq!(folder["comparison"]["children_offset"],1);
    let largest=invoke(&exe,&["query","largest","--drive",drive,"--kind","files","--limit","1","--json"],0);assert_eq!(largest["total"],1);assert_eq!(largest["has_more"],false);
    let overview=invoke(&exe,&["query","overview","--drive",drive,"--json"],0);assert_eq!(overview["interval"]["from"],first);assert_eq!(overview["interval"]["to"],second);
    let missing=invoke(&exe,&["query","largest","--drive",drive,"--kind","bad","--json"],1);assert_eq!(missing["ok"],false);
    let rule_file=temp.path().join("规则 input.json");
    fs::write(&rule_file,serde_json::to_vec(&json!({"name":"中文转储","directory":scope,"patterns":["*.dmp"],"min_age_days":0})).unwrap()).unwrap();
    let saved=invoke(&exe,&["cleanup","save","--input",rule_file.to_str().unwrap(),"--json"],0);assert_eq!(saved["rule"]["enabled"],false);
    let id=saved["rule"]["id"].as_str().unwrap();
    assert_eq!(invoke(&exe,&["cleanup","list","--json"],0)["rules"].as_array().unwrap().len(),1);
    let mut stdin_child=Command::new(&exe).args(["cleanup","save","--input","-","--json"])
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    {use std::io::Write;let mut input=stdin_child.stdin.take().unwrap();input.write_all(&serde_json::to_vec(&json!({"name":"stdin 规则","directory":scope,"patterns":["*.tmp"]})).unwrap()).unwrap();}
    let stdin_output=stdin_child.wait_with_output().unwrap();assert!(stdin_output.status.success());
    let stdin_rule:Value=serde_json::from_slice(&stdin_output.stdout).unwrap();assert_eq!(stdin_rule["rule"]["enabled"],false);
    fs::write(scope.join("预览 文件.dmp"),b"dedicated sample").unwrap();
    let preview=invoke(&exe,&["cleanup","preview","--id",id,"--json"],0);let plan=preview["plan"]["plan_id"].as_str().unwrap();assert_eq!(preview["plan"]["candidate_count"],1);
    fs::write(scope.join("after.dmp"),b"must remain").unwrap();
    let result=invoke(&exe,&["cleanup","execute","--plan",plan,"--request","integration-request","--confirm-permanent-delete","--json"],2);
    assert_eq!(result["run"]["deleted"],1);assert_eq!(result["run"]["verification"],"unverified");
    assert!(scope.join("after.dmp").exists());assert!(!scope.join("预览 文件.dmp").exists());
    let retry=invoke(&exe,&["cleanup","execute","--plan",plan,"--request","integration-request","--confirm-permanent-delete","--json"],2);
    assert_eq!(retry["run"]["run_id"],result["run"]["run_id"]);
    let run=result["run"]["run_id"].as_str().unwrap();assert_eq!(invoke(&exe,&["cleanup","status","--id",run,"--json"],2)["items"][0]["state"],"deleted");
    assert_eq!(invoke(&exe,&["cleanup","runs","--limit","1","--offset","1","--json"],0)["items"],json!([]));
    // Enable only the dedicated sample scope, never a login task; saving edits pauses it.
    let preview=invoke(&exe,&["cleanup","preview","--id",id,"--json"],0);
    let enabled=invoke(&exe,&["cleanup","enable","--id",id,"--revision","1","--plan",preview["plan"]["plan_id"].as_str().unwrap(),"--confirm-automatic-rule","--json"],0);
    assert_eq!(enabled["rule"]["enabled"],true);
    fs::write(&rule_file,serde_json::to_vec(&saved["rule"]).unwrap()).unwrap();
    assert_eq!(invoke(&exe,&["cleanup","save","--input",rule_file.to_str().unwrap(),"--json"],4)["error"]["code"],"revision_conflict");
    invoke(&exe,&["cleanup","pause","--id",id,"--revision","2","--json"],0);
    let mut expired=cleanup::preview(&store,&cleanup::Safety::for_data(&bin.join("data")).unwrap(),&[id.into()],&||false).unwrap();
    expired.plan_id=cleanup::new_id();expired.expires_at=chrono::Utc::now().timestamp()-1;store.save_cleanup_plan(&expired).unwrap();
    assert_eq!(invoke(&exe,&["cleanup","execute","--plan",&expired.plan_id,"--request","expired","--confirm-permanent-delete","--json"],4)["error"]["code"],"plan_expired");
    let rule=store.cleanup_rule(id).unwrap();assert!(!rule.enabled);
    // Kill only this freshly spawned portable fixture during deletion. A retry
    // returns the interrupted run and must not delete another surviving object.
    for n in 0..500 {fs::write(scope.join(format!("crash-{n}.dmp")),b"disposable crash sample").unwrap();}
    let preview=invoke(&exe,&["cleanup","preview","--id",id,"--json"],0);let crash_plan=preview["plan"]["plan_id"].as_str().unwrap();
    let mut child=Command::new(&exe).args(["cleanup","execute","--plan",crash_plan,"--request","crash-request","--confirm-permanent-delete","--json"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let until=std::time::Instant::now()+std::time::Duration::from_secs(5);let mut observed=None;
    while std::time::Instant::now()<until {
        if let Some(run)=store.cleanup_request("crash-request").unwrap() {if run.state=="running" && run.deleted>0 {observed=Some(run);break;}}
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    child.kill().unwrap();child.wait().unwrap();let interrupted=observed.expect("observe real deletion before fixture interruption");
    let count_before=fs::read_dir(&scope).unwrap().count();
    let status=invoke(&exe,&["cleanup","status","--id",&interrupted.run_id,"--json"],4);assert_eq!(status["run"]["state"],"interrupted");
    let retry=invoke(&exe,&["cleanup","execute","--plan",crash_plan,"--request","crash-request","--confirm-permanent-delete","--json"],4);
    assert_eq!(retry["run"]["run_id"],interrupted.run_id);assert_eq!(fs::read_dir(&scope).unwrap().count(),count_before);
    // A physical verification point must not change default growth to a seconds-long cleanup interval.
    let third=store.publish(root,&[Mutation::Delete(30)],&checkpoint(&volume,30)).unwrap();store.mark_verification(second,"verified",Some(third)).unwrap();
    let growth=invoke(&exe,&["query","growth","--drive",drive,"--json"],0);assert_eq!((growth["from"].as_i64(),growth["to"].as_i64()),(Some(first),Some(second)));
}
