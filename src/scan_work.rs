//! Durable scan inputs and directory-statistic batches, separate from published history.
use crate::{model::*, scanner::Journal, store::Store};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}, time::Duration};

const FORMAT: u32 = 1;

pub(crate) fn path(data:&Path, root:&str)->PathBuf {
    data.join(format!("scan-work-{}.db",root.chars().next().unwrap().to_ascii_uppercase()))
}

pub(crate) fn remove(path:&Path)->Result<()> {
    // Only these application-owned files are removed, never a directory tree.
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let file=PathBuf::from(format!("{}{suffix}",path.display()));
        match fs::remove_file(&file) {
            Ok(())=>{}, Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},
            Err(error)=>return Err(error).with_context(||format!("Remove scan work {}",file.display())),
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct State {
    pub format:u32,
    pub task:String,
    pub root:String,
    pub base:Option<Checkpoint>,
    pub base_scan:i64,
    pub verification:Option<(Option<i64>,String)>,
    pub volume:String,
    pub root_id:u64,
    pub journal:u64,
    pub mode:ScanMode,
    pub high_usn:i64,
    pub mft_cursor:u64,
    pub mft_done:bool,
    pub journal_cursor:i64,
    pub processed:u64,
    pub counters:ScanCounters,
    pub pending:Vec<u64>,
    pub warnings:Vec<String>,
}

impl State {
    pub fn new(root:&str, base:Option<Checkpoint>, base_scan:i64, verification:Option<(Option<i64>,String)>,
        volume:&str, root_id:u64, journal:Journal, mode:ScanMode, pending:Vec<u64>, warnings:Vec<String>)->Self {
        let journal_cursor=if mode==ScanMode::Full {journal.next_usn} else {base.as_ref().unwrap().next_usn};
        Self {format:FORMAT,task:crate::cleanup::new_id(),root:root.into(),base,base_scan,verification,
            volume:volume.into(),root_id,journal:journal.id,mode,high_usn:journal.next_usn,
            mft_cursor:0,mft_done:mode==ScanMode::Incremental,journal_cursor,processed:0,
            counters:ScanCounters::default(),pending,warnings}
    }

    pub fn compatible(&self, root:&str, base:&Option<Checkpoint>, base_scan:i64, verification:&Option<(Option<i64>,String)>,
        volume:&str, root_id:u64, journal:Journal)->bool {
        let needed=if self.mft_done {self.journal_cursor} else {self.high_usn};
        self.same_target(root,verification,volume,root_id,journal.id) && &self.base==base && self.base_scan==base_scan
            && needed>=journal.first_usn && needed<=journal.next_usn
    }

    pub fn same_target(&self,root:&str,verification:&Option<(Option<i64>,String)>,volume:&str,root_id:u64,journal:u64)->bool {
        self.format==FORMAT && self.root==root && &self.verification==verification
            && self.volume==volume && self.root_id==root_id && self.journal==journal
    }
}

pub(crate) struct Work {
    pub db:Connection,
    pub state:State,
    pub path:PathBuf,
}

fn connection(path:&Path)->Result<Connection> {
    let db=Connection::open(path)?;
    db.busy_timeout(Duration::from_secs(3))?;
    db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;
    Ok(db)
}

impl Work {
    pub fn load(path:&Path)->Result<Option<Self>> {
        if !path.exists() {return Ok(None);}
        let db=connection(path)?;
        let payload:String=db.query_row("SELECT payload FROM meta WHERE id=1",[],|r|r.get(0))?;
        Ok(Some(Self {db,state:serde_json::from_str(&payload)?,path:path.into()}))
    }

    pub fn create(path:&Path,state:State)->Result<Self> {
        ensure!(!path.exists(),"Scan work already exists");
        let db=connection(path)?;
        db.execute_batch("CREATE TABLE meta(id INTEGER PRIMARY KEY CHECK(id=1),payload TEXT NOT NULL);
            CREATE TABLE entries(id INTEGER PRIMARY KEY,payload TEXT);
            CREATE TABLE events(id INTEGER PRIMARY KEY,deleted INTEGER NOT NULL,processed INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE statistics(sign INTEGER NOT NULL,part INTEGER NOT NULL,cursor INTEGER NOT NULL,
                complete INTEGER NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(sign,part));")?;
        db.execute("INSERT INTO meta VALUES(1,?)",[serde_json::to_string(&state)?])?;
        Ok(Self {db,state,path:path.into()})
    }

    pub fn begin(&self)->Result<()> {self.db.execute_batch("BEGIN IMMEDIATE;")?;Ok(())}

    pub fn save(&self)->Result<()> {
        self.db.execute("UPDATE meta SET payload=? WHERE id=1",[serde_json::to_string(&self.state)?])?;
        self.db.execute_batch("COMMIT;")?;Ok(())
    }

    pub fn stage(&self,mutation:Mutation)->Result<()> {
        let (id,payload)=match mutation {
            Mutation::Upsert(entry)=>(entry.id,Some(serde_json::to_string(&entry)?)),
            Mutation::Delete(id)=>(id,None),
        };
        let old:Option<Option<String>>=self.db.prepare_cached("SELECT payload FROM entries WHERE id=?")?.query_row([id as i64],|r|r.get(0)).optional()?;
        if old.as_ref()==Some(&payload) {return Ok(());}
        self.db.prepare_cached("INSERT INTO entries VALUES(?,?) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload")?.execute(params![id as i64,payload])?;
        self.db.prepare_cached("DELETE FROM statistics")?.execute([])?; // cached statistics belong to this exact input set
        Ok(())
    }

    pub fn event(&self,id:u64,deleted:bool)->Result<()> {
        self.db.prepare_cached("INSERT INTO events(id,deleted) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET deleted=MAX(deleted,excluded.deleted),processed=0")?.execute(params![id as i64,deleted])?;Ok(())
    }

    pub fn events(&self)->Result<Vec<(u64,bool)>> {
        let mut stmt=self.db.prepare("SELECT id,deleted FROM events WHERE processed=0 ORDER BY id LIMIT 512")?;
        Ok(stmt.query_map([],|r|Ok((r.get::<_,i64>(0)? as u64,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn import(&self,store:&mut Store,cancelled:&dyn Fn()->bool)->Result<()> {
        let mut stmt=self.db.prepare("SELECT id,payload FROM entries ORDER BY id")?;
        let mut rows=stmt.query([])?;
        let mut count=0;
        while let Some(row)=rows.next()? {
            if count%512==0 {ensure!(!cancelled(),"Scan cancelled");}count+=1;
            let mutation=match row.get::<_,Option<String>>(1)? {
                Some(payload)=>Mutation::Upsert(serde_json::from_str(&payload)?),
                None=>Mutation::Delete(row.get::<_,i64>(0)? as u64),
            };
            store.stage(mutation)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(base:Option<Checkpoint>)->State {
        State::new("C:\\",base,1,None,"volume",5,Journal {id:7,first_usn:0,next_usn:100},
            ScanMode::Full,vec![],vec![])
    }
    fn file(id:u64,size:u64)->Entry {
        Entry {id,parent:21,name:format!("file-{id}.log"),is_dir:false,logical:size,allocated:size,
            modified:1,attributes:0,links:vec![]}
    }
    fn outcome(usn:i64)->ScanOutcome {
        ScanOutcome {checkpoint:Checkpoint {volume_id:"volume".into(),journal_id:7,next_usn:usn,root_id:5},
            mode:if usn==10 {ScanMode::Full} else {ScanMode::Incremental},total:100000,free:50000,
            sampled_at:format!("2026-10-24T12:00:{usn:02}Z"),pending:vec![],warnings:vec![]}
    }
    fn base()->Vec<Mutation> {
        let mut entries=vec![];
        for (id,parent,name) in [(20,5,"before"),(21,20,"child"),(22,21,"nested")] {
            entries.push(Mutation::Upsert(Entry {id,parent,name:name.into(),is_dir:true,logical:0,allocated:0,
                modified:1,attributes:0,links:vec![]}));
        }
        entries.extend((100..2100).map(|id|Mutation::Upsert(file(id,123))));entries
    }
    fn input()->Vec<Mutation> {
        let mut entries=base();
        let Mutation::Upsert(mut directory)=entries.remove(0) else {unreachable!()};
        directory.name="after".into();
        vec![Mutation::Upsert(directory),Mutation::Upsert(file(100,456)),Mutation::Delete(101)]
    }

    #[test]
    fn collection_rows_and_cursor_commit_together_across_restart() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("work.db");
        let mut work=Work::create(&path,state(None)).unwrap();
        work.begin().unwrap();work.stage(Mutation::Upsert(file(100,1))).unwrap();
        work.state.mft_cursor=400;work.state.pending=vec![123];work.save().unwrap();
        work.begin().unwrap();work.stage(Mutation::Upsert(file(101,2))).unwrap();work.state.mft_cursor=800;
        drop(work);
        let work=Work::load(&path).unwrap().unwrap();
        assert_eq!(work.state.mft_cursor,400);assert_eq!(work.state.pending,vec![123]);
        assert_eq!(work.db.query_row("SELECT COUNT(*) FROM entries",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        work.begin().unwrap();work.event(100,false).unwrap();work.event(100,true).unwrap();work.save().unwrap();
        assert_eq!(work.events().unwrap(),vec![(100,true)]);
    }

    #[test]
    fn resume_requires_matching_identity_base_and_retained_journal_cursor() {
        let base=Some(outcome(10).checkpoint);let mut state=state(base.clone());
        let journal=Journal {id:7,first_usn:20,next_usn:200};
        assert!(state.compatible("C:\\",&base,1,&None,"volume",5,journal));
        state.mft_done=true;state.journal_cursor=50;
        assert!(state.compatible("C:\\",&base,1,&None,"volume",5,journal));
        assert!(!state.compatible("C:\\",&base,2,&None,"volume",5,journal));
        assert!(!state.compatible("C:\\",&base,1,&None,"other",5,journal));
        assert!(!state.compatible("C:\\",&base,1,&None,"volume",6,journal));
        assert!(!state.compatible("C:\\",&base,1,&None,"volume",5,Journal {id:8,..journal}));
        assert!(!state.compatible("C:\\",&base,1,&None,"volume",5,Journal {first_usn:51,..journal}));
        assert!(!state.same_target("C:\\",&Some((Some(1),"verification-task".into())),"volume",5,7));
        assert!(!state.same_target("D:\\",&None,"volume",5,7));
        assert!(!state.compatible("C:\\",&Some(outcome(20).checkpoint),1,&None,"volume",5,journal));
        state.format+=1;assert!(!state.compatible("C:\\",&base,1,&None,"volume",5,journal));
    }

    #[test]
    fn new_tail_requeues_identities_and_input_changes_invalidate_statistics_atomically() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("work.db");
        let work=Work::create(&path,state(None)).unwrap();
        work.begin().unwrap();work.stage(Mutation::Upsert(file(100,1))).unwrap();work.event(100,false).unwrap();work.save().unwrap();
        work.db.execute("UPDATE events SET processed=1",[]).unwrap();
        let stats=Statistics::open(&path).unwrap();stats.save(-1,0,100,true,&[("C:\\before".into(),".log".into(),1)]).unwrap();
        work.begin().unwrap();work.stage(Mutation::Upsert(file(100,1))).unwrap();work.event(100,false).unwrap();work.save().unwrap();
        assert_eq!(work.events().unwrap(),vec![(100,false)]);assert_eq!(stats.batches(-1).unwrap().len(),1);
        work.begin().unwrap();work.stage(Mutation::Upsert(file(100,2))).unwrap();
        work.db.execute_batch("ROLLBACK").unwrap();assert_eq!(stats.batches(-1).unwrap().len(),1);
        work.begin().unwrap();work.stage(Mutation::Upsert(file(100,2))).unwrap();work.save().unwrap();
        assert!(stats.batches(-1).unwrap().is_empty());
    }

    fn table(db:&Connection,name:&str)->Vec<Vec<rusqlite::types::Value>> {
        let mut stmt=db.prepare(&format!("SELECT * FROM {name} ORDER BY 1,2,3")).unwrap();
        let columns=stmt.column_count();
        stmt.query_map([],|r|(0..columns).map(|i|r.get(i)).collect()).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap()
    }

    #[test]
    fn both_statistics_phases_resume_without_duplicate_deltas_and_publish_once() {
        let dir=tempfile::tempdir().unwrap();let main=dir.path().join("history.db");let expected=dir.path().join("expected.db");
        let mut store=Store::open(&main).unwrap();let mut reference=Store::open(&expected).unwrap();
        store.publish("C:\\",&base(),&outcome(10)).unwrap();reference.publish("C:\\",&base(),&outcome(10)).unwrap();
        let path=dir.path().join("work.db");let work=Work::create(&path,state(Some(outcome(10).checkpoint))).unwrap();
        work.begin().unwrap();for mutation in input() {work.stage(mutation).unwrap();}work.save().unwrap();
        for sign in [-1,1] {
            store.begin_stage().unwrap();work.import(&mut store,&||false).unwrap();
            store.set_scan_work(path.clone(),work.state.task.clone());
            let result=store.commit_stage_control("C:\\",&outcome(20),&mut || {
                let count:i64=work.db.query_row("SELECT COUNT(*) FROM statistics WHERE sign=?",[sign],|r|r.get(0))?;
                ensure!(count==0,"injected statistics interruption");Ok(())
            });
            assert!(result.is_err());assert_eq!(store.reports().unwrap().len(),1);
            assert_eq!(store.checkpoint("C:\\").unwrap().unwrap().next_usn,10);
            let actual=Connection::open(&main).unwrap();let wanted=Connection::open(&expected).unwrap();
            for name in ["nodes","folders","volumes","volume_totals","changes","folder_changes","extension_changes"] {
                assert_eq!(table(&actual,name),table(&wanted,name),"interrupted {sign}: {name}");
            }
            assert!(!Statistics::open(&path).unwrap().batches(sign).unwrap().is_empty());
        }
        store.begin_stage().unwrap();work.import(&mut store,&||false).unwrap();
        let report=store.commit_stage("C:\\",&outcome(20)).unwrap();store.clear_scan_work();
        assert_eq!(store.published_task(&work.state.task).unwrap(),Some(report));
        assert_eq!(store.reports().unwrap().len(),2);
        reference.publish("C:\\",&input(),&outcome(20)).unwrap();
        let actual=Connection::open(&main).unwrap();let wanted=Connection::open(&expected).unwrap();
        for name in ["nodes","folders","volumes","volume_totals","changes","folder_changes","extension_changes"] {
            assert_eq!(table(&actual,name),table(&wanted,name),"{name}");
        }
        assert_eq!(actual.query_row("PRAGMA integrity_check",[],|r|r.get::<_,String>(0)).unwrap(),"ok");
        assert!(actual.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
        // A completed job is recognized even if its work file survived a process exit.
        drop(work);let saved=Work::load(&path).unwrap().unwrap();
        assert_eq!(store.published_task(&saved.state.task).unwrap(),Some(report));
        drop(saved);remove(&path).unwrap();assert!(!path.exists());
        store.publish("C:\\",&[Mutation::Upsert(file(100,789))],&outcome(30)).unwrap();
    }

    #[test]
    fn crash_fixture() {
        let Ok(directory)=std::env::var("VOLUMETRAIL_WORK_CRASH_FIXTURE") else {return;};
        let path=Path::new(&directory).join("work.db");let mut work=Work::create(&path,state(None)).unwrap();
        work.begin().unwrap();work.stage(Mutation::Upsert(file(100,1))).unwrap();work.state.mft_cursor=400;work.save().unwrap();
        work.begin().unwrap();work.stage(Mutation::Upsert(file(101,2))).unwrap();work.state.mft_cursor=800;
        std::process::exit(99);
    }

    #[test]
    fn abrupt_process_exit_recovers_the_last_complete_batch() {
        let dir=tempfile::tempdir().unwrap();
        let output=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","scan_work::tests::crash_fixture","--nocapture"])
            .env("VOLUMETRAIL_WORK_CRASH_FIXTURE",dir.path()).output().unwrap();
        assert_eq!(output.status.code(),Some(99));
        let work=Work::load(&dir.path().join("work.db")).unwrap().unwrap();
        assert_eq!(work.state.mft_cursor,400);
        assert_eq!(work.db.query_row("SELECT COUNT(*) FROM entries",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(work.db.query_row("PRAGMA integrity_check",[],|r|r.get::<_,String>(0)).unwrap(),"ok");
    }
}

/// Each part contains derived (parent path, extension, allocated bytes) contributions.
/// The cursor and its data are one SQLite row, so recovery cannot apply half a batch.
pub(crate) struct Statistics {db:Connection}
pub(crate) type Contribution=(String,String,u64);
impl Statistics {
    pub fn open(path:&Path)->Result<Self> {Ok(Self {db:connection(path)?})}
    pub fn batches(&self,sign:i64)->Result<Vec<(i64,bool,Vec<Contribution>)>> {
        let mut stmt=self.db.prepare("SELECT cursor,complete,payload FROM statistics WHERE sign=? ORDER BY part")?;
        let rows=stmt.query_map([sign],|r|Ok((r.get(0)?,r.get(1)?,r.get::<_,String>(2)?)))?
            .collect::<rusqlite::Result<Vec<(i64,bool,String)>>>()?;
        rows.into_iter().map(|(cursor,complete,payload)|Ok((cursor,complete,serde_json::from_str(&payload)?))).collect()
    }
    pub fn save(&self,sign:i64,part:i64,cursor:i64,complete:bool,rows:&[Contribution])->Result<()> {
        self.db.execute("INSERT INTO statistics VALUES(?,?,?,?,?)",params![sign,part,cursor,complete,serde_json::to_string(rows)?])?;Ok(())
    }
}
