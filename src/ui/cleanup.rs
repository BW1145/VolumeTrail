use super::*;
use crate::cleanup::{self as service,Plan,Rule as CleanupRule,Run,RunItem,Safety};

#[derive(Default)]
pub(super) struct CleanupState {
    loaded:bool,
    rules:Vec<CleanupRule>,
    runs:Vec<Run>,
    editor:Option<CleanupRule>,
    original:Option<CleanupRule>,
    patterns:String,
    plan:Option<Plan>,
    acknowledge:bool,
    result:Option<(Run,Vec<RunItem>)>,
    message:Option<String>,
    rx:Option<Receiver<Result<Reply,String>>>,
    cancel_drive:Option<char>,
}
struct Reply {rules:Vec<CleanupRule>,runs:Vec<Run>,saved:Option<CleanupRule>,plan:Option<Plan>,result:Option<(Run,Vec<RunItem>)>}
enum Job {Load,Save(CleanupRule),Preview(String),Enable(String,u64,String),Pause(String,u64),Delete(String,u64),Execute(String,String),Result(String)}

fn perform(data:&Path,job:Job)->Result<Reply> {
    let mut saved=None;let mut plan=None;let mut result=None;
    match job {
        Job::Load=>{}
        Job::Execute(plan,request)=>{let run=crate::worker_cleanup::execute(data,&plan,&request)?;
            let store=Store::open_read_only(&worker::store_path(data))?;let items=store.cleanup_items(&run.run_id)?;result=Some((run,items));}
        Job::Result(id)=>{let store=Store::open_read_only(&worker::store_path(data))?;result=Some((store.cleanup_run(&id)?,store.cleanup_items(&id)?));}
        action=>{
            let _lock=worker::global_scan_lock(data)?;
            let store=Store::open(&worker::store_path(data))?;let safety=Safety::for_data(data)?;
            match action {
                Job::Save(rule)=>saved=Some(service::save_rule(&store,&safety,rule)?),
                Job::Preview(id)=>plan=Some(service::preview(&store,&safety,&[id],&||false)?),
                Job::Enable(id,revision,plan)=>saved=Some(service::set_enabled(&store,&safety,&id,revision,Some(&plan),true)?),
                Job::Pause(id,revision)=>saved=Some(service::set_enabled(&store,&safety,&id,revision,None,false)?),
                Job::Delete(id,revision)=>store.delete_cleanup_rule(&id,revision)?,
                _=>unreachable!(),
            }
        }
    }
    let store=Store::open_read_only(&worker::store_path(data))?;
    Ok(Reply {rules:store.cleanup_rules()?,runs:store.cleanup_runs(None,10,0)?.1,saved,plan,result})
}

impl VolumeTrailApp {
    fn cleanup_job(&mut self,job:Job) {
        self.cleanup_state.cancel_drive=if matches!(&job,Job::Execute(..)) {
            self.cleanup_state.plan.as_ref().and_then(|p|p.rules.first()).and_then(|r|r.directory.chars().next())
        } else {None};
        let (tx,rx)=mpsc::channel();let data=self.data.clone();
        std::thread::spawn(move || {let _=tx.send(perform(&data,job).map_err(|e|format!("{e:#}")));});
        self.cleanup_state.rx=Some(rx);self.cleanup_state.message=None;
    }
    pub(super) fn poll_cleanup(&mut self) {
        let Some(rx)=&self.cleanup_state.rx else {return;};
        match rx.try_recv() {
            Ok(reply)=>{
                self.cleanup_state.rx=None;
                match reply {
                    Ok(reply)=>{
                        let state=&mut self.cleanup_state;state.rules=reply.rules;state.runs=reply.runs;state.loaded=true;
                        if state.editor.as_ref().is_some_and(|e|!e.id.is_empty() && !state.rules.iter().any(|r|r.id==e.id)) {state.editor=None;state.original=None;state.plan=None;}
                        if let Some(rule)=reply.saved {state.patterns=rule.patterns.join("; ");state.original=Some(rule.clone());state.editor=Some(rule);state.plan=None;state.acknowledge=false;}
                        if let Some(plan)=reply.plan {state.plan=Some(plan);state.acknowledge=false;}
                        if let Some(result)=reply.result {state.result=Some(result);state.plan=None;state.acknowledge=false;}
                        state.message=Some("操作结果已更新".into());
                        self.overview_state.key=None;
                        self.refresh();
                    }
                    Err(error)=>self.cleanup_state.message=Some(error),
                }
            }
            Err(mpsc::TryRecvError::Disconnected)=>{self.cleanup_state.rx=None;self.cleanup_state.message=Some("清理操作中断；请查询最近结果，勿盲目重试删除。".into());}
            Err(mpsc::TryRecvError::Empty)=>{}
        }
    }
    pub(super) fn new_cleanup_rule(&mut self,directory:String) {
        let state=&mut self.cleanup_state;
        state.editor=Some(CleanupRule {name:"新规则".into(),directory,patterns:vec!["*.dmp".into()],..Default::default()});
        state.original=None;state.patterns="*.dmp".into();state.plan=None;state.acknowledge=false;
        self.view=View::Cleanup;
    }
    pub(super) fn cleanup_view(&mut self,ui:&mut egui::Ui) {
        if !self.cleanup_state.loaded && self.cleanup_state.rx.is_none() {self.cleanup_job(Job::Load);}
        let mut job=None;let mut create=false;
        ui.heading("文件清理规则");
        ui.label(RichText::new("仅处理你指定目录内匹配的普通文件。直接删除，不进入回收站。").color(MUTED));
        let state=&mut self.cleanup_state;
        let busy=state.rx.is_some();
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy,egui::Button::new("新建规则")).clicked() {create=true;}
            if ui.add_enabled(!busy,egui::Button::new("刷新")).clicked() {job=Some(Job::Load);}
            if busy {ui.spinner();ui.label("正在处理…");
                if let Some(letter)=state.cancel_drive {if ui.add_enabled(worker::is_running(&self.data,letter),egui::Button::new("停止后续文件")).clicked() {
                    if let Err(e)=worker::stop_scan(&self.data,letter) {state.message=Some(format!("{e:#}"));}
                }
                }
            }
        });
        if let Some(message)=&state.message {ui.colored_label(AMBER,message);}
        ui.separator();
        if state.rules.is_empty() && !busy {ui.label("尚无文件清理规则。保存不会启用自动清理。");}
        for rule in state.rules.clone() {
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(state.editor.as_ref().is_some_and(|e|e.id==rule.id),&rule.name).clicked() {
                    state.editor=Some(rule.clone());state.original=Some(rule.clone());state.patterns=rule.patterns.join("; ");state.plan=None;state.acknowledge=false;
                }
                ui.colored_label(if rule.enabled {GREEN} else {MUTED},if rule.enabled {"已启用"} else {"已暂停"});
                ui.label(RichText::new(format!("修订 {}",rule.revision)).small().color(MUTED));
                if rule.enabled && ui.add_enabled(!busy,egui::Button::new("暂停")).clicked() {job=Some(Job::Pause(rule.id.clone(),rule.revision));}
            });
            ui.add(egui::Label::new(&rule.directory).truncate()).on_hover_text(&rule.directory);
        }
        ui.separator();
        if let Some(rule)=&mut state.editor {
            ui.add_enabled_ui(!busy,|ui| {
                egui::Grid::new("cleanup_editor").num_columns(2).spacing([16.0,9.0]).show(ui,|ui| {
                    ui.label("名称");ui.text_edit_singleline(&mut rule.name);ui.end_row();
                    ui.label("目标目录");ui.add(egui::TextEdit::singleline(&mut rule.directory).desired_width(ui.available_width().max(120.0)));ui.end_row();
                    ui.label("文件名匹配");if ui.text_edit_singleline(&mut state.patterns).changed() {rule.patterns=state.patterns.split(';').map(str::trim).filter(|p|!p.is_empty()).map(str::to_owned).collect();}ui.end_row();
                    ui.label("最小修改年龄");ui.horizontal(|ui| {ui.add(egui::DragValue::new(&mut rule.min_age_days).range(0..=36500).suffix(" 天"));if rule.min_age_days==0 {ui.label("不按年龄保留");}});ui.end_row();
                    ui.label("子目录");ui.checkbox(&mut rule.recursive,"包含子目录（默认关闭）");ui.end_row();
                });
                ui.label(RichText::new("匹配支持 * 和 ?；多个匹配以英文分号分隔。目录本身不会删除。").small().color(MUTED));
                if !rule.volume_id.is_empty() {ui.label(RichText::new(format!("绑定卷 {}",rule.volume_id)).small().color(MUTED));}
                let dirty=state.original.as_ref()!=Some(rule);
                if dirty {state.plan=None;state.acknowledge=false;}
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(dirty,egui::Button::new("保存并暂停")).clicked() {job=Some(Job::Save(rule.clone()));}
                    if ui.add_enabled(!dirty && !rule.id.is_empty(),egui::Button::new("预览匹配")).clicked() {job=Some(Job::Preview(rule.id.clone()));}
                    if ui.add_enabled(!rule.id.is_empty(),egui::Button::new("删除规则")).on_hover_text("仅删除规则，不删除文件").clicked() {job=Some(Job::Delete(rule.id.clone(),rule.revision));state.plan=None;}
                });
            });
        }
        if let Some(plan)=&state.plan {
            ui.add_space(10.0);ui.strong("本次预览");
            ui.label(format!("{} 个候选 · 占用估算 {} · 跳过 {} 项",plan.candidate_count,bytes(plan.allocated_bytes_estimate),plan.skipped_count));
            if plan.truncated {ui.colored_label(AMBER,"候选已截断；本次只处理显示计划中的对象。自动启用需要缩小范围后重新预览。");}
            ui.collapsing("查看候选文件与跳过原因",|ui| {
                egui::ScrollArea::vertical().id_salt("cleanup_candidates").max_height(180.0).show(ui,|ui| {
                    for candidate in plan.candidates.iter().take(200) {ui.horizontal(|ui|{ui.label(bytes(candidate.snapshot.allocated_bytes));ui.add(egui::Label::new(&candidate.path).truncate()).on_hover_text(&candidate.path);});}
                    if plan.candidates.len()>200 {ui.label("界面显示前 200 项；CLI JSON 可读取计划完整候选。");}
                    for (reason,count) in &plan.skip_reasons {ui.label(format!("{reason}: {count}"));}
                });
            });
            let expired=chrono::Utc::now().timestamp()>=plan.expires_at;
            ui.label(RichText::new(format!("计划 {} · 剩余 {} 秒",plan.plan_id,(plan.expires_at-chrono::Utc::now().timestamp()).max(0))).small().color(MUTED));
            ui.label("自动启用后：仅在登录扫描成功后执行，同一规则每天最多一次。手动扫描不会清理文件。");
            ui.checkbox(&mut state.acknowledge,"我确认以上目录和匹配范围，理解直接删除不可撤销");
            ui.add_enabled_ui(!busy && !expired && state.acknowledge,|ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("立即执行本次预览").clicked() {job=Some(Job::Execute(plan.plan_id.clone(),format!("gui-{}",service::new_id())));}
                    if !plan.truncated && plan.rules.len()==1 && ui.button("启用此规则：登录扫描后自动清理").clicked() {
                        let rule=&plan.rules[0];job=Some(Job::Enable(rule.id.clone(),rule.revision,plan.plan_id.clone()));
                    }
                });
            });
            if expired {ui.colored_label(AMBER,"预览已过期，请重新预览。");}
        }
        if !state.runs.is_empty() {
            ui.add_space(14.0);ui.separator();ui.strong("最近清理结果");
            for run in &state.runs {
                if ui.button(format!("{} · {} · 删除 {} · 跳过 {} · 失败 {}",run_time(run.started_at),run_state(&run.state),run.deleted,run.skipped,run.failed)).clicked() {job=Some(Job::Result(run.run_id.clone()));}
                ui.label(RichText::new(verification_state(&run.verification)).color(if run.verification=="verified" || run.verification=="not_needed" {MUTED} else {AMBER}));
            }
        }
        if let Some((run,items))=&state.result {
            ui.collapsing("执行明细",|ui| {
                ui.label(format!("{} · 核验 {}",run.run_id,verification_state(&run.verification)));
                ui.label(format!("已删除占用估算 {}",bytes(run.allocated_bytes_estimate)));
                if let (Some(a),Some(b))=(run.free_before,run.free_after) {ui.label(format!("磁盘实际可用空间变化 {}（含其他程序并发变化）",signed_bytes(i128::from(b)-i128::from(a))));}
                if let Some(message)=&run.message {ui.colored_label(AMBER,message);}
                if run.details_trimmed {ui.label("逐项明细已裁剪，汇总仍保留。");}
                for item in items {ui.label(format!("{} · {} · {}",item.state,item.path,item.reason.as_deref().unwrap_or("")));}
            });
        }
        if create {self.new_cleanup_rule(String::new());}
        if let Some(job)=job {self.cleanup_job(job);}
        if busy {ui.ctx().request_repaint_after(Duration::from_millis(50));}
    }
}

pub(super) fn run_state(value:&str)->&str {match value {"completed"=>"完成","partial"=>"部分完成","cancelled"=>"已取消","interrupted"=>"异常中断","running"=>"执行中",_=>value}}
pub(super) fn verification_state(value:&str)->&str {match value {"verified"=>"已核验","verified_by_later_scan"=>"后续扫描已更新","not_needed"=>"无需核验","pending"=>"待核验","cancelled"|"unverified"=>"文件统计待下次扫描更新",_=>value}}

#[cfg(feature="ui-capture")]
pub(super) fn prepare_capture(app:&mut VolumeTrailApp,scope:&Path,scenario:&str)->Result<()> {
    for name in ["一个 temporary.dmp","第二份 crash.dmp","keep.txt"] {std::fs::write(scope.join(name),vec![0u8;8192])?;}
    let safety=Safety::for_data(&app.data)?;
    let rule=service::save_rule(&app.store,&safety,CleanupRule {name:"样本转储文件".into(),directory:scope.to_str().unwrap().into(),patterns:vec!["*.dmp".into()],..Default::default()})?;
    let plan=service::preview(&app.store,&safety,&[rule.id.clone()],&||false)?;
    let state=&mut app.cleanup_state;state.loaded=true;state.rules=vec![rule.clone()];state.patterns="*.dmp".into();state.editor=Some(rule.clone());state.original=Some(rule);
    if scenario=="preview" {state.plan=Some(plan);}
    else if scenario=="result" {
        let run=crate::worker_cleanup::execute(&app.data,&plan.plan_id,"visual-fixture")?;
        state.runs=vec![run.clone()];state.result=Some((run.clone(),app.store.cleanup_items(&run.run_id)?));
    } else if scenario=="error" {state.message=Some("样本错误：文件正在被其他程序占用，已跳过。请检查预览范围后重试。".into());}
    app.view=View::Cleanup;Ok(())
}

fn run_time(timestamp:i64)->String {chrono::DateTime::from_timestamp(timestamp,0).map(|at|at.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "时间未知".into())}
