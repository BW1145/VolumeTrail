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
