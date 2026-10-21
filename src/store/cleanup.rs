use super::*;
use crate::cleanup::{Plan,Rule,Run,RunItem,fault,MAX_DETAILS};

impl Store {
    pub fn cleanup_durable(&self)->Result<()> {self.db.pragma_update(None,"synchronous","FULL")?;Ok(())}
    pub(super) fn migrate_cleanup(db:&Connection)->Result<()> {
        if db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?<9 {
            db.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE cleanup_rules(id TEXT PRIMARY KEY,revision INTEGER NOT NULL,payload TEXT NOT NULL);
                CREATE TABLE cleanup_plans(id TEXT PRIMARY KEY,expires INTEGER NOT NULL,payload TEXT NOT NULL);
                CREATE TABLE cleanup_runs(id TEXT PRIMARY KEY,request TEXT NOT NULL UNIQUE,volume TEXT NOT NULL,
                    started INTEGER NOT NULL,state TEXT NOT NULL,payload TEXT NOT NULL);
                CREATE TABLE cleanup_items(run TEXT NOT NULL REFERENCES cleanup_runs(id) ON DELETE CASCADE,
                    seq INTEGER NOT NULL,path TEXT NOT NULL,state TEXT NOT NULL,reason TEXT,allocated INTEGER NOT NULL,
                    PRIMARY KEY(run,seq));
                CREATE TABLE cleanup_daily(rule TEXT NOT NULL,date TEXT NOT NULL,revision INTEGER NOT NULL,run TEXT NOT NULL,
                    PRIMARY KEY(rule,date));
                CREATE INDEX cleanup_runs_volume ON cleanup_runs(volume,started DESC);
                PRAGMA user_version=9; COMMIT;")?;
        }
        Ok(())
    }

    fn has_cleanup(&self)->Result<bool> {Ok(self.db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?>=9)}

    pub fn cleanup_rules(&self)->Result<Vec<Rule>> {
        if !self.has_cleanup()? {return Ok(Vec::new());}
        let mut stmt=self.db.prepare("SELECT payload FROM cleanup_rules ORDER BY id")?;
        stmt.query_map([],|r|r.get::<_,String>(0))?.map(|r|Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn cleanup_rule(&self,id:&str)->Result<Rule> {
        self.cleanup_rules()?.into_iter().find(|r|r.id==id).ok_or_else(||fault("rule_not_found","Rule does not exist"))
    }
    pub fn save_cleanup_rule(&self,rule:&Rule,expected:u64)->Result<()> {
        let tx=self.db.unchecked_transaction()?;
        let current:Option<u64>=tx.query_row("SELECT revision FROM cleanup_rules WHERE id=?",[&rule.id],|r|r.get(0)).optional()?;
        if current.unwrap_or(0)!=expected {return Err(fault("revision_conflict","Rule was changed; read it again"));}
        tx.execute("INSERT INTO cleanup_rules VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,payload=excluded.payload",
            params![rule.id,rule.revision,serde_json::to_string(rule)?])?;
        tx.execute("DELETE FROM cleanup_plans WHERE id IN (SELECT id FROM cleanup_plans WHERE EXISTS
            (SELECT 1 FROM json_each(payload,'$.rules') WHERE json_extract(value,'$.id')=?))",[&rule.id])?;
        tx.commit()?;Ok(())
    }
    pub fn delete_cleanup_rule(&self,id:&str,revision:u64)->Result<()> {
        if self.db.execute("DELETE FROM cleanup_rules WHERE id=? AND revision=?",params![id,revision])?!=1 {
            return Err(fault("revision_conflict","Rule missing or changed; read it again"));
        }Ok(())
    }
    pub fn save_cleanup_plan(&self,plan:&Plan)->Result<()> {
        self.db.execute("DELETE FROM cleanup_plans WHERE expires<?",[chrono::Utc::now().timestamp()])?;
        self.db.execute("INSERT INTO cleanup_plans VALUES(?,?,?)",params![plan.plan_id,plan.expires_at,serde_json::to_string(plan)?])?;
        self.db.execute("DELETE FROM cleanup_plans WHERE id NOT IN (SELECT id FROM cleanup_plans ORDER BY expires DESC LIMIT 32)",[])?;
        Ok(())
    }
    pub fn cleanup_plan(&self,id:&str)->Result<Plan> {
        let payload:Option<String>=self.db.query_row("SELECT payload FROM cleanup_plans WHERE id=?",[id],|r|r.get(0)).optional()?;
        serde_json::from_str(&payload.ok_or_else(||fault("plan_not_found","Preview missing or invalidated; preview again"))?).map_err(Into::into)
    }
    pub fn cleanup_request(&self,request:&str)->Result<Option<Run>> {
        if !self.has_cleanup()? {return Ok(None);}
        let value:Option<String>=self.db.query_row("SELECT payload FROM cleanup_runs WHERE request=?",[request],|r|r.get(0)).optional()?;
        value.map(|v|serde_json::from_str(&v).map_err(Into::into)).transpose()
    }
    pub fn cleanup_run(&self,id:&str)->Result<Run> {
        let value:Option<String>=self.db.query_row("SELECT payload FROM cleanup_runs WHERE id=?",[id],|r|r.get(0)).optional()?;
        serde_json::from_str(&value.ok_or_else(||fault("run_not_found","Cleanup run does not exist"))?).map_err(Into::into)
    }
    pub fn cleanup_runs(&self,volume:Option<&str>,limit:u64,offset:u64)->Result<(u64,Vec<Run>)> {
        if !self.has_cleanup()? {return Ok((0,Vec::new()));}
        let total=self.db.query_row("SELECT COUNT(*) FROM cleanup_runs WHERE (? IS NULL OR volume=?)",params![volume,volume],|r|r.get(0))?;
        let mut stmt=self.db.prepare("SELECT payload FROM cleanup_runs WHERE (? IS NULL OR volume=?) ORDER BY started DESC,id DESC LIMIT ? OFFSET ?")?;
        let items=stmt.query_map(params![volume,volume,limit.min(500),offset],|r|r.get::<_,String>(0))?
            .map(|r|Ok(serde_json::from_str(&r?)?)).collect::<Result<_>>()?;
        Ok((total,items))
    }
    pub fn cleanup_items(&self,id:&str)->Result<Vec<RunItem>> {
        let mut stmt=self.db.prepare("SELECT path,state,reason,allocated FROM cleanup_items WHERE run=? ORDER BY (state IN ('pending','unknown')) DESC,(state='failed') DESC,seq LIMIT ?")?;
        Ok(stmt.query_map(params![id,MAX_DETAILS],|r|Ok(RunItem {path:r.get(0)?,state:r.get(1)?,reason:r.get(2)?,allocated_bytes:r.get(3)?}))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn cleanup_item_counts(&self,id:&str)->Result<(u64,u64,u64)> {
        Ok(self.db.query_row("SELECT COUNT(*),COALESCE(SUM(state='pending'),0),COALESCE(SUM(state='unknown'),0) FROM cleanup_items WHERE run=?",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
    }
    pub fn observed_cleanup_run(&self,id:&str,active:bool)->Result<Run> {
        let mut run=self.cleanup_run(id)?;
        if run.state=="running" && !active {
            let (_,pending,unknown)=self.cleanup_item_counts(id)?;
            run.state="interrupted".into();run.unknown=run.unknown.max(pending+unknown);
            if run.unknown>0 || run.deleted>0 {run.verification="pending".into();}
            run.message=Some("No matching active cleanup operation; interrupted state inferred without replay".into());
        }Ok(run)
    }
    pub fn cleanup_due(&self,id:&str,date:&str)->Result<bool> {
        Ok(!self.db.query_row("SELECT EXISTS(SELECT 1 FROM cleanup_daily WHERE rule=? AND date=?)",params![id,date],|r|r.get::<_,bool>(0))?)
    }
    pub fn start_cleanup_run(&self,run:&Run)->Result<()> {
        let tx=self.db.unchecked_transaction()?;
        if let Some(date)=&run.automatic_date {for rule in &run.rules {
            tx.execute("INSERT INTO cleanup_daily VALUES(?,?,?,?)",params![rule.id,date,rule.revision,run.run_id])?;
        }}
        tx.execute("INSERT INTO cleanup_runs VALUES(?,?,?,?,?,?)",params![run.run_id,run.request_id,run.volume_id,
            run.started_at,run.state,serde_json::to_string(run)?])?;
        tx.commit()?;Ok(())
    }
    pub fn update_cleanup_run(&self,run:&Run)->Result<()> {
        self.db.execute("UPDATE cleanup_runs SET state=?,payload=? WHERE id=?",params![run.state,serde_json::to_string(run)?,run.run_id])?;
        Ok(())
    }
    pub fn record_cleanup_item(&self,run:&Run,seq:usize,item:&RunItem)->Result<()> {
        let tx=self.db.unchecked_transaction()?;
        tx.execute("INSERT INTO cleanup_items VALUES(?,?,?,?,?,?) ON CONFLICT(run,seq) DO UPDATE SET state=excluded.state,reason=excluded.reason",
            params![run.run_id,seq,item.path,item.state,item.reason,item.allocated_bytes])?;
        tx.execute("UPDATE cleanup_runs SET state=?,payload=? WHERE id=?",params![run.state,serde_json::to_string(run)?,run.run_id])?;
        // Keep first errors plus the currently pending item, bounded even during a large run.
        tx.execute("DELETE FROM cleanup_items WHERE run=? AND state<>'pending' AND seq NOT IN
            (SELECT seq FROM cleanup_items WHERE run=? AND state<>'pending' ORDER BY (state='deleted'),seq LIMIT ?)",
            params![run.run_id,run.run_id,MAX_DETAILS])?;
        tx.commit()?;Ok(())
    }
    // Called only while holding the global operation lock; never from a read-only UI query.
    pub fn recover_cleanup_runs(&self)->Result<()> {
        let mut stmt=self.db.prepare("SELECT payload FROM cleanup_runs WHERE state='running'")?;
        let runs=stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;drop(stmt);
        for value in runs {
            let mut run:Run=serde_json::from_str(&value)?;
            let tx=self.db.unchecked_transaction()?;
            run.unknown=tx.query_row("SELECT COUNT(*) FROM cleanup_items WHERE run=? AND state IN ('pending','unknown')",[&run.run_id],|r|r.get(0))?;
            tx.execute("UPDATE cleanup_items SET state='unknown',reason='Process interrupted; do not replay' WHERE run=? AND state='pending'",[&run.run_id])?;
            run.state="interrupted".into();run.finished_at=Some(chrono::Utc::now().timestamp());
            if run.deleted>0 || run.unknown>0 {run.verification="pending".into();}
            self.update_cleanup_run(&run)?;
            tx.commit()?;
        }Ok(())
    }
    pub fn prune_cleanup(&self,now:i64)->Result<()> {
        self.db.execute("DELETE FROM cleanup_plans WHERE expires<?",[now])?;
        self.db.execute("UPDATE cleanup_runs SET payload=json_set(payload,'$.details_trimmed',json('true')) WHERE state<>'running' AND started<?",[now-30*86400])?;
        self.db.execute("DELETE FROM cleanup_items WHERE run IN (SELECT id FROM cleanup_runs WHERE state<>'running' AND started<?)",[now-30*86400])?;
        // Run/request summaries are bounded at 1000 and 365 days. Idempotency retention is explicit in capabilities.
        self.db.execute("DELETE FROM cleanup_runs WHERE state<>'running' AND (started<? OR id NOT IN (SELECT id FROM cleanup_runs ORDER BY started DESC LIMIT 1000))",[now-365*86400])?;
        self.db.execute("DELETE FROM cleanup_daily WHERE date<?",[chrono::Local::now().date_naive().checked_sub_signed(chrono::Duration::days(370)).unwrap().to_string()])?;
        Ok(())
    }
    pub fn reconcile_cleanup_after_scan(&self,volume:&str,scan:i64)->Result<()> {
        for mut run in self.cleanup_runs(Some(volume),1000,0)?.1 {
            if ["pending","unverified","cancelled"].contains(&run.verification.as_str()) && run.pre_scan.is_none_or(|pre|pre<scan) {
                run.verification="verified_by_later_scan".into();
                run.message=Some(format!("Current index refreshed by later scan {scan}; no cleanup replay"));
                self.update_cleanup_run(&run)?;
            }
        }Ok(())
    }
}
