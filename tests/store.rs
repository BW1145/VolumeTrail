use volumetrail::{model::*, store::Store};

fn directory(id: u64, parent: u64, name: &str) -> Entry {
    Entry { id, parent, name: name.into(), is_dir: true, logical: 0, allocated: 0,
        modified: 0, attributes: 0, links: vec![] }
}

fn child_file(id: u64, parent: u64, name: &str, size: u64) -> Entry {
    Entry { id, parent, name: name.into(), is_dir: false, logical: size, allocated: size,
        modified: 0, attributes: 0, links: vec![] }
}

#[test]
fn extension_changes_follow_moves_resizes_and_case_insensitive_paths() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let first = store.publish("C:\\", &[Mutation::Upsert(directory(20,5,"Left")),
        Mutation::Upsert(directory(21,5,"Right")), Mutation::Upsert(child_file(30,20,"one.mp4",100))],
        &outcome(ScanMode::Full,10)).unwrap();
    let second = store.publish("C:\\", &[Mutation::Upsert(child_file(30,21,"one.zip",150))],
        &outcome(ScanMode::Incremental,20)).unwrap();
    let left = store.extensions_between_path("volume-A",first,second,"c:\\LEFT\\").unwrap();
    assert_eq!(left.items, vec![(".mp4".into(),-100,1)]);
    assert_eq!((left.complete_intervals,left.intervals),(1,1));
    assert_eq!(store.extensions_between_path("volume-A",first,second,"c:\\right").unwrap().items,
        vec![(".zip".into(),150,1)]);
    assert_eq!(store.changes_count("volume-A",first,second,Some("c:\\left")).unwrap(),1);
    assert_eq!(store.changes_between_path("volume-A",first,second,"c:\\RIGHT",50,0).unwrap().len(),1);
    let third = store.publish("C:\\", &[Mutation::Upsert(child_file(30,21,"one.txt",150))],
        &outcome(ScanMode::Incremental,30)).unwrap();
    let types = store.extensions_between_path("volume-A",second,third,"C:\\Right").unwrap();
    assert!(types.items.iter().any(|(ext,delta,_)| ext==".zip" && *delta == -150));
    assert!(types.items.iter().any(|(ext,delta,_)| ext==".txt" && *delta == 150));
    assert_eq!(types.items.iter().map(|(_,delta,_)| *delta).sum::<i128>(),0);
}

#[test]
fn incremental_directory_changes_match_full_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let mut incremental = Store::open(&dir.path().join("incremental.db")).unwrap();
    let mut full = Store::open(&dir.path().join("full.db")).unwrap();
    let snapshots = vec![
        vec![directory(20,5,"A"),directory(21,20,"B"),directory(22,5,"Target"),
            child_file(30,20,"one.bin",100),child_file(31,21,"two.txt",200),child_file(32,22,"three.mp4",300)],
        vec![directory(20,22,"A"),directory(21,20,"B"),directory(22,5,"Target"),
            child_file(30,20,"one.bin",150),child_file(31,21,"two.txt",200),child_file(32,22,"three.mp4",300)],
        vec![directory(20,21,"A"),directory(21,5,"B"),directory(22,5,"Target"),
            child_file(30,20,"one.bin",150),child_file(31,21,"two.txt",220),child_file(32,22,"three.mp4",300)],
        vec![directory(20,21,"Renamed"),directory(21,5,"B"),directory(22,5,"Target"),
            child_file(30,20,"one.bin",150),child_file(31,21,"two.txt",220),child_file(32,20,"three.mp4",320)],
        vec![directory(21,5,"B"),directory(22,5,"Target"),child_file(31,21,"two.txt",220)],
        vec![directory(21,5,"B"),directory(22,5,"Target"),child_file(31,21,"two.txt",220),child_file(33,99,"orphan.dat",50)],
        vec![directory(21,5,"B"),directory(22,5,"Target"),directory(99,22,"Recovered"),
            child_file(31,21,"two.txt",220),child_file(33,99,"orphan.dat",50)],
    ];
    let mut previous: Vec<Entry> = Vec::new();
    let mut prev_inc = 0;
    let mut prev_full = 0;
    for (index, entries) in snapshots.into_iter().enumerate() {
        let mut changes: Vec<_> = entries.iter().filter(|e| !previous.contains(e)).cloned().map(Mutation::Upsert).collect();
        for old in &previous {
            if !entries.iter().any(|e| e.id == old.id) { changes.push(Mutation::Delete(old.id)); }
        }
        // USN can include a child whose contents did not change while its ancestor moved.
        if index == 1 { changes.push(Mutation::Upsert(entries.iter().find(|e| e.id==31).unwrap().clone())); }
        let usn = (index as i64 + 1)*10;
        let inc_id = incremental.publish("C:\\",&changes,
            &outcome(if index==0 { ScanMode::Full } else { ScanMode::Incremental },usn)).unwrap();
        let full_id = full.publish("C:\\",&entries.iter().cloned().map(Mutation::Upsert).collect::<Vec<_>>(),
            &outcome(ScanMode::Full,usn)).unwrap();
        assert_eq!(incremental.totals("volume-A").unwrap(),full.totals("volume-A").unwrap());
        for id in [5,20,21,22,99] {
            assert_eq!(incremental.folder_stats("volume-A",id).unwrap(),full.folder_stats("volume-A",id).unwrap(),
                "snapshot {index}, folder {id}");
        }
        if index>0 {
            let growth = |store: &Store,from,to| store.folder_growth("volume-A",from,to,10).unwrap()
                .into_iter().map(|item|(item.path,item.allocated_delta,item.moved_delta)).collect::<Vec<_>>();
            assert_eq!(growth(&incremental,prev_inc,inc_id),growth(&full,prev_full,full_id),"snapshot {index}");
            for path in ["C:\\","c:\\B","C:\\Target"] {
                let types = |store: &Store,from,to| {
                    let mut items: Vec<_> = store.extensions_between_path("volume-A",from,to,path).unwrap().items
                        .into_iter().filter(|(_,delta,_)| *delta!=0).map(|(ext,delta,_)|(ext,delta)).collect();
                    items.sort(); items
                };
                assert_eq!(types(&incremental,prev_inc,inc_id),types(&full,prev_full,full_id),"snapshot {index}, {path}");
            }
        }
        previous = entries;
        prev_inc = inc_id;
        prev_full = full_id;
    }
}

#[test]
fn extension_history_survives_detail_cleanup_and_deleted_intermediate_points() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let first = store.publish("C:\\", &[file(30,"a.bin",10)], &outcome(ScanMode::Full,10)).unwrap();
    let middle = store.publish("C:\\", &[file(30,"a.bin",20)], &outcome(ScanMode::Incremental,20)).unwrap();
    let last = store.publish("C:\\", &[file(30,"a.bin",25)], &outcome(ScanMode::Incremental,30)).unwrap();
    store.clear_details(&[middle,last]).unwrap();
    store.delete_scans(&[middle]).unwrap();
    let types=store.extensions_between_path("volume-A",first,last,"C:\\").unwrap();
    assert_eq!(types.items,vec![(".bin".into(),15,2)]);
    assert_eq!(types.complete_intervals,1);
    store.delete_scans(&[last]).unwrap();
    let next=store.publish("C:\\", &[file(30,"a.bin",28)], &outcome(ScanMode::Incremental,40)).unwrap();
    assert_eq!(store.extensions_between_path("volume-A",first,next,"C:\\").unwrap().items,
        vec![(".bin".into(),18,3)]);
    store.prune(1).unwrap();
    let types=store.extensions_between_path("volume-A",first,next,"C:\\").unwrap();
    assert!(types.items.is_empty());
    assert_eq!(types.complete_intervals,0);
}

#[test]
fn legacy_extension_coverage_is_explicit_and_carries_forward() {
    let dir=tempfile::tempdir().unwrap();
    let path=dir.path().join("history.db");
    let mut store=Store::open(&path).unwrap();
    let first=store.publish("C:\\", &[file(30,"a.bin",10)], &outcome(ScanMode::Full,10)).unwrap();
    let legacy=store.publish("C:\\", &[file(30,"a.bin",20)], &outcome(ScanMode::Incremental,20)).unwrap();
    let db=rusqlite::Connection::open(&path).unwrap();
    db.execute("DELETE FROM extension_changes WHERE scan=?",[legacy]).unwrap();
    db.execute("UPDATE scans SET extensions_complete=0 WHERE id=?",[legacy]).unwrap();
    let types=store.extensions_between_path("volume-A",first,legacy,"c:\\").unwrap();
    assert_eq!(types.complete_intervals,0);
    assert_eq!(types.items[0].1,10);
    store.delete_scans(&[legacy]).unwrap();
    let next=store.publish("C:\\", &[file(30,"a.bin",25)], &outcome(ScanMode::Incremental,30)).unwrap();
    let types=store.extensions_between_path("volume-A",first,next,"C:\\").unwrap();
    assert_eq!(types.complete_intervals,0);
    assert_eq!(types.items[0].1,5);
}

#[test]
fn unresolved_growth_remains_visible_and_missing_parents_can_be_repaired() {
    let dir=tempfile::tempdir().unwrap();
    let mut store=Store::open(&dir.path().join("history.db")).unwrap();
    let first=store.publish("C:\\", &[], &outcome(ScanMode::Full,10)).unwrap();
    let second=store.publish("C:\\", &[Mutation::Upsert(child_file(30,99,"a.bin",100))],
        &outcome(ScanMode::Incremental,20)).unwrap();
    let growth=store.folder_growth("volume-A",first,second,5).unwrap();
    assert_eq!((growth[0].path.as_str(),growth[0].allocated_delta),("[unresolved]",100));
    assert_eq!(store.folder_breakdown("volume-A",first,second,"C:\\").unwrap().allocated_delta,100);
    store.begin_stage().unwrap();
    assert!(store.missing_parent_ids("volume-A",false).unwrap().contains(&99));
    store.stage(Mutation::Upsert(directory(99,5,"Recovered"))).unwrap();
    assert!(!store.missing_parent_ids("volume-A",false).unwrap().contains(&99));
    store.discard_stage().unwrap();
    let third=store.publish("C:\\", &[Mutation::Upsert(directory(99,5,"Recovered"))],
        &outcome(ScanMode::Incremental,30)).unwrap();
    let growth=store.folder_growth("volume-A",second,third,5).unwrap();
    assert!(growth.iter().any(|g|g.path=="[unresolved]" && g.allocated_delta == -100));
    assert!(growth.iter().any(|g|g.path=="C:\\Recovered" && g.allocated_delta == 100));
    assert_eq!(store.folder_stats("volume-A",5).unwrap().1,100);
}

fn file(id: u64, name: &str, size: u64) -> Mutation {
    Mutation::Upsert(Entry {
        id, parent: 5, name: name.into(), is_dir: false,
        logical: size, allocated: size, modified: 0, attributes: 0,
        links: vec![Link { parent: 5, name: name.into() }],
    })
}

fn outcome(mode: ScanMode, usn: i64) -> ScanOutcome {
    ScanOutcome {
        checkpoint: Checkpoint { volume_id: "volume-A".into(), journal_id: 7, next_usn: usn, root_id: 5 },
        mode, total: 1_000_000, free: 800_000, sampled_at: chrono::Local::now().to_rfc3339(), pending: vec![], warnings: vec![],
    }
}

#[test]
fn baseline_counts_unique_file_identity_and_persists_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let mut store = Store::open(&path).unwrap();
    store.publish("C:\\", &[file(10, "one", 100), file(10, "second-link", 100), file(11, "other", 200)], &outcome(ScanMode::Full, 50)).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.totals("volume-A").unwrap(), (300, 300));
    assert_eq!(store.checkpoint("C:\\").unwrap().unwrap().next_usn, 50);
}

#[test]
fn repeated_full_scan_keeps_unchanged_files_and_reports_only_deltas() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    store.publish("C:\\", &[file(10, "same", 100), file(11, "grows", 200), file(12, "gone", 300)],
        &outcome(ScanMode::Full, 50)).unwrap();
    let scan = store.publish("C:\\", &[file(10, "same", 100), file(11, "grows", 400), file(13, "new", 50)],
        &outcome(ScanMode::Full, 80)).unwrap();
    let changes = store.changes(scan).unwrap();
    assert_eq!(changes.len(), 3);
    assert!(changes.iter().any(|change| change.new_path == "C:\\grows" && change.allocated_delta == 200));
    assert!(changes.iter().any(|change| change.old_path == "C:\\gone" && change.allocated_delta == -300));
    assert!(changes.iter().any(|change| change.new_path == "C:\\new" && change.allocated_delta == 50));
    assert_eq!(store.totals("volume-A").unwrap(), (550, 550));
}

#[test]
fn incremental_reports_growth_delete_and_zero_growth_rename() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    store.publish("C:\\", &[file(10, "growing", 100), file(11, "deleted", 200), file(12, "old-name", 50)], &outcome(ScanMode::Full, 50)).unwrap();
    let scan = store.publish("C:\\", &[file(10, "growing", 400), Mutation::Delete(11), file(12, "new-name", 50)], &outcome(ScanMode::Incremental, 80)).unwrap();
    assert_eq!(store.totals("volume-A").unwrap(), (450, 450));
    let changes = store.changes(scan).unwrap();
    assert_eq!(changes.len(), 3);
    assert!(changes.iter().any(|c| c.logical_delta == 300 && c.new_path == "C:\\growing"));
    assert!(changes.iter().any(|c| c.logical_delta == -200 && c.old_path == "C:\\deleted"));
    assert!(changes.iter().any(|c| c.kind == "moved" && c.logical_delta == 0 && c.new_path == "C:\\new-name"));
}

#[test]
fn folder_growth_uses_all_changes_across_selected_scans() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let names = ["Users", "Example", "AppData", "Local", "LogiOptionsPlus"];
    let folders: Vec<_> = names.iter().enumerate().map(|(index, name)| Mutation::Upsert(Entry {
        id: 20 + index as u64,
        parent: if index == 0 { 5 } else { 19 + index as u64 },
        name: (*name).into(), is_dir: true, logical: 0, allocated: 0,
        modified: 0, attributes: 0, links: vec![],
    })).collect();
    let files = |size| (0..2001u64).map(|index| {
        let mut entry = match file(100 + index, &format!("cache-{index}"), size) {
            Mutation::Upsert(entry) => entry, _ => unreachable!(),
        };
        entry.parent = 24;
        Mutation::Upsert(entry)
    }).collect::<Vec<_>>();
    let mut baseline = folders;
    baseline.extend(files(10));
    let first = store.publish("C:\\", &baseline, &outcome(ScanMode::Full, 50)).unwrap();
    let second = store.publish("C:\\", &files(20), &outcome(ScanMode::Incremental, 80)).unwrap();
    let third = store.publish("C:\\", &[file(9000, "other", 50)], &outcome(ScanMode::Incremental, 100)).unwrap();

    assert_eq!(store.changes_between("volume-A", first, second).unwrap().len(), 2000);
    let growth = store.folder_growth("volume-A", first, third, 5).unwrap();
    assert_eq!(growth[0].path, "C:\\Users\\Example\\AppData\\Local\\LogiOptionsPlus");
    assert_eq!(growth[0].allocated_delta, 20010);
    assert_eq!(growth[0].changes, 2001);
    assert_eq!(store.folder_growth("volume-A", second, third, 5).unwrap()[0].allocated_delta, 50);
    assert_eq!(store.folder_growth("volume-A", first, second, 4).unwrap()[0].path,
        "C:\\Users\\Example\\AppData\\Local");
    let fourth = store.publish("C:\\", &[Mutation::Delete(100)], &outcome(ScanMode::Incremental, 120)).unwrap();
    let released = store.folder_growth("volume-A", third, fourth, 5).unwrap();
    assert_eq!(released[0].path, "C:\\Users\\Example\\AppData\\Local\\LogiOptionsPlus");
    assert_eq!(released[0].allocated_delta, -20);
}

#[test]
fn journal_change_cannot_silently_apply_partial_increment() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    store.publish("C:\\", &[file(10, "file", 100)], &outcome(ScanMode::Full, 50)).unwrap();
    let mut invalid = outcome(ScanMode::Incremental, 80);
    invalid.checkpoint.journal_id = 8;
    assert!(store.publish("C:\\", &[file(10, "file", 999)], &invalid).is_err());
    assert_eq!(store.totals("volume-A").unwrap(), (100, 100));
    assert_eq!(store.checkpoint("C:\\").unwrap().unwrap().next_usn, 50);
}

#[test]
fn cancelled_staging_keeps_published_index_and_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let mut store = Store::open(&path).unwrap();
    store.publish("C:\\", &[file(10, "file", 100)], &outcome(ScanMode::Full, 50)).unwrap();
    store.begin_stage().unwrap();
    store.stage(file(10, "file", 999)).unwrap();
    store.discard_stage().unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.totals("volume-A").unwrap(), (100, 100));
    assert_eq!(store.checkpoint("C:\\").unwrap().unwrap().next_usn, 50);
}

#[test]
fn directory_move_does_not_report_descendant_growth() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let folder = |name: &str| Mutation::Upsert(Entry { id: 20, parent: 5, name: name.into(), is_dir: true, logical: 0, allocated: 0, modified: 0, attributes: 0, links: vec![] });
    let mut child = if let Mutation::Upsert(e) = file(30, "child", 123) { e } else { unreachable!() };
    child.parent = 20;
    store.publish("C:\\", &[folder("before"), Mutation::Upsert(child)], &outcome(ScanMode::Full, 50)).unwrap();
    let scan = store.publish("C:\\", &[folder("after")], &outcome(ScanMode::Incremental, 80)).unwrap();
    let changes = store.changes(scan).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].logical_delta, 0);
    assert_eq!(changes[0].old_path, "C:\\before");
    assert_eq!(changes[0].new_path, "C:\\after");
    assert_eq!(store.totals("volume-A").unwrap(), (123, 123));
}

#[test]
fn incremental_folder_totals_follow_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let folder = |id, name: &str| Mutation::Upsert(Entry {
        id, parent: 5, name: name.into(), is_dir: true, logical: 0, allocated: 0,
        modified: 0, attributes: 0, links: vec![],
    });
    let nested = |id, parent, name: &str, size| {
        let mut entry = match file(id, name, size) { Mutation::Upsert(entry) => entry, _ => unreachable!() };
        entry.parent = parent;
        Mutation::Upsert(entry)
    };
    store.publish("C:\\", &[folder(20, "left"), folder(21, "right"), nested(30, 20, "file", 100)], &outcome(ScanMode::Full, 50)).unwrap();
    store.publish("C:\\", &[nested(30, 21, "file", 150), nested(31, 20, "new", 30)], &outcome(ScanMode::Incremental, 80)).unwrap();
    let roots = store.children("volume-A", 5).unwrap();
    assert_eq!(roots.iter().find(|item| item.id == 20).unwrap().allocated, 30);
    assert_eq!(roots.iter().find(|item| item.id == 21).unwrap().allocated, 150);
    store.publish("C:\\", &[Mutation::Delete(31), nested(30, 21, "file", 90)], &outcome(ScanMode::Incremental, 100)).unwrap();
    let roots = store.children("volume-A", 5).unwrap();
    assert_eq!(roots.iter().find(|item| item.id == 20).unwrap().allocated, 0);
    assert_eq!(roots.iter().find(|item| item.id == 21).unwrap().allocated, 90);
    assert_eq!(store.totals("volume-A").unwrap(), (90, 90));
}

#[test]
fn moving_directory_rebuilds_ancestor_totals() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let folder = |id, parent, name: &str| Mutation::Upsert(Entry {
        id, parent, name: name.into(), is_dir: true, logical: 0, allocated: 0,
        modified: 0, attributes: 0, links: vec![],
    });
    let mut child = match file(30, "child", 123) { Mutation::Upsert(entry) => entry, _ => unreachable!() };
    child.parent = 20;
    store.publish("C:\\", &[folder(20, 5, "moving"), folder(21, 5, "destination"), Mutation::Upsert(child)], &outcome(ScanMode::Full, 50)).unwrap();
    store.publish("C:\\", &[folder(20, 21, "moving")], &outcome(ScanMode::Incremental, 80)).unwrap();
    let root = store.children("volume-A", 5).unwrap();
    assert_eq!(root.iter().find(|item| item.id == 21).unwrap().allocated, 123);
    let destination = store.children("volume-A", 21).unwrap();
    assert_eq!(destination.iter().find(|item| item.id == 20).unwrap().allocated, 123);
}

#[test]
fn pruning_keeps_current_index_and_space_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    store.publish("C:\\", &[file(10, "file", 100)], &outcome(ScanMode::Full, 50)).unwrap();
    store.publish("C:\\", &[file(10, "file", 400)], &outcome(ScanMode::Incremental, 80)).unwrap();
    assert_eq!(store.scan_count("C:\\").unwrap(), 2);
    assert_eq!(store.scan_count("D:\\").unwrap(), 0);
    let remaining = store.prune(1).unwrap();
    assert!(remaining > 1);
    assert_eq!(store.reports().unwrap().len(), 2);
    assert_eq!(store.scan_count("C:\\").unwrap(), 2);
    assert!(store.reports().unwrap().iter().all(|report| !report.details && report.aggregate == 0));
    assert_eq!(store.totals("volume-A").unwrap(), (400, 400));
    let current = store.index_state("volume-A").unwrap().unwrap();
    assert_eq!((current.0, current.1, current.2), (400, 1_000_000, 800_000));
    assert_eq!(store.checkpoint("C:\\").unwrap().unwrap().next_usn, 80);
}

#[test]
fn deleting_selected_reports_keeps_the_incremental_index() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let first = store.publish("C:\\", &[file(10, "file", 100)], &outcome(ScanMode::Full, 50)).unwrap();
    let second = store.publish("C:\\", &[file(10, "file", 400)], &outcome(ScanMode::Incremental, 80)).unwrap();
    assert_eq!(store.delete_scans(&[second]).unwrap(), 1);
    assert!(store.changes(second).unwrap().is_empty());
    assert_eq!(store.delete_scans(&[first]).unwrap(), 1);
    assert!(store.reports().unwrap().is_empty());
    assert_eq!(store.totals("volume-A").unwrap(), (400, 400));
    assert_eq!(store.children("volume-A", 5).unwrap()[0].allocated, 400);
    assert_eq!(store.checkpoint("C:\\").unwrap().unwrap().next_usn, 80);
    let next = store.publish("C:\\", &[file(10, "file", 500)], &outcome(ScanMode::Incremental, 100)).unwrap();
    assert!(next > second);
    assert_eq!(store.reports().unwrap().len(), 1);
    assert_eq!(store.totals("volume-A").unwrap(), (500, 500));
}

#[test]
fn pruning_releases_free_database_pages() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let mut store = Store::open(&path).unwrap();
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE filler(data BLOB); WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<2000) INSERT INTO filler SELECT zeroblob(4096) FROM n; DELETE FROM filler;").unwrap();
    }
    let before = std::fs::metadata(&path).unwrap().len();
    assert!(before > 8 * 1024 * 1024);
    let after = store.prune(u64::MAX).unwrap();
    assert!(after < before / 2, "database stayed at {after} bytes after pruning {before} bytes");
}

#[test]
fn deleting_middle_scan_keeps_end_to_end_folder_growth() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let first = store.publish("C:\\", &[file(10, "file", 10)], &outcome(ScanMode::Full, 10)).unwrap();
    let middle = store.publish("C:\\", &[file(10, "file", 20)], &outcome(ScanMode::Incremental, 20)).unwrap();
    let last = store.publish("C:\\", &[file(10, "file", 22)], &outcome(ScanMode::Incremental, 30)).unwrap();
    assert_eq!(store.folder_growth("volume-A", first, last, 3).unwrap()[0].allocated_delta, 12);
    store.delete_scans(&[middle]).unwrap();
    assert_eq!(store.folder_growth("volume-A", first, last, 3).unwrap()[0].allocated_delta, 12);
    assert_eq!(store.reports().unwrap().len(), 2);
}

#[test]
fn deleting_latest_scan_carries_growth_to_next_scan() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let first = store.publish("C:\\", &[file(10, "file", 10)], &outcome(ScanMode::Full, 10)).unwrap();
    let latest = store.publish("C:\\", &[file(10, "file", 20)], &outcome(ScanMode::Incremental, 20)).unwrap();
    store.delete_scans(&[latest]).unwrap();
    let next = store.publish("C:\\", &[file(10, "file", 22)], &outcome(ScanMode::Incremental, 30)).unwrap();
    assert!(next > latest);
    assert_eq!(store.folder_growth("volume-A", first, next, 3).unwrap()[0].allocated_delta, 12);
    assert_eq!(store.totals("volume-A").unwrap(), (22, 22));
}

#[test]
fn moved_file_changes_source_and_destination_without_disk_growth() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let folder = |id, name: &str| Mutation::Upsert(Entry { id, parent: 5, name: name.into(),
        is_dir: true, logical: 0, allocated: 0, modified: 0, attributes: 0, links: vec![] });
    let mut original = match file(10, "data", 100) { Mutation::Upsert(e) => e, _ => unreachable!() };
    original.parent = 20;
    let first = store.publish("C:\\", &[folder(20, "left"), folder(21, "right"), Mutation::Upsert(original.clone())],
        &outcome(ScanMode::Full, 10)).unwrap();
    original.parent = 21;
    let second = store.publish("C:\\", &[Mutation::Upsert(original)], &outcome(ScanMode::Incremental, 20)).unwrap();
    let growth = store.folder_growth("volume-A", first, second, 1).unwrap();
    assert_eq!(growth.len(), 2);
    assert_eq!(growth.iter().map(|item| item.allocated_delta).sum::<i128>(), 0);
    assert_eq!(growth.iter().map(|item| item.moved_delta.unsigned_abs()).sum::<u128>(), 200);
}

#[test]
fn clearing_details_preserves_folder_growth_and_scan_points() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let first = store.publish("C:\\", &[file(10, "file", 10)], &outcome(ScanMode::Full, 10)).unwrap();
    let second = store.publish("C:\\", &[file(10, "file", 20)], &outcome(ScanMode::Incremental, 20)).unwrap();
    assert_eq!(store.clear_details(&[second]).unwrap(), 1);
    assert!(store.changes(second).unwrap().is_empty());
    assert_eq!(store.folder_growth("volume-A", first, second, 1).unwrap()[0].allocated_delta, 10);
    assert_eq!(store.reports().unwrap().len(), 2);
}

#[test]
fn moving_directory_with_child_resize_counts_each_side_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let folder = |name: &str| Mutation::Upsert(Entry { id: 20, parent: 5, name: name.into(),
        is_dir: true, logical: 0, allocated: 0, modified: 0, attributes: 0, links: vec![] });
    let child = |size| {
        let mut entry = match file(30, "child", size) { Mutation::Upsert(e) => e, _ => unreachable!() };
        entry.parent = 20;
        Mutation::Upsert(entry)
    };
    let first = store.publish("C:\\", &[folder("before"), child(100)], &outcome(ScanMode::Full, 10)).unwrap();
    let second = store.publish("C:\\", &[folder("after"), child(150)], &outcome(ScanMode::Incremental, 20)).unwrap();
    let growth = store.folder_growth("volume-A", first, second, 1).unwrap();
    assert_eq!(growth.len(), 2);
    assert_eq!(growth.iter().find(|item| item.path == "C:\\before").unwrap().allocated_delta, -100);
    assert_eq!(growth.iter().find(|item| item.path == "C:\\after").unwrap().allocated_delta, 150);
    assert_eq!(growth.iter().map(|item| item.allocated_delta).sum::<i128>(), 50);
}

#[test]
fn legacy_migration_keeps_summary_and_creates_consistent_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE scans(id INTEGER PRIMARY KEY,root TEXT,volume TEXT,finished TEXT,mode TEXT,
                total INTEGER,free INTEGER,logical INTEGER,allocated INTEGER,file_count INTEGER,warnings TEXT);
            CREATE TABLE changes(scan INTEGER,old_path TEXT,new_path TEXT,logical_delta INTEGER,
                allocated_delta INTEGER,kind TEXT);
            INSERT INTO scans VALUES(1,'C:\\','old-volume','2026-01-01T00:00:00+00:00','full',1000,800,100,100,1,'[]');
            INSERT INTO changes VALUES(1,'','C:\\cache\\file',10,10,'created');") .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.reports().unwrap().len(), 1);
    assert_eq!(store.reports().unwrap()[0].aggregate, 1);
    let backup = std::fs::read_dir(dir.path()).unwrap().map(|item| item.unwrap().path())
        .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("history-before-v2-"))
        .expect("migration backup");
    let old = rusqlite::Connection::open(backup).unwrap();
    let count: i64 = old.query_row("SELECT COUNT(*) FROM changes", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn read_only_queries_use_current_index_without_scanning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let mut store = Store::open(&path).unwrap();
    let folder = Mutation::Upsert(Entry { id: 20, parent: 5, name: "cache".into(),
        is_dir: true, logical: 0, allocated: 0, modified: 0, attributes: 0, links: vec![] });
    let mut entry = match file(30, "one.bin", 123) { Mutation::Upsert(e) => e, _ => unreachable!() };
    entry.parent = 20;
    let first = store.publish("C:\\", &[folder, Mutation::Upsert(entry)], &outcome(ScanMode::Full, 10)).unwrap();
    drop(store);
    let store = Store::open_read_only(&path).unwrap();
    assert_eq!(store.resolve_folder("volume-A", 5, "C:\\cache").unwrap(), 20);
    assert_eq!(store.children_page("volume-A", 20, 50, 0).unwrap()[0].allocated, 123);
    assert_eq!(store.largest("volume-A", "C:\\", 5, true, 50, 0).unwrap()[0].path, "C:\\cache");
    assert_eq!(store.largest("volume-A", "C:\\", 5, false, 50, 0).unwrap()[0].path, "C:\\cache\\one.bin");
    assert_eq!(store.coverage("volume-A", 0, first).unwrap(), (1, 1, 1, 1));
}

#[test]
fn production_schema_migrates_in_isolated_copy_when_requested() {
    let Ok(source) = std::env::var("DISKHISTORY_MIGRATION_SOURCE") else { return; };
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("history.db");
    let old = rusqlite::Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let expected_scans: i64 = old.query_row("SELECT COUNT(*) FROM scans", [], |r| r.get(0)).unwrap();
    let expected_nodes: i64 = old.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0)).unwrap();
    let expected_reports = {
        let mut stmt = old.prepare("SELECT id,allocated,free,file_count FROM scans ORDER BY id DESC").unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u64,
            r.get::<_, i64>(2)? as u64, r.get::<_, i64>(3)? as u64))).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap()
    };
    old.backup(rusqlite::MAIN_DB, &copy, None).unwrap();
    drop(old);
    let store = Store::open(&copy).unwrap();
    let reports = store.reports().unwrap();
    assert!(!reports.is_empty());
    assert_eq!(reports.len() as i64, expected_scans);
    assert_eq!(reports.iter().map(|report| (report.id, report.allocated, report.free, report.file_count))
        .collect::<Vec<_>>(), expected_reports);
    let migrated = rusqlite::Connection::open_with_flags(&copy, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(migrated.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get::<_, i64>(0)).unwrap(), expected_nodes);
    assert!(reports.iter().all(|item| !item.sampled.is_empty()));
    let newest = &reports[0];
    assert!(store.totals(&newest.volume).unwrap().1 > 0);
    assert!(store.used_bytes().unwrap() > 0);
}

#[test]
fn cancellation_during_commit_rolls_back_index_and_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let stop = dir.path().join("stop.flag");
    let mut store = Store::open(&path).unwrap();
    store.publish("C:\\", &[file(10, "file", 10)], &outcome(ScanMode::Full, 10)).unwrap();
    store.set_cancel_path(stop.clone());
    store.begin_stage().unwrap();
    store.stage(file(10, "file", 100)).unwrap();
    std::fs::write(&stop, b"stop").unwrap();
    assert!(store.commit_stage("C:\\", &outcome(ScanMode::Incremental, 20)).is_err());
    assert_eq!(store.totals("volume-A").unwrap(), (10, 10));
    assert_eq!(store.checkpoint("C:\\").unwrap().unwrap().next_usn, 10);
}

#[test]
fn unreadable_record_stays_indexed_until_a_later_scan_retries_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let mut store = Store::open(&path).unwrap();
    store.publish("C:\\", &[file(10, "file", 100)], &outcome(ScanMode::Full, 10)).unwrap();
    let mut deferred = outcome(ScanMode::Incremental, 20);
    deferred.pending = vec![10];
    store.publish("C:\\", &[], &deferred).unwrap();
    assert_eq!(store.totals("volume-A").unwrap(), (100, 100));
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.pending("C:\\").unwrap(), vec![10]);
    store.publish("C:\\", &[file(10, "file", 150)], &outcome(ScanMode::Incremental, 30)).unwrap();
    assert!(store.pending("C:\\").unwrap().is_empty());
    assert_eq!(store.totals("volume-A").unwrap(), (150, 150));
}

#[test]
fn full_scan_keeps_a_deferred_record_and_deletes_a_confirmed_missing_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    store.publish("C:\\", &[file(10, "deferred", 100), file(11, "removed", 50)],
        &outcome(ScanMode::Full, 10)).unwrap();
    let mut scan = outcome(ScanMode::Full, 20);
    scan.pending = vec![10];
    store.publish("C:\\", &[file(10, "deferred", 100), Mutation::Delete(11)], &scan).unwrap();
    assert_eq!(store.totals("volume-A").unwrap(), (100, 100));
    assert_eq!(store.pending("C:\\").unwrap(), vec![10]);
    assert!(store.indexed_entry("volume-A", 10).unwrap().is_some());
    assert!(store.indexed_entry("volume-A", 11).unwrap().is_none());
}

#[test]
fn folder_breakdown_includes_direct_files_children_and_deleted_paths() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("history.db")).unwrap();
    let folder = |id, parent, name: &str| Mutation::Upsert(Entry {
        id, parent, name: name.into(), is_dir: true, logical: 0, allocated: 0,
        modified: 0, attributes: 0, links: vec![],
    });
    let nested = |id, parent, size| {
        let mut entry = match file(id, "one.bin", size) { Mutation::Upsert(entry) => entry, _ => unreachable!() };
        entry.parent = parent;
        Mutation::Upsert(entry)
    };
    let first = store.publish("C:\\", &[folder(20, 5, "cache"), folder(21, 20, "sub"),
        folder(22, 5, "cache-other"), nested(30, 20, 100), nested(31, 21, 200), nested(32, 22, 500)],
        &outcome(ScanMode::Full, 10)).unwrap();
    let second = store.publish("C:\\", &[nested(30, 20, 150), nested(31, 21, 230), nested(32, 22, 900)],
        &outcome(ScanMode::Incremental, 20)).unwrap();
    let breakdown = store.folder_breakdown("volume-A", first, second, "C:\\CACHE\\").unwrap();
    assert_eq!((breakdown.allocated_delta, breakdown.direct_delta), (80, 50));
    assert_eq!(breakdown.children.len(), 1);
    assert_eq!(breakdown.children[0].path, "C:\\cache\\sub");
    assert_eq!(breakdown.children[0].allocated_delta, 30);
    let root = store.folder_breakdown("volume-A", first, second, "C:\\").unwrap();
    assert_eq!(root.allocated_delta, 480);
    let removed = store.publish("C:\\", &[Mutation::Delete(30), Mutation::Delete(31),
        Mutation::Delete(21), Mutation::Delete(20)], &outcome(ScanMode::Incremental, 30)).unwrap();
    assert_eq!(store.find_folder("volume-A", 5, "C:\\cache").unwrap(), None);
    let historical = store.folder_breakdown("volume-A", second, removed, "C:\\cache").unwrap();
    assert_eq!((historical.allocated_delta, historical.direct_delta), (-380, -150));
}

#[test]
fn history_budget_excludes_current_index_timeline_and_migration_backups() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let mut store = Store::open(&path).unwrap();
    store.publish("C:\\", &[file(10, "file", 100)], &outcome(ScanMode::Full, 10)).unwrap();
    let scan = store.publish("C:\\", &[file(10, "file", 200)], &outcome(ScanMode::Incremental, 20)).unwrap();
    let performance = ScanPerformance { duration_ms: 1200, cpu_time_ms: Some(30),
        process_peak_working_set_bytes: Some(20 * 1024 * 1024),
        counters: ScanCounters { journal_records: 100, unique_changed_files: 1, entry_reads: 1 },
        ..Default::default() };
    store.save_performance(scan, &performance).unwrap();
    std::fs::write(dir.path().join("history-before-v2-test.db"), vec![0u8; 1024]).unwrap();
    let history_budget = store.history_bytes().unwrap();
    let physical = store.prune(history_budget).unwrap();
    assert!(physical > history_budget);
    assert!(store.reports().unwrap().iter().all(|report| report.details && report.aggregate == 2));
    let usage = store.storage_usage().unwrap();
    assert_eq!(usage.history_bytes, history_budget);
    assert_eq!(usage.backup_bytes, 1024);
    assert!(usage.index_bytes > 0 && usage.timeline_bytes > 0);
    store.prune(1).unwrap();
    assert_eq!(store.reports().unwrap()[0].performance.as_ref().unwrap().counters.journal_records, 100);
    assert_eq!(store.totals("volume-A").unwrap(), (200, 200));
    drop(store);
    let store = Store::open_read_only(&path).unwrap();
    assert_eq!(store.reports().unwrap()[0].performance.as_ref().unwrap().duration_ms, 1200);
}

#[test]
fn production_increment_replay_on_isolated_copy_when_requested() {
    let Ok(source) = std::env::var("DISKHISTORY_BENCH_SOURCE") else { return; };
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("history.db");
    let old = rusqlite::Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    old.backup(rusqlite::MAIN_DB, &copy, None).unwrap();
    drop(old);
    let mut store = Store::open(&copy).unwrap();
    let mut cp = store.checkpoint("C:\\").unwrap().unwrap();
    let db = rusqlite::Connection::open_with_flags(&copy, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut stmt = db.prepare("SELECT payload FROM nodes WHERE volume=? AND is_dir=0 LIMIT 14000").unwrap();
    let entries = stmt.query_map([&cp.volume_id], |r| r.get::<_, String>(0)).unwrap()
        .map(|entry| {
            let mut entry: Entry = serde_json::from_str(&entry.unwrap()).unwrap();
            entry.modified += 1;
            entry.logical += 4096;
            entry.allocated += 4096;
            Mutation::Upsert(entry)
        }).collect::<Vec<_>>();
    drop(stmt);
    drop(db);
    assert!(entries.len() > 10000);
    cp.next_usn += 1;
    let mut sample = outcome(ScanMode::Incremental, cp.next_usn);
    sample.checkpoint = cp;
    store.begin_stage().unwrap();
    for entry in entries { store.stage(entry).unwrap(); }
    let start = std::time::Instant::now();
    store.commit_stage("C:\\", &sample).unwrap();
    println!("isolated commit of 14000 resized records: {:.2}s", start.elapsed().as_secs_f64());
}
