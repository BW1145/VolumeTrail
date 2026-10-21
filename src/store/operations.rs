use super::*;

impl Store {
    pub(crate) fn source_directory(&self)->Option<&Path> {self.path.parent()}
    pub(crate) fn verification_context(&mut self,context:Option<(Option<i64>,String)>) {self.verification_context=context;}
    pub fn latest_scan_id(&self,volume:&str)->Result<Option<i64>> {
        Ok(self.db.query_row("SELECT last_scan FROM scan_baselines WHERE volume=?",[volume],|r|r.get(0)).optional()?.flatten())
    }

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

    pub(super) fn migrate_baselines(db:&Connection)->Result<()> {
        if db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?<10 {
            db.execute_batch("BEGIN IMMEDIATE;
                ALTER TABLE scans ADD COLUMN comparison_from INTEGER;
                CREATE TABLE scan_baselines(volume TEXT PRIMARY KEY,last_scan INTEGER);
                INSERT INTO scan_baselines SELECT v.volume,(SELECT s.id FROM scans s
                    WHERE s.volume=v.volume AND s.sampled=v.indexed_at ORDER BY s.id DESC LIMIT 1) FROM volume_totals v;
                WITH previous AS (SELECT id,LAG(id) OVER(PARTITION BY volume ORDER BY id) AS prior FROM scans)
                UPDATE scans SET comparison_from=(SELECT prior FROM previous p WHERE p.id=scans.id AND prior IS NOT NULL
                    AND (SELECT COUNT(*) FROM scans all_points WHERE all_points.id>p.prior AND all_points.id<p.id)=p.id-p.prior-1);
                PRAGMA user_version=10; COMMIT;")?;
            // Only a gap-free global ID interval proves a legacy previous point.
            // Gaps stay unknown rather than inferring which volume lost a point.
        }Ok(())
    }
    pub(super) fn migrate_pending_baselines(db:&Connection)->Result<()> {
        if db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?<11 {
            db.execute_batch("BEGIN IMMEDIATE;
                ALTER TABLE scan_baselines ADD COLUMN cleanup_pending INTEGER NOT NULL DEFAULT 0;
                UPDATE scan_baselines SET cleanup_pending=1 WHERE EXISTS(SELECT 1 FROM cleanup_runs r
                    WHERE r.volume=scan_baselines.volume AND COALESCE(json_extract(r.payload,'$.verification'),'pending') NOT IN ('verified','verified_by_later_scan')
                    AND (json_extract(r.payload,'$.deleted')>0 OR json_extract(r.payload,'$.unknown')>0
                        OR EXISTS(SELECT 1 FROM cleanup_items i WHERE i.run=r.id AND i.state IN ('pending','unknown'))));
                PRAGMA user_version=11;COMMIT;")?;
        }Ok(())
    }
    pub fn cleanup_baseline_pending(&self,volume:&str)->Result<bool> {
        Ok(self.db.query_row("SELECT cleanup_pending FROM scan_baselines WHERE volume=?",[volume],|r|r.get(0)).optional()?.unwrap_or(false))
    }
    pub fn mark_cleanup_baseline_pending(&self,volume:&str,pending:bool)->Result<()> {
        self.db.execute("INSERT INTO scan_baselines(volume,last_scan,cleanup_pending) VALUES(?,NULL,?) ON CONFLICT(volume) DO UPDATE SET cleanup_pending=excluded.cleanup_pending",params![volume,pending])?;Ok(())
    }

    pub fn operation_pre(&self, scan: i64) -> Result<i64> {
        let version: i64 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 8 { return Ok(scan); }
        Ok(self.db.query_row("SELECT COALESCE(operation_pre,id) FROM scans WHERE id=?", [scan], |r| r.get(0)).optional()?.unwrap_or(scan))
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
        let Some(primary)=reports.iter().find(|r|r.id==pre) else {return Ok(None);};
        Ok(primary.comparison_from.filter(|id|reports.iter().any(|r|r.id==*id && r.volume==latest.volume && *id<pre)).map(|from|(from,pre)))
    }
}
