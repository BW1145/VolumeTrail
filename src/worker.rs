use crate::{config::AppConfig, model::{ScanCounters, ScanPerformance, ScanProgress}, scanner::scan_volume, store::Store};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    fs::{File, OpenOptions},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    time::Instant,
};
use windows::{Win32::{Foundation::{ERROR_PROCESS_MODE_ALREADY_BACKGROUND, FILETIME}, System::{ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS}, Threading::{GetCurrentProcess, GetProcessTimes, PROCESS_MODE_BACKGROUND_BEGIN, SetPriorityClass}}, UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_HIDE}}, core::{HRESULT, PCWSTR}};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScanStatus {
    pub root: String,
    pub phase: String,
    pub processed: u64,
    pub finished: bool,
    pub report_id: Option<i64>,
    pub error: Option<String>,
    #[serde(default)]
    pub warning: Option<String>,
    #[serde(default)]
    pub read_ms: Option<u64>,
    #[serde(default)]
    pub commit_ms: Option<u64>,
    #[serde(default)]
    pub maintenance_ms: Option<u64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub paused_ms: Option<u64>,
    #[serde(default)]
    pub performance: Option<ScanPerformance>,
}

pub fn data_dir() -> Result<PathBuf> {
    let exe = env::current_exe()?;
    let path = exe.parent().context("Executable has no parent directory")?.join("data");
    fs::create_dir_all(&path)?;
    Ok(path)
}

pub fn existing_data_dir() -> Result<PathBuf> {
    Ok(env::current_exe()?.parent().context("Executable has no parent directory")?.join("data"))
}

pub fn store_path(data: &Path) -> PathBuf { data.join("history.db") }

pub fn status_path(data: &Path, letter: char) -> PathBuf {
    data.join(format!("scan-{}.json", letter.to_ascii_uppercase()))
}

pub fn stop_path(data: &Path, letter: char) -> PathBuf {
    data.join(format!("stop-{}.flag", letter.to_ascii_uppercase()))
}

fn lock_path(data: &Path, letter: char) -> PathBuf {
    data.join(format!("scan-{}.lock", letter.to_ascii_uppercase()))
}

fn scan_lock(data: &Path, letter: char) -> Result<File> {
    OpenOptions::new().read(true).write(true).create(true).share_mode(0)
        .open(lock_path(data, letter)).context("Another scan is running")
}

fn global_scan_lock(data: &Path) -> Result<File> {
    OpenOptions::new().read(true).write(true).create(true).share_mode(0)
        .open(data.join("scan-all.lock")).context("Another disk is already being scanned")
}

pub fn is_running(data: &Path, letter: char) -> bool {
    scan_lock(data, letter).is_err()
}

pub fn is_any_running(data: &Path) -> bool {
    global_scan_lock(data).is_err() || ('A'..='Z').any(|letter| {
        lock_path(data, letter).exists() && is_running(data, letter)
    })
}

pub fn read_status(data: &Path, letter: char) -> Option<ScanStatus> {
    fs::read(status_path(data, letter)).ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn write_status(data: &Path, letter: char, status: &ScanStatus) -> Result<()> {
    fs::write(status_path(data, letter), serde_json::to_vec(status)?)?;
    Ok(())
}

pub fn stop_scan(data: &Path, letter: char) -> Result<()> {
    fs::write(stop_path(data, letter), b"stop")?;
    Ok(())
}

pub fn maintain(letter: char) -> Result<u64> {
    let data = data_dir()?;
    let _lock = global_scan_lock(&data)?;
    let mut store = Store::open(&store_path(&data))?;
    let budget = AppConfig::load(&data)?.budget_bytes();
    let size = store.prune(budget)?;
    if store.history_bytes()? <= budget {
        if let Some(mut status) = read_status(&data, letter) {
            if status.finished && status.error.is_none() {
                status.warning = None;
                write_status(&data, letter, &status)?;
            }
        }
    }
    Ok(size)
}

pub enum HistoryAction {
    ClearDetails(Vec<i64>),
    Delete(Vec<i64>),
    Prune(u64),
}

pub fn edit_history(data: &Path, action: HistoryAction) -> Result<String> {
    let _lock = global_scan_lock(data)?;
    let mut store = Store::open(&store_path(data))?;
    match action {
        HistoryAction::ClearDetails(ids) => {
            let count = store.clear_details(&ids)?;
            Ok(format!("已清理 {count} 条记录的文件明细，时间点和文件夹汇总仍保留"))
        }
        HistoryAction::Delete(ids) => {
            let count = store.delete_scans(&ids)?;
            Ok(format!("已删除 {count} 条扫描记录"))
        }
        HistoryAction::Prune(budget) => {
            store.prune(budget)?;
            Ok(format!("整理完成，历史明细和目录汇总占用 {} MiB", store.history_bytes()? / 1024 / 1024))
        }
    }
}

pub fn run_scan(letter: char) -> Result<()> {
    let data = data_dir()?;
    let budget = AppConfig::load(&data)?.budget_bytes();
    run_scan_configured(letter, &data, budget)
}

pub fn run_auto() -> Result<()> {
    let data = data_dir()?;
    let config = AppConfig::load(&data)?;
    let budget = config.budget_bytes();
    let _global_lock = global_scan_lock(&data)?;
    let mut failures = Vec::new();
    for letter in config.drives {
        let root = format!("{letter}:\\");
        if !Path::new(&root).exists() {
            write_status(&data, letter, &ScanStatus {
                root, phase: "已跳过".into(), finished: true,
                warning: Some("自动扫描时磁盘不可用".into()), ..Default::default()
            })?;
            continue;
        }
        if let Err(error) = run_scan_locked(letter, &data, budget) {
            failures.push(format!("{letter}: {error:#}"));
        }
    }
    ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(())
}

fn run_scan_configured(letter: char, data: &Path, budget: u64) -> Result<()> {
    let _global_lock = global_scan_lock(data)?;
    run_scan_locked(letter, data, budget)
}

fn run_scan_locked(letter: char, data: &Path, budget: u64) -> Result<()> {
    let started = Instant::now();
    let cpu_started = process_cpu_ms();
    ensure!(letter.is_ascii_alphabetic(), "Invalid drive letter");
    match unsafe { SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN) } {
        Ok(()) => {}
        Err(error) if error.code() == HRESULT::from_win32(ERROR_PROCESS_MODE_ALREADY_BACKGROUND.0) => {}
        Err(error) => return Err(error).context("Enter background scan mode"),
    }
    let letter = letter.to_ascii_uppercase();
    let _lock = scan_lock(data, letter)?;
    let stop = stop_path(data, letter);
    if stop.exists() { fs::remove_file(&stop)?; }
    let root = format!("{letter}:\\");
    let mut status = ScanStatus { root: root.clone(), phase: "开始扫描".into(), ..Default::default() };
    write_status(data, letter, &status)?;
    let mut pause_started: Option<Instant> = None;
    let mut paused_ms = 0u64;
    let mut counters = ScanCounters::default();
    let result = (|| {
        let mut store = Store::open(&store_path(data))?;
        store.set_cancel_path(stop.clone());
        let scan_started = Instant::now();
        let mut commit_started = None;
        let report = scan_volume(&root, &mut store, &|| stop.exists(), &mut |progress: ScanProgress| {
            if progress.phase == "系统繁忙，已暂停" {
                pause_started.get_or_insert_with(Instant::now);
            } else if let Some(start) = pause_started.take() {
                paused_ms += start.elapsed().as_millis() as u64;
            }
            if progress.phase == "提交索引" && commit_started.is_none() {
                status.read_ms = Some(scan_started.elapsed().as_millis() as u64);
                commit_started = Some(Instant::now());
            }
            println!("{} {}: {} records", chrono::Local::now().format("%H:%M:%S"), progress.phase, progress.processed);
            status.phase = progress.phase;
            status.processed = progress.processed;
            let _ = write_status(data, letter, &status);
        }, &mut counters)?;
        status.commit_ms = commit_started.map(|instant: Instant| instant.elapsed().as_millis() as u64);
        let warnings = store.report_warnings(report)?;
        if !warnings.is_empty() { status.warning = Some(warnings.join("；")); }
        status.phase = "整理历史".into();
        println!("{} 整理历史", chrono::Local::now().format("%H:%M:%S"));
        write_status(data, letter, &status)?;
        let maintenance_started = Instant::now();
        match store.prune(budget).and_then(|_| store.history_bytes()) {
            Ok(size) if size > budget => {
                append_warning(&mut status.warning, "历史汇总占用已超过预算".into());
            }
            Err(error) => {
                append_warning(&mut status.warning, format!("历史整理失败：{error:#}"));
            }
            _ => {}
        }
        status.maintenance_ms = Some(maintenance_started.elapsed().as_millis() as u64);
        Ok((report, store))
    })();
    if let Some(start) = pause_started.take() { paused_ms += start.elapsed().as_millis() as u64; }
    status.finished = true;
    status.duration_ms = Some(started.elapsed().as_millis() as u64);
    status.paused_ms = Some(paused_ms);
    let cpu_time_ms = cpu_started.zip(process_cpu_ms()).map(|(before, after)| after.saturating_sub(before));
    let duration_ms = status.duration_ms.unwrap_or(0);
    let performance = ScanPerformance {
        duration_ms, read_ms: status.read_ms, commit_ms: status.commit_ms,
        maintenance_ms: status.maintenance_ms, paused_ms, counters,
        cpu_time_ms,
        average_cpu_percent: cpu_time_ms.and_then(|cpu| (duration_ms > 0).then(||
            cpu as f64 / duration_ms as f64 / std::thread::available_parallelism().map_or(1, |n| n.get()) as f64 * 100.0)),
        process_peak_working_set_bytes: process_peak_memory(),
    };
    if let Ok((id, store)) = &result {
        if let Err(error) = store.save_performance(*id, &performance) {
            append_warning(&mut status.warning, format!("保存性能记录失败：{error:#}"));
        }
    }
    status.performance = Some(performance);
    match &result {
        Ok((id, _)) => { status.phase = "完成".into(); status.report_id = Some(*id); }
        Err(error) => { status.phase = "失败".into(); status.error = Some(format!("{error:#}")); }
    }
    write_status(data, letter, &status)?;
    println!("{} {} in {:.1} seconds", chrono::Local::now().format("%H:%M:%S"), status.phase, started.elapsed().as_secs_f64());
    result.map(|_| ())
}

fn process_cpu_ms() -> Option<u64> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    let ticks = |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    Some((ticks(kernel) + ticks(user)) / 10_000)
}

fn process_peak_memory() -> Option<u64> {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default()
    };
    let size = counters.cb;
    unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) }.ok()?;
    Some(counters.PeakWorkingSetSize as u64)
}

fn append_warning(current: &mut Option<String>, message: String) {
    if let Some(existing) = current { existing.push_str("；"); existing.push_str(&message); }
    else { *current = Some(message); }
}

pub fn start_elevated_scan(letter: char) -> Result<()> {
    ensure!(letter.is_ascii_alphabetic(), "Invalid drive letter");
    let exe = env::current_exe()?;
    let path: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let params: Vec<u16> = format!("scan {}", letter.to_ascii_uppercase())
        .encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let result = unsafe {
        ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(path.as_ptr()),
            PCWSTR(params.as_ptr()), PCWSTR::null(), SW_HIDE)
    };
    ensure!(result.0 as isize > 32, "Windows did not start the elevated scan");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_process_metrics_are_available_without_elevation() {
        assert!(process_cpu_ms().is_some());
        assert!(process_peak_memory().is_some_and(|peak| peak > 0));
        let old: ScanStatus = serde_json::from_str(r#"{"root":"C:\\","phase":"完成","processed":1,"finished":true,"report_id":1,"error":null}"#).unwrap();
        assert!(old.performance.is_none());
    }

    #[test]
    fn global_lock_blocks_other_drive_and_detects_existing_drive_lock() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        assert!(!is_any_running(data));
        let global = global_scan_lock(data).unwrap();
        assert!(global_scan_lock(data).is_err());
        assert!(is_any_running(data));
        drop(global);
        let existing = scan_lock(data, 'C').unwrap();
        assert!(is_any_running(data));
        drop(existing);
        assert!(!is_any_running(data));
    }
}
