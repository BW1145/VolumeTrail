use super::*;

impl Store {
    pub fn children_count(&self, volume: &str, parent: u64) -> Result<u64> {
        Ok(self.db.query_row("SELECT COUNT(*) FROM nodes WHERE volume=? AND parent=? AND id<>parent",
            params![volume,parent as i64],|r| r.get(0))?)
    }

    pub fn largest_count(&self, volume: &str, root_id: u64, directories: bool) -> Result<u64> {
        let sql = if directories { "SELECT COUNT(*) FROM folders f JOIN nodes n ON n.volume=f.volume AND n.id=f.id WHERE f.volume=? AND n.is_dir=1 AND n.id<>?" }
            else { "SELECT COUNT(*) FROM nodes WHERE volume=? AND is_dir=0 AND id<>?" };
        Ok(self.db.query_row(sql,params![volume,root_id as i64],|r|r.get(0))?)
    }
    // Metadata lives with its physical scan, so pruning cannot turn a verification
    // point into an ordinary scan. Missing baselines stay missing.
    pub(super) fn migrate_operations(db: &Connection) -> Result<()> {
        if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? < 8 {
            db.execute_batch("BEGIN IMMEDIATE;
                ALTER TABLE scans ADD COLUMN operation_pre INTEGER;
                ALTER TABLE scans ADD COLUMN verification TEXT NOT NULL DEFAULT 'not_needed';
                PRAGMA user_version=8; COMMIT;")?;
        }
        Ok(())
    }

    pub fn operation_pre(&self, scan: i64) -> Result<i64> {
        let version: i64 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 8 { return Ok(scan); }
        Ok(self.db.query_row("SELECT COALESCE(operation_pre,id) FROM scans WHERE id=?", [scan], |r| r.get(0))?)
    }

    pub fn verification(&self, pre: i64) -> Result<String> {
        let version: i64 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 8 { return Ok("not_needed".into()); }
        Ok(self.db.query_row("SELECT verification FROM scans WHERE id=?", [pre], |r| r.get(0))
            .optional()?.unwrap_or_else(|| "baseline_missing".into()))
    }

    pub fn mark_verification(&self, pre: i64, state: &str, post: Option<i64>) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        tx.execute("UPDATE scans SET verification=? WHERE id=?", params![state,pre])?;
        if let Some(post) = post {
            tx.execute("UPDATE scans SET operation_pre=?,verification='verified' WHERE id=?", params![pre,post])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn default_interval(&self, reports: &[Report]) -> Result<Option<(i64, i64)>> {
        let Some(latest) = reports.first() else { return Ok(None); };
        let pre = self.operation_pre(latest.id)?;
        if !reports.iter().any(|r| r.id == pre) { return Ok(None); }
        Ok(reports.iter().find(|r| r.id < pre && r.volume == latest.volume).map(|r| (r.id, pre)))
    }
}
