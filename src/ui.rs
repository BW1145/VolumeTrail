use crate::{config::AppConfig, startup::{self, StartupStatus}, store::{Change, FolderBreakdown, FolderGrowth, FolderItem, RankedItem, Report, StorageUsage, Store}, worker};
use anyhow::Result;
use chrono::{DateTime, Local};
use eframe::egui::{self, Color32, RichText, Stroke};
use std::{collections::HashSet, path::{Path, PathBuf}, process::Command, sync::mpsc::{self, Receiver}, time::{Duration, Instant}};

const INK: Color32 = Color32::from_rgb(31, 43, 43);
const MUTED: Color32 = Color32::from_rgb(99, 113, 113);
const GREEN: Color32 = Color32::from_rgb(17, 127, 112);
const RED: Color32 = Color32::from_rgb(190, 77, 68);
const AMBER: Color32 = Color32::from_rgb(158, 105, 42);
const CANVAS: Color32 = Color32::from_rgb(246, 248, 247);
const RULE: Color32 = Color32::from_rgb(219, 227, 224);

#[derive(Clone, Copy, PartialEq, Eq)]
enum View { Overview, Changes, Folders, History, Settings }

struct PendingClose {
    drive: char,
    last_report_id: i64,
    started: bool,
    launched: Instant,
}

pub struct VolumeTrailApp {
    data: PathBuf,
    store: Store,
    config: AppConfig,
    saved_config: AppConfig,
    drive: char,
    view: View,
    reports: Vec<Report>,
    scan_count: u64,
    selected: Option<i64>,
    history_selection: HashSet<i64>,
    history_message: Option<String>,
    history_rx: Option<Receiver<Result<String, String>>>,
    changes: Vec<Change>,
    growth: Vec<FolderGrowth>,
    compare_from: Option<i64>,
    comparison_key: Option<(i64, i64, u8, u64)>,
    comparison_rx: Option<Receiver<((i64, i64, u8, u64), Result<(Vec<FolderGrowth>, Vec<Change>, (u64, u64, u64, u64), Vec<(String, i128, u64)>, u64, Option<FolderBreakdown>), String>)>>,
    comparison_coverage: Option<(u64, u64, u64, u64)>,
    comparison_path: Option<String>,
    comparison_breakdown: Option<FolderBreakdown>,
    extensions: Vec<(String, i128, u64)>,
    details_page: u64,
    details_total: u64,
    folder_depth: u8,
    folders: Vec<FolderItem>,
    largest_mode: Option<bool>,
    largest_items: Vec<RankedItem>,
    largest_offset: u64,
    largest_key: Option<(String, bool, u64)>,
    largest_rx: Option<Receiver<((String, bool, u64), Result<Vec<RankedItem>, String>)>>,
    breadcrumbs: Vec<(u64, String)>,
    folder_volume: Option<String>,
    status: Option<worker::ScanStatus>,
    running: bool,
    any_running: bool,
    scan_launch_at: Option<Instant>,
    close_after_scan: Option<PendingClose>,
    error: Option<String>,
    startup_status: Option<Result<StartupStatus, String>>,
    startup_rx: Option<Receiver<Result<StartupStatus, String>>>,
    startup_refresh_at: Option<Instant>,
    storage_usage: Option<Result<StorageUsage, String>>,
    storage_rx: Option<Receiver<Result<StorageUsage, String>>>,
    last_refresh: Instant,
    last_data_check: Instant,
    data_version: i64,
}

impl VolumeTrailApp {
    pub fn new(ctx: &egui::Context, data: PathBuf) -> Result<Self> {
        if let Ok(font) = std::fs::read(r"C:\Windows\Fonts\simhei.ttf") {
            let mut definitions = egui::FontDefinitions::default();
            definitions.font_data.insert("chinese".into(), egui::FontData::from_owned(font).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                definitions.families.entry(family).or_default().insert(0, "chinese".into());
            }
            ctx.set_fonts(definitions);
        }
        let mut style = (*ctx.style()).clone();
        style.visuals = egui::Visuals::light();
        style.visuals.panel_fill = CANVAS;
        style.visuals.window_fill = Color32::WHITE;
        style.visuals.override_text_color = Some(INK);
        style.visuals.selection.bg_fill = Color32::from_rgb(211, 234, 228);
        style.visuals.selection.stroke = Stroke::new(1.0_f32, GREEN);
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(211, 234, 228);
        style.spacing.item_spacing = egui::vec2(10.0, 9.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        ctx.set_style(style);
        let store = Store::open(&worker::store_path(&data))?;
        let config = AppConfig::load(&data)?;
        let mut app = Self {
            data, store, saved_config: config.clone(), config, drive: 'C', view: View::Overview,
            reports: Vec::new(), scan_count: 0, selected: None,
            history_selection: HashSet::new(), history_message: None, history_rx: None,
            changes: Vec::new(), growth: Vec::new(), compare_from: None,
            comparison_key: None, comparison_rx: None, comparison_coverage: None,
            comparison_path: None, comparison_breakdown: None, extensions: Vec::new(), details_page: 0, details_total: 0,
            folder_depth: 5,
            folders: Vec::new(), largest_mode: None, largest_items: Vec::new(),
            largest_offset: 0, largest_key: None, largest_rx: None,
            breadcrumbs: Vec::new(), folder_volume: None,
            status: None, running: false, any_running: false, scan_launch_at: None,
            close_after_scan: None, error: None,
            startup_status: None, startup_rx: None, startup_refresh_at: None,
            storage_usage: None, storage_rx: None,
            last_refresh: Instant::now() - Duration::from_secs(10),
            last_data_check: Instant::now() - Duration::from_secs(10), data_version: 0,
        };
        app.refresh();
        Ok(app)
    }

    fn refresh(&mut self) {
        self.storage_usage = None;
        self.refresh_status();
        self.data_version = self.store.data_version().unwrap_or(self.data_version);
        self.last_data_check = Instant::now();
        match self.store.reports() {
            Ok(reports) => {
                if self.reports.iter().map(|report| report.id).ne(reports.iter().map(|report| report.id)) {
                    self.comparison_key = None;
                }
                self.reports = reports;
                self.history_selection.retain(|id| self.reports.iter().any(|report| {
                    report.id == *id && report.root.starts_with(self.drive)
                }));
                match self.store.scan_count(&format!("{}:\\", self.drive)) {
                    Ok(count) => self.scan_count = count,
                    Err(error) => self.error = Some(format!("读取扫描次数失败：{error:#}")),
                }
                if !self.reports.iter().any(|report| Some(report.id) == self.selected && report.root.starts_with(self.drive)) {
                    self.selected = self.reports.iter().find(|report| report.root.starts_with(self.drive)).map(|report| report.id);
                }
                if self.view == View::Folders { self.load_folder(); }
            }
            Err(error) => self.error = Some(format!("读取记录失败：{error:#}")),
        }
    }

    fn refresh_status(&mut self) {
        self.status = worker::read_status(&self.data, self.drive);
        self.running = worker::is_running(&self.data, self.drive);
        self.any_running = worker::is_any_running(&self.data);
        if self.scan_launch_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(5)) {
            self.scan_launch_at = None;
        }
        self.last_refresh = Instant::now();
    }

    fn load_selected(&mut self) {
        if self.view == View::Folders { self.load_folder(); }
    }

    fn load_folder(&mut self) {
        let root = format!("{}:\\", self.drive);
        let Some(checkpoint) = self.store.checkpoint(&root).ok().flatten() else {
            self.folders.clear();
            self.breadcrumbs.clear();
            self.folder_volume = None;
            return;
        };
        let volume = checkpoint.volume_id.clone();
        if self.folder_volume.as_deref() != Some(&volume) {
            self.breadcrumbs.clear();
            self.folder_volume = Some(volume.clone());
        }
        if self.breadcrumbs.is_empty() {
            self.breadcrumbs.push((checkpoint.root_id, root));
        }
        let parent = self.breadcrumbs.last().unwrap().0;
        match self.store.children(&volume, parent) {
            Ok(items) => self.folders = items,
            Err(error) => self.error = Some(format!("读取目录失败：{error:#}")),
        }
    }

    fn load_largest(&mut self) {
        let Some(directories) = self.largest_mode else { return; };
        let Some(checkpoint) = self.store.checkpoint(&format!("{}:\\", self.drive)).ok().flatten() else { return; };
        let key = (checkpoint.volume_id.clone(), directories, self.largest_offset);
        if self.largest_key.as_ref() == Some(&key) { return; }
        let root = format!("{}:\\", self.drive);
        let path = worker::store_path(&self.data);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| -> Result<_, anyhow::Error> {
                let store = Store::open_read_only(&path)?;
                store.largest(&key.0, &root, checkpoint.root_id, directories, 50, key.2)
            })().map_err(|error| format!("{error:#}"));
            let _ = tx.send((key, result));
        });
        self.largest_key = Some((checkpoint.volume_id, directories, self.largest_offset));
        self.largest_items.clear();
        self.largest_rx = Some(rx);
    }

    fn poll_largest(&mut self) {
        let Some(rx) = &self.largest_rx else { return; };
        match rx.try_recv() {
            Ok((key, result)) => {
                self.largest_rx = None;
                if self.largest_key.as_ref() != Some(&key) { return; }
                match result {
                    Ok(items) => self.largest_items = items,
                    Err(error) => self.error = Some(format!("读取占用排行失败：{error}")),
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => self.largest_rx = None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn load_comparison(&mut self, volume: &str, from: i64, to: i64) {
        if self.comparison_key.is_some_and(|key| key.0 != from || key.1 != to) {
            self.details_page = 0;
        }
        let key = (from, to, self.folder_depth, self.details_page);
        if self.comparison_key == Some(key) { return; }
        let path = worker::store_path(&self.data);
        let volume = volume.to_owned();
        let selected_path = self.comparison_path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| -> Result<_, anyhow::Error> {
                let store = Store::open_read_only(&path)?;
                let breakdown = selected_path.as_deref()
                    .map(|path| store.folder_breakdown(&volume, from, to, path)).transpose()?;
                let growth = if let Some(breakdown) = &breakdown { breakdown.children.clone() }
                    else { store.folder_growth(&volume, from, to, key.2 as usize)? };
                let (changes, extensions) = if let Some(prefix) = selected_path.as_deref() {
                    (store.changes_between_path(&volume, from, to, prefix, 100, key.3 * 100)?,
                        store.extensions_between_path(&volume, from, to, prefix)?)
                } else { (store.changes_between_page(&volume, from, to, 100, key.3 * 100)?, Vec::new()) };
                Ok((growth, changes, store.coverage(&volume, from, to)?, extensions,
                    store.changes_count(&volume, from, to, selected_path.as_deref())?, breakdown))
            })().map_err(|error| format!("{error:#}"));
            let _ = tx.send((key, result));
        });
        self.comparison_key = Some(key);
        self.comparison_rx = Some(rx);
        self.growth.clear();
        self.changes.clear();
        self.comparison_coverage = None;
        self.comparison_breakdown = None;
        self.extensions.clear();
        self.details_total = 0;
    }

    fn poll_comparison(&mut self) {
        let Some(rx) = &self.comparison_rx else { return; };
        match rx.try_recv() {
            Ok((key, result)) => {
                self.comparison_rx = None;
                if self.comparison_key != Some(key) { return; }
                match result {
                    Ok((growth, changes, coverage, extensions, total, breakdown)) => {
                        self.growth = growth;
                        self.changes = changes;
                        self.comparison_coverage = Some(coverage);
                        self.extensions = extensions;
                        self.details_total = total;
                        self.comparison_breakdown = breakdown;
                    }
                    Err(error) => self.error = Some(format!("读取空间变化失败：{error}")),
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.comparison_rx = None;
                self.comparison_key = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn selected_report(&self) -> Option<&Report> {
        self.reports.iter().find(|report| Some(report.id) == self.selected)
    }

    fn folder_path(&self) -> String {
        let mut path = self.breadcrumbs.first().map(|(_, name)| name.clone()).unwrap_or_default();
        for (_, name) in self.breadcrumbs.iter().skip(1) {
            path.push_str(name);
            path.push('\\');
        }
        path
    }

    fn open_path(&mut self, path: &str, select: bool) {
        let argument = if select { format!("/select,{path}") } else { path.into() };
        if let Err(error) = Command::new("explorer.exe").arg(argument).spawn() {
            self.error = Some(format!("打开资源管理器失败：{error}"));
        }
    }

    fn open_change(&mut self, change: &Change) {
        let path = if change.new_path.is_empty() { &change.old_path } else { &change.new_path };
        if change.new_path.is_empty() {
            if let Some(parent) = Path::new(path).parent().and_then(|path| path.to_str()) {
                self.open_path(parent, false);
            }
        } else {
            self.open_path(path, !Path::new(path).is_dir());
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(format!("{}:\\", self.drive));
            egui::ComboBox::from_id_salt("drive")
                .selected_text(format!("{} 盘", self.drive))
                .show_ui(ui, |ui| {
                    for letter in 'A'..='Z' {
                        if PathBuf::from(format!("{letter}:\\")).exists()
                            && ui.selectable_value(&mut self.drive, letter, format!("{letter} 盘")).clicked()
                        { self.selected = None; self.history_selection.clear(); self.breadcrumbs.clear(); self.refresh(); }
                    }
                });
            ui.separator();
            if ui.add_enabled(!self.any_running && self.scan_launch_at.is_none(),
                egui::Button::new("开始扫描")).clicked() {
                if worker::is_any_running(&self.data) {
                    self.any_running = true;
                } else {
                    match worker::start_elevated_scan(self.drive) {
                        Ok(()) => {
                            self.scan_launch_at = Some(Instant::now());
                            if self.config.close_after_manual_scan {
                                let root = format!("{}:\\", self.drive);
                                let last_report_id = self.reports.iter().find(|report| report.root == root)
                                    .map_or(0, |report| report.id);
                                self.close_after_scan = Some(PendingClose {
                                    drive: self.drive, last_report_id, started: false, launched: Instant::now(),
                                });
                            }
                        }
                        Err(error) => self.error = Some(format!("启动扫描失败：{error:#}")),
                    }
                }
            }
            if self.any_running && !self.running { ui.label("其他磁盘正在扫描"); }
            else if self.scan_launch_at.is_some() && !self.running { ui.label("正在启动扫描"); }
            if ui.button("刷新").clicked() { self.comparison_key = None; self.refresh(); }
        });
        if self.running {
            let phase = self.status.as_ref().filter(|status| !status.finished)
                .map_or("正在启动", |status| status.phase.as_str());
            let processed = self.status.as_ref().filter(|status| !status.finished)
                .map_or(0, |status| status.processed);
            egui::Frame::new().fill(Color32::from_rgb(224, 241, 236))
                .inner_margin(12).corner_radius(4u8).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(20.0).color(GREEN));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(format!("正在扫描 {} 盘", self.drive)).strong().color(GREEN));
                            ui.label(format!("{} · 已处理 {} 项", phase, processed));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("停止本次扫描").clicked() {
                                if let Err(error) = worker::stop_scan(&self.data, self.drive) {
                                    self.error = Some(format!("停止失败：{error:#}"));
                                }
                            }
                        });
                    });
                });
        } else if let Some(status) = &self.status {
            if let Some(error) = &status.error {
                ui.colored_label(RED, format!("上次扫描失败：{error}"));
            } else if let Some(duration) = status.duration_ms {
                ui.label(format!("上次扫描用时 {:.1} 秒", duration as f64 / 1000.0));
                if let Some(paused) = status.paused_ms.filter(|value| *value > 0) {
                    ui.label(format!("其中等待系统空闲 {:.1} 秒", paused as f64 / 1000.0));
                }
            }
            if let Some(warning) = &status.warning {
                ui.colored_label(AMBER, warning);
            }
        }
        if let Some(error) = &self.error {
            ui.colored_label(RED, error);
        }
        ui.separator();
    }

    fn overview(&mut self, ui: &mut egui::Ui) {
        let Some(report) = self.selected_report().cloned() else {
            ui.heading("还没有扫描记录");
            ui.label("选择磁盘并开始扫描，完成后会在这里显示空间变化。");
            return;
        };
        ui.label(RichText::new("磁盘概览").size(25.0).strong());
        ui.label(RichText::new(format!("所选记录  {}  ·  {}", scan_time(&report.finished), scan_mode_label(&report.mode))).color(MUTED));
        ui.add_space(14.0);
        let metrics = [
            ("已用空间", report.total.saturating_sub(report.free), INK),
            ("可用空间", report.free, GREEN),
            ("文件占用", report.allocated, INK),
            ("未归入文件统计", report.total.saturating_sub(report.free).saturating_sub(report.allocated), AMBER),
        ];
        let columns = if ui.available_width() >= 760.0 { 4 } else { 2 };
        for row in metrics.chunks(columns) {
            ui.columns(columns, |areas| {
                for (area, (title, value, color)) in areas.iter_mut().zip(row) {
                    metric(area, title, *value, *color);
                }
            });
            if columns == 2 { ui.add_space(8.0); }
        }
        ui.add_space(14.0);
        let fraction = if report.total == 0 { 0.0 } else { 1.0 - report.free as f32 / report.total as f32 };
        ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0)).fill(INK).desired_height(8.0));
        ui.add_space(19.0);
        ui.separator();
        ui.add_space(12.0);
        ui.label(RichText::new("已用空间趋势").size(18.0).strong());
        ui.horizontal(|ui| {
            for (days, label) in [(7, "一周"), (30, "一月"), (365, "一年"), (0, "全部")] {
                if ui.selectable_value(&mut self.config.trend_days, days, label).changed() {
                    let mut saved = self.saved_config.clone();
                    saved.trend_days = self.config.trend_days;
                    if let Err(error) = saved.save(&self.data) {
                        self.error = Some(format!("保存趋势范围失败：{error:#}"));
                    } else { self.saved_config = saved; }
                }
            }
        });
        self.space_chart(ui);
        ui.add_space(16.0);
        ui.separator();
        ui.add_space(12.0);
        ui.label(RichText::new("扫描概况").size(18.0).strong());
        let latest = self.reports.iter().find(|item| item.root == report.root);
        let previous = self.reports.iter().find(|item| item.root == report.root && item.id < report.id);
        let (change, change_color) = match previous {
            Some(previous) => {
                let used = report.total.saturating_sub(report.free) as i128;
                let prior_used = previous.total.saturating_sub(previous.free) as i128;
                let delta = used - prior_used;
                let value = if delta > 0 { format!("+{}", bytes(delta as u64)) }
                    else if delta < 0 { format!("-{}", bytes((-delta) as u64)) }
                    else { "0 B".into() };
                (value, if delta > 0 { RED } else if delta < 0 { GREEN } else { MUTED })
            }
            None => ("暂无对比".into(), MUTED),
        };
        let detailed = self.reports.iter().filter(|item| item.root == report.root && item.details).count();
        let summary = [
            ("已保留扫描", format!("{} 次", self.scan_count), INK),
            ("有文件明细", format!("{} 次", detailed), INK),
            ("距最近完成", latest.map(|item| scan_age(&item.finished)).unwrap_or_else(|| "暂无记录".into()), INK),
            ("所选记录已用变化", change, change_color),
        ];
        let columns = if ui.available_width() >= 760.0 { 3 } else { 2 };
        for row in summary.chunks(columns) {
            ui.columns(columns, |areas| {
                for (area, (title, value, color)) in areas.iter_mut().zip(row) {
                    area.label(RichText::new(*title).color(MUTED));
                    area.label(RichText::new(value).size(22.0).color(*color));
                }
            });
            if columns == 2 { ui.add_space(8.0); }
        }
        for warning in &report.warnings { ui.colored_label(AMBER, warning); }
        if let Some(performance) = &report.performance {
            ui.add_space(12.0);
            ui.collapsing("扫描性能", |ui| {
                ui.label(format!("总用时 {:.1} 秒 · 等待系统空闲 {:.1} 秒",
                    performance.duration_ms as f64 / 1000.0, performance.paused_ms as f64 / 1000.0));
                ui.label(format!("读取 {:.1} 秒 · 提交 {:.1} 秒 · 整理 {:.1} 秒",
                    performance.read_ms.unwrap_or(0) as f64 / 1000.0,
                    performance.commit_ms.unwrap_or(0) as f64 / 1000.0,
                    performance.maintenance_ms.unwrap_or(0) as f64 / 1000.0));
                ui.label(format!("变化事件 {} 条 · 涉及文件 {} 个 · 实际读取 {} 次",
                    performance.counters.journal_records, performance.counters.unique_changed_files, performance.counters.entry_reads));
                if let Some(cpu) = performance.average_cpu_percent {
                    ui.label(format!("平均 CPU 占用 {:.2}%（整机口径） · CPU 用时 {:.2} 秒",
                        cpu, performance.cpu_time_ms.unwrap_or(0) as f64 / 1000.0));
                }
                if let Some(peak) = performance.process_peak_working_set_bytes {
                    ui.label(format!("扫描进程峰值内存 {}", bytes(peak)));
                }
            });
        }
    }

    fn space_chart(&mut self, ui: &mut egui::Ui) {
        let mut reports: Vec<&Report> = self.reports.iter()
            .filter(|item| item.root.starts_with(self.drive))
            .filter(|item| self.config.trend_days == 0 || DateTime::parse_from_rfc3339(&item.sampled)
                .is_ok_and(|at| Local::now().signed_duration_since(at).num_days() < i64::from(self.config.trend_days)))
            .collect();
        reports.reverse();
        if reports.len() < 2 {
            ui.label(RichText::new("再完成一次扫描，就能看到空间变化趋势。").color(MUTED));
            return;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} 个时间点", reports.len())).color(MUTED));
            ui.colored_label(RED, "●");
            ui.label(RichText::new("增加").color(MUTED));
            ui.colored_label(GREEN, "●");
            ui.label(RichText::new("释放").color(MUTED));
        });
        if reports.len() > 600 {
            let mut selected = vec![reports[0]];
            let window = reports.len().div_ceil(300);
            for chunk in reports[1..reports.len()-1].chunks(window) {
                let min = chunk.iter().min_by_key(|item| item.total.saturating_sub(item.free)).unwrap();
                let max = chunk.iter().max_by_key(|item| item.total.saturating_sub(item.free)).unwrap();
                if min.id < max.id { selected.extend([*min, *max]); }
                else if min.id > max.id { selected.extend([*max, *min]); }
                else { selected.push(*min); }
            }
            selected.push(reports[reports.len()-1]);
            selected.sort_by_key(|item| item.id);
            selected.dedup_by_key(|item| item.id);
            reports = selected;
        }
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 208.0), egui::Sense::click());
        let plot = egui::Rect::from_min_max(rect.min + egui::vec2(66.0, 12.0), rect.max - egui::vec2(12.0, 30.0));
        let used = |report: &Report| report.total.saturating_sub(report.free);
        let min_used = reports.iter().map(|item| used(item)).min().unwrap_or(0) as f64;
        let max_used = reports.iter().map(|item| used(item)).max().unwrap_or(0) as f64;
        let padding = if max_used == min_used { 1024.0 * 1024.0 * 1024.0 }
            else { ((max_used - min_used) * 0.15).max(16.0 * 1024.0 * 1024.0) };
        let low = (min_used - padding).max(0.0);
        let high = max_used + padding;
        let range = high - low;
        let painter = ui.painter_at(rect);
        for step in 0..=2 {
            let t = step as f32 / 2.0;
            let y = egui::lerp(plot.bottom()..=plot.top(), t);
            painter.line_segment([egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)], Stroke::new(1.0_f32, RULE));
            painter.text(egui::pos2(plot.left() - 8.0, y), egui::Align2::RIGHT_CENTER,
                chart_bytes((low + range * t as f64) as u64), egui::FontId::proportional(11.0), MUTED);
        }
        let timestamps: Vec<i64> = reports.iter().map(|item| DateTime::parse_from_rfc3339(&item.sampled)
            .map_or(0, |at| at.timestamp())).collect();
        let start = timestamps[0];
        let span = timestamps[timestamps.len()-1].saturating_sub(start).max(1);
        let positions: Vec<egui::Pos2> = reports.iter().enumerate().map(|(index, item)| {
            let x = egui::lerp(plot.left()..=plot.right(), (timestamps[index]-start) as f32 / span as f32);
            let y = egui::lerp(plot.bottom()..=plot.top(), ((used(item) as f64 - low) / range) as f32);
            egui::pos2(x, y)
        }).collect();
        for index in 1..positions.len() {
            let color = if used(reports[index]) > used(reports[index - 1]) { RED }
                else if used(reports[index]) < used(reports[index - 1]) { GREEN }
                else { MUTED };
            painter.line_segment([positions[index - 1], positions[index]], Stroke::new(2.5_f32, color));
        }
        let clicked = response.clicked();
        let nearest = response.hover_pos().or_else(|| response.interact_pointer_pos()).map(|pointer| {
            positions.iter().enumerate().min_by(|(_, a), (_, b)|
                (a.x - pointer.x).abs().total_cmp(&(b.x - pointer.x).abs())).unwrap().0
        });
        for (index, point) in positions.iter().enumerate() {
            if Some(reports[index].id) == self.selected || Some(index) == nearest {
                painter.circle_filled(*point, 5.0, INK);
                painter.circle_filled(*point, 2.5, Color32::WHITE);
            }
        }
        painter.text(egui::pos2(plot.left(), rect.bottom() - 5.0), egui::Align2::LEFT_BOTTOM,
            scan_time(&reports[0].sampled), egui::FontId::proportional(11.0), MUTED);
        painter.text(egui::pos2(plot.right(), rect.bottom() - 5.0), egui::Align2::RIGHT_BOTTOM,
            scan_time(&reports[reports.len() - 1].sampled), egui::FontId::proportional(11.0), MUTED);
        if let Some(index) = nearest {
            let item = reports[index];
            let tooltip = format!("{}\n已用 {}\n文件明细：{}", scan_time(&item.sampled), chart_bytes(used(item)),
                if item.details { "有" } else { "已清理" });
            response.on_hover_text(tooltip);
            if clicked {
                self.selected = Some(item.id);
                self.load_selected();
            }
        }
    }

    fn changes_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("空间变化");
        let reports: Vec<Report> = self.reports.iter()
            .filter(|report| report.root.starts_with(self.drive)).cloned().collect();
        let Some(initial_end) = reports.iter().find(|report| Some(report.id) == self.selected).or_else(|| reports.first()) else {
            ui.label("至少完成两次扫描后，才能比较文件夹增长。");
            return;
        };
        let mut end_id = initial_end.id;
        let mut from_id = self.compare_from.unwrap_or_else(|| reports.iter()
            .find(|report| report.id < end_id && report.volume == initial_end.volume)
            .map_or(0, |report| report.id));
        let starts: Vec<_> = reports.iter().filter(|report| report.id < end_id && report.volume == initial_end.volume).collect();
        if !starts.iter().any(|report| report.id == from_id) {
            from_id = starts.first().map_or(0, |report| report.id);
        }
        ui.horizontal(|ui| {
            ui.label("从");
            egui::ComboBox::from_id_salt("compare_from")
                .selected_text(starts.iter().find(|report| report.id == from_id)
                    .map_or_else(|| "选择扫描记录".into(), |report| scan_time(&report.finished)))
                .show_ui(ui, |ui| {
                    for report in &starts {
                        ui.selectable_value(&mut from_id, report.id, scan_time(&report.finished));
                    }
                });
            ui.label("到");
            egui::ComboBox::from_id_salt("compare_to")
                .selected_text(scan_time(&initial_end.finished))
                .show_ui(ui, |ui| {
                    for report in &reports {
                        ui.selectable_value(&mut end_id, report.id, scan_time(&report.finished));
                    }
                });
        });
        if Some(end_id) != self.selected {
            self.selected = Some(end_id);
            self.compare_from = None;
            self.load_selected();
        } else {
            self.compare_from = Some(from_id);
        }
        let end = reports.iter().find(|report| report.id == end_id).unwrap();
        let valid_starts: Vec<_> = reports.iter().filter(|report| report.id < end_id && report.volume == end.volume).collect();
        let from_id = self.compare_from.filter(|id| valid_starts.iter().any(|report| report.id == *id))
            .or_else(|| valid_starts.first().map(|report| report.id));
        let Some(from_id) = from_id else {
            ui.label("这条记录之前没有可比较的扫描。");
            return;
        };
        self.compare_from = Some(from_id);
        if let Some(path) = self.comparison_path.clone() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&path).strong());
                if ui.button("返回全部").clicked() {
                    self.comparison_path = None;
                    self.folder_depth = 5;
                    self.details_page = 0;
                    self.comparison_key = None;
                }
                if ui.button("打开目录").clicked() { self.open_path(&path, false); }
            });
        }
        if self.comparison_path.is_none() { ui.horizontal(|ui| {
            ui.label("文件夹层级");
            egui::ComboBox::from_id_salt("folder_depth")
                .selected_text(self.folder_depth.to_string())
                .show_ui(ui, |ui| {
                    for depth in 1..=32 { ui.selectable_value(&mut self.folder_depth, depth, depth.to_string()); }
                });
        }); }
        self.load_comparison(&end.volume, from_id, end_id);
        if self.comparison_rx.is_some() { ui.label("正在读取变化…"); }
        if let Some((intervals, complete, available, details)) = self.comparison_coverage {
            if available < intervals {
                ui.colored_label(AMBER, format!("文件夹汇总覆盖 {available}/{intervals} 个区间；排行只包含尚存的数据"));
            } else if complete < intervals {
                ui.colored_label(AMBER, "旧版扫描只记录净变化，同盘移动的目录贡献可能不完整");
            }
            if details < intervals {
                ui.label(RichText::new(format!("文件明细覆盖 {details}/{intervals} 个区间")).color(MUTED));
            }
        }
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            if let Some(breakdown) = &self.comparison_breakdown {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("目录净变化 {}", signed_bytes(breakdown.allocated_delta)));
                    ui.label(format!("本层变化 {}", signed_bytes(breakdown.direct_delta)));
                    if breakdown.moved_delta != 0 {
                        ui.label(format!("其中移入移出 {}", signed_bytes(breakdown.moved_delta)));
                    }
                });
                ui.add_space(8.0);
            }
            ui.label(RichText::new(if self.comparison_path.is_some() { "子文件夹变化" } else { "文件夹净增长排行" }).size(18.0).strong());
            ui.label(RichText::new("按已记录的文件占用变化汇总；点击目录查看构成").color(MUTED));
            let growth: Vec<_> = self.growth.iter().filter(|item| item.allocated_delta > 0).take(12).cloned().collect();
            if growth.is_empty() {
                ui.label(if self.comparison_path.is_some() { "这段时间没有子文件夹净增长。" } else { "这段时间没有文件夹净增长。" });
            }
            for item in growth {
                let (selected, opened) = folder_growth_row(ui, &item, RED);
                if selected {
                    self.comparison_path = Some(item.path.clone());
                    self.folder_depth = self.folder_depth.saturating_add(1).min(32);
                    self.details_page = 0;
                    self.comparison_key = None;
                }
                if opened { self.open_path(&item.path, false); }
            }
            let released: Vec<_> = self.growth.iter().rev().filter(|item| item.allocated_delta < 0).take(5).cloned().collect();
            if !released.is_empty() {
                ui.add_space(12.0);
                ui.label(RichText::new("释放空间最多").size(18.0).strong());
                for item in released {
                    let (selected, opened) = folder_growth_row(ui, &item, GREEN);
                    if selected {
                        self.comparison_path = Some(item.path.clone());
                        self.folder_depth = self.folder_depth.saturating_add(1).min(32);
                        self.details_page = 0;
                        self.comparison_key = None;
                    }
                    if opened { self.open_path(&item.path, false); }
                }
            }
            if !self.extensions.is_empty() {
                ui.add_space(12.0);
                ui.label(RichText::new("文件类型变化").size(18.0).strong());
                for (extension, delta, count) in self.extensions.iter().take(10) {
                    ui.label(format!("{}  {}  {} 项", extension, signed_bytes(*delta), count));
                }
            }
            ui.add_space(16.0);
            ui.separator();
            ui.label(RichText::new("文件变化明细").size(18.0).strong());
            ui.horizontal(|ui| {
                ui.label(format!("共 {} 项 · 第 {} 页", self.details_total, self.details_page + 1));
                if ui.add_enabled(self.details_page > 0, egui::Button::new("上一页")).clicked() {
                    self.details_page -= 1;
                    self.comparison_key = None;
                }
                if ui.add_enabled((self.details_page + 1) * 100 < self.details_total,
                    egui::Button::new("下一页")).clicked() {
                    self.details_page += 1;
                    self.comparison_key = None;
                }
            });
            if self.changes.is_empty() { ui.label("这段时间没有文件变化。"); }
            let changes = self.changes.clone();
            for change in &changes {
                if change_row(ui, change) { self.open_change(change); }
            }
        });
    }

    fn folders_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("当前文件夹");
        let indexed_at = self.folder_volume.as_deref()
            .and_then(|volume| self.store.index_state(volume).ok().flatten())
            .map(|(_, _, _, at)| scan_time(&at)).unwrap_or_else(|| "时间未知".into());
        ui.label(RichText::new(format!("当前文件索引 · {} · 与所选历史记录无关", indexed_at)).color(MUTED));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.largest_mode, None, "浏览目录");
            ui.selectable_value(&mut self.largest_mode, Some(true), "最大文件夹");
            ui.selectable_value(&mut self.largest_mode, Some(false), "最大文件");
        });
        if self.largest_mode.is_some() {
            self.load_largest();
            if self.largest_rx.is_some() { ui.label("正在读取排行…"); }
            ui.horizontal(|ui| {
                if ui.add_enabled(self.largest_offset > 0, egui::Button::new("上一页")).clicked() {
                    self.largest_offset -= 50;
                    self.load_largest();
                }
                ui.label(format!("第 {} 页", self.largest_offset / 50 + 1));
                if ui.add_enabled(self.largest_items.len() == 50, egui::Button::new("下一页")).clicked() {
                    self.largest_offset += 50;
                    self.load_largest();
                }
            });
            let items = self.largest_items.clone();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for item in items {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(bytes(item.allocated)).strong());
                        if self.largest_mode == Some(true) { ui.label(format!("{} 文件", item.files)); }
                        if ui.small_button("打开").clicked() { self.open_path(&item.path, self.largest_mode == Some(false)); }
                        ui.add(egui::Label::new(&item.path).truncate()).on_hover_text(&item.path);
                    });
                    ui.separator();
                }
            });
            return;
        }
        ui.horizontal(|ui| {
            if self.breadcrumbs.len() > 1 && ui.button("上一级").clicked() {
                self.breadcrumbs.pop(); self.load_folder();
            }
            if ui.add_enabled(!self.breadcrumbs.is_empty(), egui::Button::new("打开文件夹")).clicked() {
                self.open_path(&self.folder_path(), false);
            }
        });
        ui.label(RichText::new(self.folder_path()).color(MUTED));
        ui.separator();
        let items = self.folders.clone();
        egui::ScrollArea::vertical().show_rows(ui, 30.0, items.len(), |ui, range| {
            for item in items[range].iter().cloned() {
                ui.horizontal(|ui| {
                    let label = if item.is_dir { format!("> {}", item.name) } else { item.name.clone() };
                    if ui.selectable_label(false, label).clicked() && item.is_dir {
                        self.breadcrumbs.push((item.id, item.name.clone()));
                        self.load_folder();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let path = format!("{}{}", self.folder_path(), item.name);
                        if ui.small_button("定位").clicked() { self.open_path(&path, !item.is_dir); }
                        ui.label(bytes(item.allocated));
                    });
                });
                ui.separator();
            }
        });
        if self.folders.len() == 5000 { ui.label("当前只显示前 5000 项。"); }
    }

    fn history_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("扫描记录");
        let reports: Vec<Report> = self.reports.iter().filter(|report| report.root.starts_with(self.drive)).cloned().collect();
        ui.horizontal(|ui| {
            let all_selected = !reports.is_empty() && reports.iter().all(|report| self.history_selection.contains(&report.id));
            if ui.add_enabled(!reports.is_empty(), egui::Button::new(if all_selected { "取消全选" } else { "全选可见记录" })).clicked() {
                if all_selected { self.history_selection.clear(); }
                else { self.history_selection.extend(reports.iter().map(|report| report.id)); }
            }
            let can_edit = !self.history_selection.is_empty() && !self.any_running && self.history_rx.is_none();
            if ui.add_enabled(can_edit, egui::Button::new(format!("清理明细 ({})", self.history_selection.len())))
                .on_hover_text("保留扫描时间、空间用量和文件夹变化汇总").clicked() {
                let ids: Vec<i64> = self.history_selection.iter().copied().collect();
                self.start_history_job(worker::HistoryAction::ClearDetails(ids));
            }
            if ui.add_enabled(can_edit, egui::Button::new(format!("删除所选 ({})", self.history_selection.len()))).clicked() {
                let ids: Vec<i64> = self.history_selection.iter().copied().collect();
                self.start_history_job(worker::HistoryAction::Delete(ids));
            }
            if ui.add_enabled(!self.any_running && self.history_rx.is_none(), egui::Button::new("按容量整理")).clicked() {
                self.start_history_job(worker::HistoryAction::Prune(self.saved_config.budget_bytes()));
            }
        });
        if self.history_rx.is_some() { ui.label("正在整理历史…"); }
        if let Some(message) = &self.history_message { ui.label(RichText::new(message).color(GREEN)); }
        egui::ScrollArea::vertical().show_rows(ui, 30.0, reports.len(), |ui, range| {
            for report in reports[range].iter().cloned() {
                ui.horizontal(|ui| {
                    let mut checked = self.history_selection.contains(&report.id);
                    if ui.checkbox(&mut checked, "").changed() {
                        if checked { self.history_selection.insert(report.id); }
                        else { self.history_selection.remove(&report.id); }
                    }
                    if ui.selectable_label(Some(report.id) == self.selected,
                        format!("{}  ·  {}  ·  {} 已用  ·  {}", scan_time(&report.finished), scan_mode_label(&report.mode),
                            bytes(report.total.saturating_sub(report.free)), if report.details { "含明细" } else { "时间点" }))
                        .clicked()
                    { self.selected = Some(report.id); self.load_selected(); self.view = View::Overview; }
                });
            }
        });
        if self.reports.len() == 500 { ui.label("当前只显示最近 500 次扫描。"); }
    }

    fn start_history_job(&mut self, action: worker::HistoryAction) {
        let data = self.data.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = worker::edit_history(&data, action).map_err(|error| format!("{error:#}"));
            let _ = tx.send(result);
        });
        self.history_rx = Some(rx);
        self.history_message = None;
    }

    fn poll_history_job(&mut self) {
        let Some(rx) = &self.history_rx else { return; };
        match rx.try_recv() {
            Ok(Ok(message)) => {
                self.history_message = Some(message);
                self.error = None;
                self.history_rx = None;
                self.refresh();
            }
            Ok(Err(error)) => {
                self.error = Some(format!("整理历史失败：{error}"));
                self.history_rx = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.error = Some("整理历史任务中断".into());
                self.history_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn request_startup_status(&mut self) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(startup::status().map_err(|error| format!("{error:#}")));
        });
        self.startup_status = None;
        self.startup_rx = Some(rx);
    }

    fn poll_startup_status(&mut self) {
        if self.startup_refresh_at.is_some_and(|at| at <= Instant::now()) {
            self.startup_refresh_at = None;
            self.request_startup_status();
        }
        if let Some(rx) = &self.startup_rx {
            match rx.try_recv() {
                Ok(status) => { self.startup_status = Some(status); self.startup_rx = None; }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.startup_status = Some(Err("自启状态查询中断".into()));
                    self.startup_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    fn settings_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("设置");
        ui.add_space(12.0);
        ui.strong("自动扫描磁盘");
        ui.horizontal_wrapped(|ui| {
            for letter in 'A'..='Z' {
                if !self.config.drives.contains(&letter)
                    && !Path::new(&format!("{letter}:\\")).exists() { continue; }
                let mut selected = self.config.drives.contains(&letter);
                let can_change = !selected || self.config.drives.len() > 1;
                if ui.add_enabled(can_change, egui::Checkbox::new(&mut selected, format!("{letter}:"))).changed() {
                    if selected { self.config.drives.push(letter); self.config.drives.sort_unstable(); }
                    else { self.config.drives.retain(|value| *value != letter); }
                }
            }
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.label("历史明细和目录汇总上限");
            ui.add(egui::DragValue::new(&mut self.config.history_budget_mib).range(64..=4096).speed(16.0).suffix(" MiB"));
        });
        ui.add_space(8.0);
        ui.strong("存储占用");
        match &self.storage_usage {
            Some(Ok(usage)) => {
                ui.label(format!("历史明细和目录汇总 {} · 当前索引 {}", bytes(usage.history_bytes), bytes(usage.index_bytes)));
                ui.label(format!("趋势与性能记录 {} · 迁移备份 {}", bytes(usage.timeline_bytes), bytes(usage.backup_bytes)));
                ui.label(format!("数据库 {} · 合计 {}", bytes(usage.database_bytes), bytes(usage.database_bytes + usage.backup_bytes)));
            }
            Some(Err(error)) => { ui.colored_label(AMBER, format!("读取占用失败：{error}")); }
            None => { ui.label("正在读取存储占用…"); }
        }
        if ui.add_enabled(self.storage_rx.is_none(), egui::Button::new("刷新占用")).clicked() {
            self.request_storage_usage();
        }
        ui.checkbox(&mut self.config.close_after_manual_scan, "手动扫描完成后关闭窗口");
        if ui.add_enabled(self.config != self.saved_config, egui::Button::new("保存设置")).clicked() {
            match self.config.save(&self.data) {
                Ok(()) => { self.saved_config = self.config.clone(); self.error = None; }
                Err(error) => self.error = Some(format!("保存设置失败：{error:#}")),
            }
        }
        ui.add_space(18.0);
        ui.separator();
        ui.add_space(12.0);
        ui.strong("开机自启");
        let status = self.startup_status.clone();
        match &status {
            Some(Ok(current)) => {
                let mut enabled = current.installed && current.enabled;
                if ui.checkbox(&mut enabled, "登录后扫描一次").changed() {
                    match startup::start_elevated(enabled) {
                        Ok(()) => {
                            self.startup_status = None;
                            self.startup_refresh_at = Some(Instant::now() + Duration::from_secs(2));
                        }
                        Err(error) => self.error = Some(format!("修改自启失败：{error:#}")),
                    }
                }
                if let Some(path) = &current.executable {
                    ui.label(RichText::new(path).color(MUTED));
                }
            }
            Some(Err(error)) => { ui.colored_label(RED, format!("读取自启状态失败：{error}")); }
            None => { ui.label("正在读取自启状态…"); }
        }
        if ui.button("刷新自启状态").clicked() { self.request_startup_status(); }
        ui.add_space(18.0);
        ui.separator();
        ui.add_space(12.0);
        ui.strong("数据位置");
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(self.data.display().to_string()).truncate());
            if ui.button("打开文件夹").clicked() {
                let path = self.data.display().to_string();
                self.open_path(&path, false);
            }
        });
    }

    fn request_storage_usage(&mut self) {
        let path = worker::store_path(&self.data);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = Store::open_read_only(&path).and_then(|store| store.storage_usage())
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(result);
        });
        self.storage_rx = Some(rx);
    }

    fn poll_storage_usage(&mut self) {
        let Some(rx) = &self.storage_rx else { return; };
        match rx.try_recv() {
            Ok(result) => { self.storage_usage = Some(result); self.storage_rx = None; }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.storage_usage = Some(Err("存储占用查询中断".into())); self.storage_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

impl eframe::App for VolumeTrailApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_history_job();
        self.poll_comparison();
        self.poll_largest();
        if self.last_refresh.elapsed() >= Duration::from_secs(2) {
            self.refresh_status();
            if self.last_data_check.elapsed() >= Duration::from_secs(10) {
                self.last_data_check = Instant::now();
                if self.store.data_version().is_ok_and(|version| version != self.data_version) {
                    self.refresh();
                    self.largest_key = None;
                }
            }
            if let Some(pending) = &mut self.close_after_scan {
                let status = worker::read_status(&self.data, pending.drive);
                if status.as_ref().is_some_and(|status| {
                    status.finished && status.error.is_none()
                        && status.report_id.is_some_and(|id| id > pending.last_report_id)
                }) {
                    self.close_after_scan = None;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    if worker::is_running(&self.data, pending.drive) { pending.started = true; }
                    if (pending.started && status.as_ref().is_some_and(|status| status.finished))
                        || (!pending.started && pending.launched.elapsed() >= Duration::from_secs(60)) {
                        self.close_after_scan = None;
                    }
                }
            }
        }
        if self.view == View::Settings {
            self.poll_storage_usage();
            if self.storage_usage.is_none() && self.storage_rx.is_none() { self.request_storage_usage(); }
            if self.startup_status.is_none() && self.startup_rx.is_none() && self.startup_refresh_at.is_none() {
                self.request_startup_status();
            }
            self.poll_startup_status();
        }
        ctx.request_repaint_after(Duration::from_secs(2));
        egui::SidePanel::left("navigation").resizable(false).exact_width(170.0)
            .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(egui::Margin::symmetric(18, 18)))
            .show(ctx, |ui| {
            ui.add_space(7.0);
            ui.label(RichText::new("盘迹").size(20.0).strong().color(INK));
            ui.label(RichText::new("VolumeTrail").size(12.0).color(MUTED));
            ui.add_space(27.0);
            for (view, label) in [(View::Overview, "概览"), (View::Changes, "空间变化"),
                (View::Folders, "当前文件夹"), (View::History, "扫描记录"), (View::Settings, "设置")] {
                if ui.add_sized([ui.available_width(), 32.0], egui::Button::selectable(self.view == view, label)).clicked() {
                    self.view = view;
                    if view == View::Folders { self.load_folder(); }
                }
            }
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS).inner_margin(egui::Margin::symmetric(24, 20))).show(ctx, |ui| {
            self.toolbar(ui);
            match self.view {
                View::Overview => { egui::ScrollArea::vertical().show(ui, |ui| self.overview(ui)); }
                View::Changes => self.changes_view(ui),
                View::Folders => self.folders_view(ui),
                View::History => self.history_view(ui),
                View::Settings => { egui::ScrollArea::vertical().show(ui, |ui| self.settings_view(ui)); }
            }
        });
    }
}

fn metric(ui: &mut egui::Ui, title: &str, value: u64, color: Color32) {
    ui.label(RichText::new(title).color(MUTED));
    ui.label(RichText::new(bytes(value)).size(22.0).color(color));
}

fn folder_growth_row(ui: &mut egui::Ui, item: &FolderGrowth, color: Color32) -> (bool, bool) {
    let sign = if item.allocated_delta > 0 { "+" } else { "-" };
    let amount = bytes(item.allocated_delta.unsigned_abs().min(u64::MAX as u128) as u64);
    let clicked = ui.horizontal(|ui| {
        ui.add_sized([105.0, 20.0], egui::Label::new(RichText::new(format!("{sign}{amount}")).color(color)));
        ui.label(RichText::new(format!("{} 项", item.changes)).color(MUTED));
        if item.moved_delta != 0 { ui.label(RichText::new("含移动").color(MUTED)); }
        let opened = ui.small_button("打开").on_hover_text("在资源管理器中打开文件夹").clicked();
        let selected = ui.add_sized([ui.available_width(), 20.0],
            egui::Label::new(&item.path).truncate().sense(egui::Sense::click()))
            .on_hover_text("查看子目录和文件构成").clicked();
        (selected, opened)
    }).inner;
    ui.separator();
    clicked
}

fn change_row(ui: &mut egui::Ui, change: &Change) -> bool {
    let path = if change.new_path.is_empty() { &change.old_path } else { &change.new_path };
    let delta = change.allocated_delta;
    let color = if delta > 0 { RED } else if delta < 0 { GREEN } else { MUTED };
    let clicked = ui.horizontal(|ui| {
        ui.add_sized([92.0, 20.0], egui::Label::new(RichText::new(if delta > 0 { format!("+{}", bytes(delta.unsigned_abs())) }
            else if delta < 0 { format!("-{}", bytes(delta.unsigned_abs())) }
            else { "移动".into() }).color(color)));
        let clicked = ui.add_enabled(!path.starts_with("[unresolved"), egui::Button::new("打开位置"))
            .on_hover_text("在资源管理器中打开文件所在位置").clicked();
        ui.add_sized([ui.available_width(), 20.0], egui::Label::new(path).truncate()).on_hover_text(path);
        clicked
    }).inner;
    ui.separator();
    clicked
}

fn bytes(value: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if value < 1024 { return format!("{value} B"); }
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 { size /= 1024.0; unit += 1; }
    format!("{size:.1} {}", UNITS[unit])
}

fn signed_bytes(value: i128) -> String {
    if value == 0 { return "0 B".into(); }
    format!("{}{}", if value > 0 { "+" } else { "-" }, bytes(value.unsigned_abs().min(u64::MAX as u128) as u64))
}

fn chart_bytes(value: u64) -> String {
    if value >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", value as f64 / (1024.0 * 1024.0 * 1024.0))
    } else { bytes(value) }
}

fn scan_time(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| value.to_owned())
}

fn scan_age(value: &str) -> String {
    let Ok(time) = DateTime::parse_from_rfc3339(value) else { return "时间未知".into() };
    let minutes = Local::now().signed_duration_since(time).num_minutes().max(0);
    if minutes == 0 { "刚刚".into() }
    else if minutes < 60 { format!("{minutes} 分钟前") }
    else if minutes < 1440 { format!("{} 小时前", minutes / 60) }
    else { format!("{} 天前", minutes / 1440) }
}

fn scan_mode_label(value: &str) -> &str {
    if value == "incremental" { "增量" } else { "全量" }
}
