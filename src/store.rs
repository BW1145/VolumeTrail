use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;
use std::{collections::{HashMap, HashSet}, fs, path::{Path, PathBuf}, time::{Duration, Instant}};
mod operations;
mod cleanup;
mod compact;
mod subtree;
mod paths;
mod path_migration;
mod history;

pub struct Store {
    db: Connection,
    path: PathBuf,
    staging: bool,
    verification_context: Option<(Option<i64>,String)>,
    cancel_path: Option<PathBuf>,
    scan_work: Option<(PathBuf,String)>,
    shared_paths: bool,
    pub commit_timings: CommitTimings,
}

#[derive(Clone, Debug)]
pub struct Change {
    pub old_path: String,
    pub new_path: String,
    pub logical_delta: i64,
    pub allocated_delta: i64,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub id: i64,
    pub root: String,
    pub volume: String,
    pub finished: String,
    pub mode: String,
    pub total: u64,
    pub free: u64,
    pub logical: u64,
    pub allocated: u64,
    pub file_count: u64,
    pub warnings: Vec<String>,
    pub sampled: String,
    pub details: bool,
    pub aggregate: u8,
    pub performance: Option<ScanPerformance>,
    pub operation_pre: Option<i64>,
    pub verification: String,
    pub comparison_from: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct FolderItem {
    pub id: u64,
    pub name: String,
    pub is_dir: bool,
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
}

#[derive(Clone, Debug)]
pub struct RankedItem {
    pub path: String,
    pub allocated: u64,
    pub logical: u64,
    pub files: u64,
    pub modified: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct FolderGrowth {
    pub path: String,
    pub allocated_delta: i128,
    pub moved_delta: i128,
    pub changes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct FolderBreakdown {
    pub allocated_delta: i128,
    pub moved_delta: i128,
    pub direct_delta: i128,
    pub children: Vec<FolderGrowth>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ExtensionBreakdown {
    pub items: Vec<(String, i128, u64)>,
    pub intervals: u64,
    pub complete_intervals: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct StorageUsage {
    pub history_bytes: u64,
    pub index_bytes: u64,
    pub timeline_bytes: u64,
    pub unused_bytes: u64,
    pub database_bytes: u64,
    pub backup_bytes: u64,
}

type Directories = HashMap<u64, (u64, String)>;
type FolderDeltas = HashMap<u64, (i128, i128, i128)>;
type GrowthDeltas = HashMap<String, (i128, i128, u64)>;
type ExtensionDeltas = HashMap<(String, String), (i128, u64)>;

impl Store {
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_secs(3))?;
        ensure!(db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? >= 2,
            "History database needs a one-time upgrade; open VolumeTrail first");
        compact::configure(&db)?;
        let shared_paths = db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? >= 14;
        Ok(Self { db, path: path.to_path_buf(), staging: false, verification_context: None, cancel_path: None, scan_work: None, shared_paths, commit_timings: CommitTimings::default() })
    }

    pub fn data_version(&self) -> Result<i64> {
        Ok(self.db.query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path).context("Open history database")?;
        db.busy_timeout(Duration::from_secs(3))?;
        db.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA cache_size=-8192;
            PRAGMA foreign_keys=ON;
            PRAGMA journal_size_limit=4194304;")?;
        if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? < 2 {
            db.execute_batch(
            "CREATE TABLE IF NOT EXISTS volumes(root TEXT PRIMARY KEY, checkpoint TEXT NOT NULL, pending TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS nodes(
                volume TEXT NOT NULL, id INTEGER NOT NULL, parent INTEGER NOT NULL,
                name TEXT NOT NULL, is_dir INTEGER NOT NULL, logical INTEGER NOT NULL,
                allocated INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(volume,id));
             CREATE INDEX IF NOT EXISTS nodes_parent ON nodes(volume,parent);
             CREATE TABLE IF NOT EXISTS staging(id INTEGER PRIMARY KEY, payload TEXT);
             CREATE TABLE IF NOT EXISTS scans(
                id INTEGER PRIMARY KEY, root TEXT NOT NULL, volume TEXT NOT NULL,
                finished TEXT NOT NULL, mode TEXT NOT NULL, total INTEGER NOT NULL,
                free INTEGER NOT NULL, logical INTEGER NOT NULL, allocated INTEGER NOT NULL,
                file_count INTEGER NOT NULL, warnings TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS changes(
                scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
                old_path TEXT NOT NULL, new_path TEXT NOT NULL,
                logical_delta INTEGER NOT NULL, allocated_delta INTEGER NOT NULL, kind TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS changes_scan ON changes(scan);
             CREATE TABLE IF NOT EXISTS folders(
                volume TEXT NOT NULL, id INTEGER NOT NULL, logical INTEGER NOT NULL,
                allocated INTEGER NOT NULL, files INTEGER NOT NULL, PRIMARY KEY(volume,id));"
            )?;
            let existing: i64 = db.query_row("SELECT COUNT(*) FROM scans", [], |r| r.get(0))?;
            if existing > 0 {
                let backup = path.with_file_name(format!("history-before-v2-{}-{}.db",
                    chrono::Local::now().format("%Y%m%d-%H%M%S"), std::process::id()));
                db.backup(rusqlite::MAIN_DB, &backup, None)
                    .with_context(|| format!("Back up history to {}", backup.display()))?;
            }
            db.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS folder_changes(
                    scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
                    path TEXT NOT NULL, allocated_delta INTEGER NOT NULL,
                    moved_delta INTEGER NOT NULL, events INTEGER NOT NULL,
                    PRIMARY KEY(scan,path));
                CREATE TABLE IF NOT EXISTS carry_folders(
                    volume TEXT NOT NULL, path TEXT NOT NULL,
                    allocated_delta INTEGER NOT NULL, moved_delta INTEGER NOT NULL,
                    events INTEGER NOT NULL, PRIMARY KEY(volume,path));
                CREATE TABLE IF NOT EXISTS volume_totals(
                    volume TEXT PRIMARY KEY, logical INTEGER NOT NULL,
                    allocated INTEGER NOT NULL, files INTEGER NOT NULL,
                    total INTEGER NOT NULL, free INTEGER NOT NULL, indexed_at TEXT NOT NULL);
                ALTER TABLE scans ADD COLUMN sampled TEXT NOT NULL DEFAULT '';
                ALTER TABLE scans ADD COLUMN details INTEGER NOT NULL DEFAULT 1;
                ALTER TABLE scans ADD COLUMN aggregate INTEGER NOT NULL DEFAULT 0;
                UPDATE scans SET sampled=finished,aggregate=1;
                INSERT INTO volume_totals SELECT n.volume,COALESCE(SUM(n.logical),0),
                    COALESCE(SUM(n.allocated),0),COUNT(*),
                    COALESCE((SELECT total FROM scans s WHERE s.volume=n.volume ORDER BY id DESC LIMIT 1),0),
                    COALESCE((SELECT free FROM scans s WHERE s.volume=n.volume ORDER BY id DESC LIMIT 1),0),
                    COALESCE((SELECT finished FROM scans s WHERE s.volume=n.volume ORDER BY id DESC LIMIT 1),'')
                    FROM nodes n WHERE n.is_dir=0 GROUP BY n.volume;
                PRAGMA user_version=2;")?;
            // Old changes have net deltas but not the before/after sizes needed to recover moves.
            let mut scan_ids = db.prepare("SELECT DISTINCT scan FROM changes")?;
            let ids = scan_ids.query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(scan_ids);
            for id in ids {
                let mut growth = GrowthDeltas::new();
                let mut stmt = db.prepare("SELECT old_path,new_path,allocated_delta FROM changes WHERE scan=?")?;
                let mut rows = stmt.query([id])?;
                while let Some(row) = rows.next()? {
                    let old: String = row.get(0)?;
                    let new: String = row.get(1)?;
                    let delta: i64 = row.get(2)?;
                    if let Some(path) = folder_parent(if delta >= 0 { &new } else { &old }) {
                        growth_entry(&mut growth, path, i128::from(delta), 0, 1);
                    }
                }
                drop(rows);
                drop(stmt);
                write_legacy_growth(&db, id, &growth)?;
            }
            db.execute_batch("COMMIT")?;
        }
        if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? < 3 {
            db.execute_batch("BEGIN IMMEDIATE;
                CREATE INDEX IF NOT EXISTS folders_size ON folders(volume,allocated DESC);
                PRAGMA user_version=4;
                COMMIT;")?;
        }
        if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? < 4 {
            db.execute_batch("BEGIN IMMEDIATE;
                DROP INDEX IF EXISTS nodes_size;
                PRAGMA user_version=4;
                COMMIT;")?;
        }
        if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? < 5 {
            db.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS scan_ids(next INTEGER NOT NULL);
                INSERT INTO scan_ids SELECT COALESCE(MAX(id),0)+1 FROM scans
                    WHERE NOT EXISTS(SELECT 1 FROM scan_ids);
                PRAGMA user_version=5;
                COMMIT;")?;
        }
        if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? < 6 {
            db.execute_batch("BEGIN IMMEDIATE;
                ALTER TABLE scans ADD COLUMN performance TEXT;
                PRAGMA user_version=6;
                COMMIT;")?;
        }
        if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? < 7 {
            db.execute_batch("BEGIN IMMEDIATE;
                ALTER TABLE scans ADD COLUMN extensions_complete INTEGER NOT NULL DEFAULT 0;
                CREATE TABLE extension_changes(
                    scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
                    path TEXT NOT NULL, extension TEXT NOT NULL, delta INTEGER NOT NULL,
                    events INTEGER NOT NULL, PRIMARY KEY(scan,path,extension));
                CREATE TABLE carry_extensions(
                    volume TEXT NOT NULL, path TEXT NOT NULL, extension TEXT NOT NULL,
                    delta INTEGER NOT NULL, events INTEGER NOT NULL,
                    PRIMARY KEY(volume,path,extension));
                CREATE TABLE carry_extension_quality(volume TEXT PRIMARY KEY, complete INTEGER NOT NULL);
                CREATE INDEX nodes_directories ON nodes(volume,id,parent,name) WHERE is_dir=1;
                PRAGMA user_version=7; COMMIT;")?;
        }
        Self::migrate_operations(&db)?;
        Self::migrate_cleanup(&db)?;
        Self::migrate_baselines(&db)?;
        Self::migrate_pending_baselines(&db)?;
        Self::migrate_compact(&db)?;
        if db.query_row("PRAGMA user_version", [], |r| r.get::<_,i64>(0))? < 13 {
            let exists=db.prepare("PRAGMA table_info(scans)")?.query_map([],|r|r.get::<_,String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?.iter().any(|column|column=="task_id");
            let tx=db.unchecked_transaction()?;
            if !exists {tx.execute("ALTER TABLE scans ADD COLUMN task_id TEXT",[])?;}
            tx.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS scans_task ON scans(task_id) WHERE task_id IS NOT NULL;
                PRAGMA user_version=13;")?;tx.commit()?;
        }
        path_migration::migrate(&db, path)?;
        compact::configure(&db)?;
        Ok(Self { db, path: path.to_path_buf(), staging: false, verification_context: None, cancel_path: None, scan_work: None, shared_paths: true, commit_timings: CommitTimings::default() })
    }

    pub fn set_cancel_path(&mut self, path: PathBuf) { self.cancel_path = Some(path); }

    pub(crate) fn scan_context(&self)->Option<(Option<i64>,String)> {self.verification_context.clone()}
    pub(crate) fn set_scan_work(&mut self,path:PathBuf,task:String) {self.scan_work=Some((path,task));}
    pub(crate) fn clear_scan_work(&mut self) {self.scan_work=None;}
    pub(crate) fn scan_base_id(&self,volume:&str)->Result<i64> {
        Ok(self.db.query_row("SELECT COALESCE(MAX(id),0) FROM scans WHERE volume=?",[volume],|r|r.get(0))?)
    }
    pub(crate) fn published_task(&self,task:&str)->Result<Option<i64>> {
        Ok(self.db.query_row("SELECT id FROM scans WHERE task_id=?",[task],|r|r.get(0)).optional()?)
    }

    pub fn database_drive(&self) -> Option<char> {
        self.path.to_str()?.chars().next().filter(|letter| letter.is_ascii_alphabetic())
            .map(|letter| letter.to_ascii_uppercase())
    }

    fn check_cancel(&self) -> Result<()> {
        ensure!(!self.cancel_path.as_ref().is_some_and(|path| path.exists()), "Scan cancelled");
        Ok(())
    }

    fn stop_progress_handler(&self) {
        self.db.progress_handler(0, None::<fn() -> bool>);
    }

    pub fn checkpoint(&self, root: &str) -> Result<Option<Checkpoint>> {
        let json: Option<String> = self.db.query_row("SELECT checkpoint FROM volumes WHERE root=?", [root], |r| r.get(0)).optional()?;
        json.map(|s| serde_json::from_str(&s).map_err(Into::into)).transpose()
    }

    pub fn pending(&self, root: &str) -> Result<Vec<u64>> {
        let json: Option<String> = self.db.query_row("SELECT pending FROM volumes WHERE root=?", [root], |r| r.get(0)).optional()?;
        Ok(json.map(|s| serde_json::from_str(&s)).transpose()?.unwrap_or_default())
    }

    pub fn indexed_entry(&self, volume: &str, id: u64) -> Result<Option<Entry>> {
        Ok(self.db.query_row(&format!("SELECT {} FROM nodes WHERE volume=? AND id=?", compact::ENTRY_COLUMNS),
            params![volume, id as i64], compact::read_entry).optional()?)
    }

    pub fn missing_parent_ids(&self, volume: &str, full: bool) -> Result<Vec<u64>> {
        let mut candidates = HashSet::new();
        let mut staged = HashSet::new();
        let mut stmt = self.db.prepare("SELECT id,payload FROM staging")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            staged.insert(row.get::<_,i64>(0)? as u64);
            if let Some(payload) = row.get::<_,Option<String>>(1)? {
                let entry: Entry = serde_json::from_str(&payload)?;
                if entry.parent != entry.id { candidates.insert(entry.parent); }
            }
        }
        if !full {
            let mut stmt = self.db.prepare("SELECT f.id FROM folders f LEFT JOIN nodes n ON f.volume=n.volume AND f.id=n.id
                WHERE f.volume=? AND n.id IS NULL")?;
            for row in stmt.query_map([volume], |r| r.get::<_,i64>(0))? { candidates.insert(row? as u64); }
        }
        let mut known = self.db.prepare("SELECT EXISTS(SELECT 1 FROM nodes WHERE volume=? AND id=? AND is_dir=1)")?;
        let mut missing = Vec::new();
        for id in candidates {
            if !staged.contains(&id) && (full || !known.query_row(params![volume,id as i64], |r| r.get::<_,bool>(0))?) {
                missing.push(id);
            }
        }
        Ok(missing)
    }

    pub fn report_warnings(&self, id: i64) -> Result<Vec<String>> {
        let json: String = self.db.query_row("SELECT warnings FROM scans WHERE id=?", [id], |r| r.get(0))?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn begin_stage(&mut self) -> Result<()> {
        ensure!(!self.staging, "A scan is already being staged");
        self.check_cancel()?;
        if let Some(path) = self.cancel_path.clone() {
            self.db.progress_handler(10000, Some(move || path.exists()));
        }
        if let Err(error) = self.db.execute_batch("PRAGMA synchronous=FULL; BEGIN IMMEDIATE; DELETE FROM staging;") {
            self.stop_progress_handler();
            let _ = self.db.execute_batch("ROLLBACK");
            return Err(error.into());
        }
        self.staging = true;
        Ok(())
    }

    pub fn stage(&mut self, mutation: Mutation) -> Result<()> {
        ensure!(self.staging, "No active scan");
        let (id, payload) = match mutation {
            Mutation::Upsert(e) => (e.id, Some(serde_json::to_string(&e)?)),
            Mutation::Delete(id) => (id, None),
        };
        self.db.prepare_cached("INSERT INTO staging VALUES(?,?) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload")?
            .execute(params![id as i64, payload])?;
        Ok(())
    }

    pub fn discard_stage(&mut self) -> Result<()> {
        self.stop_progress_handler();
        if self.staging {
            self.db.execute_batch("ROLLBACK")?;
            self.staging = false;
        }
        Ok(())
    }

    pub fn publish(&mut self, root: &str, entries: &[Mutation], outcome: &ScanOutcome) -> Result<i64> {
        self.begin_stage()?;
        for entry in entries {
            if let Err(error) = self.stage(entry.clone()) { self.discard_stage()?; return Err(error); }
        }
        self.commit_stage(root, outcome)
    }

    pub fn commit_stage(&mut self, root: &str, outcome: &ScanOutcome) -> Result<i64> {
        self.commit_stage_control(root, outcome, &mut || Ok(()))
    }

    pub fn commit_stage_control(&mut self, root: &str, outcome: &ScanOutcome,
        control: &mut dyn FnMut() -> Result<()>) -> Result<i64> {
        ensure!(self.staging, "No active scan");
        let result = self.apply_stage(root, outcome, control);
        match result {
            Ok(id) => {
                if let Err(error) = self.check_cancel() { self.discard_stage()?; return Err(error); }
                if let Err(error) = self.db.execute("DELETE FROM staging", []) {
                    self.discard_stage()?;
                    return Err(error.into());
                }
                if let Err(error) = self.check_cancel() { self.discard_stage()?; return Err(error); }
                self.stop_progress_handler();
                if let Some((_,task))=&self.scan_work {
                    if let Err(error)=self.db.execute("UPDATE scans SET task_id=? WHERE id=?",params![task,id]) {
                        self.discard_stage()?;return Err(error.into());
                    }
                }
                if let Err(error) = self.db.execute("UPDATE scans SET finished=? WHERE id=?",
                    params![chrono::Local::now().to_rfc3339(), id]) {
                    self.discard_stage()?;
                    return Err(error.into());
                }
                if let Err(error) = self.db.execute_batch("COMMIT") {
                    self.discard_stage()?;
                    return Err(error.into());
                }
                self.staging = false;
                Ok(id)
            }
            Err(error) => { self.discard_stage()?; Err(error) }
        }
    }

    fn apply_stage(&mut self, root: &str, outcome: &ScanOutcome,
        control: &mut dyn FnMut() -> Result<()>) -> Result<i64> {
        ensure!(self.verification_context.is_none() || outcome.mode==ScanMode::Incremental,
            "Verification commits must be incremental");
        let preparing = Instant::now();
        let mut history_paths = paths::Writer::new(&self.db);
        let cp = &outcome.checkpoint;
        let old_cp = self.checkpoint(root)?;
        if outcome.mode == ScanMode::Incremental {
            let old = old_cp.as_ref().context("Incremental scan needs a baseline")?;
            ensure!(old.volume_id == cp.volume_id && old.journal_id == cp.journal_id && old.root_id == cp.root_id,
                "Volume or journal changed: a full baseline is required");
            ensure!(cp.next_usn >= old.next_usn, "Journal checkpoint moved backwards");
        }
        let had_baseline: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM volume_totals WHERE volume=?)", [&cp.volume_id], |r| r.get(0))?;
        let old_dirs = self.directories(&cp.volume_id)?;
        let mut new_dirs = if outcome.mode == ScanMode::Full { HashMap::new() } else { old_dirs.clone() };
        let mut folder_deltas = FolderDeltas::new();
        let mut growth = GrowthDeltas::new();
        let mut extensions = ExtensionDeltas::new();
        let mut staged = self.db.prepare("SELECT id,payload FROM staging")?;
        let mut rows = staged.query([])?;
        let mut index = 0;
        while let Some(row) = rows.next()? {
            if index % 512 == 0 { self.check_cancel()?; control()?; }
            index += 1;
            let id = row.get::<_, i64>(0)? as u64;
            if let Some(json) = row.get::<_, Option<String>>(1)? {
                let entry: Entry = serde_json::from_str(&json)?;
                if entry.is_dir { new_dirs.insert(id, (entry.parent, entry.name)); } else { new_dirs.remove(&id); }
            } else { new_dirs.remove(&id); }
        }
        drop(rows);
        let changed_dirs: HashSet<u64> = old_dirs.keys().chain(new_dirs.keys()).copied()
            .filter(|id| old_dirs.get(id) != new_dirs.get(id)).collect();
        self.db.execute_batch("CREATE TEMP TABLE IF NOT EXISTS changed_directories(id INTEGER PRIMARY KEY); DELETE FROM changed_directories;")?;
        for id in &changed_dirs {
            self.db.prepare_cached("INSERT INTO changed_directories VALUES(?)")?.execute([*id as i64])?;
        }
        if had_baseline && outcome.mode == ScanMode::Incremental && !changed_dirs.is_empty() {
            self.unchanged_subtree_changes(&cp.volume_id, root, cp.root_id, &old_dirs, &new_dirs,
                -1, &mut extensions, &mut growth, control)?;
        }
        self.commit_timings.prepare_ms = preparing.elapsed().as_millis() as u64;
        let entries_started = Instant::now();
        let scan: i64 = self.db.query_row("SELECT next FROM scan_ids", [], |r| r.get(0))?;
        self.db.execute("UPDATE scan_ids SET next=next+1", [])?;
        self.db.execute("INSERT INTO scans(id,root,volume,finished,mode,total,free,logical,allocated,file_count,warnings,sampled,details,aggregate) VALUES(?,?,?,?,?,?,?,0,0,0,?,?,1,2)",
            params![scan, root, cp.volume_id, outcome.sampled_at, if outcome.mode == ScanMode::Full { "full" } else { "incremental" },
                outcome.total as i64, outcome.free as i64, serde_json::to_string(&outcome.warnings)?, outcome.sampled_at])?;
        self.db.execute("UPDATE scans SET comparison_from=(SELECT CASE WHEN cleanup_pending=0 THEN last_scan END FROM scan_baselines WHERE volume=?) WHERE id=?",params![cp.volume_id,scan])?;
        self.db.execute("INSERT INTO scan_baselines(volume,last_scan,cleanup_pending) VALUES(?,?,0) ON CONFLICT(volume) DO UPDATE SET last_scan=excluded.last_scan,cleanup_pending=0",params![cp.volume_id,scan])?;
        if let Some((pre,run)) = &self.verification_context {
            if let Some(pre)=pre {
                self.db.execute("UPDATE scans SET operation_pre=?,verification='verified' WHERE id=?",params![pre,scan])?;
                self.db.execute("UPDATE scans SET verification='verified' WHERE id=?",[pre])?;
            }
            self.db.execute("UPDATE cleanup_runs SET payload=json_set(payload,'$.post_scan',?,'$.verification','verified') WHERE id=?",params![scan,run])?;
        }
        let old_total: (i64, i64, i64) = self.db.query_row(
            "SELECT logical,allocated,files FROM volume_totals WHERE volume=?", [&cp.volume_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?.unwrap_or_default();
        let mut volume_delta = (0i128, 0i128, 0i128);
        if outcome.mode == ScanMode::Full {
            let columns = compact::ENTRY_COLUMNS.split(',').map(|name| format!("n.{name}")).collect::<Vec<_>>().join(",");
            let mut absent = self.db.prepare(&format!("SELECT {columns} FROM nodes n LEFT JOIN staging s ON s.id=n.id WHERE n.volume=? AND s.payload IS NULL"))?;
            let mut rows = absent.query([&cp.volume_id])?;
            let mut index = 0;
            while let Some(row) = rows.next()? {
                if index % 512 == 0 { self.check_cancel()?; control()?; }
                index += 1;
                let old = compact::read_entry(row)?;
                if !old.is_dir {
                    volume_delta.0 -= i128::from(old.logical);
                    volume_delta.1 -= i128::from(old.allocated);
                    volume_delta.2 -= 1;
                }
                if had_baseline {
                    self.record_change(&mut history_paths, scan, root, cp.root_id, Some(&old), None, &old_dirs, &new_dirs)?;
                    extension_delta(&mut extensions, root, cp.root_id, Some(&old), &old_dirs, -1);
                    add_growth_for_file(&mut growth, root, cp.root_id, Some(&old), None,
                        &old_dirs, &new_dirs);
                }
            }
            self.db.execute("DELETE FROM nodes WHERE volume=? AND id NOT IN (SELECT id FROM staging WHERE payload IS NOT NULL)", [&cp.volume_id])?;
        }
        let mut rows = staged.query([])?;
        let mut index = 0;
        let old_query = format!("SELECT {} FROM nodes WHERE volume=? AND id=?", compact::ENTRY_COLUMNS);
        while let Some(row) = rows.next()? {
            if index % 512 == 0 { self.check_cancel()?; control()?; }
            index += 1;
            let id: i64 = row.get(0)?;
            let old = self.db.prepare_cached(&old_query)?
                .query_row(params![cp.volume_id, id], compact::read_entry).optional()?;
            let payload: Option<String> = row.get(1)?;
            let new: Option<Entry> = payload.as_ref().map(|s| serde_json::from_str(s)).transpose()?;
            if old == new && changed_dirs.is_empty() { continue; }
            if had_baseline {
                self.record_change(&mut history_paths, scan, root, cp.root_id, old.as_ref(), new.as_ref(), &old_dirs, &new_dirs)?;
                extension_change(&mut extensions, root, cp.root_id, old.as_ref(), new.as_ref(), &old_dirs, &new_dirs);
                add_growth_for_file(&mut growth, root, cp.root_id, old.as_ref(), new.as_ref(),
                    &old_dirs, &new_dirs);
            }
            let unchanged_file = matches!((&old, &new), (Some(a), Some(b))
                if !a.is_dir && !b.is_dir && a.parent == b.parent
                    && a.logical == b.logical && a.allocated == b.allocated);
            if !unchanged_file {
                if let Some(entry) = old.as_ref().filter(|entry| !entry.is_dir) {
                    add_folder_delta(&mut folder_deltas, entry, -1, cp.root_id, &old_dirs);
                    volume_delta.0 -= i128::from(entry.logical);
                    volume_delta.1 -= i128::from(entry.allocated);
                    volume_delta.2 -= 1;
                }
                if let Some(entry) = new.as_ref().filter(|entry| !entry.is_dir) {
                    add_folder_delta(&mut folder_deltas, entry, 1, cp.root_id, &new_dirs);
                    volume_delta.0 += i128::from(entry.logical);
                    volume_delta.1 += i128::from(entry.allocated);
                    volume_delta.2 += 1;
                }
            }
            if let Some(e) = new {
                self.db.prepare_cached("INSERT INTO nodes VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(volume,id) DO UPDATE SET parent=excluded.parent,name=excluded.name,is_dir=excluded.is_dir,logical=excluded.logical,allocated=excluded.allocated,payload=excluded.payload")?
                    .execute(params![cp.volume_id, id, e.parent as i64, e.name, e.is_dir, e.logical as i64, e.allocated as i64, compact::entry_payload(&e)?])?;
            } else { self.db.prepare_cached("DELETE FROM nodes WHERE volume=? AND id=?")?.execute(params![cp.volume_id, id])?; }
        }
        let logical = i64::try_from(i128::from(old_total.0) + volume_delta.0)?;
        let allocated = i64::try_from(i128::from(old_total.1) + volume_delta.1)?;
        let count = i64::try_from(i128::from(old_total.2) + volume_delta.2)?;
        ensure!(logical >= 0 && allocated >= 0 && count >= 0, "Negative volume total");
        self.db.execute("UPDATE scans SET logical=?,allocated=?,file_count=? WHERE id=?", params![logical as i64, allocated as i64, count, scan])?;
        self.db.execute("INSERT INTO volume_totals VALUES(?,?,?,?,?,?,?) ON CONFLICT(volume) DO UPDATE SET
            logical=excluded.logical,allocated=excluded.allocated,files=excluded.files,
            total=excluded.total,free=excluded.free,indexed_at=excluded.indexed_at",
            params![cp.volume_id, logical, allocated, count, outcome.total as i64,
                outcome.free as i64, outcome.sampled_at])?;
        self.db.execute("INSERT INTO volumes VALUES(?,?,?) ON CONFLICT(root) DO UPDATE SET checkpoint=excluded.checkpoint,pending=excluded.pending",
            params![root, serde_json::to_string(cp)?, serde_json::to_string(&outcome.pending)?])?;
        self.commit_timings.entries_ms = entries_started.elapsed().as_millis() as u64;
        let folders_started = Instant::now();
        if outcome.mode == ScanMode::Full {
            self.rebuild_folders(&cp.volume_id, cp.root_id, &new_dirs)?;
        } else if !changed_dirs.is_empty() {
            let mut affected: HashSet<u64> = folder_deltas.keys().copied().collect();
            for id in &changed_dirs {
                affected.extend(ancestors(*id, &old_dirs));
                affected.extend(ancestors(*id, &new_dirs));
            }
            self.refresh_folder_totals(&cp.volume_id, affected, &new_dirs, control)?;
        } else {
            self.apply_folder_deltas(&cp.volume_id, folder_deltas)?;
        }
        self.commit_timings.folders_ms = folders_started.elapsed().as_millis() as u64;
        let history_started = Instant::now();
        if had_baseline {
            let mut carry = self.db.prepare("SELECT path_id,allocated_delta,moved_delta,events FROM carry_folders WHERE volume=?")?;
            let mut path_reader = paths::Reader::new(&self.db);
            let mut rows = carry.query([&cp.volume_id])?;
            while let Some(row) = rows.next()? {
                growth_entry(&mut growth, path_reader.read(row, 0)?, i128::from(row.get::<_, i64>(1)?),
                    i128::from(row.get::<_, i64>(2)?), row.get::<_, i64>(3)? as u64);
            }
            drop(rows);
            drop(carry);
        }
        if had_baseline && outcome.mode == ScanMode::Incremental && !changed_dirs.is_empty() {
            self.unchanged_subtree_changes(&cp.volume_id, root, cp.root_id, &new_dirs, &old_dirs,
                1, &mut extensions, &mut growth, control)?;
        }
        history::write_growth(&self.db, &mut history_paths, scan, &growth)?;
        let mut insert = self.db.prepare("INSERT INTO extension_changes VALUES(?,?,?,?,?)")?;
        for ((path, extension), (delta, events)) in extensions {
            if delta != 0 {
                insert.execute(params![scan, history_paths.intern(&path)?, extension, i64::try_from(delta)?, events as i64])?;
            }
        }
        self.db.execute("INSERT INTO extension_changes SELECT ?,path_id,extension,delta,events FROM carry_extensions
            WHERE volume=? ON CONFLICT(scan,path_id,extension) DO UPDATE SET
            delta=delta+excluded.delta,events=events+excluded.events", params![scan, cp.volume_id])?;
        self.db.execute("UPDATE scans SET extensions_complete=COALESCE(
            (SELECT complete FROM carry_extension_quality WHERE volume=?),1) WHERE id=?", params![cp.volume_id, scan])?;
        self.db.execute("DELETE FROM carry_extensions WHERE volume=?", [&cp.volume_id])?;
        self.db.execute("DELETE FROM carry_extension_quality WHERE volume=?", [&cp.volume_id])?;
        self.db.execute("DELETE FROM carry_folders WHERE volume=?", [&cp.volume_id])?;
        self.commit_timings.history_ms = history_started.elapsed().as_millis() as u64;
        Ok(scan)
    }

    fn refresh_folder_totals(&self, volume: &str, affected: HashSet<u64>, dirs: &Directories,
        control: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        let mut affected: Vec<_> = affected.into_iter().map(|id| (ancestors(id, dirs).len(), id)).collect();
        affected.sort_unstable_by(|a, b| b.cmp(a));
        let mut read = self.db.prepare("SELECT
            COALESCE(SUM(CASE WHEN n.is_dir=0 THEN n.logical ELSE COALESCE(f.logical,0) END),0),
            COALESCE(SUM(CASE WHEN n.is_dir=0 THEN n.allocated ELSE COALESCE(f.allocated,0) END),0),
            COALESCE(SUM(CASE WHEN n.is_dir=0 THEN 1 ELSE COALESCE(f.files,0) END),0)
            FROM nodes n LEFT JOIN folders f ON n.volume=f.volume AND n.id=f.id
            WHERE n.volume=? AND n.parent=? AND n.id<>n.parent")?;
        let mut write = self.db.prepare("INSERT INTO folders VALUES(?,?,?,?,?) ON CONFLICT(volume,id)
            DO UPDATE SET logical=excluded.logical,allocated=excluded.allocated,files=excluded.files")?;
        for (index, (_, id)) in affected.into_iter().enumerate() {
            if index % 128 == 0 { self.check_cancel()?; control()?; }
            let (logical, allocated, files): (i64,i64,i64) = read.query_row(params![volume,id as i64],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if files == 0 {
                self.db.prepare_cached("DELETE FROM folders WHERE volume=? AND id=?")?.execute(params![volume,id as i64])?;
            } else { write.execute(params![volume,id as i64,logical,allocated,files])?; }
        }
        Ok(())
    }

    fn apply_folder_deltas(&self, volume: &str, deltas: FolderDeltas) -> Result<()> {
        let mut read = self.db.prepare("SELECT logical,allocated,files FROM folders WHERE volume=? AND id=?")?;
        let mut write = self.db.prepare("INSERT INTO folders VALUES(?,?,?,?,?) ON CONFLICT(volume,id) DO UPDATE SET logical=excluded.logical,allocated=excluded.allocated,files=excluded.files")?;
        let mut delete = self.db.prepare("DELETE FROM folders WHERE volume=? AND id=?")?;
        for (id, (logical, allocated, files)) in deltas {
            if logical == 0 && allocated == 0 && files == 0 { continue; }
            let current: Option<(i64, i64, i64)> = read.query_row(params![volume, id as i64], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
            let (old_logical, old_allocated, old_files) = current.unwrap_or_default();
            let next = (
                i64::try_from(i128::from(old_logical) + logical)?,
                i64::try_from(i128::from(old_allocated) + allocated)?,
                i64::try_from(i128::from(old_files) + files)?,
            );
            ensure!(next.0 >= 0 && next.1 >= 0 && next.2 >= 0, "Negative folder total for {id}");
            if next.2 == 0 {
                ensure!(next.0 == 0 && next.1 == 0, "Empty folder has nonzero size for {id}");
                delete.execute(params![volume, id as i64])?;
            } else {
                write.execute(params![volume, id as i64, next.0, next.1, next.2])?;
            }
        }
        Ok(())
    }

    fn record_change(&self, paths: &mut paths::Writer<'_>, scan: i64, root: &str, root_id: u64, old: Option<&Entry>, new: Option<&Entry>, old_dirs: &Directories, new_dirs: &Directories) -> Result<()> {
        let logical = new.map_or(0, |e| e.logical as i64) - old.map_or(0, |e| e.logical as i64);
        let allocated = new.map_or(0, |e| e.allocated as i64) - old.map_or(0, |e| e.allocated as i64);
        let kind = match (old, new) {
            (None, Some(_)) => "created",
            (Some(_), None) => "deleted",
            (Some(a), Some(b)) if a.parent != b.parent || a.name != b.name => "moved",
            (Some(_), Some(_)) if logical != 0 || allocated != 0 => "resized",
            _ => return Ok(()),
        };
        let old = paths.intern(&old.map(|e| entry_path(root, root_id, e, old_dirs)).unwrap_or_default())?;
        let new = paths.intern(&new.map(|e| entry_path(root, root_id, e, new_dirs)).unwrap_or_default())?;
        self.db.prepare_cached("INSERT INTO changes(scan,old_path_id,new_path_id,logical_delta,allocated_delta,kind)
            VALUES(?,?,?,?,?,?)")?.execute(params![scan, old, new, logical, allocated, kind])?;
        Ok(())
    }

    fn directories(&self, volume: &str) -> Result<Directories> {
        let mut stmt = self.db.prepare("SELECT id,parent,name FROM nodes WHERE volume=? AND is_dir=1")?;
        Ok(stmt.query_map([volume], |r| Ok((r.get::<_, i64>(0)? as u64, (r.get::<_, i64>(1)? as u64, r.get(2)?))))?.collect::<rusqlite::Result<_>>()?)
    }

    fn rebuild_folders(&self, volume: &str, root: u64, dirs: &Directories) -> Result<()> {
        let mut totals: HashMap<u64, (u64, u64, u64)> = HashMap::new();
        let mut stmt = self.db.prepare("SELECT parent,SUM(logical),SUM(allocated),COUNT(*) FROM nodes WHERE volume=? AND is_dir=0 GROUP BY parent")?;
        let mut rows = stmt.query([volume])?;
        while let Some(row) = rows.next()? {
            let mut id = row.get::<_, i64>(0)? as u64;
            let delta = (row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)? as u64, row.get::<_, i64>(3)? as u64);
            for _ in 0..1024 {
                let value = totals.entry(id).or_default();
                value.0 += delta.0; value.1 += delta.1; value.2 += delta.2;
                if id == root { break; }
                match dirs.get(&id) { Some((parent, _)) if *parent != id => id = *parent, _ => break }
            }
        }
        self.db.execute("DELETE FROM folders WHERE volume=?", [volume])?;
        let mut insert = self.db.prepare("INSERT INTO folders VALUES(?,?,?,?,?)")?;
        for (id, (logical, allocated, files)) in totals { insert.execute(params![volume, id as i64, logical as i64, allocated as i64, files as i64])?; }
        Ok(())
    }

    pub fn totals(&self, volume: &str) -> Result<(u64, u64)> {
        Ok(self.db.query_row("SELECT logical,allocated FROM volume_totals WHERE volume=?", [volume],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)))
            .optional()?.unwrap_or_default())
    }

    pub fn index_state(&self, volume: &str) -> Result<Option<(u64, u64, u64, String)>> {
        Ok(self.db.query_row("SELECT allocated,total,free,indexed_at FROM volume_totals WHERE volume=?",
            [volume], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)? as u64, r.get(3)?))).optional()?)
    }

    pub fn save_performance(&self, scan: i64, performance: &ScanPerformance) -> Result<()> {
        self.db.execute("UPDATE scans SET performance=? WHERE id=?",
            params![serde_json::to_string(performance)?, scan])?;
        Ok(())
    }

    pub fn reports(&self) -> Result<Vec<Report>> {
        let version: i64 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let performance = if version >= 6 { "performance" } else { "NULL" };
        let operation = if version >= 8 {"operation_pre,verification"} else {"NULL,'not_needed'"};
        let baseline=if version>=10 {"comparison_from"} else {"NULL"};
        let mut stmt = self.db.prepare(&format!("SELECT id,root,volume,finished,mode,total,free,logical,allocated,file_count,warnings,sampled,details,aggregate,{performance},{operation},{baseline} FROM scans ORDER BY id DESC"))?;
        let mut rows = stmt.query([])?;
        let mut reports = vec![];
        while let Some(r) = rows.next()? {
            reports.push(Report { id: r.get(0)?, root: r.get(1)?, volume: r.get(2)?, finished: r.get(3)?, mode: r.get(4)?,
                total: r.get::<_, i64>(5)? as u64, free: r.get::<_, i64>(6)? as u64, logical: r.get::<_, i64>(7)? as u64,
                allocated: r.get::<_, i64>(8)? as u64, file_count: r.get::<_, i64>(9)? as u64, warnings: serde_json::from_str(&r.get::<_, String>(10)?)?,
                sampled: r.get(11)?, details: r.get::<_, i64>(12)? != 0, aggregate: r.get::<_, i64>(13)? as u8,
                performance: r.get::<_, Option<String>>(14)?.map(|json| serde_json::from_str(&json)).transpose()?,
                operation_pre:r.get(15)?,verification:r.get(16)?,comparison_from:r.get(17)? });
        }
        Ok(reports)
    }

    pub fn scan_count(&self, root: &str) -> Result<u64> {
        Ok(self.db.query_row("SELECT COUNT(*) FROM scans WHERE root=?", [root], |row| row.get(0))?)
    }

    pub fn coverage(&self, volume: &str, from: i64, to: i64) -> Result<(u64, u64, u64, u64)> {
        Ok(self.db.query_row("SELECT COUNT(*),COALESCE(SUM(aggregate=2),0),
            COALESCE(SUM(aggregate>0),0),COALESCE(SUM(details),0)
            FROM scans WHERE volume=? AND id>? AND id<=?", params![volume, from, to],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?)
    }

    pub fn clear_details(&mut self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() { return Ok(0); }
        let tx = self.db.transaction()?;
        let mut cleared = 0;
        for id in ids {
            cleared += tx.execute("UPDATE scans SET details=0 WHERE id=? AND details<>0", [id])?;
            tx.execute("DELETE FROM changes WHERE scan=?", [id])?;
            tx.execute("DELETE FROM change_archives WHERE scan=?", [id])?;
        }
        paths::collect_unused(&tx)?;
        tx.commit()?;
        self.reclaim_free_pages()?;
        Ok(cleared)
    }

    pub fn delete_scans(&mut self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() { return Ok(0); }
        let tx = self.db.transaction()?;
        let mut deleted = 0;
        let mut ordered = ids.to_vec();
        ordered.sort_unstable();
        ordered.dedup();
        for id in ordered {
            let current: Option<(String, i64, i64)> = tx.query_row("SELECT volume,aggregate,extensions_complete FROM scans WHERE id=?", [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
            let Some((volume, quality, extension_quality)) = current else { continue; };
            let previous: Option<i64> = tx.query_row("SELECT MAX(id) FROM scans WHERE volume=? AND id<?", params![volume, id], |r| r.get(0))?;
            let next: Option<i64> = tx.query_row("SELECT MIN(id) FROM scans WHERE volume=? AND id>?", params![volume, id], |r| r.get(0))?;
            if previous.is_some() {
                if let Some(next) = next {
                    tx.execute("INSERT INTO extension_changes SELECT ?,path_id,extension,delta,events
                        FROM extension_changes WHERE scan=? ON CONFLICT(scan,path_id,extension) DO UPDATE SET
                        delta=delta+excluded.delta,events=events+excluded.events", params![next,id])?;
                    tx.execute("UPDATE scans SET extensions_complete=CASE WHEN extensions_complete=1 AND ?=1 THEN 1 ELSE 2 END WHERE id=?",
                        params![extension_quality,next])?;
                    tx.execute("INSERT INTO folder_changes SELECT ?,path_id,allocated_delta,moved_delta,events
                        FROM folder_changes WHERE scan=? ON CONFLICT(scan,path_id) DO UPDATE SET
                        allocated_delta=allocated_delta+excluded.allocated_delta,
                        moved_delta=moved_delta+excluded.moved_delta,events=events+excluded.events",
                        params![next, id])?;
                    tx.execute("UPDATE scans SET aggregate=MIN(aggregate,?) WHERE id=?", params![quality, next])?;
                } else {
                    tx.execute("INSERT INTO carry_extensions SELECT ?,path_id,extension,delta,events
                        FROM extension_changes WHERE scan=? ON CONFLICT(volume,path_id,extension) DO UPDATE SET
                        delta=delta+excluded.delta,events=events+excluded.events", params![volume,id])?;
                    tx.execute("INSERT INTO carry_extension_quality VALUES(?,?) ON CONFLICT(volume) DO UPDATE SET
                        complete=CASE WHEN complete=1 AND excluded.complete=1 THEN 1 ELSE 2 END",
                        params![volume,if extension_quality == 1 { 1 } else { 2 }])?;
                    tx.execute("INSERT INTO carry_folders SELECT ?,path_id,allocated_delta,moved_delta,events
                        FROM folder_changes WHERE scan=? ON CONFLICT(volume,path_id) DO UPDATE SET
                        allocated_delta=allocated_delta+excluded.allocated_delta,
                        moved_delta=moved_delta+excluded.moved_delta,events=events+excluded.events",
                        params![volume, id])?;
                }
            } else if next.is_none() {
                tx.execute("DELETE FROM carry_folders WHERE volume=?", [&volume])?;
                tx.execute("DELETE FROM carry_extensions WHERE volume=?", [&volume])?;
                tx.execute("DELETE FROM carry_extension_quality WHERE volume=?", [&volume])?;
            }
            deleted += tx.execute("DELETE FROM scans WHERE id=?", [id])?;
        }
        paths::collect_unused(&tx)?;
        tx.commit()?;
        self.reclaim_free_pages()?;
        Ok(deleted)
    }

    pub fn children(&self, volume: &str, parent: u64) -> Result<Vec<FolderItem>> {
        self.children_page(volume, parent, 5000, 0)
    }

    pub fn children_page(&self, volume: &str, parent: u64, limit: u64, offset: u64) -> Result<Vec<FolderItem>> {
        let mut stmt = self.db.prepare("SELECT n.id,n.name,n.is_dir,CASE WHEN n.is_dir THEN COALESCE(f.logical,0) ELSE n.logical END,CASE WHEN n.is_dir THEN COALESCE(f.allocated,0) ELSE n.allocated END,COALESCE(f.files,1) FROM nodes n LEFT JOIN folders f ON f.volume=n.volume AND f.id=n.id WHERE n.volume=? AND n.parent=? AND n.id<>n.parent ORDER BY 5 DESC,n.name LIMIT ? OFFSET ?")?;
        Ok(stmt.query_map(params![volume, parent as i64, limit.min(5000) as i64, offset as i64], |r| Ok(FolderItem { id: r.get::<_, i64>(0)? as u64, name: r.get(1)?, is_dir: r.get(2)?, logical: r.get::<_, i64>(3)? as u64, allocated: r.get::<_, i64>(4)? as u64, files: r.get::<_, i64>(5)? as u64 }))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn resolve_folder(&self, volume: &str, root_id: u64, path: &str) -> Result<u64> {
        self.find_folder(volume, root_id, path)?.with_context(|| format!("Folder is not in the current index: {path}"))
    }

    pub fn find_folder(&self, volume: &str, root_id: u64, path: &str) -> Result<Option<u64>> {
        let path = normalize_folder_path(path)?;
        let mut parent = root_id;
        for name in path[3..].split('\\').filter(|part| !part.is_empty()) {
            let id = self.db.query_row("SELECT id FROM nodes WHERE volume=? AND parent=? AND name=? COLLATE NOCASE AND is_dir=1",
                params![volume, parent as i64, name], |r| r.get::<_, i64>(0)).optional()?;
            let Some(id) = id else { return Ok(None); };
            parent = id as u64;
        }
        Ok(Some(parent))
    }

    pub fn folder_stats(&self, volume: &str, id: u64) -> Result<(u64, u64, u64)> {
        Ok(self.db.query_row("SELECT logical,allocated,files FROM folders WHERE volume=? AND id=?",
            params![volume, id as i64], |r| Ok((r.get::<_, i64>(0)? as u64,
                r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u64)))
            .optional()?.unwrap_or_default())
    }

    pub fn largest(&self, volume: &str, root: &str, root_id: u64, directories: bool,
        limit: u64, offset: u64) -> Result<Vec<RankedItem>> {
        let query = if directories {
            "SELECT n.id,f.allocated,f.logical,f.files,0 FROM folders f
             JOIN nodes n ON n.volume=f.volume AND n.id=f.id
             WHERE f.volume=? AND n.is_dir=1 AND n.id<>? ORDER BY f.allocated DESC,n.id LIMIT ? OFFSET ?"
        } else {
            "SELECT n.id,n.allocated,n.logical,1,json_extract(n.payload,'$.modified') FROM nodes n
             WHERE n.volume=? AND n.is_dir=0 AND n.id<>? ORDER BY n.allocated DESC,n.id LIMIT ? OFFSET ?"
        };
        let mut stmt = self.db.prepare(query)?;
        let ids = stmt.query_map(params![volume, root_id as i64, limit.min(5000) as i64, offset as i64],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)? as u64, r.get::<_, i64>(3)? as u64, r.get::<_, i64>(4)?)))?
            .collect::<rusqlite::Result<Vec<(u64,u64,u64,u64,i64)>>>()?;
        let mut result = Vec::with_capacity(ids.len());
        for (id, allocated, logical, files, modified) in ids {
            let path = self.path_for_id(volume, root, root_id, id)?;
            result.push(RankedItem { path, allocated, logical, files, modified });
        }
        Ok(result)
    }

    fn path_for_id(&self, volume: &str, root: &str, root_id: u64, mut id: u64) -> Result<String> {
        let mut parts = Vec::new();
        for _ in 0..1024 {
            if id == root_id {
                return Ok(format!("{}{}", root, parts.into_iter().rev().collect::<Vec<_>>().join("\\")));
            }
            let entry: Option<(i64, String)> = self.db.query_row(
                "SELECT parent,name FROM nodes WHERE volume=? AND id=?",
                params![volume, id as i64], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let Some((parent, name)) = entry else { return Ok(format!("[unresolved #{id}]")); };
            if parent as u64 == id { break; }
            parts.push(name);
            id = parent as u64;
        }
        Ok(format!("[unresolved #{id}]"))
    }

    fn reclaim_free_pages(&self) -> Result<()> {
        self.checkpoint_wal()?;
        let mut stmt = self.db.prepare("PRAGMA incremental_vacuum")?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
        drop(rows);
        drop(stmt);
        self.checkpoint_wal()
    }

    fn checkpoint_wal(&self) -> Result<()> {
        let busy: i64 = self.db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        ensure!(busy == 0, "Database reader prevented WAL checkpoint");
        Ok(())
    }

    pub fn used_bytes(&self) -> Result<u64> {
        let mut size = 0;
        for path in [self.path.clone(), self.path.with_extension("db-wal"), self.path.with_extension("db-shm")] {
            if let Ok(metadata) = fs::metadata(path) { size += metadata.len(); }
        }
        Ok(size)
    }

    pub fn history_bytes(&self) -> Result<u64> {
        Ok(self.db.query_row("SELECT COALESCE(SUM(pgsize),0) FROM dbstat WHERE aggregate=TRUE
            AND name IN ('changes','changes_scan','folder_changes','sqlite_autoindex_folder_changes_1',
                'carry_folders','sqlite_autoindex_carry_folders_1',
                'extension_changes','sqlite_autoindex_extension_changes_1',
                'carry_extensions','sqlite_autoindex_carry_extensions_1',
                  'carry_extension_quality','sqlite_autoindex_carry_extension_quality_1',
                  'change_archives','sqlite_autoindex_change_archives_1',
                  'history_paths','sqlite_autoindex_history_paths_1')", [], |row| row.get(0))?)
    }

    pub fn storage_usage(&self) -> Result<StorageUsage> {
        let mut usage = StorageUsage { history_bytes: self.history_bytes()?, database_bytes: self.used_bytes()?, ..Default::default() };
        usage.timeline_bytes = self.db.query_row("SELECT COALESCE(SUM(pgsize),0) FROM dbstat
            WHERE aggregate=TRUE AND name IN ('scans','scan_ids')", [], |row| row.get(0))?;
        let (allocated, unused): (u64, u64) = self.db.query_row(
            "SELECT (SELECT page_count FROM pragma_page_count)*(SELECT page_size FROM pragma_page_size),
                (SELECT freelist_count FROM pragma_freelist_count)*(SELECT page_size FROM pragma_page_size)",
            [], |row| Ok((row.get(0)?, row.get(1)?)))?;
        usage.unused_bytes = unused;
        usage.index_bytes = allocated.saturating_sub(unused + usage.history_bytes + usage.timeline_bytes);
        if let Some(parent) = self.path.parent() {
            for entry in fs::read_dir(parent)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("history-before-") && name.ends_with(".db") && entry.file_type()?.is_file() {
                    usage.backup_bytes += entry.metadata()?.len();
                }
            }
        }
        Ok(usage)
    }
}

fn normalize_folder_path(path: &str) -> Result<String> {
    let bytes = path.as_bytes();
    ensure!(bytes.len() >= 3 && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':' && bytes[2] == b'\\',
        "Expected an absolute drive path such as C:\\ or C:\\folder");
    ensure!(!path.contains('/') && !path.contains('\0'), "Invalid drive path");
    let parts: Vec<_> = path[3..].split('\\').filter(|part| !part.is_empty()).collect();
    Ok(format!("{}{}", &path[..3], parts.join("\\")))
}

fn folder_prefix(path: &str) -> String {
    if path.len() == 3 { path.to_owned() } else { format!("{path}\\") }
}

fn folder_parent(path: &str) -> Option<String> {
    let (parent, _) = path.rsplit_once('\\')?;
    if parent.len() == 2 && parent.as_bytes()[1] == b':' {
        Some(format!("{parent}\\"))
    } else { Some(parent.to_owned()) }
}

fn folder_bucket_dir(path: &str, depth: usize) -> Option<String> {
    if path.starts_with("[unresolved #") { return Some("[unresolved]".into()); }
    let bytes = path.as_bytes();
    if bytes.len() < 3 || bytes[1] != b':' || bytes[2] != b'\\' { return None; }
    let parts: Vec<_> = path[3..].split('\\').filter(|part| !part.is_empty()).collect();
    let count = parts.len().min(depth.max(1));
    if count == 0 { Some(path[..3].to_owned()) }
    else { Some(format!("{}{}", &path[..3], parts[..count].join("\\"))) }
}

fn growth_entry(growth: &mut GrowthDeltas, path: String, delta: i128, moved: i128, events: u64) {
    let item = growth.entry(path).or_default();
    item.0 += delta;
    item.1 += moved;
    item.2 += events;
}

fn write_legacy_growth(db: &Connection, scan: i64, growth: &GrowthDeltas) -> Result<()> {
    let mut stmt = db.prepare_cached("INSERT INTO folder_changes VALUES(?,?,?,?,?)
        ON CONFLICT(scan,path) DO UPDATE SET allocated_delta=allocated_delta+excluded.allocated_delta,
        moved_delta=moved_delta+excluded.moved_delta,events=events+excluded.events")?;
    for (path, (delta, moved, events)) in growth {
        if *delta != 0 || *moved != 0 {
            stmt.execute(params![scan, path, i64::try_from(*delta)?, i64::try_from(*moved)?, i64::try_from(*events)?])?;
        }
    }
    Ok(())
}

fn ancestors(mut id: u64, dirs: &Directories) -> Vec<u64> {
    let mut result = Vec::new();
    for _ in 0..1024 {
        if result.contains(&id) { break; }
        result.push(id);
        let Some((parent, _)) = dirs.get(&id) else { break; };
        id = *parent;
    }
    result
}

fn extension_delta(result: &mut ExtensionDeltas, root: &str, root_id: u64,
    entry: Option<&Entry>, dirs: &Directories, sign: i128) {
    let Some(entry) = entry.filter(|e| !e.is_dir && e.allocated != 0) else { return; };
    let path = entry_path(root, root_id, entry, dirs);
    let Some(parent) = folder_parent(&path) else { return; };
    let extension = entry.name.rsplit_once('.').map(|(_, ext)| format!(".{}", ext.to_ascii_lowercase()))
        .unwrap_or_else(|| "(无扩展名)".into());
    let value = result.entry((parent, extension)).or_default();
    value.0 += sign * i128::from(entry.allocated);
    value.1 += 1;
}

fn extension_change(result: &mut ExtensionDeltas, root: &str, root_id: u64,
    old: Option<&Entry>, new: Option<&Entry>, old_dirs: &Directories, new_dirs: &Directories) {
    let key = |entry: &Entry, dirs: &Directories| {
        let parent = folder_parent(&entry_path(root, root_id, entry, dirs));
        let extension = entry.name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase());
        (parent, extension)
    };
    if let (Some(old), Some(new)) = (old.filter(|e| !e.is_dir), new.filter(|e| !e.is_dir)) {
        if key(old, old_dirs) == key(new, new_dirs) {
            if old.allocated != new.allocated {
                let mut change = new.clone();
                change.allocated = old.allocated.abs_diff(new.allocated);
                extension_delta(result, root, root_id, Some(&change), new_dirs,
                    if new.allocated > old.allocated { 1 } else { -1 });
            }
            return;
        }
    }
    extension_delta(result, root, root_id, old, old_dirs, -1);
    extension_delta(result, root, root_id, new, new_dirs, 1);
}

fn add_growth_for_file(growth: &mut GrowthDeltas, root: &str, root_id: u64,
    old: Option<&Entry>, new: Option<&Entry>, old_dirs: &Directories,
    new_dirs: &Directories) {
    let old = old.filter(|e| !e.is_dir);
    let new = new.filter(|e| !e.is_dir);
    if old.is_none() && new.is_none() { return; }
    let old_path = old.map(|e| entry_path(root, root_id, e, old_dirs));
    let new_path = new.map(|e| entry_path(root, root_id, e, new_dirs));
    if old_path == new_path && old.map(|e| e.allocated) == new.map(|e| e.allocated) { return; }
    let old_parent = old_path.as_deref().and_then(folder_parent);
    let new_parent = new_path.as_deref().and_then(folder_parent);
    if old_parent == new_parent {
        if let Some(parent) = old_parent {
            let delta = i128::from(new.map_or(0, |e| e.allocated)) - i128::from(old.map_or(0, |e| e.allocated));
            growth_entry(growth, parent, delta, 0, 1);
        }
        return;
    }
    let transfer = if old_path != new_path {
        old.zip(new).map_or(0, |(a, b)| a.allocated.min(b.allocated))
    } else { 0 };
    if let (Some(entry), Some(path)) = (old, old_path.as_deref()) {
            if let Some(parent) = folder_parent(path) {
                growth_entry(growth, parent, -i128::from(entry.allocated), -i128::from(transfer), 1);
            }
    }
    if let (Some(entry), Some(path)) = (new, new_path.as_deref()) {
            if let Some(parent) = folder_parent(path) {
                growth_entry(growth, parent, i128::from(entry.allocated), i128::from(transfer), 1);
            }
    }
}

fn add_folder_delta(deltas: &mut FolderDeltas, entry: &Entry, sign: i128, root: u64, dirs: &Directories) {
    let mut id = entry.parent;
    for _ in 0..1024 {
        let total = deltas.entry(id).or_default();
        total.0 += sign * i128::from(entry.logical);
        total.1 += sign * i128::from(entry.allocated);
        total.2 += sign;
        if id == root { break; }
        match dirs.get(&id) {
            Some((parent, _)) if *parent != id => id = *parent,
            _ => break,
        }
    }
}

fn entry_path(root: &str, root_id: u64, entry: &Entry, dirs: &Directories) -> String {
    if entry.id == root_id { return root.into(); }
    let mut parts = vec![entry.name.as_str()];
    let mut parent = entry.parent;
    for _ in 0..1024 {
        if parent == root_id { break; }
        match dirs.get(&parent) {
            Some((next, name)) if *next != parent => { parts.push(name); parent = *next; }
            _ => return format!("[unresolved #{parent}]\\{}", parts.into_iter().rev().collect::<Vec<_>>().join("\\")),
        }
    }
    format!("{}{}", root, parts.into_iter().rev().collect::<Vec<_>>().join("\\"))
}
