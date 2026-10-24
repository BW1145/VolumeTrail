//! Repeatable isolated scope/SQLite measurements; no real user directories.
use anyhow::Result;
use std::{fs,time::Instant};
use volumetrail::{cleanup::*,model::*,store::Store};
use ::windows::Win32::System::{ProcessStatus::{GetProcessMemoryInfo,PROCESS_MEMORY_COUNTERS},Threading::GetCurrentProcess};

fn bytes(data:&std::path::Path)->u64 {
    fs::read_dir(data).unwrap().filter_map(|e|e.ok()).filter_map(|e|e.metadata().ok()).map(|m|m.len()).sum()
}
fn logical(data:&std::path::Path)->u64 {let db=rusqlite::Connection::open_with_flags(data.join("history.db"),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let pages:u64=db.query_row("PRAGMA page_count",[],|r|r.get(0)).unwrap();let size:u64=db.query_row("PRAGMA page_size",[],|r|r.get(0)).unwrap();pages*size}
fn peak()->usize {let mut m=PROCESS_MEMORY_COUNTERS {cb:std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,..Default::default()};let size=m.cb;unsafe {GetProcessMemoryInfo(GetCurrentProcess(),&mut m,size)}.unwrap();m.PeakWorkingSetSize}
fn main()->Result<()> {
    let mode=std::env::args().nth(1).unwrap_or_else(||"no-rules".into());
    let temp=tempfile::tempdir()?;let data=temp.path().join("data");let scope=temp.path().join("scoped samples");fs::create_dir(&data)?;fs::create_dir(&scope)?;
    let mut store=Store::open(&data.join("history.db"))?;
    if mode=="baseline" || mode=="no-rules" {
        let file=|i:u64,size:u64|Mutation::Upsert(Entry {id:i+30,parent:5,name:format!("sample-{i}.bin"),is_dir:false,logical:size,allocated:size,modified:0,attributes:0,links:vec![]});
        let mut scan=ScanOutcome {checkpoint:Checkpoint {volume_id:"bench-volume".into(),journal_id:1,next_usn:10,root_id:5},mode:ScanMode::Full,
            total:1_000_000_000,free:500_000_000,sampled_at:"2026-10-21T09:00:00+00:00".into(),pending:vec![],warnings:vec![]};
        store.publish("E:\\",&(0..20000).map(|i|file(i,4096)).collect::<Vec<_>>(),&scan)?;
        let before=bytes(&data);let logical_before=logical(&data);let start=Instant::now();let mut samples=Vec::new();
        for i in 0..30 {
            scan.mode=ScanMode::Incremental;scan.checkpoint.next_usn+=10;scan.sampled_at=format!("2026-10-21T10:{i:02}:00+00:00");
            let begin=Instant::now();let id=store.publish("E:\\",&[file(i,8192)],&scan)?;
            if mode=="no-rules" {
                store.recover_cleanup_runs()?;store.reconcile_cleanup_after_scan("bench-volume",id)?;
                assert!(store.cleanup_rules()?.is_empty());store.prune_cleanup(chrono::Utc::now().timestamp())?;
            }
            store.compact_history()?;samples.push(begin.elapsed().as_micros() as u64);
        }
        println!("{}",serde_json::json!({"mode":mode,"indexed_files":20000,"incremental_commits":30,"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,
            "samples_us":samples,"peak_working_set_bytes":peak(),"database_files_before_bytes":before,"database_files_after_bytes":bytes(&data),"database_delta_bytes":i128::from(bytes(&data))-i128::from(before),"database_logical_delta_bytes":i128::from(logical(&data))-i128::from(logical_before),"raw_usn_scan":false}));
    } else {
        for i in 0..1000 {fs::write(scope.join(format!("sample-{i}.dmp")),vec![0u8;4096])?;}
        let safety=Safety::for_data(&data)?;let rule=save_rule(&store,&safety,Rule {name:"dedicated benchmark".into(),directory:scope.to_str().unwrap().into(),patterns:vec!["*.dmp".into()],..Default::default()})?;
        let before=bytes(&data);let logical_before=logical(&data);let start=Instant::now();let plan=preview(&store,&safety,&[rule.id],&||false)?;let preview_ms=start.elapsed().as_secs_f64()*1000.0;
        let (execute_ms,deleted)=if mode=="execute" {let start=Instant::now();let run=execute_plan(&store,&safety,&plan,"benchmark",None,None,&||false)?;(Some(start.elapsed().as_secs_f64()*1000.0),run.deleted)} else {(None,0)};
        println!("{}",serde_json::json!({"mode":mode,"scoped_files":1000,"preview_ms":preview_ms,"execute_ms":execute_ms,"deleted":deleted,
            "peak_working_set_bytes":peak(),"database_files_before_bytes":before,"database_files_after_bytes":bytes(&data),"database_delta_bytes":i128::from(bytes(&data))-i128::from(before),"database_logical_delta_bytes":i128::from(logical(&data))-i128::from(logical_before),"candidate_bytes_estimate":plan.allocated_bytes_estimate,
            "raw_usn_scan":false,"verification":"not measured; requires isolated elevated NTFS fixture"}));
    }
    Ok(())
}
