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
    technical_message:Option<String>,
    rx:Option<Receiver<Result<Reply,String>>>,
    cancel_request:Option<String>,
    #[cfg(feature="ui-capture")]
    capture_section:Option<&'static str>,
    #[cfg(feature="ui-capture")]
    capture_expand:bool,
}
struct Reply {rules:Vec<CleanupRule>,runs:Vec<Run>,saved:Option<CleanupRule>,plan:Option<Plan>,result:Option<(Run,Vec<RunItem>)>,directory:Option<String>}
enum Job {PickDirectory,Load,Save(CleanupRule),Preview(String),Enable(String,u64,String),Pause(String,u64),Delete(String,u64),Execute(String,String),Result(String)}

fn perform(data:&Path,job:Job)->Result<Reply> {
    let mut saved=None;let mut plan=None;let mut result=None;let mut directory=None;
    match job {
        Job::PickDirectory=>directory=super::folder_picker::choose()?,
        Job::Load=>{}
        Job::Execute(plan,request)=>{let run=crate::worker_cleanup::execute(data,&plan,&request)?;
            let store=Store::open_read_only(&worker::store_path(data))?;let items=store.cleanup_items(&run.run_id)?;result=Some((run,items));}
        Job::Result(id)=>{let store=Store::open_read_only(&worker::store_path(data))?;result=Some((store.observed_cleanup_run(&id,worker::cleanup_is_active(data,&id))?,store.cleanup_items(&id)?));}
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
    Ok(Reply {rules:store.cleanup_rules()?,runs:store.cleanup_runs(None,10,0)?.1,saved,plan,result,directory})
}

impl VolumeTrailApp {
    fn cleanup_job(&mut self,job:Job) {
        if self.cleanup_state.rx.is_some() {return;}
        self.cleanup_state.cancel_request=if let Job::Execute(_,request)=&job {Some(request.clone())} else {None};
        let (tx,rx)=mpsc::channel();let data=self.data.clone();
        std::thread::spawn(move || {let _=tx.send(perform(&data,job).map_err(|e|format!("{e:#}")));});
        self.cleanup_state.rx=Some(rx);self.cleanup_state.message=None;self.cleanup_state.technical_message=None;
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
                        if let Some(directory)=reply.directory {if let Some(rule)=&mut state.editor {rule.directory=directory;rule.volume_id.clear();state.plan=None;state.acknowledge=false;}}
                        if let Some(rule)=reply.saved {state.patterns=rule.patterns.join("; ");state.original=Some(rule.clone());state.editor=Some(rule);state.plan=None;state.acknowledge=false;}
                        if let Some(plan)=reply.plan {state.plan=Some(plan);state.acknowledge=false;}
                        if let Some(result)=reply.result {state.result=Some(result);state.plan=None;state.acknowledge=false;}
                        state.message=Some("操作结果已更新".into());
                        self.overview_state.key=None;
                        self.refresh();
                    }
                    Err(error)=>{self.cleanup_state.message=Some(friendly_error(&error).into());self.cleanup_state.technical_message=Some(error);}
                }
            }
            Err(mpsc::TryRecvError::Disconnected)=>{self.cleanup_state.rx=None;self.cleanup_state.message=Some("清理操作中断；请查询最近结果，勿盲目重试删除。".into());}
            Err(mpsc::TryRecvError::Empty)=>{}
        }
    }
    pub(super) fn new_cleanup_rule(&mut self,directory:String) {
        if self.cleanup_state.rx.is_some() {self.view=View::Cleanup;return;}
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
                if let Some(request)=&state.cancel_request {
                    let active=worker::active_cleanup(&self.data).filter(|a|a.request_id==*request);
                    if ui.add_enabled(active.is_some(),egui::Button::new("停止后续文件")).clicked() {
                        if let Some(active)=active {if let Err(e)=worker::cancel_cleanup(&self.data,&active.run_id) {state.message=Some(friendly_error(&e.to_string()).into());state.technical_message=Some(format!("{e:#}"));}}
                    }
                }
            }
        });
        if let Some(message)=&state.message {ui.colored_label(AMBER,message);}
        if let Some(details)=&state.technical_message {ui.collapsing("错误详情",|ui|{ui.label(details);});}
        ui.separator();
        if state.rules.is_empty() && !busy {ui.label("尚无文件清理规则。保存不会启用自动清理。");}
        for rule in state.rules.clone() {
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!busy,egui::Button::selectable(state.editor.as_ref().is_some_and(|e|e.id==rule.id),&rule.name)).clicked() {
                    state.editor=Some(rule.clone());state.original=Some(rule.clone());state.patterns=rule.patterns.join("; ");state.plan=None;state.acknowledge=false;
                }
                ui.colored_label(if rule.enabled {GREEN} else {MUTED},if rule.enabled {"已启用"} else {"已暂停"});
                if rule.enabled && ui.add_enabled(!busy,egui::Button::new("暂停")).clicked() {job=Some(Job::Pause(rule.id.clone(),rule.revision));}
            });
            path_label(ui,&rule.directory);
        }
        ui.separator();
        if let Some(rule)=&mut state.editor {
            ui.add_enabled_ui(!busy,|ui| {
                egui::Grid::new("cleanup_editor").num_columns(2).spacing([16.0,9.0]).show(ui,|ui| {
                    ui.label("名称");ui.text_edit_singleline(&mut rule.name);ui.end_row();
                    ui.label("目标目录");ui.vertical(|ui| {
                        if ui.add(egui::TextEdit::singleline(&mut rule.directory).desired_width(ui.available_width().max(120.0))).changed() {rule.volume_id.clear();}
                        if ui.button("选择目录…").clicked() {job=Some(Job::PickDirectory);}
                    });ui.end_row();
                    ui.label("文件名匹配");if ui.text_edit_singleline(&mut state.patterns).changed() {rule.patterns=state.patterns.split(';').map(str::trim).filter(|p|!p.is_empty()).map(str::to_owned).collect();}ui.end_row();
                    ui.label("修改时间");ui.vertical(|ui| {
                        ui.horizontal(|ui| {ui.label("至少");ui.add(egui::DragValue::new(&mut rule.min_age_days).range(0..=36500));ui.label("天未修改");});
                        if rule.min_age_days==0 {ui.label(RichText::new("0 天：不按时间保留").small().color(MUTED));}
                    });ui.end_row();
                    ui.label("子目录");ui.checkbox(&mut rule.recursive,"包含子目录（默认关闭）");ui.end_row();
                });
                ui.label(RichText::new("匹配支持 * 和 ?；多个匹配以英文分号分隔。目录本身不会删除。").small().color(MUTED));
                if !rule.id.is_empty() {ui.collapsing("规则技术详情",|ui| {ui.label(format!("规则 {} · 修订 {}",rule.id,rule.revision));ui.label(format!("卷身份 {}",rule.volume_id));});}
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
            ui.add_space(10.0);let heading=ui.strong("本次预览");
            #[cfg(feature="ui-capture")]
            if state.capture_section==Some("preview") {heading.scroll_to_me(Some(egui::Align::TOP));}
            #[cfg(not(feature="ui-capture"))]
            let _=heading;
            ui.label(format!("{} 个候选 · 占用估算 {} · 跳过 {} 项",plan.candidate_count,bytes(plan.allocated_bytes_estimate),plan.skipped_count));
            if plan.truncated {ui.colored_label(AMBER,"候选已截断；本次只处理显示计划中的对象。自动启用需要缩小范围后重新预览。");}
            #[cfg(not(feature="ui-capture"))]
            let expanded=None;
            #[cfg(feature="ui-capture")]
            let expanded=state.capture_expand.then_some(true);
            egui::CollapsingHeader::new("查看候选文件与跳过原因").id_salt("cleanup_preview_files").open(expanded).show(ui,|ui| {
                egui::ScrollArea::vertical().id_salt("cleanup_candidates").max_height(180.0).show(ui,|ui| {
                    for candidate in plan.candidates.iter().take(200) {ui.horizontal(|ui|{ui.label(bytes(candidate.snapshot.allocated_bytes));path_label(ui,&candidate.path);});}
                    if plan.candidates.len()>200 {ui.label("界面显示前 200 项；CLI JSON 可读取计划完整候选。");}
                    for (reason,count) in &plan.skip_reasons {ui.label(format!("{}：{count}",skip_reason(reason)));}
                });
            });
            let expired=chrono::Utc::now().timestamp()>=plan.expires_at;
            ui.label(RichText::new(format!("预览有效期还剩 {} 秒",(plan.expires_at-chrono::Utc::now().timestamp()).max(0))).small().color(MUTED));
            ui.collapsing("预览技术详情",|ui| {ui.label(format!("计划 {}",plan.plan_id));for r in &plan.rules {ui.label(format!("规则 {} · 修订 {} · {}",r.id,r.revision,r.volume_id));}for (reason,count) in &plan.skip_reasons {ui.label(format!("{reason}: {count}"));}});
            ui.label("自动启用后：仅在登录扫描成功后执行，同一规则每天最多一次。手动扫描不会清理文件。");
            ui.add_enabled(!busy,egui::Checkbox::new(&mut state.acknowledge,"我确认以上目录和匹配范围，理解直接删除不可撤销"));
            ui.add_enabled_ui(!busy && !expired && state.acknowledge,|ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("删除本次匹配文件").clicked() {job=Some(Job::Execute(plan.plan_id.clone(),format!("gui-{}",service::new_id())));}
                    if !plan.truncated && plan.rules.len()==1 && ui.button("启用此规则：登录扫描后自动清理").clicked() {
                        let rule=&plan.rules[0];job=Some(Job::Enable(rule.id.clone(),rule.revision,plan.plan_id.clone()));
                    }
                });
            });
            if expired {ui.colored_label(AMBER,"预览已过期，请重新预览。");}
        }
        if !state.runs.is_empty() {
            ui.add_space(14.0);ui.separator();let heading=ui.strong("最近清理结果");
            #[cfg(feature="ui-capture")]
            if state.capture_section==Some("result") {heading.scroll_to_me(Some(egui::Align::TOP));}
            #[cfg(not(feature="ui-capture"))]
            let _=heading;
            for run in &state.runs {
                if ui.add_enabled(!busy,egui::Button::new(format!("{} · {} · 删除 {} · 跳过 {} · 失败 {}",run_time(run.started_at),run_state(&run.state),run.deleted,run.skipped,run.failed))).clicked() {job=Some(Job::Result(run.run_id.clone()));}
                ui.label(RichText::new(format!("已删除占用估算 {} · {}",bytes(run.allocated_bytes_estimate),verification_state(&run.verification))).color(if run.verification=="verified" || run.verification=="not_needed" {MUTED} else {AMBER}));
                if run.unknown>0 {ui.colored_label(AMBER,format!("{} 项处理结果尚未确定",run.unknown));}
            }
        }
        if let Some((run,items))=&state.result {
            #[cfg(not(feature="ui-capture"))]
            let expanded=None;
            #[cfg(feature="ui-capture")]
            let expanded=state.capture_expand.then_some(true);
            egui::CollapsingHeader::new("执行明细").id_salt("cleanup_result_items").open(expanded).show(ui,|ui| {
                ui.label(format!("{} · {}",run_time(run.started_at),verification_state(&run.verification)));
                ui.label(format!("已删除占用估算 {}",bytes(run.allocated_bytes_estimate)));
                if let (Some(a),Some(b))=(run.free_before,run.free_after) {ui.label(format!("磁盘实际可用空间变化 {}（含其他程序并发变化）",signed_bytes(i128::from(b)-i128::from(a))));}
                if run.message.is_some() {ui.colored_label(AMBER,if run.state=="interrupted" {"执行意外中断，未确定的项目不会重试删除。"} else if run.state=="cancelled" {"已停止后续文件，之前完成的删除已保留。"} else {"清理记录已保存；文件统计需等下次扫描更新。"});}
                if run.details_trimmed {ui.label("逐项明细已裁剪，汇总仍保留。");}
                for item in items {ui.horizontal(|ui| {ui.label(item_state(&item.state));path_label(ui,&item.path);});if let Some(reason)=&item.reason {ui.label(skip_reason(reason));}}
                ui.collapsing("执行技术详情",|ui| {ui.label(format!("执行 {} · 请求 {} · 计划 {}",run.run_id,run.request_id,run.plan_id));if let Some(message)=&run.message {ui.label(message);}for item in items {if let Some(reason)=&item.reason {ui.label(format!("{}: {reason}",item.path));}}});
            });
        }
        if create {self.new_cleanup_rule(String::new());}
        if let Some(job)=job {self.cleanup_job(job);}
        if busy {ui.ctx().request_repaint_after(Duration::from_millis(50));}
    }
}

fn fitted_path(ui:&egui::Ui,path:&str)->String {
    let font=egui::TextStyle::Body.resolve(ui.style());
    let width=ui.available_width().max(1.0);
    let mut limit=(width/6.0).max(3.0) as usize;
    loop {
        let text=super::overview::middle_path(path,limit);
        if limit<=3 || ui.painter().layout_no_wrap(text.clone(),font.clone(),INK).size().x<=width {return text;}
        limit-=1;
    }
}
fn path_label(ui:&mut egui::Ui,path:&str)->egui::Response {
    ui.add(egui::Label::new(fitted_path(ui,path)).truncate()).on_hover_text(path)
}

pub(super) fn run_state(value:&str)->&str {match value {"completed"=>"完成","partial"=>"部分完成","cancelled"=>"已取消","interrupted"=>"异常中断","running"=>"执行中",_=>"状态未知"}}
pub(super) fn verification_state(value:&str)->&str {match value {
    "verified"=>"文件统计已更新","verified_by_later_scan"=>"文件统计已由后续扫描更新","not_needed"=>"文件统计无需更新",
    "pending"=>"等待扫描更新","cancelled"|"unverified"=>"文件统计待下次扫描更新",
    "baseline_missing"|"history_point_missing"=>"所需历史观测已整理","no_scan"=>"尚未扫描",_=>"文件统计状态未知"}}
fn item_state(value:&str)->&str {match value {"deleted"=>"已删除","skipped"=>"已跳过","failed"=>"处理失败","pending"=>"处理中","unknown"=>"结果未确定",_=>"状态未知"}}
fn skip_reason(reason:&str)->&str {
    if reason.contains("multiple_hardlinks") {"文件存在多个硬链接，已跳过"}
    else if reason.contains("reparse_point") {"联接或符号链接，已跳过"}
    else if reason.contains("special_attributes") {"只读、系统、离线或云占位等特殊文件，已跳过"}
    else if reason.contains("filename_mismatch") {"文件名不匹配"}
    else if reason.contains("too_recent") {"尚未达到保留天数"}
    else if reason.contains("parent_changed") {"父目录已变化，请重新预览"}
    else if reason.contains("file_changed") {"文件已变化，请重新预览"}
    else if reason.contains("volume_changed") {"磁盘身份已变化"}
    else if reason.contains("traversal_limit") {"范围较大，达到预览上限"}
    else if reason.contains("0x80070005") {"没有访问或删除权限，已跳过"}
    else if reason.contains("0x80070020") || reason.contains("0x80070021") {"文件正在使用，已跳过"}
    else if reason.contains("directory_unavailable") {"目录暂时无法访问，已跳过"}
    else if reason.contains("non_unicode") {"文件名无法显示，已跳过"}
    else if reason.contains("Process interrupted") {"处理中断，结果未确认；不会重放删除"}
    else {"无法安全处理，已跳过；原始原因见技术详情"}
}
fn friendly_error(error:&str)->&str {
    if error.contains("Rule name") {"请填写 1 至 100 字的规则名称。"}
    else if error.contains("Provide 1-32") {"请填写文件名匹配，例如 *.dmp。"}
    else if error.contains("filename wildcards") {"文件名匹配支持 * 和 ?；多个模式用分号分隔，请勿填写路径。"}
    else if error.contains("Rule does not exist") {"规则已被删除，请刷新列表。"}
    else if error.contains("Rule") && (error.contains("changed") || error.contains("revision")) {"规则已更新，请刷新后重试。"}
    else if error.contains("expired") || error.contains("invalidated") {"预览已过期或设置已变化，请重新预览。"}
    else if error.contains("Protected directory") || error.contains("Volume roots are protected") {"所选目录属于受保护范围，请选择具体的可清理文件夹。"}
    else if error.contains("Volume identity mismatch") {"磁盘身份已变化，请确认目标目录后重新保存规则。"}
    else if error.contains("Another") && error.contains("scan") {"扫描或清理正在进行，请稍后重试。"}
    else if error.contains("absolute local") || error.contains("Ambiguous") {"请选择完整的本地磁盘目录。"}
    else if error.contains("0x80070005") {"无法访问所选目录，请选择有权限读取的文件夹。"}
    else if error.contains("0x80070003") || error.contains("0x80070002") {"目标目录已移动或不存在，请重新选择。"}
    else if error.contains("Preview was truncated") {"预览未覆盖完整范围，请缩小目录或匹配条件后重新预览。"}
    else {"操作未完成，请检查目录和规则设置；原始原因见错误详情。"}
}


#[cfg(feature="ui-capture")]
pub(super) fn prepare_capture(app:&mut VolumeTrailApp,scope:&Path,scenario:&str)->Result<()> {
    for name in ["一个 temporary.dmp","第二份 crash.dmp","keep.txt"] {std::fs::write(scope.join(name),vec![0u8;8192])?;}
    let safety=Safety::for_data(&app.data)?;
    let rule=service::save_rule(&app.store,&safety,CleanupRule {name:"样本转储文件".into(),directory:scope.to_str().unwrap().into(),patterns:vec!["*.dmp".into()],..Default::default()})?;
    let plan=service::preview(&app.store,&safety,&[rule.id.clone()],&||false)?;
    let state=&mut app.cleanup_state;state.loaded=true;state.rules=vec![rule.clone()];state.patterns="*.dmp".into();state.editor=Some(rule.clone());state.original=Some(rule);
    state.capture_expand=matches!(scenario,"preview-expanded"|"preview-confirmed"|"long-path"|"result-details"|"partial"|"interrupted");
    if scenario.starts_with("preview") || scenario=="long-path" {
        state.plan=Some(plan);state.acknowledge=matches!(scenario,"preview-confirmed"|"preview-bottom");
        if matches!(scenario,"preview-expanded"|"preview-confirmed"|"preview-bottom") {state.capture_section=Some("preview");}
    }
    else if matches!(scenario,"result"|"result-details"|"partial"|"interrupted") {
        let run=crate::worker_cleanup::execute(&app.data,&plan.plan_id,"visual-fixture")?;
        state.runs=vec![run.clone()];state.result=Some((run.clone(),app.store.cleanup_items(&run.run_id)?));
        if scenario!="result" {state.capture_section=Some("result");}
        // Synthetic failure presentation only; these scenarios do not inject OS failures.
        if matches!(scenario,"partial"|"interrupted") {
            let (run,items)=state.result.as_mut().unwrap();run.deleted=1;run.allocated_bytes_estimate=8192;
            run.state=if scenario=="partial" {"partial"} else {"interrupted"}.into();run.verification="unverified".into();
            if scenario=="partial" {run.failed=1;items[1].state="failed".into();items[1].reason=Some("0x80070005".into());}
            else {run.unknown=1;run.message=Some("Synthetic interruption presentation".into());items[1].state="unknown".into();items[1].reason=Some("Process interrupted; do not replay".into());}
            state.runs=vec![run.clone()];
        }
    } else if scenario=="error" {state.message=Some("样本错误：文件正在被其他程序占用，已跳过。请检查预览范围后重试。".into());}
    // Presentation-only fixture aliases keep review screenshots free of host/user paths.
    let actual=scope.to_string_lossy();let shown=if scenario=="long-path" || scenario=="preview-expanded" {
        format!("{}:\\VolumeTrail Samples\\很长的项目目录与同名父目录\\2026 年归档样本\\应用运行与诊断日志\\临时转储文件夹",actual.chars().next().unwrap())
    } else {format!("{}:\\VolumeTrail Samples\\临时转储",actual.chars().next().unwrap())};
    for rule in &mut state.rules {rule.directory=shown.clone();}
    if let Some(rule)=&mut state.editor {rule.directory=shown.clone();}
    if let Some(rule)=&mut state.original {rule.directory=shown.clone();}
    if let Some(plan)=&mut state.plan {
        for rule in &mut plan.rules {rule.directory=shown.clone();}
        for candidate in &mut plan.candidates {candidate.path=candidate.path.replacen(actual.as_ref(),&shown,1);}
        for item in &mut plan.skipped {item.path=item.path.replacen(actual.as_ref(),&shown,1);}
    }
    for run in &mut state.runs {for rule in &mut run.rules {rule.directory=shown.clone();}}
    if let Some((run,items))=&mut state.result {for rule in &mut run.rules {rule.directory=shown.clone();}for item in items {item.path=item.path.replacen(actual.as_ref(),&shown,1);}}
    app.view=View::Cleanup;Ok(())
}

fn run_time(timestamp:i64)->String {chrono::DateTime::from_timestamp(timestamp,0).map(|at|at.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_else(|| "时间未知".into())}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_candidate_path_fits_narrow_column_and_keeps_filename() {
        let ctx=egui::Context::default();let path="E:\\VolumeTrail Samples\\很长的项目目录与同名父目录\\2026 年归档样本\\应用运行与诊断日志\\临时转储文件夹\\crash.dmp";
        let input=egui::RawInput {screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(196.0,500.0))),..Default::default()};
        let _=ctx.run(input,|ctx| {egui::CentralPanel::default().show(ctx,|ui| {
            let short=fitted_path(ui,path);
            assert!(short.starts_with("E:\\"));assert!(short.ends_with("crash.dmp"));assert!(short.contains('…'));
            let width=ui.painter().layout_no_wrap(short,egui::TextStyle::Body.resolve(ui.style()),INK).size().x;
            assert!(width<=180.0);
        });});
    }
    #[test]
    fn busy_cleanup_keeps_its_receipt_cancel_request_and_editor() {
        let temp=tempfile::tempdir().unwrap();let mut app=VolumeTrailApp::new(&egui::Context::default(),temp.path().into()).unwrap();
        let (tx,rx)=mpsc::channel();app.cleanup_state.rx=Some(rx);app.cleanup_state.cancel_request=Some("active-request".into());
        let rule=CleanupRule {name:"执行中的规则".into(),directory:"E:\\fixture".into(),..Default::default()};app.cleanup_state.editor=Some(rule.clone());
        app.cleanup_job(Job::Result("old-result".into()));app.cleanup_job(Job::Load);app.new_cleanup_rule("E:\\replacement".into());
        assert_eq!(app.cleanup_state.cancel_request.as_deref(),Some("active-request"));assert_eq!(app.cleanup_state.editor,Some(rule));
        tx.send(Err("original receipt".into())).unwrap();
        assert!(matches!(app.cleanup_state.rx.take().unwrap().try_recv(),Ok(Err(message)) if message=="original receipt"));
    }
}
