use std::{fs,path::Path};
use volumetrail::{cleanup::*,store::Store};

fn fixture()->(tempfile::TempDir,Store,Safety,Rule) {
    let temp=tempfile::tempdir().unwrap();
    let data=temp.path().join("data");let scope=temp.path().join("中文 scope");
    fs::create_dir_all(&data).unwrap();fs::create_dir_all(&scope).unwrap();
    let store=Store::open(&data.join("history.db")).unwrap();let safety=Safety::for_data(&data).unwrap();
    let rule=save_rule(&store,&safety,Rule {name:"临时转储".into(),directory:scope.to_str().unwrap().into(),patterns:vec!["*.dmp".into()],..Rule::default()}).unwrap();
    (temp,store,safety,rule)
}

#[test]
fn preview_unicode_age_recursion_overlap_and_no_deletion() {
    let (_temp,store,safety,rule)=fixture();let scope=Path::new(&rule.directory);
    fs::write(scope.join("一 个.DMP"),b"sample").unwrap();fs::write(scope.join("keep.txt"),b"keep").unwrap();
    fs::create_dir(scope.join("sub")).unwrap();fs::write(scope.join("sub/two.dmp"),b"two").unwrap();
    let plan=preview(&store,&safety,&[rule.id.clone()],&||false).unwrap();assert_eq!(plan.candidate_count,1);
    assert!(scope.join("一 个.DMP").exists());assert_eq!(plan.skip_reasons["filename_mismatch"],1);
    let mut recursive=rule.clone();recursive.recursive=true;
    let recursive=save_rule(&store,&safety,recursive).unwrap();
    let mut another=recursive.clone();another.id.clear();another.revision=0;
    let another=save_rule(&store,&safety,another).unwrap();
    let plan=plan_rules(vec![recursive,another],&safety,chrono::Utc::now().timestamp(),&||false).unwrap();
    assert_eq!(plan.candidate_count,2);assert!(plan.candidates.iter().all(|c|c.rule_ids.len()==2));
    let now=1_700_000_000;let ticks=(now-86400+11644473600)*10_000_000;
    assert!(old_enough(ticks,1,now));assert!(!old_enough(ticks+1,1,now));assert!(old_enough(i64::MAX,0,now));
}

#[test]
fn rules_are_disabled_revision_checked_and_preview_required() {
    let (_temp,store,safety,rule)=fixture();assert!(!rule.enabled);
    assert!(set_enabled(&store,&safety,&rule.id,rule.revision,None,true).is_err());
    let plan=preview(&store,&safety,&[rule.id.clone()],&||false).unwrap();
    let active=set_enabled(&store,&safety,&rule.id,rule.revision,Some(&plan.plan_id),true).unwrap();assert!(active.enabled);
    assert!(save_rule(&store,&safety,rule).is_err());
    let paused=save_rule(&store,&safety,active).unwrap();assert!(!paused.enabled);
    let plan=preview(&store,&safety,&[paused.id.clone()],&||false).unwrap();
    assert!(validate_plan(&store,&safety,&plan,plan.expires_at).is_err());
    assert!(preview(&store,&safety,&[paused.id],&||true).is_err());
}

#[test]
fn roots_protected_paths_aliases_and_volume_swaps_are_rejected() {
    let (_temp,store,safety,mut rule)=fixture();
    rule.directory=rule.directory[..3].into();assert!(save_rule(&store,&safety,rule.clone()).is_err());
    rule.directory=safety.protected[0].to_str().unwrap().into();assert!(save_rule(&store,&safety,rule.clone()).is_err());
    assert!(normal_path(r"C:\test\..\Windows").is_err());assert!(normal_path(r"\\server\share").is_err());
    assert!(normal_path(r"C:\test:stream").is_err());assert!(!within(r"c:\temp2",r"c:\temp"));
    rule.directory=_temp.path().join("中文 scope").to_str().unwrap().into();rule.volume_id="ntfs:0000000000000000".into();
    assert!(save_rule(&store,&safety,rule).is_err());
}

#[test]
fn preview_skips_hardlinks_readonly_and_reparse_directories() {
    let (_temp,store,safety,mut rule)=fixture();let scope=Path::new(&rule.directory);
    fs::write(scope.join("hard.dmp"),b"sample").unwrap();fs::hard_link(scope.join("hard.dmp"),scope.join("also.dmp")).unwrap();
    let readonly=scope.join("readonly.dmp");fs::write(&readonly,b"read only").unwrap();
    let mut permissions=fs::metadata(&readonly).unwrap().permissions();permissions.set_readonly(true);fs::set_permissions(&readonly,permissions).unwrap();
    let outside=_temp.path().join("outside");fs::create_dir(&outside).unwrap();fs::write(outside.join("keep.dmp"),b"keep").unwrap();
    let output=std::process::Command::new("cmd").args(["/C","mklink","/J"]).arg(scope.join("junction")).arg(&outside).output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    rule.recursive=true;let rule=save_rule(&store,&safety,rule).unwrap();
    let plan=preview(&store,&safety,&[rule.id],&||false).unwrap();assert_eq!(plan.candidate_count,0);
    assert_eq!(plan.skip_reasons["reparse_point"],1);assert!(outside.join("keep.dmp").exists());
    let mut permissions=fs::metadata(&readonly).unwrap().permissions();permissions.set_readonly(false);fs::set_permissions(readonly,permissions).unwrap();
}

#[test]
fn filename_wildcards_are_bounded_unicode_and_case_insensitive() {
    for (name,pattern,expected) in [("中文.DMP","*.dmp",true),("甲乙.txt","??.txt",true),("abc","a*c?",false),("abbbc","a*b*c",true),("abc","*",true),("abc","*.dmp",false)] {
        assert_eq!(filename_matches(name,pattern),expected,"{name} {pattern}");
    }
}

#[test]
fn execution_only_deletes_previewed_ordinary_objects_and_is_idempotent() {
    let (_temp,store,safety,rule)=fixture();let scope=Path::new(&rule.directory);
    fs::write(scope.join("old.dmp"),b"sample").unwrap();
    let plan=preview(&store,&safety,&[rule.id],&||false).unwrap();
    fs::write(scope.join("new.dmp"),b"appeared after preview").unwrap();
    let run=execute_plan(&store,&safety,&plan,"one-time-request",None,None,&||false).unwrap();
    assert_eq!(run.deleted,1);assert_eq!(run.state,"completed");assert_eq!(run.verification,"pending");
    assert!(!scope.join("old.dmp").exists());assert!(scope.join("new.dmp").exists());
    let retry=execute_plan(&store,&safety,&plan,"one-time-request",None,None,&||panic!("must not execute on retry")).unwrap();
    assert_eq!(retry.run_id,run.run_id);assert_eq!(store.cleanup_runs(None,50,0).unwrap().0,1);
    assert_eq!(store.cleanup_items(&run.run_id).unwrap()[0].state,"deleted");
}

#[test]
fn replaced_files_and_parent_junction_races_do_not_escape_preview() {
    let (temp,store,safety,rule)=fixture();let scope=Path::new(&rule.directory);
    fs::write(scope.join("old.dmp"),b"sample").unwrap();
    let plan=preview(&store,&safety,&[rule.id.clone()],&||false).unwrap();
    fs::rename(scope.join("old.dmp"),scope.join("renamed.keep")).unwrap();fs::write(scope.join("old.dmp"),b"sample").unwrap();
    let run=execute_plan(&store,&safety,&plan,"replaced",None,None,&||false).unwrap();
    assert_eq!((run.deleted,run.skipped),(0,1));assert!(scope.join("old.dmp").exists());
    let plan=preview(&store,&safety,&[rule.id.clone()],&||false).unwrap();let link=temp.path().join("new-link.keep");
    fs::hard_link(scope.join("old.dmp"),&link).unwrap();
    let run=execute_plan(&store,&safety,&plan,"hardlink-added",None,None,&||false).unwrap();
    assert_eq!((run.deleted,run.skipped),(0,1));fs::remove_file(link).unwrap();
    let plan=preview(&store,&safety,&[rule.id],&||false).unwrap();
    fs::rename(scope,temp.path().join("original")).unwrap();let outside=temp.path().join("outside");fs::create_dir(&outside).unwrap();fs::write(outside.join("old.dmp"),b"keep").unwrap();
    let output=std::process::Command::new("cmd").args(["/C","mklink","/J"]).arg(scope).arg(&outside).output().unwrap();assert!(output.status.success());
    // A reparse scope is rejected before a run starts; its target stays untouched.
    assert!(execute_plan(&store,&safety,&plan,"junction-race",None,None,&||false).is_err());
    assert_eq!(fs::read(outside.join("old.dmp")).unwrap(),b"keep");
}

#[test]
fn shared_files_are_skipped_and_cancel_keeps_completed_deletions() {
    use std::{fs::OpenOptions,os::windows::fs::OpenOptionsExt,cell::Cell};
    let (_temp,store,safety,rule)=fixture();let scope=Path::new(&rule.directory);
    for name in ["one.dmp","two.dmp","busy.dmp"] {fs::write(scope.join(name),b"sample").unwrap();}
    let plan=preview(&store,&safety,&[rule.id],&||false).unwrap();
    let held=OpenOptions::new().read(true).share_mode(0).open(scope.join("busy.dmp")).unwrap();
    let run=execute_plan(&store,&safety,&plan,"shared",None,None,&||false).unwrap();
    assert_eq!((run.deleted,run.skipped),(2,1));assert_eq!(run.state,"partial");drop(held);
    for name in ["one.dmp","two.dmp"] {fs::write(scope.join(name),b"sample").unwrap();}
    let plan=plan_rules(plan.rules,&safety,chrono::Utc::now().timestamp(),&||false).unwrap();
    let calls=Cell::new(0);
    let run=execute_plan(&store,&safety,&plan,"cancel",None,None,&||{let n=calls.get();calls.set(n+1);n>=1}).unwrap();
    assert_eq!(run.deleted,1);assert_eq!(run.state,"cancelled");
    assert_eq!(fs::read_dir(scope).unwrap().count(),2);
}

#[test]
fn incremental_verification_policy_refuses_full_modes() {
    use volumetrail::{scanner::{ScanPolicy,enforce_scan_policy},model::ScanMode};
    assert!(enforce_scan_policy(ScanPolicy::RequireIncremental,ScanMode::Full).is_err());
    assert!(enforce_scan_policy(ScanPolicy::RequireIncremental,ScanMode::Incremental).is_ok());
    assert!(enforce_scan_policy(ScanPolicy::AllowFull,ScanMode::Full).is_ok());
}

#[test]
fn interrupted_intents_recover_without_replay_and_daily_claims_survive_revision_changes() {
    let (_temp,store,safety,rule)=fixture();let scope=Path::new(&rule.directory);
    fs::write(scope.join("keep.dmp"),b"not actually deleted").unwrap();
    let plan=preview(&store,&safety,&[rule.id.clone()],&||false).unwrap();let date=chrono::Local::now().date_naive().to_string();
    let mut run=execute_plan(&store,&safety,&plan,"interrupted",None,Some(date.clone()),&||true).unwrap();
    assert_eq!(run.state,"cancelled");assert!(!store.cleanup_due(&rule.id,&date).unwrap());
    run.state="running".into();run.finished_at=None;
    let item=RunItem {path:scope.join("keep.dmp").to_str().unwrap().into(),state:"pending".into(),reason:None,allocated_bytes:4096};
    store.record_cleanup_item(&run,0,&item).unwrap();
    store.recover_cleanup_runs().unwrap();store.recover_cleanup_runs().unwrap();
    let recovered=store.cleanup_request("interrupted").unwrap().unwrap();assert_eq!(recovered.state,"interrupted");assert_eq!(recovered.unknown,1);assert_eq!(recovered.verification,"pending");
    assert_eq!(store.cleanup_items(&run.run_id).unwrap()[0].state,"unknown");assert!(scope.join("keep.dmp").exists());
    let edited=save_rule(&store,&safety,rule).unwrap();assert!(!store.cleanup_due(&edited.id,&date).unwrap());
    store.reconcile_cleanup_after_scan(&run.volume_id,100).unwrap();assert_eq!(store.cleanup_run(&run.run_id).unwrap().verification,"verified_by_later_scan");
}

#[test]
fn cleanup_detail_caps_and_retention_keep_summaries() {
    let (_temp,store,safety,rule)=fixture();let plan=preview(&store,&safety,&[rule.id],&||false).unwrap();
    let mut run=execute_plan(&store,&safety,&plan,"retention",None,None,&||false).unwrap();
    for i in 0..250 {
        store.record_cleanup_item(&run,i,&RunItem {path:format!("dedicated-synthetic-item-{i}"),state:"skipped".into(),reason:Some("fixture".into()),allocated_bytes:1}).unwrap();
    }
    assert_eq!(store.cleanup_items(&run.run_id).unwrap().len(),MAX_DETAILS);
    run.started_at=chrono::Utc::now().timestamp()-31*86400;run.details_trimmed=false;store.update_cleanup_run(&run).unwrap();
    // started is a separate indexed column, so use a fresh old run summary to exercise age pruning.
    run.run_id=new_id();run.request_id="old-retention".into();store.start_cleanup_run(&run).unwrap();
    store.record_cleanup_item(&run,0,&RunItem {path:"synthetic-old-item".into(),state:"skipped".into(),reason:None,allocated_bytes:1}).unwrap();
    store.prune_cleanup(chrono::Utc::now().timestamp()).unwrap();
    assert!(store.cleanup_items(&run.run_id).unwrap().is_empty());assert!(store.cleanup_run(&run.run_id).unwrap().details_trimmed);
    assert!(store.cleanup_request("old-retention").unwrap().is_some());
}

#[test]
fn interrupted_pending_after_200_terminal_rows_is_counted_and_visible() {
    let (_temp,store,safety,rule)=fixture();let plan=preview(&store,&safety,&[rule.id],&||false).unwrap();
    let mut run=execute_plan(&store,&safety,&plan,"pending-201",None,None,&||true).unwrap();run.state="running".into();run.unknown=0;
    for i in 0..200 {store.record_cleanup_item(&run,i,&RunItem {path:format!("fixture-{i}"),state:"deleted".into(),reason:None,allocated_bytes:1}).unwrap();}
    store.record_cleanup_item(&run,200,&RunItem {path:"pending-at-boundary".into(),state:"pending".into(),reason:None,allocated_bytes:1}).unwrap();
    assert_eq!(store.cleanup_item_counts(&run.run_id).unwrap(),(201,1,0));
    let items=store.cleanup_items(&run.run_id).unwrap();assert_eq!(items.len(),200);assert_eq!(items[0].state,"pending");
    let observed=store.observed_cleanup_run(&run.run_id,false).unwrap();assert_eq!(observed.unknown,1);assert_eq!(observed.state,"interrupted");
    store.recover_cleanup_runs().unwrap();assert_eq!(store.cleanup_run(&run.run_id).unwrap().unknown,1);assert_eq!(store.cleanup_items(&run.run_id).unwrap()[0].state,"unknown");
}

#[test]
fn first_pending_intent_in_v9_migration_protects_the_next_growth_baseline() {
    use volumetrail::model::*;
    let (temp,mut store,safety,rule)=fixture();let root=&rule.directory[..3];let path=temp.path().join("data/history.db");
    let sample=Path::new(&rule.directory).join("first.dmp");fs::write(&sample,b"dedicated first-pending fixture").unwrap();
    let mut scan=ScanOutcome {checkpoint:Checkpoint {volume_id:rule.volume_id.clone(),journal_id:1,next_usn:10,root_id:5},mode:ScanMode::Full,total:100000,free:50000,
        sampled_at:"2026-10-21T10:00:00+00:00".into(),pending:vec![],warnings:vec![]};
    store.publish(root,&[Mutation::Upsert(Entry {id:30,parent:5,name:"first.dmp".into(),is_dir:false,logical:4096,allocated:4096,modified:0,attributes:0,links:vec![]})],&scan).unwrap();
    let plan=preview(&store,&safety,&[rule.id],&||false).unwrap();let mut run=execute_plan(&store,&safety,&plan,"first-pending-v9",Some(1),None,&||true).unwrap();
    run.state="running".into();run.deleted=0;run.unknown=0;run.verification="not_needed".into(); // stale summary must not override durable intent
    store.record_cleanup_item(&run,0,&RunItem {path:sample.to_str().unwrap().into(),state:"pending".into(),reason:None,allocated_bytes:4096}).unwrap();
    windows::delete_verified(sample.to_str().unwrap(),&plan.candidates[0].snapshot).unwrap();drop(store);
    let db=rusqlite::Connection::open(&path).unwrap();db.execute_batch("ALTER TABLE scans DROP COLUMN comparison_from;DROP TABLE scan_baselines;PRAGMA user_version=9;").unwrap();drop(db);
    let mut store=Store::open(&path).unwrap();assert!(store.cleanup_baseline_pending(&run.volume_id).unwrap());
    store.recover_cleanup_runs().unwrap();assert_eq!(store.cleanup_run(&run.run_id).unwrap().unknown,1);assert!(store.cleanup_baseline_pending(&run.volume_id).unwrap());
    scan.mode=ScanMode::Incremental;scan.checkpoint.next_usn=20;scan.sampled_at="2026-10-21T11:00:00+00:00".into();
    store.publish(root,&[Mutation::Delete(30)],&scan).unwrap();assert!(store.default_interval(&store.reports().unwrap()).unwrap().is_none());
}

#[test]
fn cleanup_run_count_and_365_day_retention_boundaries_are_exact() {
    let (_temp,store,safety,rule)=fixture();let plan=preview(&store,&safety,&[rule.id],&||false).unwrap();
    let template=execute_plan(&store,&safety,&plan,"template",None,None,&||false).unwrap();let now=chrono::Utc::now().timestamp();
    for (name,age) in [("exact-365",365*86400),("over-365",365*86400+1)] {
        let mut run=template.clone();run.run_id=name.into();run.request_id=name.into();run.started_at=now-age;store.start_cleanup_run(&run).unwrap();
    }
    store.prune_cleanup(now).unwrap();assert!(store.cleanup_run("exact-365").is_ok());assert!(store.cleanup_run("over-365").is_err());
    for i in 0..1001 {
        let mut run=template.clone();run.run_id=format!("count-{i:04}");run.request_id=run.run_id.clone();run.started_at=now-i;store.start_cleanup_run(&run).unwrap();
    }
    store.prune_cleanup(now).unwrap();assert_eq!(store.cleanup_runs(None,500,0).unwrap().0,1000);
    assert!(store.cleanup_run("count-0000").is_ok());assert!(store.cleanup_run("count-1000").is_err());
}

#[test]
fn long_paths_and_locked_ancestor_namespace_are_supported() {
    let (_temp,store,safety,mut rule)=fixture();let mut deep=std::path::PathBuf::from(&rule.directory);
    for _ in 0..6 {deep.push("长路径 sample directory 0123456789");}
    fs::create_dir_all(format!(r"\\?\{}",deep.display())).unwrap();
    let file=deep.join("sample.dmp");fs::write(format!(r"\\?\{}",file.display()),b"long path sample").unwrap();
    rule.recursive=true;let rule=save_rule(&store,&safety,rule).unwrap();let plan=preview(&store,&safety,&[rule.id],&||false).unwrap();
    assert_eq!(plan.candidate_count,1);assert!(plan.candidates[0].path.len()>260);
    let pinned=windows::DirectoryGuard::open(deep.to_str().unwrap()).unwrap();
    assert!(fs::rename(&deep,deep.with_file_name("replaced")).is_err());drop(pinned);
    let run=execute_plan(&store,&safety,&plan,"long-path",None,None,&||false).unwrap();assert_eq!(run.deleted,1);
}

#[test]
fn access_denied_is_skipped_without_permission_changes_by_executor() {
    let (_temp,store,safety,rule)=fixture();let scope=Path::new(&rule.directory);let file=scope.join("denied.dmp");fs::write(&file,b"temporary").unwrap();
    let plan=preview(&store,&safety,&[rule.id.clone()],&||false).unwrap();assert_eq!(plan.candidate_count,1);
    let user=format!("{}\\{}",std::env::var("USERDOMAIN").unwrap(),std::env::var("USERNAME").unwrap());
    struct Restore {paths:Vec<std::path::PathBuf>,user:String}
    impl Drop for Restore {fn drop(&mut self) {for path in &self.paths {let _=std::process::Command::new("icacls").arg(path).args(["/remove:d",&self.user]).output();}}}
    let _restore=Restore {paths:vec![file.clone(),scope.into()],user:user.clone()};
    for (path,right) in [(file.as_path(),"D"),(scope,"DC")] {
        let output=std::process::Command::new("icacls").arg(path).args(["/deny",&format!("{user}:({right})")]).output().unwrap();assert!(output.status.success(),"ACL fixture setup failed");
    }
    let blocked=preview(&store,&safety,&[rule.id],&||false).unwrap();assert_eq!(blocked.candidate_count,0);
    let run=execute_plan(&store,&safety,&plan,"permission-denied",None,None,&||false).unwrap();assert_eq!((run.deleted,run.skipped),(0,1));assert!(file.exists());
    assert!(store.cleanup_items(&run.run_id).unwrap()[0].reason.as_ref().unwrap().contains("Open object"));
}
