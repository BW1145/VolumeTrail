//! History queries resolve path IDs only after filtering and pagination. Legacy
//! read-only databases use the same public operations with text path references.
use super::*;
use rusqlite::{Row, types::Value, params_from_iter};

fn interval(volume: &str, from: i64, to: i64) -> Vec<Value> {
    vec![volume.to_owned().into(), from.into(), to.into()]
}

fn change(row: &Row<'_>, paths: &mut paths::Reader<'_>) -> Result<Change> {
    Ok(Change { old_path: paths.read(row, 0)?, new_path: paths.read(row, 1)?,
        logical_delta: row.get(2)?, allocated_delta: row.get(3)?, kind: row.get(4)? })
}

impl Store {
    fn detail_filter(&self, path: Option<&str>, values: &mut Vec<Value>) -> Result<String> {
        let Some(path) = path else { return Ok(String::new()); };
        let path = normalize_folder_path(path)?;
        if self.shared_paths {
            paths::select_folder(&self.db, &path, false)?;
            Ok(" AND (c.old_path IN history_path_filter OR c.new_path IN history_path_filter)".into())
        } else {
            values.push(folder_prefix(&path).into());
            Ok(" AND (substr(c.old_path,1,length(?4))=?4 COLLATE NOCASE
                OR substr(c.new_path,1,length(?4))=?4 COLLATE NOCASE)".into())
        }
    }

    fn summary_column(&self) -> &'static str { if self.shared_paths { "path_id" } else { "path" } }

    fn summary_filter(&self, alias: &str, path: &str, values: &mut Vec<Value>) -> Result<String> {
        if self.shared_paths {
            paths::select_folder(&self.db, path, true)?;
            Ok(format!("{alias}.path_id IN history_path_filter"))
        } else {
            values.extend([path.to_owned().into(), folder_prefix(path).into(), (path.len() as i64).into()]);
            Ok(format!("({alias}.path=?4 COLLATE NOCASE
                OR substr({alias}.path,1,length(?5))=?5 COLLATE NOCASE
                OR (?6=3 AND {alias}.path LIKE '[unresolved #%'))"))
        }
    }

    pub fn changes(&self, scan: i64) -> Result<Vec<Change>> {
        let mut stmt = self.db.prepare("SELECT old_path,new_path,logical_delta,allocated_delta,kind
            FROM change_history WHERE scan=?
            ORDER BY ABS(allocated_delta) DESC,ABS(logical_delta) DESC,seq LIMIT 2000")?;
        let mut rows = stmt.query([scan])?;
        let mut reader = paths::Reader::new(&self.db);
        let mut result = Vec::new();
        while let Some(row) = rows.next()? { result.push(change(row, &mut reader)?); }
        Ok(result)
    }

    pub fn changes_between(&self, volume: &str, from: i64, to: i64) -> Result<Vec<Change>> {
        self.changes_between_page(volume, from, to, 2000, 0)
    }

    pub fn changes_between_page(&self, volume: &str, from: i64, to: i64,
        limit: u64, offset: u64) -> Result<Vec<Change>> {
        self.detail_page(volume, from, to, None, limit, offset)
    }

    pub fn changes_between_path(&self, volume: &str, from: i64, to: i64,
        path: &str, limit: u64, offset: u64) -> Result<Vec<Change>> {
        self.detail_page(volume, from, to, Some(path), limit, offset)
    }

    fn detail_page(&self, volume: &str, from: i64, to: i64, path: Option<&str>,
        limit: u64, offset: u64) -> Result<Vec<Change>> {
        let mut values = interval(volume, from, to);
        let filter = self.detail_filter(path, &mut values)?;
        values.extend([(limit.min(5000) as i64).into(), i64::try_from(offset)?.into()]);
        let mut stmt = self.db.prepare(&format!(
            "SELECT c.old_path,c.new_path,c.logical_delta,c.allocated_delta,c.kind
            FROM change_history c JOIN scans s ON s.id=c.scan
            WHERE s.volume=?1 AND c.scan>?2 AND c.scan<=?3 {filter}
            ORDER BY ABS(c.allocated_delta) DESC,ABS(c.logical_delta) DESC,c.scan,c.seq LIMIT ? OFFSET ?"))?;
        let mut rows = stmt.query(params_from_iter(values))?;
        let mut reader = paths::Reader::new(&self.db);
        let mut result = Vec::new();
        while let Some(row) = rows.next()? { result.push(change(row, &mut reader)?); }
        Ok(result)
    }

    pub fn changes_count(&self, volume: &str, from: i64, to: i64, path: Option<&str>) -> Result<u64> {
        let mut values = interval(volume, from, to);
        let sql = if path.is_some() {
            let filter = self.detail_filter(path, &mut values)?;
            format!("SELECT COUNT(*) FROM change_history c JOIN scans s ON s.id=c.scan
                WHERE s.volume=?1 AND c.scan>?2 AND c.scan<=?3 {filter}")
        } else {
            "SELECT COALESCE(SUM(c.entries),0) FROM change_counts c JOIN scans s ON s.id=c.scan
                WHERE s.volume=?1 AND c.scan>?2 AND c.scan<=?3".into()
        };
        Ok(self.db.query_row(&sql, params_from_iter(values), |r| r.get(0))?)
    }

    pub fn extensions_between_path(&self, volume: &str, from: i64, to: i64,
        path: &str) -> Result<ExtensionBreakdown> {
        let path = normalize_folder_path(path)?;
        let version: i64 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let intervals = self.db.query_row("SELECT COUNT(*) FROM scans WHERE volume=? AND id>? AND id<=?",
            params![volume, from, to], |r| r.get(0))?;
        let mut complete_intervals = 0;
        let mut grouped: HashMap<String, (i128, u64)> = HashMap::new();
        if version >= 7 {
            complete_intervals = self.db.query_row("SELECT COUNT(*) FROM scans
                WHERE volume=? AND id>? AND id<=? AND extensions_complete=1",
                params![volume, from, to], |r| r.get(0))?;
            let mut values = interval(volume, from, to);
            let filter = self.summary_filter("e", &path, &mut values)?;
            let mut stmt = self.db.prepare(&format!("SELECT e.extension,SUM(e.delta),SUM(e.events)
                FROM extension_changes e JOIN scans s ON e.scan=s.id
                WHERE s.volume=?1 AND e.scan>?2 AND e.scan<=?3 AND {filter} GROUP BY e.extension"))?;
            let mut rows = stmt.query(params_from_iter(values))?;
            while let Some(row) = rows.next()? {
                grouped.insert(row.get(0)?, (i128::from(row.get::<_, i64>(1)?), row.get::<_, i64>(2)? as u64));
            }
        }
        let mut values = interval(volume, from, to);
        let filter = self.detail_filter(Some(&path), &mut values)?;
        let legacy = if version >= 7 { " AND s.extensions_complete=0" } else { "" };
        let mut stmt = self.db.prepare(&format!("SELECT c.old_path,c.new_path,c.allocated_delta
            FROM change_history c JOIN scans s ON s.id=c.scan
            WHERE s.volume=?1 AND c.scan>?2 AND c.scan<=?3 AND c.allocated_delta<>0 {filter}{legacy}"))?;
        let mut rows = stmt.query(params_from_iter(values))?;
        let mut reader = paths::Reader::new(&self.db);
        while let Some(row) = rows.next()? {
            let delta: i64 = row.get(2)?;
            let path = reader.read(row, if delta > 0 { 1 } else { 0 })?;
            let name = path.rsplit('\\').next().unwrap_or("");
            let extension = name.rsplit_once('.').map(|(_, ext)| format!(".{}", ext.to_ascii_lowercase()))
                .unwrap_or_else(|| "(无扩展名)".into());
            let entry = grouped.entry(extension).or_default();
            entry.0 += i128::from(delta); entry.1 += 1;
        }
        let mut items = grouped.into_iter().map(|(extension, (delta, count))| (extension, delta, count)).collect::<Vec<_>>();
        items.sort_by(|a, b| b.1.abs().cmp(&a.1.abs()));
        Ok(ExtensionBreakdown { items, intervals, complete_intervals })
    }

    pub fn folder_growth(&self, volume: &str, from: i64, to: i64, depth: usize) -> Result<Vec<FolderGrowth>> {
        let mut stmt = self.db.prepare(&format!("SELECT f.{},f.allocated_delta,f.moved_delta,f.events
            FROM folder_changes f JOIN scans s ON s.id=f.scan
            WHERE s.volume=? AND f.scan>? AND f.scan<=?", self.summary_column()))?;
        let mut rows = stmt.query(params![volume, from, to])?;
        let mut reader = paths::Reader::new(&self.db);
        let mut grouped = GrowthDeltas::new();
        while let Some(row) = rows.next()? {
            if let Some(folder) = folder_bucket_dir(&reader.read(row, 0)?, depth) {
                growth_entry(&mut grouped, folder, i128::from(row.get::<_, i64>(1)?),
                    i128::from(row.get::<_, i64>(2)?), row.get::<_, i64>(3)? as u64);
            }
        }
        let mut result = grouped.into_iter().filter(|(path, (delta, _, _))| *delta != 0 || path == "[unresolved]")
            .map(|(path, (allocated_delta, moved_delta, changes))| FolderGrowth { path, allocated_delta, moved_delta, changes })
            .collect::<Vec<_>>();
        result.sort_by(|a, b| b.allocated_delta.cmp(&a.allocated_delta).then_with(|| a.path.cmp(&b.path)));
        Ok(result)
    }

    pub fn folder_breakdown(&self, volume: &str, from: i64, to: i64, path: &str) -> Result<FolderBreakdown> {
        let path = normalize_folder_path(path)?;
        let depth = path[3..].split('\\').filter(|part| !part.is_empty()).count() + 1;
        let mut values = interval(volume, from, to);
        let filter = self.summary_filter("f", &path, &mut values)?;
        let mut stmt = self.db.prepare(&format!("SELECT f.{},f.allocated_delta,f.moved_delta,f.events
            FROM folder_changes f JOIN scans s ON s.id=f.scan
            WHERE s.volume=?1 AND f.scan>?2 AND f.scan<=?3 AND {filter}", self.summary_column()))?;
        let mut rows = stmt.query(params_from_iter(values))?;
        let mut reader = paths::Reader::new(&self.db);
        let mut result = FolderBreakdown::default();
        let mut children = GrowthDeltas::new();
        while let Some(row) = rows.next()? {
            let folder = reader.read(row, 0)?;
            let delta = i128::from(row.get::<_, i64>(1)?);
            let moved = i128::from(row.get::<_, i64>(2)?);
            result.allocated_delta += delta; result.moved_delta += moved;
            if folder.eq_ignore_ascii_case(&path) { result.direct_delta += delta; }
            else if let Some(child) = folder_bucket_dir(&folder, depth) {
                growth_entry(&mut children, child, delta, moved, row.get::<_, i64>(3)? as u64);
            }
        }
        result.children = children.into_iter().filter(|(_, (delta, _, _))| *delta != 0)
            .map(|(path, (allocated_delta, moved_delta, changes))| FolderGrowth { path, allocated_delta, moved_delta, changes }).collect();
        result.children.sort_by(|a, b| b.allocated_delta.cmp(&a.allocated_delta).then_with(|| a.path.cmp(&b.path)));
        Ok(result)
    }
}

pub(super) fn write_growth(db: &Connection, paths: &mut paths::Writer<'_>, scan: i64, growth: &GrowthDeltas) -> Result<()> {
    let mut stmt = db.prepare_cached("INSERT INTO folder_changes VALUES(?,?,?,?,?)
        ON CONFLICT(scan,path_id) DO UPDATE SET allocated_delta=allocated_delta+excluded.allocated_delta,
        moved_delta=moved_delta+excluded.moved_delta,events=events+excluded.events")?;
    for (path, (delta, moved, events)) in growth {
        if *delta != 0 || *moved != 0 {
            stmt.execute(params![scan, paths.intern(path)?, i64::try_from(*delta)?, i64::try_from(*moved)?, i64::try_from(*events)?])?;
        }
    }
    Ok(())
}
