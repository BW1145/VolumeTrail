mod common;
use flate2::{Compression, write::ZlibEncoder};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{collections::HashMap, io::Write};
use volumetrail::{model::*, store::{Change, Store}};

fn outcome(usn: i64) -> ScanOutcome {
    ScanOutcome { checkpoint: Checkpoint { volume_id: "v".into(), journal_id: 1, next_usn: usn, root_id: 5 },
        mode: if usn == 1 { ScanMode::Full } else { ScanMode::Incremental }, total: 1_000_000, free: 800_000,
        sampled_at: format!("2026-10-24T00:00:{usn:02}Z"), pending: vec![], warnings: vec![] }
}

fn files(size: u64) -> Vec<Mutation> {
    let mut entries = vec![Mutation::Upsert(Entry { id: 20, parent: 5, name: "目录 😀".into(),
        is_dir: true, logical: 0, allocated: 0, modified: 1, attributes: 0, links: vec![] })];
    entries.extend((100..700).map(|id| Mutation::Upsert(Entry { id, parent: 20,
        name: format!("File-{id}.log"), is_dir: false, logical: size, allocated: size,
        modified: 1, attributes: 0, links: vec![] })));
    entries
}

fn values(rows: Vec<Change>) -> Value {
    json!(rows.into_iter().map(|r| (r.old_path, r.new_path, r.logical_delta, r.allocated_delta, r.kind)).collect::<Vec<_>>())
}

fn snapshot(store: &Store, to: i64) -> Value {
    json!({"reports":store.reports().unwrap(),
        "details":values(store.changes_between_page("v", 0, to, 5000, 0).unwrap()),
        "filtered":values(store.changes_between_path("v", 0, to, "c:\\目录 😀", 100, 100).unwrap()),
        "count":store.changes_count("v", 0, to, Some("c:\\目录 😀")).unwrap(),
        "growth":store.folder_growth("v", 0, to, 3).unwrap(),
        "breakdown":format!("{:?}",store.folder_breakdown("v", 0, to, "C:\\").unwrap()),
        "extensions":store.extensions_between_path("v", 0, to, "c:\\目录 😀").unwrap().items})
}

fn archive_legacy(db: &Connection, scan: i64) {
    let mut stmt = db.prepare("SELECT rowid,old_path,new_path,logical_delta,allocated_delta,kind FROM changes WHERE scan=? ORDER BY rowid").unwrap();
    let rows = stmt.query_map([scan], |r| Ok(json!([r.get::<_, i64>(0)?, r.get::<_, String>(1)?,
        r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, String>(5)?])))
        .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    for (part, chunk) in rows.chunks(256).enumerate() {
        let mut data = Value::Array(chunk.to_vec());
        if part % 2 == 0 {
            let mut ids = HashMap::new();
            let mut shared = chunk.to_vec();
            for row in &mut shared {
                for col in [1, 2] {
                    let path = row[col].as_str().unwrap().to_owned();
                    if let Some(&id) = ids.get(&path) { row[col] = json!(id); }
                    else { let id = ids.len(); ids.insert(path, id); }
                }
            }
            data = json!({"rows": shared});
        }
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(serde_json::to_string(&data).unwrap().as_bytes()).unwrap();
        db.execute("INSERT INTO change_archives VALUES(?,?,?,?)", params![scan,part as i64,chunk.len() as i64,encoder.finish().unwrap()]).unwrap();
    }
    db.execute("DELETE FROM changes WHERE scan=?", [scan]).unwrap();
}

#[test]
fn migration_preserves_mixed_archives_live_rows_summaries_carries_and_future_scans() {
    for carry in [false, true] {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("history.db");
        let mut store = Store::open(&path).unwrap();
        store.publish("C:\\", &files(1), &outcome(1)).unwrap();
        let middle = store.publish("C:\\", &files(2), &outcome(2)).unwrap();
        let last = store.publish("C:\\", &files(3), &outcome(3)).unwrap();
        if carry { store.delete_scans(&[last]).unwrap(); }
        let expected = snapshot(&store, last); drop(store);
        let db = Connection::open(&path).unwrap(); common::text_history(&db);
        archive_legacy(&db, middle); drop(db);
        let legacy = Store::open_read_only(&path).unwrap();
        assert_eq!(snapshot(&legacy, last), expected); drop(legacy);
        let mut migrated = Store::open(&path).unwrap();
        assert_eq!(snapshot(&migrated, last), expected);
        migrated.publish("C:\\", &files(4), &outcome(4)).unwrap();
        migrated.compact_history().unwrap();
        // New writes and compression leave all earlier intervals intact.
        assert_eq!(values(migrated.changes_between_page("v",0,last,5000,0).unwrap()), expected["details"]);
        drop(migrated);
        let db = Connection::open(&path).unwrap();
        assert_eq!(db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
        assert!(db.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
        let backups = std::fs::read_dir(dir.path()).unwrap().map(|p| p.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e=="db") && p.file_name().unwrap().to_string_lossy().starts_with("history-before-v14-")).collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        let backup = Store::open_read_only(&backups[0]).unwrap(); assert_eq!(snapshot(&backup, last), expected);
        drop(backup); drop(db);
        drop(Store::open(&path).unwrap());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().filter(|p| {let p=p.as_ref().unwrap().path();p.extension().is_some_and(|e| e=="db") && p.file_name().unwrap().to_string_lossy().starts_with("history-before-v14-")}).count(), 1);
    }
}

#[test]
fn clearing_and_deleting_history_keep_referenced_paths_and_release_unused_ones() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("history.db");
    let mut store = Store::open(&path).unwrap();
    let first = store.publish("C:\\", &files(1), &outcome(1)).unwrap();
    let middle = store.publish("C:\\", &files(2), &outcome(2)).unwrap();
    let last = store.publish("C:\\", &files(3), &outcome(3)).unwrap(); store.compact_history().unwrap();
    store.clear_details(&[last]).unwrap();
    assert_eq!(store.changes_count("v", first, last, None).unwrap(), 600);
    assert_eq!(store.changes(middle).unwrap().len(), 600);
    store.delete_scans(&[first, middle, last]).unwrap();
    let db = Connection::open(&path).unwrap();
    assert_eq!(db.query_row("SELECT COUNT(*) FROM history_paths", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    // IDs from a previous operation cannot leak through caches after collection.
    let next = store.publish("C:\\", &files(4), &outcome(4)).unwrap();
    assert_eq!(store.changes(next).unwrap().len(), 600);
}

#[test]
fn missing_global_path_reference_reports_damaged_history() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("history.db");
    let mut store = Store::open(&path).unwrap();
    let first = store.publish("C:\\", &files(1), &outcome(1)).unwrap();
    let second = store.publish("C:\\", &files(2), &outcome(2)).unwrap();
    store.publish("C:\\", &files(3), &outcome(3)).unwrap(); store.compact_history().unwrap();
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(b"[[1,0,999999999,1,1,\"created\"]]").unwrap();
    Connection::open(&path).unwrap().execute("UPDATE change_archives SET entries=1,data=? WHERE scan=? AND part=0",
        params![encoder.finish().unwrap(), second]).unwrap();
    assert!(store.changes_between("v", first, second).unwrap_err().to_string().contains("Missing history path"));
}
