//! Schema 14: migrate all history references together, after a consistent backup.
use super::*;

const SUMMARIES: &[&str] = &["folder_changes", "extension_changes", "carry_folders", "carry_extensions"];

pub(super) fn migrate(db: &Connection, path: &Path) -> Result<()> {
    if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? >= 14 { return Ok(()); }
    db.execute_batch("PRAGMA synchronous=FULL;")?;
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    // Another process may have completed the migration while we waited.
    if tx.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? >= 14 {
        tx.commit()?;
        db.execute_batch("PRAGMA synchronous=NORMAL;")?;
        return Ok(());
    }
    if tx.query_row("SELECT EXISTS(SELECT 1 FROM scans)", [], |r| r.get::<_, bool>(0))? {
        let backup = path.with_file_name(format!("history-before-v14-{}-{}.db",
            chrono::Local::now().format("%Y%m%d-%H%M%S-%f"), std::process::id()));
        ensure!(!backup.exists(), "Migration backup already exists");
        // The write reservation prevents changes while this second connection
        // backs up the committed state, before the first schema/data mutation.
        let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source.backup(rusqlite::MAIN_DB, &backup, None)
            .with_context(|| format!("Back up history to {}", backup.display()))?;
    }
    convert(&tx)?;
    tx.commit()?;
    db.execute_batch("PRAGMA synchronous=NORMAL;")?;
    Ok(())
}

fn convert(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE history_paths(
        id INTEGER PRIMARY KEY, parent INTEGER NOT NULL REFERENCES history_paths(id),
        fragment TEXT NOT NULL, UNIQUE(parent,fragment),
        CHECK((id=0 AND parent=0 AND fragment='') OR (id>0 AND parent>=0 AND parent<id)));
        INSERT INTO history_paths VALUES(0,0,'');
        CREATE TABLE changes_v14(
            seq INTEGER PRIMARY KEY,scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
            old_path_id INTEGER NOT NULL REFERENCES history_paths(id),
            new_path_id INTEGER NOT NULL REFERENCES history_paths(id),
            logical_delta INTEGER NOT NULL,allocated_delta INTEGER NOT NULL,kind TEXT NOT NULL);
        CREATE TABLE change_archives_v14(
            scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
            part INTEGER NOT NULL,entries INTEGER NOT NULL,data BLOB NOT NULL,PRIMARY KEY(scan,part));
        CREATE TABLE folder_changes_v14(
            scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
            path_id INTEGER NOT NULL REFERENCES history_paths(id),
            allocated_delta INTEGER NOT NULL,moved_delta INTEGER NOT NULL,events INTEGER NOT NULL,
            PRIMARY KEY(scan,path_id));
        CREATE TABLE extension_changes_v14(
            scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
            path_id INTEGER NOT NULL REFERENCES history_paths(id),extension TEXT NOT NULL,
            delta INTEGER NOT NULL,events INTEGER NOT NULL,PRIMARY KEY(scan,path_id,extension));
        CREATE TABLE carry_folders_v14(
            volume TEXT NOT NULL,path_id INTEGER NOT NULL REFERENCES history_paths(id),
            allocated_delta INTEGER NOT NULL,moved_delta INTEGER NOT NULL,events INTEGER NOT NULL,
            PRIMARY KEY(volume,path_id));
        CREATE TABLE carry_extensions_v14(
            volume TEXT NOT NULL,path_id INTEGER NOT NULL REFERENCES history_paths(id),
            extension TEXT NOT NULL,delta INTEGER NOT NULL,events INTEGER NOT NULL,
            PRIMARY KEY(volume,path_id,extension));")?;
    let mut paths = paths::Writer::new(db);
    let mut archive = db.prepare("SELECT scan,part,entries,data FROM change_archives ORDER BY scan,part")?;
    let mut rows = archive.query([])?;
    while let Some(row) = rows.next()? {
        let original = compact::decode_legacy(&row.get::<_, Vec<u8>>(3)?)?;
        let entries: i64 = row.get(2)?;
        ensure!(entries == original.len() as i64, "History archive entry count mismatch");
        let converted = original.into_iter().map(|(seq, old, new, logical, allocated, kind)|
            Ok((seq, paths.intern(&old)?, paths.intern(&new)?, logical, allocated, kind)))
            .collect::<Result<Vec<_>>>()?;
        let data = compact::encode_ids(&converted)?;
        db.prepare_cached("INSERT INTO change_archives_v14 VALUES(?,?,?,?)")?.execute(
            params![row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, entries, data])?;
    }
    drop(rows); drop(archive);
    let mut live = db.prepare("SELECT rowid,scan,old_path,new_path,logical_delta,allocated_delta,kind
        FROM changes ORDER BY rowid")?;
    let mut rows = live.query([])?;
    while let Some(row) = rows.next()? {
        let old = paths.intern(&row.get::<_, String>(2)?)?;
        let new = paths.intern(&row.get::<_, String>(3)?)?;
        db.prepare_cached("INSERT INTO changes_v14 VALUES(?,?,?,?,?,?,?)")?.execute(params![
            row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, old, new, row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?, row.get::<_, String>(6)?])?;
    }
    drop(rows); drop(live);
    for table in SUMMARIES {
        let mut stmt = db.prepare(&format!("SELECT * FROM {table}"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id = paths.intern(&row.get::<_, String>(1)?)?;
            db.prepare_cached(&format!("INSERT INTO {table}_v14 VALUES(?,?,?,?,?)"))?.execute(params![
                row.get::<_, rusqlite::types::Value>(0)?, id,
                row.get::<_, rusqlite::types::Value>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(4)?])?;
        }
    }
    drop(paths);
    for table in ["changes", "change_archives"].into_iter().chain(SUMMARIES.iter().copied()) {
        let count: i64 = db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
        let converted: i64 = db.query_row(&format!("SELECT COUNT(*) FROM {table}_v14"), [], |r| r.get(0))?;
        ensure!(count == converted, "History migration row count mismatch: {table}");
        db.execute_batch(&format!("DROP TABLE {table}; ALTER TABLE {table}_v14 RENAME TO {table};"))?;
    }
    db.execute_batch("CREATE INDEX changes_scan ON changes(scan); PRAGMA user_version=14;")?;
    let mut check = db.prepare("PRAGMA foreign_key_check")?;
    ensure!(check.query([])?.next()?.is_none(), "History migration foreign key check failed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_migration_fixture() {
        let Ok(path) = std::env::var("VOLUMETRAIL_MIGRATION_CRASH_FIXTURE") else { return; };
        let db = Connection::open(&path).unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        db.progress_handler(1000, Some(move || {
            if calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 25 { std::process::exit(74); }
            false
        }));
        migrate(&db, Path::new(&path)).unwrap();
        panic!("Fixture did not interrupt the migration");
    }

    #[test]
    fn abrupt_exit_recovers_original_tables_and_migration_can_restart() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("history.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA user_version=13;
            CREATE TABLE scans(id INTEGER PRIMARY KEY); INSERT INTO scans VALUES(1);
            CREATE TABLE changes(scan,old_path,new_path,logical_delta,allocated_delta,kind);
            CREATE TABLE change_archives(scan,part,entries,data);
            CREATE TABLE folder_changes(scan,path,allocated_delta,moved_delta,events);
            CREATE TABLE extension_changes(scan,path,extension,delta,events);
            CREATE TABLE carry_folders(volume,path,allocated_delta,moved_delta,events);
            CREATE TABLE carry_extensions(volume,path,extension,delta,events);
            BEGIN;").unwrap();
        for n in 0..2000 {
            db.execute("INSERT INTO changes VALUES(1,'',?,1,1,'created')", [format!("C:\\目录\\file-{n}")]).unwrap();
        }
        db.execute_batch("COMMIT; PRAGMA wal_checkpoint(TRUNCATE);").unwrap(); drop(db);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "store::path_migration::tests::interrupted_migration_fixture"])
            .env("VOLUMETRAIL_MIGRATION_CRASH_FIXTURE", &path).status().unwrap();
        assert_eq!(status.code(), Some(74));
        let db = Connection::open(&path).unwrap();
        assert_eq!(db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 13);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM changes WHERE new_path LIKE 'C:%'", [], |r| r.get::<_, i64>(0)).unwrap(), 2000);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='history_paths'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        migrate(&db, &path).unwrap();
        let mut reader = paths::Reader::new(&db);
        let mut stmt = db.prepare("SELECT seq,new_path_id FROM changes ORDER BY seq").unwrap();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap() {
            let (seq, id) = row.unwrap(); assert_eq!(reader.resolve(id).unwrap(), format!("C:\\目录\\file-{}", seq-1));
        }
        assert_eq!(db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
    }

    #[test]
    fn malformed_archive_rolls_back_the_whole_conversion_and_keeps_backup() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("history.db");
        let db = Connection::open(&path).unwrap();
        // Only the tables reached before archive decoding are needed here.
        db.execute_batch("PRAGMA user_version=13; CREATE TABLE scans(id INTEGER PRIMARY KEY);
            INSERT INTO scans VALUES(1);
            CREATE TABLE changes(scan,old_path,new_path,logical_delta,allocated_delta,kind);
            CREATE TABLE change_archives(scan,part,entries,data);
            INSERT INTO change_archives VALUES(1,0,1,x'00000000');").unwrap();
        assert!(migrate(&db, &path).is_err());
        assert!(db.is_autocommit());
        assert_eq!(db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 13);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='history_paths'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        let backup = fs::read_dir(dir.path()).unwrap().map(|p| p.unwrap().path())
            .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("history-before-v14-")).unwrap();
        let backup = Connection::open(backup).unwrap();
        assert_eq!(backup.query_row("SELECT hex(data) FROM change_archives", [], |r| r.get::<_, String>(0)).unwrap(), "00000000");
    }
}
