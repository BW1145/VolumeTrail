//! Opt-in destructive journal checks only on the CI-created VHD, never a host volume.
use std::{fs,path::{Path,PathBuf},process::{Command,Stdio},time::{Duration,Instant}};
use volumetrail::{cleanup::{self,Rule,Safety},config::AppConfig,model::ScanCounters,scanner::{self,ScanPolicy,Volume},store::Store,worker,worker_cleanup};

fn portable(base:&Path,drive:char,scope:&Path)->(PathBuf,PathBuf,Store) {
    let data=base.join("data");fs::create_dir_all(&data).unwrap();
    let exe=base.join("VolumeTrail-cli.exe");fs::copy(env!("CARGO_BIN_EXE_volumetrail"),&exe).unwrap();
    AppConfig {drives:vec![drive],..Default::default()}.save(&data).unwrap();
    let store=Store::open(&data.join("history.db")).unwrap();let safety=Safety::for_data(&data).unwrap();
    let rule=cleanup::save_rule(&store,&safety,Rule {name:"isolated automatic fixture".into(),directory:scope.to_str().unwrap().into(),patterns:vec!["*.dmp".into()],..Default::default()}).unwrap();
    let plan=cleanup::preview(&store,&safety,&[rule.id.clone()],&||false).unwrap();
    cleanup::set_enabled(&store,&safety,&rule.id,rule.revision,Some(&plan.plan_id),true).unwrap();
    (exe,data,store)
}

#[test]
#[ignore = "requires tests/run-isolated-ntfs.ps1 on a GitHub-hosted Windows VM"]
fn isolated_ntfs_cleanup_and_incremental_verification() {
    assert_eq!(std::env::var("GITHUB_ACTIONS").unwrap(),"true");
    assert_eq!(std::env::var("RUNNER_ENVIRONMENT").unwrap(),"github-hosted");
    let drive=std::env::var("VT_NTFS_FIXTURE_DRIVE").unwrap();
    assert!(drive.len()==1 && ('R'..='Z').contains(&drive.chars().next().unwrap()));
    assert!(!std::env::current_exe().unwrap().to_str().unwrap().starts_with(&format!("{drive}:")));
    let root=format!("{drive}:\\");
    let marker=PathBuf::from(std::env::var("VT_NTFS_FIXTURE_MARKER").unwrap());
    assert!(marker.starts_with(&root));
    assert_eq!(fs::read_to_string(marker).unwrap(),std::env::var("VT_NTFS_FIXTURE_TOKEN").unwrap());
    let data=PathBuf::from(std::env::var("VT_NTFS_FIXTURE_DATA").unwrap());
    assert!(data.starts_with(std::env::var("RUNNER_TEMP").unwrap()));
    let scope=PathBuf::from(&root).join("VolumeTrail Samples");fs::create_dir(&scope).unwrap();
    let sample=scope.join("only-this-file.dmp");fs::write(&sample,vec![0u8;4096]).unwrap();
    for n in 0..1000 {fs::write(scope.join(format!("kept-{n}.bin")),vec![0u8;4096]).unwrap();}
    let mut store=Store::open(&data.join("history.db")).unwrap();let mut counters=ScanCounters::default();
    let began=Instant::now();let first=scanner::scan_volume(&root,&mut store,&||false,&mut |_|{},&mut counters).unwrap();
    let full_ms=began.elapsed().as_millis();let volume=store.checkpoint(&root).unwrap().unwrap().volume_id;
    assert_eq!(volume,cleanup::windows::volume_id(scope.to_str().unwrap()).unwrap());
    fs::write(&sample,vec![0u8;8192]).unwrap();
    let began=Instant::now();let pre=scanner::scan_volume_with_policy(&root,&mut store,&||false,&mut |_|{},&mut ScanCounters::default(),ScanPolicy::RequireIncremental).unwrap();
    let incremental_ms=began.elapsed().as_millis();
    let safety=Safety::for_data(&data).unwrap();let rule=cleanup::save_rule(&store,&safety,Rule {name:"CI fixture only".into(),directory:scope.to_str().unwrap().into(),patterns:vec!["*.dmp".into()],..Default::default()}).unwrap();
    let plan=cleanup::preview(&store,&safety,&[rule.id],&||false).unwrap();assert_eq!(plan.candidate_count,1);
    let allocated=plan.candidates[0].snapshot.allocated_bytes;
    let began=Instant::now();let run=worker_cleanup::execute(&data,&plan.plan_id,"ci-isolated-request").unwrap();let cleanup_verify_ms=began.elapsed().as_millis();
    assert_eq!(run.deleted,1);assert_eq!(run.verification,"verified");assert!(!sample.exists());
    let post=run.post_scan.unwrap();assert_eq!(store.operation_pre(post).unwrap(),pre);
    assert_eq!(store.folder_breakdown(&volume,pre,post,scope.to_str().unwrap()).unwrap().allocated_delta,-i128::from(allocated));
    let post_cursor=store.checkpoint(&root).unwrap().unwrap();
    let next=scanner::scan_volume_with_policy(&root,&mut store,&||false,&mut |_|{},&mut ScanCounters::default(),ScanPolicy::RequireIncremental).unwrap();
    assert_eq!(store.folder_breakdown(&volume,post,next,scope.to_str().unwrap()).unwrap().allocated_delta,0);
    assert!(store.checkpoint(&root).unwrap().unwrap().next_usn>=post_cursor.next_usn);
    let mut empty=Store::open(&data.join("requires-baseline.db")).unwrap();
    let error=scanner::scan_volume_with_policy(&root,&mut empty,&||false,&mut |_|{},&mut ScanCounters::default(),ScanPolicy::RequireIncremental).unwrap_err();
    assert_eq!(error.downcast_ref::<cleanup::Fault>().unwrap().code,"incremental_unavailable");assert!(empty.reports().unwrap().is_empty());
    let before=store.checkpoint(&root).unwrap().unwrap();let count=store.reports().unwrap().len();
    // /n waits for deletion. The drive was verified as this job's VHD before test launch.
    assert!(Command::new("fsutil").args(["usn","deletejournal","/n",&format!("{drive}:")]).status().unwrap().success());
    assert!(Command::new("fsutil").args(["usn","createjournal","m=8388608","a=1048576",&format!("{drive}:")]).status().unwrap().success());
    assert_ne!(Volume::open(drive.chars().next().unwrap()).unwrap().journal().unwrap().id,before.journal_id);
    let error=scanner::scan_volume_with_policy(&root,&mut store,&||false,&mut |_|{},&mut ScanCounters::default(),ScanPolicy::RequireIncremental).unwrap_err();
    assert_eq!(error.downcast_ref::<cleanup::Fault>().unwrap().code,"incremental_unavailable");
    assert_eq!(store.checkpoint(&root).unwrap().unwrap(),before);assert_eq!(store.reports().unwrap().len(),count);
    // Run the actual portable CLI lifecycle, without creating a Windows login task.
    let auto_sample=scope.join("auto-first.dmp");fs::write(&auto_sample,vec![0u8;8192]).unwrap();
    let (auto_exe,_auto_data,auto_store)=portable(&data.parent().unwrap().join("auto-success"),drive.chars().next().unwrap(),&scope);
    let output=Command::new(&auto_exe).arg("scan-auto").output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    assert!(!auto_sample.exists());let auto_run=auto_store.cleanup_runs(Some(&volume),50,0).unwrap().1[0].clone();
    assert_eq!(auto_run.deleted,1);assert_eq!(auto_run.verification,"verified");assert!(auto_run.automatic_date.is_some());
    let retained=scope.join("manual-scan-must-keep.dmp");fs::write(&retained,b"only dedicated sample data").unwrap();
    let output=Command::new(&auto_exe).args(["scan",&drive]).output().unwrap();assert!(output.status.success());assert!(retained.exists());
    let output=Command::new(&auto_exe).arg("scan-auto").output().unwrap();assert!(output.status.success());assert!(retained.exists());
    assert_eq!(auto_store.cleanup_runs(Some(&volume),50,0).unwrap().0,1);
    let (cancel_exe,cancel_data,cancel_store)=portable(&data.parent().unwrap().join("auto-cancelled-scan"),drive.chars().next().unwrap(),&scope);
    let mut child=Command::new(&cancel_exe).arg("scan-auto").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let deadline=Instant::now()+Duration::from_secs(5);let mut observed=false;
    while Instant::now()<deadline {
        if worker::read_status(&cancel_data,drive.chars().next().unwrap()).is_some_and(|status|!status.finished) {observed=true;break;}
        std::thread::sleep(Duration::from_millis(1));
    }
    if !observed {let _=child.kill();let _=child.wait();panic!("Did not observe isolated automatic scan start");}
    worker::stop_scan(&cancel_data,drive.chars().next().unwrap()).unwrap();assert!(!child.wait().unwrap().success());
    assert!(retained.exists());assert_eq!(cancel_store.cleanup_runs(None,50,0).unwrap().0,0);
    fs::write(std::env::var("VT_NTFS_FIXTURE_REPORT").unwrap(),serde_json::to_vec_pretty(&serde_json::json!({
        "status":"passed","isolated_vhd":true,"initial_full_scan":first,"pre_scan":pre,"verification_scan":post,"next_scan":next,
        "full_ms":full_ms,"incremental_ms":incremental_ms,"cleanup_and_verify_ms":cleanup_verify_ms,
        "deleted":run.deleted,"allocated_bytes":allocated,"delete_counted_once":true,"missing_baseline_refused":true,
        "journal_reset_refused":true,"cursor_preserved_after_refusal":true,"host_journal_modified":false,
        "scan_auto_cleanup_verified":true,"same_day_auto_deduplicated":true,"manual_scan_did_not_delete":true,
        "cancelled_primary_scan_did_not_cleanup":true,"windows_login_task_modified":false
    })).unwrap()).unwrap();
}
