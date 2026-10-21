//! Opt-in development harness: actual Windows/OpenGL frames with isolated fixtures.
//! It is excluded from normal builds and does not drive the user's desktop.
use super::*;
use crate::model::*;
use std::{fs,sync::{Arc,atomic::{AtomicBool,Ordering}}};

pub fn run()->Result<()> {
    let mut args=std::env::args().skip(1);
    let output=PathBuf::from(args.next().ok_or_else(||anyhow::anyhow!("Usage: ui_capture OUTPUT.png overview|preview|result|error|empty WIDTH HEIGHT SCALE"))?);
    let output=if output.is_absolute() {output} else {std::env::current_dir()?.join(output)};
    let output=PathBuf::from(output.to_string_lossy().replace('/',"\\"));
    let scenario=args.next().unwrap_or_else(||"overview".into());
    let width=args.next().unwrap_or_else(||"1050".into()).parse::<f32>()?;
    let height=args.next().unwrap_or_else(||"720".into()).parse::<f32>()?;
    let scale=args.next().unwrap_or_else(||"1.0".into()).parse::<f32>()?;
    let parent=output.parent().unwrap();fs::create_dir_all(parent)?;
    let fixture=parent.join(format!("fixture-{}",crate::cleanup::new_id()));
    let data=fixture.join("data");let scope=fixture.join("中文 temporary sample scope");fs::create_dir_all(&data)?;fs::create_dir_all(&scope)?;
    let drive=scope.to_str().unwrap().chars().next().unwrap().to_ascii_uppercase();let root=format!("{drive}:\\");
    let mut store=Store::open(&worker::store_path(&data))?;
    if scenario!="empty" {
        let volume=crate::cleanup::windows::volume_id(scope.to_str().unwrap())?;
        let mut sample=ScanOutcome {checkpoint:Checkpoint {volume_id:volume,journal_id:1,next_usn:10,root_id:5},mode:ScanMode::Full,
            total:1024*1024*1024*1024,free:310*1024*1024*1024,sampled_at:"2026-10-20T09:00:00+00:00".into(),pending:vec![],warnings:vec![]};
        let directory=|id,parent,name:&str|Mutation::Upsert(Entry {id,parent,name:name.into(),is_dir:true,logical:0,allocated:0,modified:0,attributes:0,links:vec![]});
        let file=|id,parent,name:&str,size|Mutation::Upsert(Entry {id,parent,name:name.into(),is_dir:false,logical:size,allocated:size,modified:0,attributes:0,links:vec![]});
        store.publish(&root,&[directory(20,5,"Samples"),directory(21,20,"相同目录名"),directory(22,5,"工作目录"),directory(23,22,"相同目录名"),
            file(30,21,"cache.bin",1024*1024*1024),file(31,23,"archive.bin",3*1024*1024*1024)],&sample)?;
        sample.mode=ScanMode::Incremental;sample.checkpoint.next_usn=20;sample.free-=2*1024*1024*1024;sample.sampled_at="2026-10-21T09:00:00+00:00".into();
        let second=store.publish(&root,&[file(30,21,"cache.bin",4*1024*1024*1024),file(31,23,"archive.bin",2*1024*1024*1024)],&sample)?;
        if scenario=="history" {
            sample.checkpoint.next_usn=30;sample.free+=1024*1024*1024;sample.sampled_at="2026-10-21T09:00:08+00:00".into();
            let post=store.publish(&root,&[file(30,21,"cache.bin",3*1024*1024*1024)],&sample)?;
            store.mark_verification(second,"verified",Some(post))?;
        }
    }
    let captured=Arc::new(AtomicBool::new(false));let done=captured.clone();
    eframe::run_native("VolumeTrail visual fixture",eframe::NativeOptions {
        viewport:egui::ViewportBuilder::default().with_inner_size([width,height]).with_position([-10000.0,-10000.0]).with_visible(false),
        ..Default::default()
    },Box::new(move |cc| {
        cc.egui_ctx.set_zoom_factor(scale);
        let mut app=VolumeTrailApp::new(&cc.egui_ctx,data.clone())?;app.drive=drive;app.refresh();
        if scenario=="history" {app.view=View::History;}
        else if scenario!="overview" && scenario!="empty" {super::cleanup::prepare_capture(&mut app,&scope,&scenario)?;}
        Ok(Box::new(Capture {app,output,done,started:Instant::now(),requested:false,frames:0}))
    })).map_err(|e|anyhow::anyhow!("Native visual capture: {e}"))?;
    anyhow::ensure!(captured.load(Ordering::SeqCst),"Native capture timed out without an image");
    Ok(())
}

struct Capture {app:VolumeTrailApp,output:PathBuf,done:Arc<AtomicBool>,started:Instant,requested:bool,frames:u32}
impl eframe::App for Capture {
    fn update(&mut self,ctx:&egui::Context,frame:&mut eframe::Frame) {
        self.app.update(ctx,frame);self.frames+=1;
        if let Some(image)=ctx.input(|i|i.events.iter().find_map(|event|if let egui::Event::Screenshot {image,..}=event {Some(image.clone())} else {None})) {
            let bytes:Vec<_>=image.pixels.iter().flat_map(|p|p.to_array()).collect();
            image::save_buffer(&self.output,&bytes,image.size[0] as u32,image.size[1] as u32,image::ColorType::Rgba8).unwrap();
            self.done.store(true,Ordering::SeqCst);ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if !self.requested && self.frames>15 && self.started.elapsed()>Duration::from_millis(500) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));self.requested=true;
        }
        if self.started.elapsed()>Duration::from_secs(10) {ctx.send_viewport_cmd(egui::ViewportCommand::Close);}
        ctx.request_repaint_after(Duration::from_millis(30));
    }
}
