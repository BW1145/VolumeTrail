use super::*;
use crate::{store::Store,worker};
use anyhow::{Context,Result,ensure,bail};
use serde_json::{Value,json};
use std::{collections::HashMap,fs,io::Read};

pub fn run(args:Vec<String>)->Result<()> {
    let machine=args.iter().any(|a|a=="--json");
    let command=args.first().cloned().unwrap_or_default();
    let (value,code)=match execute(args) {
        Ok(mut value)=>{
            value["schema_version"]=json!(1);value["ok"]=json!(true);
            value["command"]=json!(format!("cleanup {command}"));
            let state=value["run"]["state"].as_str().unwrap_or("completed").to_owned();
            let unverified=value["run"]["verification"].as_str().is_some_and(|s|!["verified","verified_by_later_scan","not_needed"].contains(&s));
            let code=match state.as_str() {"partial"=>2,"cancelled"=>3,"interrupted"=>4,"running"=>0,_=>if unverified {2} else {0}};
            value["status"]=json!(if unverified && state=="completed" {"completed_unverified"} else {&state});
            (value,code)
        }
        Err(error)=>{
            let code=error.downcast_ref::<Fault>().map_or("cleanup_failed",|e|e.code);
            (json!({"schema_version":1,"ok":false,"command":format!("cleanup {command}"),"status":"failed","data":null,
                "error":{"code":code,"message":format!("{error:#}")}}),if code.ends_with("_conflict") || code=="plan_expired" {4} else {1})
        }
    };
    if machine {println!("{}",serde_json::to_string(&value)?);}
    else {println!("{}",serde_json::to_string_pretty(&value)?);}
    if code!=0 {std::process::exit(code);}Ok(())
}

fn execute(args:Vec<String>)->Result<Value> {
    let (command,rest)=args.split_first().context("Use cleanup list|get|save|preview|enable|pause|delete|execute|runs|status|cancel")?;
    let mut options=HashMap::<String,String>::new();let mut acknowledge_delete=false;let mut acknowledge_auto=false;
    let mut iter=rest.iter();
    while let Some(flag)=iter.next() {
        match flag.as_str() {
            "--json"=>continue,
            "--confirm-permanent-delete"=>{acknowledge_delete=true;continue;}
            "--confirm-automatic-rule"=>{acknowledge_auto=true;continue;}
            "--id"|"--ids"|"--input"|"--revision"|"--plan"|"--request"|"--drive"|"--limit"|"--offset"=>{}
            _=>bail!("Unknown cleanup option {flag}"),
        }
        let value=iter.next().with_context(||format!("Missing value for {flag}"))?;
        ensure!(options.insert(flag.clone(),value.clone()).is_none(),"Repeated option {flag}");
    }
    let required=|key:&str|options.get(key).with_context(||format!("Missing {key}"));
    let data=worker::existing_data_dir()?;
    if command=="execute" {
        ensure!(acknowledge_delete,"Direct deletion is irreversible; review the plan and provide --confirm-permanent-delete only after user authorization");
        return Ok(json!({"run":crate::worker_cleanup::execute(&data,required("--plan")?,required("--request")?)?}));
    }
    if ["list","get","runs","status"].contains(&command.as_str()) {
        let store=Store::open_read_only(&worker::store_path(&data))?;
        return match command.as_str() {
            "list"=>Ok(json!({"rules":store.cleanup_rules()?})),
            "get"=>Ok(json!({"rule":store.cleanup_rule(required("--id")?)?})),
            "status"=>{
                let id=required("--id")?;let mut run=store.cleanup_run(id)?;let items=store.cleanup_items(id)?;
                let active=worker::operation_active_read_only(&data);
                if run.state=="running" && !active {
                    run.state="interrupted".into();run.unknown+=items.iter().filter(|i|i.state=="pending").count() as u64;
                    if run.unknown>0 || run.deleted>0 {run.verification="pending".into();}
                    run.message=Some("No operation lock is held; interrupted state inferred without replay or database writes".into());
                }
                Ok(json!({"run":run,"items":items,"operation_active":active}))
            }
            _=>{
                let limit=options.get("--limit").map(|s|s.parse::<u64>()).transpose()?.unwrap_or(50);
                let offset=options.get("--offset").map(|s|s.parse::<u64>()).transpose()?.unwrap_or(0);
                ensure!((1..=500).contains(&limit) && offset<=i64::MAX as u64,"Limit must be 1-500; offset must fit i64");
                let volume=options.get("--drive").map(|d|->Result<_> {ensure!(d.len()==1 && d.as_bytes()[0].is_ascii_alphabetic(),"Expected one drive letter");
                    Ok(store.checkpoint(&format!("{}:\\",d.to_uppercase()))?.map(|cp|cp.volume_id))}).transpose()?.flatten();
                if options.contains_key("--drive") && volume.is_none() {return Ok(json!({"total":0,"offset":offset,"limit":limit,"has_more":false,"items":[]}));}
                let (total,items)=store.cleanup_runs(volume.as_deref(),limit,offset)?;
                Ok(json!({"total":total,"offset":offset,"limit":limit,"has_more":offset.saturating_add(limit)<total,"items":items}))
            }
        };
    }
    if command=="cancel" {
        let run=Store::open_read_only(&worker::store_path(&data))?.cleanup_run(required("--id")?)?;
        let active=worker::operation_active_read_only(&data);
        let cancel=active && (run.state=="running" || run.verification=="pending");
        if cancel {let letter=run.rules[0].directory.chars().next().unwrap();worker::stop_scan(&data,letter)?;}
        return Ok(json!({"run_id":run.run_id,"cancel_requested":cancel}));
    }
    ensure!(["save","preview","enable","pause","delete"].contains(&command.as_str()),"Unknown cleanup command");
    fs::create_dir_all(&data)?;
    let _lock=worker::global_scan_lock(&data)?;
    let store=Store::open(&worker::store_path(&data))?;let safety=Safety::for_data(&data)?;
    match command.as_str() {
        "save"=>{
            let input=required("--input")?;let mut bytes=Vec::new();
            if input=="-" {std::io::stdin().take(1024*1024+1).read_to_end(&mut bytes)?;}
            else {ensure!(fs::metadata(input)?.len()<=1024*1024,"Rule JSON must be at most 1 MiB");bytes=fs::read(input)?;}
            ensure!(bytes.len()<=1024*1024,"Rule JSON must be at most 1 MiB");
            let bytes=bytes.strip_prefix(&[0xef,0xbb,0xbf]).unwrap_or(&bytes);
            let rule:Rule=serde_json::from_slice(bytes).context("Parse UTF-8 rule JSON")?;
            Ok(json!({"rule":save_rule(&store,&safety,rule)?}))
        }
        "preview"=>{
            let ids=options.get("--ids").or_else(||options.get("--id")).context("Provide --id or comma-separated --ids")?
                .split(',').map(str::to_owned).collect::<Vec<_>>();
            Ok(json!({"plan":preview(&store,&safety,&ids,&||false)?}))
        }
        "enable"|"pause"=>{
            let enabled=command=="enable";ensure!(!enabled || acknowledge_auto,"Review the continuous scope and provide --confirm-automatic-rule only after user authorization");
            Ok(json!({"rule":set_enabled(&store,&safety,required("--id")?,required("--revision")?.parse()?,options.get("--plan").map(String::as_str),enabled)?}))
        }
        "delete"=>{store.delete_cleanup_rule(required("--id")?,required("--revision")?.parse()?)?;Ok(json!({"deleted_rule":required("--id")?}))}
        _=>unreachable!(),
    }
}
