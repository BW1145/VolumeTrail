//! Exact, case-preserving path IDs. Caches belong to a single operation so a
//! rolled-back transaction or another connection cannot leave stale IDs behind.
use super::*;
use rusqlite::{Row, types::ValueRef};

pub(super) struct Writer<'a> {
    db: &'a Connection,
    ids: HashMap<String, i64>,
}

impl<'a> Writer<'a> {
    pub fn new(db: &'a Connection) -> Self {
        Self { db, ids: HashMap::from([(String::new(), 0)]) }
    }

    pub fn intern(&mut self, path: &str) -> Result<i64> {
        // Keep the separator in the fragment: concatenation preserves drive
        // roots, empty components, case-only renames and unresolved names exactly.
        let mut missing = Vec::new();
        let mut remaining = path;
        while !self.ids.contains_key(remaining) {
            let split = remaining.rfind('\\').unwrap_or(0);
            missing.push((remaining, &remaining[split..]));
            remaining = &remaining[..split];
        }
        let mut parent = self.ids[remaining];
        for (path, fragment) in missing.into_iter().rev() {
            let existing = self.db.prepare_cached(
                "SELECT id FROM history_paths WHERE parent=? AND fragment=?")?
                .query_row(params![parent, fragment], |r| r.get(0)).optional()?;
            let id = match existing {
                Some(id) => id,
                None => self.db.prepare_cached(
                    "INSERT INTO history_paths(parent,fragment) VALUES(?,?) RETURNING id")?
                    .query_row(params![parent, fragment], |r| r.get(0))?,
            };
            self.ids.insert(path.to_owned(), id);
            parent = id;
        }
        Ok(parent)
    }
}

pub(super) struct Reader<'a> {
    db: &'a Connection,
    paths: HashMap<i64, String>,
}

impl<'a> Reader<'a> {
    pub fn new(db: &'a Connection) -> Self {
        Self { db, paths: HashMap::from([(0, String::new())]) }
    }

    pub fn resolve(&mut self, id: i64) -> Result<String> {
        if let Some(path) = self.paths.get(&id) { return Ok(path.clone()); }
        // Iterative traversal also handles deep paths without growing the stack.
        let mut pending = Vec::new();
        let mut cursor = id;
        while !self.paths.contains_key(&cursor) {
            let (parent, fragment): (i64, String) = self.db.prepare_cached(
                "SELECT parent,fragment FROM history_paths WHERE id=?")?
                .query_row([cursor], |r| Ok((r.get(0)?, r.get(1)?)))
                .with_context(|| format!("Missing history path {cursor}"))?;
            ensure!(parent >= 0 && parent < cursor, "Invalid history path parent for {cursor}");
            pending.push((cursor, fragment));
            cursor = parent;
        }
        let mut path = self.paths[&cursor].clone();
        for (id, fragment) in pending.into_iter().rev() {
            path.push_str(&fragment);
            self.paths.insert(id, path.clone());
        }
        Ok(path)
    }

    /// Old read-only databases expose text; schema 14 exposes IDs in these views.
    pub fn read(&mut self, row: &Row<'_>, column: usize) -> Result<String> {
        match row.get_ref(column)? {
            ValueRef::Integer(id) => self.resolve(id),
            ValueRef::Text(_) => Ok(row.get(column)?),
            _ => anyhow::bail!("Invalid history path value"),
        }
    }
}

/// Populate one connection-local filter. Rebuild on each query so new scans and
/// independent writers are visible without cache invalidation state.
pub(super) fn select_folder(db: &Connection, path: &str, include_self: bool) -> Result<()> {
    db.execute_batch("CREATE TEMP TABLE IF NOT EXISTS history_path_filter(id INTEGER PRIMARY KEY);
        DELETE FROM history_path_filter;")?;
    let base = path.trim_end_matches('\\');
    let mut fragments = Vec::new();
    let mut remaining = base;
    while !remaining.is_empty() {
        let split = remaining.rfind('\\').unwrap_or(0);
        fragments.push(&remaining[split..]);
        remaining = &remaining[..split];
    }
    let mut parents = vec![0i64];
    for fragment in fragments.into_iter().rev() {
        let mut next = Vec::new();
        for parent in parents {
            let mut stmt = db.prepare_cached("SELECT id FROM history_paths
                WHERE parent=? AND fragment=? COLLATE NOCASE AND id<>parent")?;
            next.extend(stmt.query_map(params![parent, fragment], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?);
        }
        parents = next;
    }
    for parent in parents {
        db.execute("WITH RECURSIVE descendants(id) AS (
            SELECT id FROM history_paths WHERE parent=? AND id<>parent UNION ALL
            SELECT n.id FROM descendants d CROSS JOIN history_paths n
                WHERE n.parent=d.id AND n.id<>n.parent)
            INSERT OR IGNORE INTO history_path_filter SELECT id FROM descendants", [parent])?;
        if include_self && path.len() > 3 {
            db.execute("INSERT OR IGNORE INTO history_path_filter VALUES(?)", [parent])?;
        }
    }
    if include_self && path.len() == 3 {
        db.execute("INSERT OR IGNORE INTO history_path_filter
            SELECT id FROM history_paths WHERE parent=0 AND fragment LIKE '[unresolved #%'", [])?;
    }
    Ok(())
}

/// Explicit history deletion also releases paths used only by the deleted rows.
/// Archive references live inside compressed blocks, so mark them before sweeping.
pub(super) fn collect_unused(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TEMP TABLE IF NOT EXISTS retained_history_paths(id INTEGER PRIMARY KEY);
        DELETE FROM retained_history_paths;
        INSERT INTO retained_history_paths VALUES(0);
        INSERT OR IGNORE INTO retained_history_paths SELECT old_path_id FROM changes;
        INSERT OR IGNORE INTO retained_history_paths SELECT new_path_id FROM changes;
        INSERT OR IGNORE INTO retained_history_paths SELECT path_id FROM folder_changes;
        INSERT OR IGNORE INTO retained_history_paths SELECT path_id FROM extension_changes;
        INSERT OR IGNORE INTO retained_history_paths SELECT path_id FROM carry_folders;
        INSERT OR IGNORE INTO retained_history_paths SELECT path_id FROM carry_extensions;")?;
    let mut stmt = db.prepare("SELECT data FROM change_archives")?;
    for data in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
        for row in compact::decode_ids(&data?)? {
            db.prepare_cached("INSERT OR IGNORE INTO retained_history_paths VALUES(?),(?)")?
                .execute(params![row.1, row.2])?;
        }
    }
    db.execute_batch("WITH RECURSIVE ancestors(id) AS (
        SELECT id FROM retained_history_paths UNION
        SELECT p.parent FROM ancestors a CROSS JOIN history_paths p WHERE p.id=a.id)
        INSERT OR IGNORE INTO retained_history_paths SELECT id FROM ancestors;
        DELETE FROM history_paths WHERE id NOT IN (SELECT id FROM retained_history_paths);
        DELETE FROM retained_history_paths;")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_paths_reuse_parents_and_survive_writer_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("history.db")).unwrap();
        let paths = ["", "C:\\", "C:\\目录 😀\\File", "C:\\目录 😀\\file", "[unresolved #42]", "\\\\server\\share"];
        let ids = {
            let tx = store.db.transaction().unwrap();
            let mut writer = Writer::new(&tx);
            let ids = paths.iter().map(|p| writer.intern(p).unwrap()).collect::<Vec<_>>();
            assert_eq!(writer.intern(paths[2]).unwrap(), ids[2]);
            assert_ne!(ids[2], ids[3]);
            drop(writer); tx.commit().unwrap(); ids
        };
        let mut reader = Reader::new(&store.db);
        for (id, path) in ids.into_iter().zip(paths) { assert_eq!(reader.resolve(id).unwrap(), path); }
        store.db.execute_batch("BEGIN").unwrap();
        Writer::new(&store.db).intern("C:\\rollback").unwrap();
        store.db.execute_batch("ROLLBACK").unwrap();
        let id = Writer::new(&store.db).intern("C:\\different").unwrap();
        assert_eq!(Reader::new(&store.db).resolve(id).unwrap(), "C:\\different");
    }
}
