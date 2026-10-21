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
    if rule.id.is_empty() {ensure!(expected==0,"New rules must use revision 0");rule.id=new_id();}
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
    let mut found:HashMap<(u64,[u8;16]),usize>=HashMap::new();
    let mut visited=0usize;
    for rule in &rules {
        safety.validate(rule)?;
        let guard=windows::DirectoryGuard::open(&rule.directory)?;
        ensure!(guard.volume_id()==rule.volume_id,"Volume identity mismatch");
        walk(&rule.directory,rule,&mut plan,&mut found,&mut visited,now,cancelled)?;
    }
    plan.candidate_count=plan.candidates.len();
    plan.skip_details_truncated=plan.skipped_count>plan.skipped.len() as u64;
    Ok(plan)
}

fn skip(plan:&mut Plan,path:&str,reason:&str) {
    plan.skipped_count+=1;*plan.skip_reasons.entry(reason.into()).or_default()+=1;
    if plan.skipped.len()<MAX_DETAILS {plan.skipped.push(Skip {path:path.into(),reason:reason.into()});}
}
fn walk(path:&str,rule:&Rule,plan:&mut Plan,found:&mut HashMap<(u64,[u8;16]),usize>,visited:&mut usize,now:i64,cancelled:&dyn Fn()->bool)->Result<()> {
    if cancelled() {return Err(fault("cancelled","Preview cancelled"));}
    // Pin and validate this directory for the entire enumeration; child inspect pins its own chain.
    let _guard=match windows::DirectoryGuard::open(path) {Ok(g)=>g,Err(e)=>{skip(plan,path,&format!("directory_unavailable: {e:#}"));return Ok(());}};
    let entries=match fs::read_dir(format!(r"\\?\{}",path.strip_prefix(r"\\?\").unwrap_or(path))) {
        Ok(e)=>e,Err(e)=>{skip(plan,path,&format!("directory_unavailable: {e}"));return Ok(());}
    };
    for entry in entries {
        if cancelled() {return Err(fault("cancelled","Preview cancelled"));}
        *visited+=1;if *visited>200_000 {plan.truncated=true;break;}
        let entry=match entry {Ok(e)=>e,Err(e)=>{skip(plan,path,&format!("entry_unavailable: {e}"));continue;}};
        let raw=entry.path();let Some(raw)=raw.to_str() else {skip(plan,path,"non_unicode_path");continue;};
        let raw=raw.strip_prefix(r"\\?\").unwrap_or(raw);
        let metadata=match entry.metadata() {Ok(m)=>m,Err(e)=>{skip(plan,raw,&format!("metadata_unavailable: {e}"));continue;}};
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {skip(plan,raw,"reparse_point");continue;}
        if metadata.is_dir() {if rule.recursive {walk(raw,rule,plan,found,visited,now,cancelled)?;}continue;}
        let name=entry.file_name();let Some(name)=name.to_str() else {skip(plan,raw,"non_unicode_name");continue;};
        if !rule.patterns.iter().any(|p|filename_matches(name,p)) {skip(plan,raw,"filename_mismatch");continue;}
        let snapshot=match windows::inspect(raw) {Ok(s)=>s,Err(e)=>{skip(plan,raw,&format!("file_unavailable: {e:#}"));continue;}};
        if snapshot.identity.volume_id()!=rule.volume_id {skip(plan,raw,"volume_changed");continue;}
        if !old_enough(snapshot.modified_ticks,rule.min_age_days,now) {skip(plan,raw,"too_recent");continue;}
        let key=(snapshot.identity.volume,snapshot.identity.id);
        if let Some(index)=found.get(&key) {if !plan.candidates[*index].rule_ids.contains(&rule.id) {plan.candidates[*index].rule_ids.push(rule.id.clone());}continue;}
        if plan.candidates.len()>=MAX_CANDIDATES {plan.truncated=true;continue;}
        found.insert(key,plan.candidates.len());
        plan.allocated_bytes_estimate=plan.allocated_bytes_estimate.saturating_add(snapshot.allocated_bytes);
        plan.candidates.push(Candidate {path:raw.into(),snapshot,rule_ids:vec![rule.id.clone()]});
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
