mod common;
use volumetrail::{model::*, store::{Change, Store}};

fn outcome(usn: i64) -> ScanOutcome {
    ScanOutcome { checkpoint: Checkpoint { volume_id: "test-volume".into(), journal_id: 1, next_usn: usn, root_id: 5 },
        mode: if usn == 1 { ScanMode::Full } else { ScanMode::Incremental }, total: 1_000_000,
        free: 800_000, sampled_at: format!("2026-10-24T00:00:{usn:02}Z"), pending: vec![], warnings: vec![] }
}

fn files(size: u64) -> Vec<Mutation> {
    (10..810).map(|id| Mutation::Upsert(Entry { id, parent: 5,
        name: format!("日志-重复目录说明-{id}.log"), is_dir: false, logical: size * id,
        allocated: size * id, modified: 1, attributes: 0, links: vec![] })).collect()
}

fn canonical(changes: Vec<Change>) -> Vec<(String,String,i64,i64,String)> {
    let mut rows = changes.into_iter().map(|c| (c.old_path,c.new_path,c.logical_delta,c.allocated_delta,c.kind)).collect::<Vec<_>>();
    rows.sort(); rows
}

#[test]
fn compressed_details_preserve_filtering_pagination_and_reopening() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("history.db");
    let mut store = Store::open(&path).unwrap();
    let first = store.publish("C:\\", &files(1), &outcome(1)).unwrap();
    let middle = store.publish("C:\\", &files(2), &outcome(2)).unwrap();
    let last = store.publish("C:\\", &files(3), &outcome(3)).unwrap();
    let expected = canonical(store.changes_between_page("test-volume",first,last,5000,0).unwrap());
    let growth = serde_json::to_string(&store.folder_growth("test-volume",first,last,5).unwrap()).unwrap();
    let extensions = store.extensions_between_path("test-volume",first,last,"C:\\").unwrap().items;
    store.compact_history().unwrap(); store.compact_history().unwrap();
    drop(store);
    let store = Store::open_read_only(&path).unwrap();
    assert_eq!(store.changes_count("test-volume",first,last,None).unwrap(),1600);
    assert_eq!(store.changes_count("test-volume",first,last,Some("c:\\")).unwrap(),1600);
    assert_eq!(canonical(store.changes_between_path("test-volume",first,last,"c:\\",5000,0).unwrap()),expected);
    let mut pages = vec![];
    for offset in (0..1600).step_by(100) {
        pages.extend(store.changes_between_page("test-volume",first,last,100,offset).unwrap());
    }
    assert_eq!(canonical(pages),expected);
    assert_eq!(store.extensions_between_path("test-volume",first,last,"C:\\").unwrap().items,extensions);
    assert_eq!(serde_json::to_string(&store.folder_growth("test-volume",first,last,5).unwrap()).unwrap(),growth);
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(db.query_row("SELECT SUM(entries) FROM change_archives WHERE scan=?",[middle],|r|r.get::<_,i64>(0)).unwrap(),800);
    assert_eq!(db.query_row("SELECT COUNT(*) FROM changes WHERE scan=?",[middle],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(db.query_row("PRAGMA integrity_check",[],|r|r.get::<_,String>(0)).unwrap(),"ok");
}

#[test]
fn compressed_history_supports_future_scans_and_explicit_deletion() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("history.db");let mut store=Store::open(&path).unwrap();
    let first=store.publish("C:\\",&files(1),&outcome(1)).unwrap();
    let middle=store.publish("C:\\",&files(2),&outcome(2)).unwrap();
    store.publish("C:\\",&files(3),&outcome(3)).unwrap();store.compact_history().unwrap();
    let last=store.publish("C:\\",&files(4),&outcome(4)).unwrap();store.compact_history().unwrap();
    assert_eq!(store.changes_count("test-volume",first,last,None).unwrap(),2400);
    store.clear_details(&[middle]).unwrap();
    assert_eq!(store.changes_count("test-volume",first,last,None).unwrap(),1600);
    store.delete_scans(&[last-1]).unwrap();
    assert_eq!(store.changes_count("test-volume",first,last,None).unwrap(),800);
    let db=rusqlite::Connection::open(&path).unwrap();
    assert_eq!(db.query_row("SELECT COUNT(*) FROM change_archives",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn compact_index_migrates_legacy_payload_without_changing_file_identity_or_links() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("history.db");let mut store=Store::open(&path).unwrap();
    let entry=Entry {id:u64::MAX-10,parent:5,name:"中文 😀.txt".into(),is_dir:false,logical:13,allocated:4096,
        modified:-1,attributes:0x40000,links:vec![Link {parent:5,name:"中文 😀.txt".into()}]};
    store.publish("C:\\",&[Mutation::Upsert(entry.clone())],&outcome(1)).unwrap();drop(store);
    let db=rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE nodes SET payload=? WHERE id=?",rusqlite::params![serde_json::to_string(&entry).unwrap(),entry.id as i64]).unwrap();
    common::text_history(&db);db.execute_batch("DROP TABLE change_archives; PRAGMA user_version=11;").unwrap();drop(db);
    let store=Store::open(&path).unwrap();
    assert_eq!(store.indexed_entry("test-volume",entry.id).unwrap(),Some(entry.clone()));drop(store);
    let store=Store::open_read_only(&path).unwrap();assert_eq!(store.indexed_entry("test-volume",entry.id).unwrap(),Some(entry));
    let db=rusqlite::Connection::open(&path).unwrap();
    let payload:String=db.query_row("SELECT payload FROM nodes LIMIT 1",[],|r|r.get(0)).unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&payload).unwrap().get("name").is_none());
}

#[test]
fn cancelled_compaction_preserves_details_and_can_resume() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("history.db");let stop=dir.path().join("stop.flag");
    let mut store=Store::open(&path).unwrap();
    let first=store.publish("C:\\",&files(1),&outcome(1)).unwrap();
    store.publish("C:\\",&files(2),&outcome(2)).unwrap();let last=store.publish("C:\\",&files(3),&outcome(3)).unwrap();
    let expected=canonical(store.changes_between("test-volume",first,last).unwrap());
    store.set_cancel_path(stop.clone());std::fs::write(&stop,b"stop").unwrap();assert!(store.compact_history().is_err());
    assert_eq!(canonical(store.changes_between("test-volume",first,last).unwrap()),expected);
    std::fs::remove_file(stop).unwrap();store.compact_history().unwrap();
    assert_eq!(canonical(store.changes_between("test-volume",first,last).unwrap()),expected);
}

#[test]
fn damaged_compressed_history_returns_an_error_instead_of_empty_details() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("history.db");let mut store=Store::open(&path).unwrap();
    let first=store.publish("C:\\",&files(1),&outcome(1)).unwrap();
    store.publish("C:\\",&files(2),&outcome(2)).unwrap();let last=store.publish("C:\\",&files(3),&outcome(3)).unwrap();
    store.compact_history().unwrap();
    let db=rusqlite::Connection::open(&path).unwrap();db.execute("UPDATE change_archives SET data=x'00000000'",[]).unwrap();
    assert!(store.changes_between("test-volume",first,last).is_err());
}

#[test]
fn equal_size_details_keep_their_page_order_after_compression() {
    let dir=tempfile::tempdir().unwrap(); let path=dir.path().join("history.db");
    let mut store=Store::open(&path).unwrap();
    let mutations=|size| files(1).into_iter().map(|mutation| match mutation {
        Mutation::Upsert(mut entry) => {entry.logical=size;entry.allocated=size;Mutation::Upsert(entry)},
        other => other,
    }).collect::<Vec<_>>();
    let first=store.publish("C:\\",&mutations(1),&outcome(1)).unwrap();
    store.publish("C:\\",&mutations(2),&outcome(2)).unwrap();
    let last=store.publish("C:\\",&mutations(3),&outcome(3)).unwrap();
    let values=|changes:Vec<Change>| changes.into_iter().map(|c|
        (c.old_path,c.new_path,c.logical_delta,c.allocated_delta,c.kind)).collect::<Vec<_>>();
    let expected=values(store.changes_between_page("test-volume",first,last,5000,0).unwrap());
    store.compact_history().unwrap();
    let mut pages=vec![];
    for offset in (0..1600).step_by(100) {
        pages.extend(store.changes_between_page("test-volume",first,last,100,offset).unwrap());
    }
    assert_eq!(values(pages),expected);
}
