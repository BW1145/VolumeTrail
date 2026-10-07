use crate::{store::Store, worker};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

pub fn run(args: Vec<String>) -> Result<()> {
    let (command, rest) = args.split_first().context("Use query summary|growth|folder|largest")?;
    let mut drive = 'C';
    let mut limit = 50u64;
    let mut offset = 0u64;
    let mut from = None;
    let mut to = None;
    let mut depth = 5usize;
    let mut depth_explicit = false;
    let mut path = None;
    let mut kind = "folders".to_owned();
    let mut json_output = false;
    let mut iter = rest.iter();
    while let Some(flag) = iter.next() {
        if flag == "--json" { json_output = true; continue; }
        let value = iter.next().with_context(|| format!("Missing value for {flag}"))?;
        match flag.as_str() {
            "--drive" => {
                ensure!(value.len() == 1 && value.as_bytes()[0].is_ascii_alphabetic(), "Expected one drive letter");
                drive = value.chars().next().unwrap().to_ascii_uppercase();
            }
            "--limit" => limit = value.parse()?,
            "--offset" => offset = value.parse()?,
            "--from" => from = Some(value.parse::<i64>()?),
            "--to" => to = Some(value.parse::<i64>()?),
            "--depth" => { depth = value.parse()?; depth_explicit = true; }
            "--path" => path = Some(value.clone()),
            "--kind" => kind = value.clone(),
            _ => bail!("Unknown option {flag}"),
        }
    }
    ensure!((1..=5000).contains(&limit) && (1..=32).contains(&depth), "Limit must be 1-5000 and depth 1-32");
    let root = format!("{drive}:\\");
    if command == "growth" && !depth_explicit {
        if let Some(folder) = path.as_deref() {
            depth = folder.trim_end_matches('\\').split('\\').skip(1).count() + 1;
        }
    }
    let data = worker::existing_data_dir()?;
    let store = Store::open_read_only(&worker::store_path(&data))?;
    let checkpoint = store.checkpoint(&root)?.with_context(|| format!("No scan for {root}"))?;
    let volume = &checkpoint.volume_id;
    let indexed_at = store.index_state(volume)?.map(|(_, _, _, at)| at);
    let reports: Vec<_> = store.reports()?.into_iter().filter(|report| report.volume == *volume).collect();
    let result = match command.as_str() {
        "summary" => {
            let items = reports.iter().skip(offset as usize).take(limit as usize).map(|report| json!({
                "id": report.id, "drive": report.root, "volume": report.volume,
                "sampled": report.sampled, "finished": report.finished, "mode": report.mode,
                "total_bytes": report.total, "free_bytes": report.free,
                "used_bytes": report.total.saturating_sub(report.free),
                "indexed_bytes": report.allocated, "file_count": report.file_count,
                "detail_available": report.details, "aggregate_quality": report.aggregate,
                "performance": report.performance,
            })).collect::<Vec<_>>();
            json!({"command":"summary","drive":root,"total":reports.len(),"offset":offset,"limit":limit,"items":items})
        }
        "growth" => {
            let end = to.or_else(|| reports.first().map(|report| report.id)).context("No scan points")?;
            let start = from.or_else(|| reports.iter().find(|report| report.id < end).map(|report| report.id))
                .context("At least two scan points are needed")?;
            ensure!(start < end && reports.iter().any(|r| r.id == start) && reports.iter().any(|r| r.id == end),
                "Choose two existing scans on the same volume, earliest first");
            let (intervals, complete, available, details) = store.coverage(volume, start, end)?;
            let mut items = store.folder_growth(volume, start, end, depth)?;
            if let Some(prefix) = path.as_deref() {
                let prefix = prefix.trim_end_matches('\\').to_ascii_lowercase();
                items.retain(|item| item.path.eq_ignore_ascii_case(&prefix)
                    || (prefix.len() == 2 && item.path == "[unresolved]")
                    || item.path.to_ascii_lowercase().starts_with(&format!("{prefix}\\")));
            }
            let unresolved = items.iter().find(|item| item.path == "[unresolved]").map(|item| json!({
                "allocated_delta_bytes":item.allocated_delta,"events":item.changes}));
            let total = items.len();
            let items = items.into_iter().skip(offset as usize).take(limit as usize).map(|item| json!({
                "path":item.path,"allocated_delta_bytes":item.allocated_delta,
                "moved_delta_bytes":item.moved_delta,"events":item.changes
            })).collect::<Vec<_>>();
            json!({"command":"growth","drive":root,"volume":volume,"from":start,"to":end,
                "intervals":intervals,"complete_aggregate_intervals":complete,
                "available_aggregate_intervals":available,"detail_intervals":details,
                "depth":depth,"total":total,"offset":offset,"limit":limit,"items":items,"unresolved":unresolved})
        }
        "folder" => {
            let folder_path = path.unwrap_or_else(|| root.clone());
            ensure!(folder_path.to_ascii_uppercase().starts_with(&root), "Folder must be on {root}");
            let id = store.find_folder(volume, checkpoint.root_id, &folder_path)?;
            let stats = id.map(|id| store.folder_stats(volume, id)).transpose()?;
            let items = if let Some(id) = id { store.children_page(volume, id, limit, offset)? } else { Vec::new() };
            let items = items.into_iter().map(|item| json!({
                "name":item.name,"directory":item.is_dir,"logical_bytes":item.logical,
                "allocated_bytes":item.allocated,"files":item.files,
            })).collect::<Vec<_>>();
            let comparison = if from.is_some() || to.is_some() {
                let end = to.or_else(|| reports.first().map(|r| r.id)).context("No scan points")?;
                let start = from.or_else(|| reports.iter().find(|r| r.id < end).map(|r| r.id))
                    .context("At least two scan points are needed")?;
                ensure!(start < end && reports.iter().any(|r| r.id == start) && reports.iter().any(|r| r.id == end),
                    "Choose two existing scans on the same volume, earliest first");
                let breakdown = store.folder_breakdown(volume, start, end, &folder_path)?;
                let mut child_growth = breakdown.children;
                child_growth.sort_by(|a, b| b.allocated_delta.abs().cmp(&a.allocated_delta.abs()));
                let children = child_growth.into_iter()
                    .take(limit as usize).map(|item| json!({"path":item.path,
                        "allocated_delta_bytes":item.allocated_delta,"moved_delta_bytes":item.moved_delta,
                        "events":item.changes})).collect::<Vec<_>>();
                let types = store.extensions_between_path(volume, start, end, &folder_path)?;
                let extensions = types.items.into_iter().take(10).map(|(extension, delta, events)| json!({
                        "extension":extension,"allocated_delta_bytes":delta,"events":events})).collect::<Vec<_>>();
                Some(json!({"from":start,"to":end,"coverage":store.coverage(volume,start,end)?,
                    "allocated_delta_bytes":breakdown.allocated_delta,
                    "direct_delta_bytes":breakdown.direct_delta,"moved_delta_bytes":breakdown.moved_delta,
                    "children":children,"extensions":extensions,
                    "extension_intervals":types.intervals,"complete_extension_intervals":types.complete_intervals}))
            } else { None };
            json!({"command":"folder","drive":root,"volume":volume,"path":folder_path,
                "indexed_at":indexed_at,"current_index_present":id.is_some(),
                "logical_bytes":stats.map(|value| value.0),
                "allocated_bytes":stats.map(|value| value.1),"files":stats.map(|value| value.2),"comparison":comparison,
                "offset":offset,"limit":limit,"items":items})
        }
        "largest" => {
            ensure!(kind == "folders" || kind == "files", "Kind must be folders or files");
            let items = store.largest(volume, &root, checkpoint.root_id, kind == "folders", limit, offset)?
                .into_iter().map(|item| json!({"path":item.path,"allocated_bytes":item.allocated,
                    "logical_bytes":item.logical,"files":item.files,"modified":item.modified})).collect::<Vec<_>>();
            json!({"command":"largest","drive":root,"volume":volume,"kind":kind,
                "indexed_at":indexed_at,"offset":offset,"limit":limit,"items":items})
        }
        _ => bail!("Use query summary|growth|folder|largest"),
    };
    if json_output { println!("{}", serde_json::to_string_pretty(&result)?); }
    else { print_text(&result); }
    Ok(())
}

fn print_text(value: &Value) {
    let header = value.get("command").and_then(Value::as_str).unwrap_or("query");
    println!("{header} {}", value["drive"].as_str().unwrap_or(""));
    if header == "growth" {
        println!("{} -> {}; folder coverage {}/{} (complete {}), file detail {}/{}",
            value["from"],value["to"],value["available_aggregate_intervals"],value["intervals"],
            value["complete_aggregate_intervals"],value["detail_intervals"],value["intervals"]);
    }
    if header == "folder" {
        if value["current_index_present"] == false {
            println!("{}  not in the current index; current size unavailable", value["path"].as_str().unwrap_or(""));
        } else {
            println!("{}  allocated={}  files={}", value["path"].as_str().unwrap_or(""),
                value["allocated_bytes"], value["files"]);
        }
        if !value["comparison"].is_null() {
            println!("change {} -> {}  coverage={}", value["comparison"]["from"],
                value["comparison"]["to"], value["comparison"]["coverage"]);
            println!("net={} bytes  direct={} bytes  moved={} bytes", value["comparison"]["allocated_delta_bytes"],
                value["comparison"]["direct_delta_bytes"], value["comparison"]["moved_delta_bytes"]);
        }
    }
    if let Some(items) = value["items"].as_array() {
        for item in items {
            match header {
                "summary" => println!("{} {} used={} free={} detail={} aggregate={}", item["id"],
                    item["sampled"].as_str().unwrap_or(""),item["used_bytes"],item["free_bytes"],
                    item["detail_available"],item["aggregate_quality"]),
                "growth" => println!("{:+} bytes  moved={:+}  {}", item["allocated_delta_bytes"].as_i64().unwrap_or(0),
                    item["moved_delta_bytes"].as_i64().unwrap_or(0),item["path"].as_str().unwrap_or("")),
                "folder" => println!("{} bytes  {}", item["allocated_bytes"],item["name"].as_str().unwrap_or("")),
                _ => println!("{} bytes  {}", item["allocated_bytes"],item["path"].as_str().unwrap_or("")),
            }
        }
    }
}
