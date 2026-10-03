use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;
use std::{collections::HashMap, fs, path::{Path, PathBuf}, time::Duration};

pub struct Store {
    db: Connection,
    path: PathBuf,
    staging: bool,
    cancel_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Change {
    pub old_path: String,
    pub new_path: String,
    pub logical_delta: i64,
    pub allocated_delta: i64,
    pub kind: String,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
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

impl Store {
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_secs(3))?;
        ensure!(db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? >= 2,
            "History database needs a one-time upgrade; open VolumeTrail first");
        Ok(Self { db, path: path.to_path_buf(), staging: false, cancel_path: None })
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
                write_growth(&db, id, &growth)?;
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
        Ok(Self { db, path: path.to_path_buf(), staging: false, cancel_path: None })
    }

    pub fn set_cancel_path(&mut self, path: PathBuf) { self.cancel_path = Some(path); }

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
        let payload: Option<String> = self.db.query_row(
            "SELECT payload FROM nodes WHERE volume=? AND id=?", params![volume, id as i64],
            |r| r.get(0)).optional()?;
        payload.map(|json| serde_json::from_str(&json).map_err(Into::into)).transpose()
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
        if let Err(error) = self.db.execute_batch("BEGIN IMMEDIATE; DELETE FROM staging;") {
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

    fn apply_stage(&self, root: &str, outcome: &ScanOutcome,
        control: &mut dyn FnMut() -> Result<()>) -> Result<i64> {
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
        let mut rebuild_folders = outcome.mode == ScanMode::Full;
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
        let moved_dirs: Vec<u64> = old_dirs.iter().filter_map(|(id, old)| {
            new_dirs.get(id).filter(|new| *new != old).map(|_| *id)
        }).filter(|id| !moved_ancestor(*id, &old_dirs, &new_dirs)).collect();
        let old_moved: Vec<_> = moved_dirs.iter().map(|id| {
            let old = self.db.query_row("SELECT allocated FROM folders WHERE volume=? AND id=?",
                params![cp.volume_id, *id as i64], |r| r.get::<_, i64>(0)).optional()?.unwrap_or(0);
            Ok((*id, old))
        }).collect::<Result<Vec<_>>>()?;
        let scan: i64 = self.db.query_row("SELECT next FROM scan_ids", [], |r| r.get(0))?;
        self.db.execute("UPDATE scan_ids SET next=next+1", [])?;
        self.db.execute("INSERT INTO scans(id,root,volume,finished,mode,total,free,logical,allocated,file_count,warnings,sampled,details,aggregate) VALUES(?,?,?,?,?,?,?,0,0,0,?,?,1,2)",
            params![scan, root, cp.volume_id, outcome.sampled_at, if outcome.mode == ScanMode::Full { "full" } else { "incremental" },
                outcome.total as i64, outcome.free as i64, serde_json::to_string(&outcome.warnings)?, outcome.sampled_at])?;
        let old_total: (i64, i64, i64) = self.db.query_row(
            "SELECT logical,allocated,files FROM volume_totals WHERE volume=?", [&cp.volume_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?.unwrap_or_default();
        let mut volume_delta = (0i128, 0i128, 0i128);
        if outcome.mode == ScanMode::Full {
            let mut absent = self.db.prepare("SELECT n.payload FROM nodes n LEFT JOIN staging s ON s.id=n.id WHERE n.volume=? AND s.payload IS NULL")?;
            let mut rows = absent.query([&cp.volume_id])?;
            let mut index = 0;
            while let Some(row) = rows.next()? {
                if index % 512 == 0 { self.check_cancel()?; control()?; }
                index += 1;
                let old: Entry = serde_json::from_str(&row.get::<_, String>(0)?)?;
                if !old.is_dir {
                    volume_delta.0 -= i128::from(old.logical);
                    volume_delta.1 -= i128::from(old.allocated);
                    volume_delta.2 -= 1;
                }
                if had_baseline {
                    self.record_change(scan, root, cp.root_id, Some(&old), None, &old_dirs, &new_dirs)?;
                    add_growth_for_file(&mut growth, root, cp.root_id, Some(&old), None,
                        &old_dirs, &new_dirs, &moved_dirs);
                }
            }
            self.db.execute("DELETE FROM nodes WHERE volume=? AND id NOT IN (SELECT id FROM staging WHERE payload IS NOT NULL)", [&cp.volume_id])?;
        }
        let mut rows = staged.query([])?;
        let mut index = 0;
        while let Some(row) = rows.next()? {
            if index % 512 == 0 { self.check_cancel()?; control()?; }
            index += 1;
            let id: i64 = row.get(0)?;
            let old: Option<String> = self.db.query_row("SELECT payload FROM nodes WHERE volume=? AND id=?", params![cp.volume_id, id], |r| r.get(0)).optional()?;
            let payload: Option<String> = row.get(1)?;
            if old == payload { continue; }
            let old: Option<Entry> = old.map(|s| serde_json::from_str(&s)).transpose()?;
            let new: Option<Entry> = payload.as_ref().map(|s| serde_json::from_str(s)).transpose()?;
            if had_baseline {
                self.record_change(scan, root, cp.root_id, old.as_ref(), new.as_ref(), &old_dirs, &new_dirs)?;
                add_growth_for_file(&mut growth, root, cp.root_id, old.as_ref(), new.as_ref(),
                    &old_dirs, &new_dirs, &moved_dirs);
            }
            let old_dir_parent = old.as_ref().filter(|entry| entry.is_dir).map(|entry| entry.parent);
            let new_dir_parent = new.as_ref().filter(|entry| entry.is_dir).map(|entry| entry.parent);
            if old_dir_parent != new_dir_parent && old.as_ref().is_some_and(|entry| entry.is_dir) {
                let files: Option<i64> = self.db.query_row(
                    "SELECT files FROM folders WHERE volume=? AND id=?",
                    params![cp.volume_id, id], |r| r.get(0)).optional()?;
                if files.unwrap_or(0) > 0 { rebuild_folders = true; }
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
                    .execute(params![cp.volume_id, id, e.parent as i64, e.name, e.is_dir, e.logical as i64, e.allocated as i64, payload])?;
            } else { self.db.execute("DELETE FROM nodes WHERE volume=? AND id=?", params![cp.volume_id, id])?; }
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
        if rebuild_folders {
            self.rebuild_folders(&cp.volume_id, cp.root_id, &new_dirs)?;
        } else {
            self.apply_folder_deltas(&cp.volume_id, folder_deltas)?;
        }
        if had_baseline {
            for (id, old_size) in old_moved {
                let new_size: i64 = self.db.query_row("SELECT allocated FROM folders WHERE volume=? AND id=?",
                    params![cp.volume_id, id as i64], |r| r.get(0)).optional()?.unwrap_or(0);
                let old_path = directory_path(root, cp.root_id, id, &old_dirs);
                let new_path = directory_path(root, cp.root_id, id, &new_dirs);
                let transferred = old_size.min(new_size);
                growth_entry(&mut growth, old_path, -i128::from(old_size), -i128::from(transferred), 1);
                growth_entry(&mut growth, new_path, i128::from(new_size), i128::from(transferred), 1);
            }
            let mut carry = self.db.prepare("SELECT path,allocated_delta,moved_delta,events FROM carry_folders WHERE volume=?")?;
            let mut rows = carry.query([&cp.volume_id])?;
            while let Some(row) = rows.next()? {
                growth_entry(&mut growth, row.get(0)?, i128::from(row.get::<_, i64>(1)?),
                    i128::from(row.get::<_, i64>(2)?), row.get::<_, i64>(3)? as u64);
            }
            drop(rows);
            drop(carry);
        }
        write_growth(&self.db, scan, &growth)?;
        self.db.execute("DELETE FROM carry_folders WHERE volume=?", [&cp.volume_id])?;
        Ok(scan)
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

    fn record_change(&self, scan: i64, root: &str, root_id: u64, old: Option<&Entry>, new: Option<&Entry>, old_dirs: &Directories, new_dirs: &Directories) -> Result<()> {
        let logical = new.map_or(0, |e| e.logical as i64) - old.map_or(0, |e| e.logical as i64);
        let allocated = new.map_or(0, |e| e.allocated as i64) - old.map_or(0, |e| e.allocated as i64);
        let kind = match (old, new) {
            (None, Some(_)) => "created",
            (Some(_), None) => "deleted",
            (Some(a), Some(b)) if a.parent != b.parent || a.name != b.name => "moved",
            (Some(_), Some(_)) if logical != 0 || allocated != 0 => "resized",
            _ => return Ok(()),
        };
        self.db.prepare_cached("INSERT INTO changes VALUES(?,?,?,?,?,?)")?.execute(params![scan,
            old.map(|e| entry_path(root, root_id, e, old_dirs)).unwrap_or_default(),
            new.map(|e| entry_path(root, root_id, e, new_dirs)).unwrap_or_default(), logical, allocated, kind])?;
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

    pub fn changes(&self, scan: i64) -> Result<Vec<Change>> {
        let mut stmt = self.db.prepare("SELECT old_path,new_path,logical_delta,allocated_delta,kind FROM changes WHERE scan=? ORDER BY ABS(allocated_delta) DESC,ABS(logical_delta) DESC LIMIT 2000")?;
        Ok(stmt.query_map([scan], |r| Ok(Change { old_path: r.get(0)?, new_path: r.get(1)?, logical_delta: r.get(2)?, allocated_delta: r.get(3)?, kind: r.get(4)? }))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn changes_between(&self, volume: &str, from: i64, to: i64) -> Result<Vec<Change>> {
        self.changes_between_page(volume, from, to, 2000, 0)
    }

    pub fn changes_between_page(&self, volume: &str, from: i64, to: i64,
        limit: u64, offset: u64) -> Result<Vec<Change>> {
        let mut stmt = self.db.prepare(
            "SELECT c.old_path,c.new_path,c.logical_delta,c.allocated_delta,c.kind
             FROM changes c JOIN scans s ON s.id=c.scan
             WHERE s.volume=? AND c.scan>? AND c.scan<=?
             ORDER BY ABS(c.allocated_delta) DESC,ABS(c.logical_delta) DESC LIMIT ? OFFSET ?")?;
        Ok(stmt.query_map(params![volume, from, to, limit.min(5000) as i64, offset as i64], |r| Ok(Change {
            old_path: r.get(0)?, new_path: r.get(1)?, logical_delta: r.get(2)?,
            allocated_delta: r.get(3)?, kind: r.get(4)?,
        }))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn changes_count(&self, volume: &str, from: i64, to: i64, path: Option<&str>) -> Result<u64> {
        let count: i64 = if let Some(path) = path {
            let prefix = format!("{}\\", path.trim_end_matches('\\'));
            self.db.query_row("SELECT COUNT(*) FROM changes c JOIN scans s ON s.id=c.scan
                WHERE s.volume=? AND c.scan>? AND c.scan<=? AND
                (substr(c.old_path,1,length(?))=? OR substr(c.new_path,1,length(?))=?)",
                params![volume, from, to, prefix, prefix, prefix, prefix], |r| r.get(0))?
        } else {
            self.db.query_row("SELECT COUNT(*) FROM changes c JOIN scans s ON s.id=c.scan
                WHERE s.volume=? AND c.scan>? AND c.scan<=?",
                params![volume, from, to], |r| r.get(0))?
        };
        Ok(count as u64)
    }

    pub fn changes_between_path(&self, volume: &str, from: i64, to: i64,
        path: &str, limit: u64, offset: u64) -> Result<Vec<Change>> {
        let prefix = format!("{}\\", path.trim_end_matches('\\'));
        let mut stmt = self.db.prepare(
            "SELECT c.old_path,c.new_path,c.logical_delta,c.allocated_delta,c.kind
             FROM changes c JOIN scans s ON s.id=c.scan
             WHERE s.volume=? AND c.scan>? AND c.scan<=? AND
             (substr(c.old_path,1,length(?))=? OR substr(c.new_path,1,length(?))=?)
             ORDER BY ABS(c.allocated_delta) DESC,ABS(c.logical_delta) DESC LIMIT ? OFFSET ?")?;
        Ok(stmt.query_map(params![volume, from, to, prefix, prefix, prefix, prefix,
            limit.min(5000) as i64, offset as i64], |r| Ok(Change {
                old_path: r.get(0)?, new_path: r.get(1)?, logical_delta: r.get(2)?,
                allocated_delta: r.get(3)?, kind: r.get(4)?,
            }))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn extensions_between_path(&self, volume: &str, from: i64, to: i64,
        path: &str) -> Result<Vec<(String, i128, u64)>> {
        let prefix = format!("{}\\", path.trim_end_matches('\\'));
        let mut stmt = self.db.prepare(
            "SELECT c.old_path,c.new_path,c.allocated_delta FROM changes c JOIN scans s ON s.id=c.scan
             WHERE s.volume=? AND c.scan>? AND c.scan<=? AND c.allocated_delta<>0 AND
             (substr(c.old_path,1,length(?))=? OR substr(c.new_path,1,length(?))=?)")?;
        let mut rows = stmt.query(params![volume, from, to, prefix, prefix, prefix, prefix])?;
        let mut grouped: HashMap<String, (i128, u64)> = HashMap::new();
        while let Some(row) = rows.next()? {
            let old: String = row.get(0)?;
            let new: String = row.get(1)?;
            let delta: i64 = row.get(2)?;
            let path = if delta > 0 { &new } else { &old };
            let name = path.rsplit('\\').next().unwrap_or("");
            let extension = name.rsplit_once('.').map(|(_, ext)| format!(".{}", ext.to_ascii_lowercase()))
                .unwrap_or_else(|| "(无扩展名)".into());
            let entry = grouped.entry(extension).or_default();
            entry.0 += i128::from(delta);
            entry.1 += 1;
        }
        let mut items: Vec<_> = grouped.into_iter().map(|(extension, (delta, count))| (extension, delta, count)).collect();
        items.sort_by(|a, b| b.1.abs().cmp(&a.1.abs()));
        Ok(items)
    }

    pub fn folder_growth(&self, volume: &str, from: i64, to: i64, depth: usize) -> Result<Vec<FolderGrowth>> {
        let mut stmt = self.db.prepare(
            "SELECT f.path,f.allocated_delta,f.moved_delta,f.events
             FROM folder_changes f JOIN scans s ON s.id=f.scan
             WHERE s.volume=? AND f.scan>? AND f.scan<=?")?;
        let mut rows = stmt.query(params![volume, from, to])?;
        let mut grouped = GrowthDeltas::new();
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            if let Some(folder) = folder_bucket_dir(&path, depth) {
                growth_entry(&mut grouped, folder, i128::from(row.get::<_, i64>(1)?),
                    i128::from(row.get::<_, i64>(2)?), row.get::<_, i64>(3)? as u64);
            }
        }
        let mut result: Vec<_> = grouped.into_iter().filter(|(_, (delta, _, _))| *delta != 0)
            .map(|(path, (allocated_delta, moved_delta, changes))| FolderGrowth { path, allocated_delta, moved_delta, changes })
            .collect();
        result.sort_by(|a, b| b.allocated_delta.cmp(&a.allocated_delta).then_with(|| a.path.cmp(&b.path)));
        Ok(result)
    }

    pub fn folder_breakdown(&self, volume: &str, from: i64, to: i64, path: &str) -> Result<FolderBreakdown> {
        let path = if path.len() == 3 { path } else { path.trim_end_matches('\\') };
        let prefix = format!("{}\\", path.trim_end_matches('\\'));
        let depth = path[3..].split('\\').filter(|part| !part.is_empty()).count() + 1;
        let mut stmt = self.db.prepare("SELECT f.path,f.allocated_delta,f.moved_delta,f.events
            FROM folder_changes f JOIN scans s ON s.id=f.scan
            WHERE s.volume=? AND f.scan>? AND f.scan<=? AND
            (f.path=? COLLATE NOCASE OR substr(f.path,1,length(?))=? COLLATE NOCASE)")?;
        let mut rows = stmt.query(params![volume, from, to, path, prefix, prefix])?;
        let mut result = FolderBreakdown::default();
        let mut children = GrowthDeltas::new();
        while let Some(row) = rows.next()? {
            let folder: String = row.get(0)?;
            let delta = i128::from(row.get::<_, i64>(1)?);
            let moved = i128::from(row.get::<_, i64>(2)?);
            result.allocated_delta += delta;
            result.moved_delta += moved;
            if folder.eq_ignore_ascii_case(path) {
                result.direct_delta += delta;
            } else if let Some(child) = folder_bucket_dir(&folder, depth) {
                growth_entry(&mut children, child, delta, moved, row.get::<_, i64>(3)? as u64);
            }
        }
        result.children = children.into_iter().filter(|(_, (delta, _, _))| *delta != 0)
            .map(|(path, (allocated_delta, moved_delta, changes))| FolderGrowth { path, allocated_delta, moved_delta, changes })
            .collect();
        result.children.sort_by(|a, b| b.allocated_delta.cmp(&a.allocated_delta).then_with(|| a.path.cmp(&b.path)));
        Ok(result)
    }

    pub fn save_performance(&self, scan: i64, performance: &ScanPerformance) -> Result<()> {
        self.db.execute("UPDATE scans SET performance=? WHERE id=?",
            params![serde_json::to_string(performance)?, scan])?;
        Ok(())
    }

    pub fn reports(&self) -> Result<Vec<Report>> {
        let version: i64 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let performance = if version >= 6 { "performance" } else { "NULL" };
        let mut stmt = self.db.prepare(&format!("SELECT id,root,volume,finished,mode,total,free,logical,allocated,file_count,warnings,sampled,details,aggregate,{performance} FROM scans ORDER BY id DESC"))?;
        let mut rows = stmt.query([])?;
        let mut reports = vec![];
        while let Some(r) = rows.next()? {
            reports.push(Report { id: r.get(0)?, root: r.get(1)?, volume: r.get(2)?, finished: r.get(3)?, mode: r.get(4)?,
                total: r.get::<_, i64>(5)? as u64, free: r.get::<_, i64>(6)? as u64, logical: r.get::<_, i64>(7)? as u64,
                allocated: r.get::<_, i64>(8)? as u64, file_count: r.get::<_, i64>(9)? as u64, warnings: serde_json::from_str(&r.get::<_, String>(10)?)?,
                sampled: r.get(11)?, details: r.get::<_, i64>(12)? != 0, aggregate: r.get::<_, i64>(13)? as u8,
                performance: r.get::<_, Option<String>>(14)?.map(|json| serde_json::from_str(&json)).transpose()? });
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
        let tx = self.db.transaction()?;
        let mut cleared = 0;
        for id in ids {
            cleared += tx.execute("UPDATE scans SET details=0 WHERE id=? AND details<>0", [id])?;
            tx.execute("DELETE FROM changes WHERE scan=?", [id])?;
        }
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
            let current: Option<(String, i64)> = tx.query_row("SELECT volume,aggregate FROM scans WHERE id=?", [id],
                |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let Some((volume, quality)) = current else { continue; };
            let previous: Option<i64> = tx.query_row("SELECT MAX(id) FROM scans WHERE volume=? AND id<?", params![volume, id], |r| r.get(0))?;
            let next: Option<i64> = tx.query_row("SELECT MIN(id) FROM scans WHERE volume=? AND id>?", params![volume, id], |r| r.get(0))?;
            if previous.is_some() {
                if let Some(next) = next {
                    tx.execute("INSERT INTO folder_changes SELECT ?,path,allocated_delta,moved_delta,events
                        FROM folder_changes WHERE scan=? ON CONFLICT(scan,path) DO UPDATE SET
                        allocated_delta=allocated_delta+excluded.allocated_delta,
                        moved_delta=moved_delta+excluded.moved_delta,events=events+excluded.events",
                        params![next, id])?;
                    tx.execute("UPDATE scans SET aggregate=MIN(aggregate,?) WHERE id=?", params![quality, next])?;
                } else {
                    tx.execute("INSERT INTO carry_folders SELECT ?,path,allocated_delta,moved_delta,events
                        FROM folder_changes WHERE scan=? ON CONFLICT(volume,path) DO UPDATE SET
                        allocated_delta=allocated_delta+excluded.allocated_delta,
                        moved_delta=moved_delta+excluded.moved_delta,events=events+excluded.events",
                        params![volume, id])?;
                }
            } else if next.is_none() {
                tx.execute("DELETE FROM carry_folders WHERE volume=?", [&volume])?;
            }
            deleted += tx.execute("DELETE FROM scans WHERE id=?", [id])?;
        }
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
        let mut parent = root_id;
        for name in path.trim_end_matches('\\').split('\\').skip(1).filter(|part| !part.is_empty()) {
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

    pub fn prune(&mut self, budget: u64) -> Result<u64> {
        ensure!(!self.staging, "Cannot prune during a scan");
        loop {
            self.check_cancel()?;
            self.reclaim_free_pages()?;
            if self.history_bytes()? <= budget { break; }
            let oldest: Option<i64> = self.db.query_row("SELECT MIN(id) FROM scans WHERE details<>0", [], |r| r.get(0))?;
            if let Some(id) = oldest {
                self.db.execute("DELETE FROM changes WHERE scan=?", [id])?;
                self.db.execute("UPDATE scans SET details=0 WHERE id=?", [id])?;
                continue;
            }
            let oldest: Option<i64> = self.db.query_row("SELECT MIN(id) FROM scans WHERE aggregate<>0", [], |r| r.get(0))?;
            if let Some(id) = oldest {
                self.db.execute("DELETE FROM folder_changes WHERE scan=?", [id])?;
                self.db.execute("UPDATE scans SET aggregate=0 WHERE id=?", [id])?;
                continue;
            }
            break;
        }
        self.used_bytes()
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
                'carry_folders','sqlite_autoindex_carry_folders_1')", [], |row| row.get(0))?)
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

fn folder_parent(path: &str) -> Option<String> {
    let (parent, _) = path.rsplit_once('\\')?;
    if parent.len() == 2 && parent.as_bytes()[1] == b':' {
        Some(format!("{parent}\\"))
    } else { Some(parent.to_owned()) }
}

fn folder_bucket_dir(path: &str, depth: usize) -> Option<String> {
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

fn write_growth(db: &Connection, scan: i64, growth: &GrowthDeltas) -> Result<()> {
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

fn directory_path(root: &str, root_id: u64, id: u64, dirs: &Directories) -> String {
    if id == root_id { return root.to_owned(); }
    let Some((parent, name)) = dirs.get(&id) else { return format!("[unresolved #{id}]"); };
    entry_path(root, root_id, &Entry { id, parent: *parent, name: name.clone(), is_dir: true,
        logical: 0, allocated: 0, modified: 0, attributes: 0, links: vec![] }, dirs)
}

fn is_descendant(mut parent: u64, ancestor: u64, dirs: &Directories) -> bool {
    for _ in 0..1024 {
        if parent == ancestor { return true; }
        let Some((next, _)) = dirs.get(&parent) else { return false; };
        if *next == parent { return false; }
        parent = *next;
    }
    false
}

fn moved_ancestor(id: u64, old: &Directories, new: &Directories) -> bool {
    old.iter().any(|(candidate, before)| {
        *candidate != id && new.get(candidate).is_some_and(|after| after != before)
            && (is_descendant(id, *candidate, old) || is_descendant(id, *candidate, new))
    })
}

fn add_growth_for_file(growth: &mut GrowthDeltas, root: &str, root_id: u64,
    old: Option<&Entry>, new: Option<&Entry>, old_dirs: &Directories,
    new_dirs: &Directories, moved_dirs: &[u64]) {
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
        if !moved_dirs.iter().any(|id| is_descendant(entry.parent, *id, old_dirs)) {
            if let Some(parent) = folder_parent(path) {
                growth_entry(growth, parent, -i128::from(entry.allocated), -i128::from(transfer), 1);
            }
        }
    }
    if let (Some(entry), Some(path)) = (new, new_path.as_deref()) {
        if !moved_dirs.iter().any(|id| is_descendant(entry.parent, *id, new_dirs)) {
            if let Some(parent) = folder_parent(path) {
                growth_entry(growth, parent, i128::from(entry.allocated), i128::from(transfer), 1);
            }
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
