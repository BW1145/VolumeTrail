use super::*;
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use std::{collections::{HashMap,HashSet},fs,path::Path};

pub fn save_rule(store:&Store,safety:&Safety,mut rule:Rule)->Result<Rule> {
    let guard=windows::DirectoryGuard::open(&rule.directory)?;
    rule.directory=guard.canonical()?;
    let volume=guard.volume_id();
    if rule.volume_id.is_empty() {rule.volume_id=volume.clone();}
    ensure!(rule.volume_id==volume,"Volume identity mismatch");
    safety.validate(&rule)?;
    let expected=rule.revision;
    if rule.id.is_empty() {ensure!(expected==0,"New rules must use revision 0");ensure!(store.cleanup_rules()?.len()<128,"At most 128 cleanup rules are supported");rule.id=new_id();}
    else if store.cleanup_rule(&rule.id)?.revision!=expected {return Err(fault("revision_conflict","Rule was changed; read it again"));}
    rule.revision=expected.checked_add(1).context("Rule revision overflow")?;
    rule.enabled=false;
    store.save_cleanup_rule(&rule,expected)?;
    Ok(rule)
}

pub fn plan_rules(rules:Vec<Rule>,safety:&Safety,now:i64,cancelled:&dyn Fn()->bool)->Result<Plan> {
    ensure!(!rules.is_empty(),"Choose at least one rule");
    ensure!(rules.iter().all(|r|r.volume_id==rules[0].volume_id),"Rules must bind the same volume");
    let mut plan=Plan {plan_id:new_id(),created_at:now,expires_at:now+PLAN_SECONDS,rules:rules.clone(),
        deletion_method:"permanent_delete".into(),candidate_count:0,allocated_bytes_estimate:0,candidates:Vec::new(),
        skipped_count:0,skip_reasons:BTreeMap::new(),skipped:Vec::new(),truncated:false,skip_details_truncated:false};
    let mut planning=Planning::default();
    for rule in &rules {
        safety.validate(rule)?;
        let guard=windows::DirectoryGuard::open(&rule.directory)?;
        ensure!(guard.volume_id()==rule.volume_id,"Volume identity mismatch");
        walk(&rule.directory,rule,&mut plan,&mut planning,now,cancelled,0)?;
    }
    plan.candidate_count=plan.candidates.len();
    plan.skip_details_truncated=plan.skipped_count>plan.skipped.len() as u64;
    Ok(plan)
}

fn skip(plan:&mut Plan,path:&str,reason:&str) {
    plan.skipped_count+=1;*plan.skip_reasons.entry(reason.into()).or_default()+=1;
    if plan.skipped.len()<MAX_DETAILS {plan.skipped.push(Skip {path:path.into(),reason:reason.into()});}
}
#[derive(Default)]
struct Planning {found:HashMap<(u64,[u8;16]),usize>,visited:usize,candidate_bytes:usize}
fn walk(path:&str,rule:&Rule,plan:&mut Plan,planning:&mut Planning,now:i64,cancelled:&dyn Fn()->bool,depth:usize)->Result<()> {
    if planning.visited>=200_000 || depth>128 {plan.truncated=true;skip(plan,path,"traversal_limit");return Ok(());}
    if cancelled() {return Err(fault("cancelled","Preview cancelled"));}
    // Pin and validate this directory for the entire enumeration; child inspect pins its own chain.
    let _guard=match windows::DirectoryGuard::open(path) {Ok(g)=>g,Err(e)=>{skip(plan,path,&format!("directory_unavailable: {e:#}"));return Ok(());}};
    let entries=match fs::read_dir(format!(r"\\?\{}",path.strip_prefix(r"\\?\").unwrap_or(path))) {
        Ok(e)=>e,Err(e)=>{skip(plan,path,&format!("directory_unavailable: {e}"));return Ok(());}
    };
    for entry in entries {
        if cancelled() {return Err(fault("cancelled","Preview cancelled"));}
        planning.visited+=1;if planning.visited>200_000 {plan.truncated=true;break;}
        let entry=match entry {Ok(e)=>e,Err(e)=>{skip(plan,path,&format!("entry_unavailable: {e}"));continue;}};
        let raw=entry.path();let Some(raw)=raw.to_str() else {skip(plan,path,"non_unicode_path");continue;};
        let raw=raw.strip_prefix(r"\\?\").unwrap_or(raw);
        let metadata=match entry.metadata() {Ok(m)=>m,Err(e)=>{skip(plan,raw,&format!("metadata_unavailable: {e}"));continue;}};
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {skip(plan,raw,"reparse_point");continue;}
        if metadata.is_dir() {if rule.recursive {walk(raw,rule,plan,planning,now,cancelled,depth+1)?;}continue;}
        let name=entry.file_name();let Some(name)=name.to_str() else {skip(plan,raw,"non_unicode_name");continue;};
        if !rule.patterns.iter().any(|p|filename_matches(name,p)) {skip(plan,raw,"filename_mismatch");continue;}
        let snapshot=match windows::inspect(raw) {Ok(s)=>s,Err(e)=>{skip(plan,raw,&format!("file_unavailable: {e:#}"));continue;}};
        if snapshot.identity.volume_id()!=rule.volume_id {skip(plan,raw,"volume_changed");continue;}
        if !old_enough(snapshot.modified_ticks,rule.min_age_days,now) {skip(plan,raw,"too_recent");continue;}
        let key=(snapshot.identity.volume,snapshot.identity.id);
        if let Some(index)=planning.found.get(&key) {if !plan.candidates[*index].rule_ids.contains(&rule.id) {plan.candidates[*index].rule_ids.push(rule.id.clone());}continue;}
        let candidate=Candidate {path:raw.into(),snapshot:snapshot.clone(),rule_ids:vec![rule.id.clone()]};
        let encoded_bytes=serde_json::to_vec(&candidate)?.len();
        if plan.candidates.len()>=MAX_CANDIDATES || planning.candidate_bytes+encoded_bytes>8*1024*1024 {plan.truncated=true;continue;}
        planning.candidate_bytes+=encoded_bytes;
        planning.found.insert(key,plan.candidates.len());
        plan.allocated_bytes_estimate=plan.allocated_bytes_estimate.saturating_add(snapshot.allocated_bytes);
        plan.candidates.push(candidate);
    }Ok(())
}

pub fn preview(store:&Store,safety:&Safety,ids:&[String],cancelled:&dyn Fn()->bool)->Result<Plan> {
    let rules=ids.iter().map(|id|store.cleanup_rule(id)).collect::<Result<_>>()?;
    let plan=plan_rules(rules,safety,chrono::Utc::now().timestamp(),cancelled)?;
    store.save_cleanup_plan(&plan)?;Ok(plan)
}
pub fn validate_plan(store:&Store,safety:&Safety,plan:&Plan,now:i64)->Result<()> {
    if now>=plan.expires_at {return Err(fault("plan_expired","Preview expired; preview again"));}
    for rule in &plan.rules {
        if store.cleanup_rule(&rule.id)?.revision!=rule.revision {return Err(fault("revision_conflict","Rule changed after preview"));}
        safety.validate(rule)?;
        ensure!(windows::volume_id(&rule.directory)?==rule.volume_id,"Volume identity mismatch");
    }Ok(())
}
pub fn set_enabled(store:&Store,safety:&Safety,id:&str,revision:u64,plan_id:Option<&str>,enabled:bool)->Result<Rule> {
    let mut rule=store.cleanup_rule(id)?;
    if rule.revision!=revision {return Err(fault("revision_conflict","Rule changed; read it again"));}
    if enabled {
        let plan=store.cleanup_plan(plan_id.context("Enabling requires a fresh preview plan")?)?;
        validate_plan(store,safety,&plan,chrono::Utc::now().timestamp())?;
        ensure!(plan.rules.iter().any(|r|r.id==id && r.revision==revision),"Preview does not include this rule revision");
        ensure!(!plan.truncated,"Preview was truncated; narrow the rule before enabling automatic cleanup");
    }
    rule.enabled=enabled;rule.revision+=1;store.save_cleanup_rule(&rule,revision)?;Ok(rule)
}

/// Caller holds the operation lock for planning, deletion and any verification.
/// Retrying an existing request only reads its run; it never replays items.
pub fn execute_plan(store:&Store,safety:&Safety,plan:&Plan,request_id:&str,pre_scan:Option<i64>,automatic_date:Option<String>,cancelled:&dyn Fn()->bool)->Result<Run> {
    ensure!(!request_id.is_empty() && request_id.len()<=128,"Request ID must contain 1-128 UTF-8 bytes");
    if let Some(run)=store.cleanup_request(request_id)? {
        if run.plan_id!=plan.plan_id {return Err(fault("request_conflict","Request ID already belongs to another plan"));}
        return Ok(run);
    }
    validate_plan(store,safety,plan,chrono::Utc::now().timestamp())?;
    store.cleanup_durable()?;
    let root=&plan.rules[0].directory[..3];
    let mut run=Run {run_id:new_id(),request_id:request_id.into(),plan_id:plan.plan_id.clone(),rules:plan.rules.clone(),
        volume_id:plan.rules[0].volume_id.clone(),pre_scan,post_scan:None,automatic_date,
        started_at:chrono::Utc::now().timestamp(),finished_at:None,state:"running".into(),verification:"not_needed".into(),
        deleted:0,skipped:0,failed:0,unknown:0,allocated_bytes_estimate:0,
        free_before:crate::scanner::disk_space(root).ok().map(|v|v.1),free_after:None,free_observed_at:None,
        details_trimmed:plan.candidate_count>MAX_DETAILS,message:None};
    store.start_cleanup_run(&run)?;
    let mut seen=HashSet::new();
    for (seq,candidate) in plan.candidates.iter().enumerate() {
        if cancelled() {run.state="cancelled".into();break;}
        let key=(candidate.snapshot.identity.volume,candidate.snapshot.identity.id);
        if !seen.insert(key) {continue;}
        let permitted=plan.rules.iter().any(|r| {
            let path=normal_path(&candidate.path);let directory=normal_path(&r.directory);
            if let (Ok(path),Ok(directory))=(path,directory) {
                candidate.rule_ids.contains(&r.id) && within(&path,&directory) && path!=directory
                    && (r.recursive || Path::new(&path).parent()==Some(Path::new(&directory)))
                    && Path::new(&candidate.path).file_name().and_then(|n|n.to_str()).is_some_and(|name|r.patterns.iter().any(|p|filename_matches(name,p)))
                    && candidate.snapshot.identity.volume_id()==r.volume_id
                    && old_enough(candidate.snapshot.modified_ticks,r.min_age_days,chrono::Utc::now().timestamp())
            } else {false}
        });
        let mut item=RunItem {path:candidate.path.clone(),state:"pending".into(),reason:None,allocated_bytes:candidate.snapshot.allocated_bytes};
        run.verification="pending".into();
        store.record_cleanup_item(&run,seq,&item)?; // durable intent precedes any filesystem side effect
        let deletion=if permitted {windows::delete_verified(&candidate.path,&candidate.snapshot)}
            else {Err(fault("scope_changed","Candidate is outside the preview rule scope"))};
        match deletion {
            Ok(())=>{item.state="deleted".into();run.deleted+=1;run.allocated_bytes_estimate=run.allocated_bytes_estimate.saturating_add(item.allocated_bytes);}
            Err(e)=>{
                let reason=format!("{e:#}");
                if reason.starts_with("Delete verified file handle") {item.state="failed".into();run.failed+=1;}
                else {item.state="skipped".into();run.skipped+=1;}
                item.reason=Some(reason);
            }
        }
        // If this write fails, the durable pending row is intentionally left for recovery.
        store.record_cleanup_item(&run,seq,&item)?;
    }
    if run.state=="running" {run.state=if run.failed>0 || run.skipped>0 || plan.truncated {"partial"} else {"completed"}.into();}
    run.verification=if run.deleted>0 || run.unknown>0 {"pending"} else {"not_needed"}.into();
    run.finished_at=Some(chrono::Utc::now().timestamp());
    run.free_after=crate::scanner::disk_space(root).ok().map(|v|v.1);
    run.free_observed_at=Some(chrono::Local::now().to_rfc3339());
    if plan.truncated {run.message=Some("Candidate list was truncated; only previewed objects were processed".into());}
    store.update_cleanup_run(&run)?;
    Ok(run)
}
