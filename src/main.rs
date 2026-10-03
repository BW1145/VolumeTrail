#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Result, ensure};
use volumetrail::{scanner::{Volume, read_entry}, startup, ui::VolumeTrailApp, worker};
use std::env;

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let action = args.next().unwrap_or_else(|| "ui".into());
    if action == "query" { return volumetrail::query::run(args.collect()); }
    if action == "ui" {
        let data = worker::data_dir()?;
        eframe::run_native(
            "盘迹 · VolumeTrail",
            eframe::NativeOptions {
                viewport: eframe::egui::ViewportBuilder::default()
                    .with_inner_size([1050.0, 720.0])
                    .with_min_inner_size([760.0, 520.0])
                    .with_icon(app_icon()),
                ..Default::default()
            },
            Box::new(move |cc| Ok(Box::new(VolumeTrailApp::new(&cc.egui_ctx, data)?))),
        ).map_err(|error| anyhow::anyhow!("Open window: {error}"))?;
        return Ok(());
    }
    let letter = args.next().unwrap_or_else(|| "C".into());
    ensure!(letter.len() == 1 && letter.as_bytes()[0].is_ascii_alphabetic(), "Expected one drive letter");
    let root = format!("{}:\\", letter.to_ascii_uppercase());
    match action.as_str() {
        "probe" => {
            let volume = Volume::open(letter.chars().next().unwrap())?;
            let journal = volume.journal()?;
            let mut reader = volume.ntfs_reader()?;
            let ntfs = ntfs::Ntfs::new(&mut reader)?;
            println!("{root} NTFS {:016x}, journal {:016x}, next USN {}", ntfs.serial_number(), journal.id, journal.next_usn);
        }
        "probe-enum" => {
            let volume = Volume::open(letter.chars().next().unwrap())?;
            let journal = volume.journal()?;
            let mut reader = volume.ntfs_reader()?;
            let ntfs = ntfs::Ntfs::new(&mut reader)?;
            let mut enumerated = 0u64;
            let mut parsed = 0u64;
            let mut unavailable = 0u64;
            volume.enumerate(journal.next_usn, Some(1024), |record| {
                enumerated += 1;
                if (record.id & 0x0000_ffff_ffff_ffff) >= 16 {
                    if read_entry(&ntfs, &mut reader, record.id)?.is_some() {
                        parsed += 1;
                    } else {
                        unavailable += 1;
                    }
                }
                Ok(())
            })?;
            println!("{root} checked {enumerated} MFT records; parsed {parsed}; unavailable {unavailable}");
        }
        "scan" => {
            worker::run_scan(letter.chars().next().unwrap())?;
            println!("{root} scan completed");
        }
        "scan-auto" => {
            worker::run_auto()?;
        }
        "startup-status" => {
            println!("{}", serde_json::to_string_pretty(&startup::status()?)?);
        }
        "startup-enable" => {
            startup::install()?;
        }
        "startup-disable" => {
            startup::remove()?;
        }
        "status" => {
            let data = worker::data_dir()?;
            match worker::read_status(&data, letter.chars().next().unwrap()) {
                Some(status) => println!("{}", serde_json::to_string_pretty(&status)?),
                None => println!("No scan status for {root}"),
            }
        }
        "stop" => {
            let data = worker::data_dir()?;
            worker::stop_scan(&data, letter.chars().next().unwrap())?;
            println!("Stop requested for {root}");
        }
        "reports" => {
            let data = worker::data_dir()?;
            let store = volumetrail::store::Store::open(&worker::store_path(&data))?;
            for report in store.reports()? {
                println!("{} {} {}: {} files, {} allocated bytes", report.id, report.root, report.finished, report.file_count, report.allocated);
            }
        }
        "stats" => {
            let data = worker::data_dir()?;
            let db = rusqlite::Connection::open_with_flags(
                worker::store_path(&data), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let page_size: u64 = db.query_row("PRAGMA page_size", [], |row| row.get(0))?;
            let pages: u64 = db.query_row("PRAGMA page_count", [], |row| row.get(0))?;
            let free: u64 = db.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
            let auto_vacuum: u32 = db.query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
            println!("database {} bytes; free pages {} bytes; auto_vacuum {}", pages * page_size, free * page_size, auto_vacuum);
            for table in ["nodes", "folders", "staging", "changes", "scans"] {
                let count: u64 = db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))?;
                println!("{table}: {count} rows");
            }
            let mut stmt = db.prepare("SELECT name,SUM(pgsize),SUM(payload) FROM dbstat GROUP BY name ORDER BY SUM(pgsize) DESC")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                println!("{}: {} bytes pages, {} bytes payload", row.get::<_, String>(0)?, row.get::<_, u64>(1)?, row.get::<_, u64>(2)?);
            }
        }
        "maintain" => {
            let size = worker::maintain(letter.chars().next().unwrap())?;
            println!("Data directory uses {size} bytes");
        }
        _ => anyhow::bail!("Use probe, probe-enum, scan, scan-auto, startup-status, startup-enable, startup-disable, status, stop, reports, stats or maintain"),
    }
    Ok(())
}

fn app_icon() -> eframe::egui::IconData {
    let mut rgba = Vec::with_capacity(64 * 64 * 4);
    for y in 0u32..64 {
        for x in 0u32..64 {
            let dx = if x < 11 { 11 - x } else { x.saturating_sub(52) };
            let dy = if y < 11 { 11 - y } else { y.saturating_sub(52) };
            if dx * dx + dy * dy > 121 {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let bar = (14..=20).contains(&x) && (32..=46).contains(&y)
                || (28..=34).contains(&x) && (22..=46).contains(&y)
                || (42..=48).contains(&x) && (27..=46).contains(&y);
            rgba.extend_from_slice(if bar { &[255, 255, 255, 255] } else { &[17, 127, 112, 255] });
        }
    }
    eframe::egui::IconData { rgba, width: 64, height: 64 }
}
