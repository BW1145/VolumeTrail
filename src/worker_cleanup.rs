//! Cleanup lifecycle under the existing global operation lock.
use crate::{cleanup::{self,Plan,Run,Safety},model::ScanCounters,scanner::{self,ScanPolicy},store::Store,worker};
use anyhow::Result;
use std::{fs,path::Path};

fn execute_recorded(store:&Store,safety:&Safety,plan:&Plan,request:&str,pre:Option<i64>,date:Option<String>,cancelled:&dyn Fn()->bool)->Result<Run> {
    match cleanup::execute_plan(store,safety,plan,request,pre,date,cancelled) {
        Ok(run)=>Ok(run),
        Err(error)=>{
            store.recover_cleanup_runs()?;
            if let Some(mut run)=store.cleanup_request(request)? {
                run.message=Some(format!("Execution interrupted; pending results are unknown: {error:#}"));
                store.update_cleanup_run(&run)?;Ok(run)
            } else {Err(error)}
        }
    }
}

pub(crate) fn verify(data:&Path,store:&mut Store,plan:&Plan,run:&mut Run,counters:&mut ScanCounters)->Result<()> {
    if run.verification!="pending" {return Ok(());}
    let root=&plan.rules[0].directory[..3];let letter=root.chars().next().unwrap().to_ascii_uppercase();
    let stop=worker::stop_path(data,letter);
    let group=run.pre_scan.map(|id|store.operation_pre(id)).transpose()?;
    if let Some(group)=group {store.mark_verification(group,"pending",None)?;}
    store.verification_context(Some((group,run.run_id.clone())));
    let result=scanner::scan_volume_with_policy(root,store,&||stop.exists(),&mut |progress| {
        eprintln!("cleanup verification {}: {} records",progress.phase,progress.processed);
    },counters,ScanPolicy::RequireIncremental);
    store.verification_context(None);
    match result {
        Ok(post)=>{
            run.post_scan=Some(post);run.verification="verified".into();
            if let Some(group)=group {store.mark_verification(group,"verified",Some(post))?;}
        }
        Err(error)=>{
            run.verification=if stop.exists() {"cancelled"} else {"unverified"}.into();
            run.message=Some(format!("Cleanup results saved; file statistics await the next scan: {error:#}"));
            if let Some(group)=group {store.mark_verification(group,&run.verification,None)?;}
        }
    }
    store.update_cleanup_run(run)?;
    Ok(())
}

pub(crate) fn automatic(data:&Path,store:&mut Store,root:&str,pre:i64,counters:&mut ScanCounters)->Result<Option<Run>> {
    let Some(cp)=store.checkpoint(root)? else {return Ok(None);};
    let date=chrono::Local::now().date_naive().to_string();
    let rules=store.cleanup_rules()?.into_iter().filter(|r|r.enabled && r.volume_id==cp.volume_id)
        .map(|r|Ok((store.cleanup_due(&r.id,&date)?,r))).collect::<Result<Vec<_>>>()?
        .into_iter().filter_map(|(due,r)|due.then_some(r)).collect::<Vec<_>>();
    if rules.is_empty() {return Ok(None);}
    let safety=Safety::for_data(data)?;let letter=root.chars().next().unwrap();let stop=worker::stop_path(data,letter);
    let plan=cleanup::plan_rules(rules,&safety,chrono::Utc::now().timestamp(),&||stop.exists())?;
    let request=format!("auto-{}",cleanup::new_id());
    let mut run=execute_recorded(store,&safety,&plan,&request,Some(pre),Some(date),&||stop.exists())?;
    verify(data,store,&plan,&mut run,counters)?;
    Ok(Some(run))
}

pub fn execute(data:&Path,plan_id:&str,request_id:&str)->Result<Run> {
    // Fast idempotent response remains available while the original execution owns the lock.
    if let Some(run)=Store::open_read_only(&worker::store_path(data))?.cleanup_request(request_id)? {
        if run.plan_id!=plan_id {return Err(cleanup::fault("request_conflict","Request ID belongs to a different plan"));}
        if run.state!="running" || worker::operation_active_read_only(data) {return Ok(run);}
        let _global=worker::global_scan_lock(data)?;
        let store=Store::open(&worker::store_path(data))?;store.recover_cleanup_runs()?;
        return store.cleanup_run(&run.run_id);
    }
    let _global=worker::global_scan_lock(data)?;
    let mut store=Store::open(&worker::store_path(data))?;store.recover_cleanup_runs()?;
    let plan=store.cleanup_plan(plan_id)?;
    let root=&plan.rules[0].directory[..3];let letter=root.chars().next().unwrap().to_ascii_uppercase();
    let stop=worker::stop_path(data,letter);if stop.exists() {fs::remove_file(&stop)?;} // exactly once, before advertising the drive lock
    let _drive=worker::scan_lock(data,letter)?;
    let mut status=worker::ScanStatus {root:root.into(),phase:"清理文件".into(),..Default::default()};
    worker::write_status(data,letter,&status)?;
    let result=(|| {
        let safety=Safety::for_data(data)?;
        let pre=store.reports()?.into_iter().find(|r|r.volume==plan.rules[0].volume_id).map(|r|r.id);
        let mut run=execute_recorded(&store,&safety,&plan,request_id,pre,None,&||stop.exists())?;
        let mut counters=ScanCounters::default();verify(data,&mut store,&plan,&mut run,&mut counters)?;
        store.prune_cleanup(chrono::Utc::now().timestamp())?;
        Ok(run)
    })();
    status.finished=true;
    match &result {
        Ok(run)=>{status.phase="清理结束".into();status.report_id=run.post_scan.or(run.pre_scan);status.warning=run.message.clone();}
        Err(e)=>{status.phase="清理中断".into();status.error=Some(format!("{e:#}"));}
    }
    worker::write_status(data,letter,&status)?;result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verification_keeps_the_operation_cancel_flag_and_never_starts_a_full_scan() {
        let temp=tempfile::tempdir().unwrap();let data=temp.path().join("data");let scope=temp.path().join("sample");
        fs::create_dir(&data).unwrap();fs::create_dir(&scope).unwrap();fs::write(scope.join("one.dmp"),b"temporary").unwrap();
        let _lock=worker::global_scan_lock(&data).unwrap();let mut store=Store::open(&worker::store_path(&data)).unwrap();
        let safety=Safety::for_data(&data).unwrap();
        let rule=cleanup::save_rule(&store,&safety,cleanup::Rule {name:"test".into(),directory:scope.to_str().unwrap().into(),patterns:vec!["*.dmp".into()],..Default::default()}).unwrap();
        let plan=cleanup::preview(&store,&safety,&[rule.id],&||false).unwrap();
        let mut run=cleanup::execute_plan(&store,&safety,&plan,"cancel-verification",None,None,&||false).unwrap();
        let letter=scope.to_str().unwrap().chars().next().unwrap().to_ascii_uppercase();let stop=worker::stop_path(&data,letter);fs::write(&stop,b"stop").unwrap();
        verify(&data,&mut store,&plan,&mut run,&mut ScanCounters::default()).unwrap();
        assert_eq!(run.verification,"cancelled");assert!(stop.exists());assert!(store.reports().unwrap().is_empty());
        assert!(run.message.unwrap().contains("Scan cancelled"));
    }
}
