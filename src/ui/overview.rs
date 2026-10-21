use super::*;
use crate::overview::{self as service, Overview};
use std::sync::Arc;

#[derive(Default)]
pub(super) struct OverviewState {
    pub(super) key: Option<(char, i64, Option<i64>)>,
    rx: Option<Receiver<Result<Overview, String>>>,
    value: Option<Result<Arc<Overview>, String>>,
    expanded: bool,
}

impl VolumeTrailApp {
    pub(super) fn overview(&mut self, ui: &mut egui::Ui) {
        let key = (self.drive, self.data_version, self.reports.first().map(|r| r.id));
        if self.overview_state.key != Some(key) {
            let path = worker::store_path(&self.data);
            let root = format!("{}:\\", self.drive);
            let (tx,rx) = mpsc::channel();
            std::thread::spawn(move || {
                let result = Store::open_read_only(&path).and_then(|s| service::load(&s,&root))
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
            });
            self.overview_state.key = Some(key);
            self.overview_state.rx = Some(rx);
            self.overview_state.value = None;
        }
        if let Some(rx) = &self.overview_state.rx {
            match rx.try_recv() {
                Ok(value) => { self.overview_state.value = Some(value.map(Arc::new)); self.overview_state.rx = None; }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.overview_state.value = Some(Err("概览查询中断".into())); self.overview_state.rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => { ui.ctx().request_repaint_after(Duration::from_millis(50)); }
            }
        }
        ui.heading(format!("{}: 磁盘概览",self.drive));
        let Some(value) = self.overview_state.value.clone() else { ui.spinner(); return; };
        let data = match value { Ok(v) => v, Err(e) => { ui.colored_label(RED,e); return; } };
        let Some(current) = &data.current_index else { ui.label("还没有扫描记录。完成首次扫描后可查看容量与变化。"); return; };
        ui.label(RichText::new(format!("最近观测 {}",scan_time(&current.observed_at))).color(MUTED));
        ui.add_space(10.0);
        ui.columns(if ui.available_width() > 700.0 {3} else {2}, |columns| {
            metric(&mut columns[0],"剩余空间",current.free_bytes,GREEN);
            metric(&mut columns[1],"总容量",current.total_bytes,INK);
            if columns.len() > 2 {
                columns[2].label(RichText::new("本次扫描前空间变化").color(MUTED));
                columns[2].label(RichText::new(data.interval.as_ref().map(|i| signed_bytes(i.used_delta_bytes))
                    .unwrap_or_else(|| "暂无比较基线".into())).size(24.0));
            }
        });
        if ui.available_width()<=700.0 {
            ui.label(format!("本次扫描前空间变化 {}",data.interval.as_ref().map(|i|signed_bytes(i.used_delta_bytes)).unwrap_or_else(||"暂无比较基线".into())));
        }
        ui.add_space(12.0);
        if let Some(interval) = &data.interval {
            ui.label(RichText::new(format!("{} → {}",scan_time(&interval.from_observed_at),scan_time(&interval.to_observed_at))).color(MUTED));
            let (all,complete,available,_) = interval.coverage;
            if all == 0 || complete != all {
                ui.colored_label(AMBER,format!("历史覆盖不足：完整目录汇总 {complete}/{all}，可用 {available}/{all}；排行仅代表已保存变化。"));
            }
            ui.horizontal(|ui| {
                ui.strong("主要目录变化");
                ui.toggle_value(&mut self.overview_state.expanded,"展开全部");
            });
            let limit = if self.overview_state.expanded { 20 } else { 4 };
            let mut chosen = None;
            for (title,items,color) in [("增长", &data.growth,RED),("减少", &data.reductions,GREEN)] {
                ui.colored_label(color,title);
                if items.is_empty() { ui.label(if complete == all && all > 0 { "没有记录到变化" } else { "没有可用变化；覆盖不完整" }); }
                for item in items.iter().take(limit) {
                    ui.horizontal(|ui| {
                        ui.add_sized([108.0,22.0],egui::Label::new(RichText::new(signed_bytes(item.allocated_delta)).color(color)));
                        let length = (ui.available_width()/8.0).max(20.0) as usize;
                        let response = ui.link(middle_path(&item.path,length)).on_hover_text(&item.path);
                        if response.clicked() { chosen = Some(item.path.clone()); }
                        response.context_menu(|ui| { if ui.button("复制完整路径").clicked() { ui.ctx().copy_text(item.path.clone()); ui.close(); } });
                    });
                }
                if self.overview_state.expanded && items.len()>limit {ui.label(format!("显示前 {limit} 项，共 {} 项；完整排行见空间变化。",items.len()));}
            }
            if let Some(path) = chosen {
                self.selected = Some(interval.to);
                self.compare_from = Some(interval.from);
                self.comparison_path = Some(path);
                self.comparison_key = None;
                self.details_page = 0;
                self.view = View::Changes;
            }
        } else { ui.colored_label(MUTED,"尚无前次扫描或基线已被删除；目录变化未知。"); }
        if data.verification != "not_needed" {
            ui.collapsing("本次清理后核验",|ui| {
                ui.label(format!("状态：{}",data.verification));
                ui.label(format!("清理前扫描 {:?} · 最近有效扫描 {}",data.pre_cleanup_scan,data.latest.as_ref().map_or_else(|| "历史点已删除".into(),|r|r.id.to_string())));
                ui.label(format!("最近有效观测 {}",current.observed_at));
                if let Some(pre)=data.pre_cleanup_scan.and_then(|id|data.trend.iter().find(|r|r.id==id)) {ui.label(format!("清理前真实观测 {}",pre.sampled));}
                if data.verification != "verified" { ui.colored_label(AMBER,"文件统计待下次扫描更新"); }
            });
        }
        ui.add_space(14.0);
        ui.separator();
        ui.strong("容量趋势");
        ui.horizontal(|ui| {
            for (days,label) in [(7,"一周"),(30,"一月"),(365,"一年"),(0,"全部")] {
                if ui.selectable_value(&mut self.config.trend_days,days,label).changed() {
                    let mut saved = self.saved_config.clone(); saved.trend_days = days;
                    match saved.save(&self.data) { Ok(()) => self.saved_config = saved, Err(e) => self.error = Some(format!("{e:#}")) }
                }
            }
        });
        self.space_chart(ui,&data.trend);
        if let Some(run)=&data.latest_cleanup {
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.strong("最近清理");
                ui.label(format!("{} · 删除 {} 项 · 占用估算 {}",super::cleanup::run_state(&run.state),run.deleted,bytes(run.allocated_bytes_estimate)));
                if ui.button("查看结果").clicked() {self.view=View::Cleanup;}
            });
            ui.label(super::cleanup::verification_state(&run.verification));
        }
        if let Some(latest)=&data.latest {for warning in &latest.warnings { ui.colored_label(AMBER,warning); }}
    }
}

fn middle_path(path: &str, max: usize) -> String {
    let chars: Vec<_> = path.chars().collect();
    if chars.len() <= max { return path.into(); }
    let left = max.saturating_sub(1)/2;
    format!("{}…{}",chars[..left].iter().collect::<String>(),chars[chars.len()-left..].iter().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_paths_elide_in_the_middle() {
        let path = "E:\\中文目录\\多个同名目录\\临时文件";
        let short = middle_path(path,16);
        assert!(short.starts_with("E:\\")); assert!(short.ends_with("临时文件")); assert!(short.chars().count() <= 16);
    }
}

impl VolumeTrailApp {
    fn space_chart(&mut self, ui: &mut egui::Ui, source:&[Report]) {
        let mut reports: Vec<&Report> = source.iter()
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

}
