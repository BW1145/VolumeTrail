/// Restore schema-13 text tables when a migration fixture starts from the
/// current schema. Fixture scans must still have their details in live rows.
pub fn text_history(db: &rusqlite::Connection) {
    assert_eq!(db.query_row("SELECT COUNT(*) FROM change_archives", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    db.execute_batch("PRAGMA foreign_keys=OFF; BEGIN;
        CREATE TEMP TABLE fixture_paths AS WITH RECURSIVE paths(id,path) AS (
            SELECT 0,'' UNION ALL SELECT h.id,p.path||h.fragment
            FROM paths p CROSS JOIN history_paths h WHERE h.parent=p.id AND h.id<>0)
            SELECT * FROM paths;
        CREATE TABLE changes_text(scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
            old_path TEXT NOT NULL,new_path TEXT NOT NULL,logical_delta INTEGER NOT NULL,
            allocated_delta INTEGER NOT NULL,kind TEXT NOT NULL);
        INSERT INTO changes_text(rowid,scan,old_path,new_path,logical_delta,allocated_delta,kind)
            SELECT c.seq,c.scan,a.path,b.path,c.logical_delta,c.allocated_delta,c.kind
            FROM changes c JOIN fixture_paths a ON a.id=c.old_path_id JOIN fixture_paths b ON b.id=c.new_path_id;
        DROP TABLE changes; ALTER TABLE changes_text RENAME TO changes;
        CREATE INDEX changes_scan ON changes(scan);").unwrap();
    for (table, key, fields, primary) in [
        ("folder_changes", "scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE", "allocated_delta INTEGER NOT NULL,moved_delta INTEGER NOT NULL,events INTEGER NOT NULL", "scan,path"),
        ("extension_changes", "scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE", "extension TEXT NOT NULL,delta INTEGER NOT NULL,events INTEGER NOT NULL", "scan,path,extension"),
        ("carry_folders", "volume TEXT NOT NULL", "allocated_delta INTEGER NOT NULL,moved_delta INTEGER NOT NULL,events INTEGER NOT NULL", "volume,path"),
        ("carry_extensions", "volume TEXT NOT NULL", "extension TEXT NOT NULL,delta INTEGER NOT NULL,events INTEGER NOT NULL", "volume,path,extension"),
    ] {
        let suffix = if table.contains("extension") { "t.extension,t.delta,t.events" } else { "t.allocated_delta,t.moved_delta,t.events" };
        let first = if table.starts_with("carry") { "volume" } else { "scan" };
        db.execute_batch(&format!("CREATE TABLE {table}_text({key},path TEXT NOT NULL,{fields},PRIMARY KEY({primary}));
            INSERT INTO {table}_text SELECT t.{first},p.path,{suffix} FROM {table} t JOIN fixture_paths p ON p.id=t.path_id;
            DROP TABLE {table}; ALTER TABLE {table}_text RENAME TO {table};")).unwrap();
    }
    db.execute_batch("DROP TABLE history_paths; DROP TABLE fixture_paths; PRAGMA user_version=13; COMMIT; PRAGMA foreign_keys=ON;").unwrap();
}
