use super::*;
use std::collections::BTreeMap;

impl VolumeTrailApp {
    pub(super) fn history_view(&mut self,ui:&mut egui::Ui) {
        ui.heading("扫描记录");
        let reports:Vec<_>=self.reports.iter().filter(|r|r.root.starts_with(self.drive)).cloned().collect();
        let mut grouped:BTreeMap<i64,Vec<Report>>=BTreeMap::new();
        for report in &reports {grouped.entry(report.operation_pre.unwrap_or(report.id)).or_default().push(report.clone());}
        let mut groups:Vec<_>=grouped.into_iter().collect();
        groups.sort_by_key(|(_,items)|std::cmp::Reverse(items[0].id));
        ui.horizontal_wrapped(|ui| {
            let all=!reports.is_empty() && reports.iter().all(|r|self.history_selection.contains(&r.id));
            if ui.add_enabled(!reports.is_empty(),egui::Button::new(if all {"取消全选"} else {"全选可见记录"})).clicked() {
                if all {self.history_selection.clear();} else {self.history_selection.extend(reports.iter().map(|r|r.id));}
            }
            let can_edit=!self.history_selection.is_empty() && !self.any_running && self.history_rx.is_none();
            if ui.add_enabled(can_edit,egui::Button::new(format!("清除明细 ({})",self.history_selection.len()))).on_hover_text("保留扫描时间、容量与目录变化汇总").clicked() {
                self.start_history_job(worker::HistoryAction::ClearDetails(self.history_selection.iter().copied().collect()));
            }
            if ui.add_enabled(can_edit,egui::Button::new(format!("删除所选 ({})",self.history_selection.len()))).clicked() {
                self.start_history_job(worker::HistoryAction::Delete(self.history_selection.iter().copied().collect()));
            }
            if ui.add_enabled(!self.any_running && self.history_rx.is_none(),egui::Button::new("无损压缩历史")).clicked() {
                self.start_history_job(worker::HistoryAction::Compact);
            }
        });
        ui.label(RichText::new("清理后核验归入原扫描操作；展开观测时间可查看或选择各真实记录。").color(MUTED));
        if self.history_rx.is_some() {ui.label("正在整理历史…");}
        if let Some(message)=&self.history_message {ui.colored_label(GREEN,message);}
        egui::ScrollArea::vertical().show_rows(ui,34.0,groups.len(),|ui,range| {
            for (pre,items) in &groups[range] {
                let latest=&items[0];let primary=items.iter().find(|r|r.id==*pre);
                ui.horizontal(|ui| {
                    let mut checked=items.iter().all(|r|self.history_selection.contains(&r.id));
                    if ui.checkbox(&mut checked,"").changed() {for r in items {
                        if checked {self.history_selection.insert(r.id);} else {self.history_selection.remove(&r.id);}
                    }}
                    let label=format!("{} · {} · {} 已用",scan_time(&primary.unwrap_or(latest).sampled),
                        scan_mode_label(&primary.unwrap_or(latest).mode),bytes(latest.total.saturating_sub(latest.free)));
                    if ui.add_enabled(primary.is_some(),egui::Button::new(label)).on_hover_text("查看此操作清理前的空间变化").clicked() {
                        self.selected=Some(*pre);self.compare_from=None;self.compare_explicit=false;self.comparison_path=None;self.comparison_key=None;self.view=View::Changes;
                    }
                    ui.menu_button(format!("{} 个观测 ▾",items.len()),|ui| {
                        for report in items.iter().rev() {
                            let mut checked=self.history_selection.contains(&report.id);
                            let kind=if report.operation_pre.is_some() {"清理后核验"} else {"正常扫描 / 清理前"};
                            if ui.checkbox(&mut checked,format!("#{} {} · {}",report.id,report.sampled,kind)).changed() {
                                if checked {self.history_selection.insert(report.id);} else {self.history_selection.remove(&report.id);}
                            }
                            ui.label(format!("{} · {}",if report.details {"含文件明细"} else {"明细已整理"},super::cleanup::verification_state(&report.verification)));
                        }
                        if primary.is_none() {ui.colored_label(AMBER,"原扫描点已删除；比较基线不可用。");}
                    });
                });
            }
        });
    }
}
