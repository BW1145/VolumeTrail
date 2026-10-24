use super::*;

// Parent-first traversal prevents SQLite from rescanning every directory per child.
const FILES: &str = "WITH RECURSIVE subtree(id) AS (
    SELECT id FROM changed_directories
    UNION SELECT n.id FROM subtree p CROSS JOIN nodes n INDEXED BY nodes_parent
        WHERE n.volume=? AND n.parent=p.id AND n.is_dir=1 AND n.id<>n.parent)
    SELECT n.id,n.parent,n.name,n.allocated
    FROM subtree p CROSS JOIN nodes n INDEXED BY nodes_parent
    WHERE n.volume=? AND n.parent=p.id AND n.is_dir=0
        AND n.id NOT IN (SELECT id FROM staging)";

impl Store {
    pub(super) fn unchanged_subtree_changes(&self, volume: &str, root: &str, root_id: u64,
        dirs: &Directories, other_dirs: &Directories, sign: i128,
        result: &mut ExtensionDeltas, growth: &mut GrowthDeltas,
        control: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        let cache=self.scan_work.as_ref().map(|(path,_)|crate::scan_work::Statistics::open(path)).transpose()?;
        let mut cursor=None;
        let mut part=0;
        if let Some(cache)=&cache {
            for (last,complete,contributions) in cache.batches(sign as i64)? {
                self.check_cancel()?;control()?;
                apply(&contributions,sign,result,growth);
                cursor=Some(last);part+=1;
                if complete {return Ok(());}
            }
        }
        let sql=format!("{FILES} AND (? IS NULL OR n.id>?) ORDER BY n.id");
        let mut stmt = self.db.prepare(&sql)?;
        let mut rows = stmt.query(params![volume, volume, cursor, cursor])?;
        let mut count = 0;
        let mut batch=Vec::new();
        while let Some(row) = rows.next()? {
            if count % 512 == 0 { self.check_cancel()?; control()?; }
            count += 1;
            cursor=Some(row.get::<_,i64>(0)?);
            let entry = Entry { id: row.get::<_, i64>(0)? as u64, parent: row.get::<_, i64>(1)? as u64,
                name: row.get(2)?, allocated: row.get::<_, i64>(3)? as u64, is_dir: false,
                logical: 0, modified: 0, attributes: 0, links: vec![] };
            let path = entry_path(root, root_id, &entry, dirs);
            if path != entry_path(root, root_id, &entry, other_dirs) {
                if let Some(parent)=folder_parent(&path) {
                    let extension=entry.name.rsplit_once('.').map(|(_,ext)|format!(".{}",ext.to_ascii_lowercase()))
                        .unwrap_or_else(||"(无扩展名)".into());
                    batch.push((parent,extension,entry.allocated));
                }
            }
            if count % 512 == 0 {
                if let Some(cache)=&cache {cache.save(sign as i64,part,cursor.unwrap(),false,&batch)?;}
                apply(&batch,sign,result,growth);batch.clear();part+=1;
            }
        }
        if let Some(cache)=&cache {cache.save(sign as i64,part,cursor.unwrap_or(i64::MIN),true,&batch)?;}
        apply(&batch,sign,result,growth);
        Ok(())
    }
}

fn apply(rows:&[crate::scan_work::Contribution],sign:i128,result:&mut ExtensionDeltas,growth:&mut GrowthDeltas) {
    for (parent,extension,allocated) in rows {
        let delta=sign*i128::from(*allocated);
        if *allocated!=0 {
            let value=result.entry((parent.clone(),extension.clone())).or_default();
            value.0+=delta;value.1+=1;
        }
        growth_entry(growth,parent.clone(),delta,delta,1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ORIGINAL: &str = "WITH RECURSIVE subtree(id) AS (
        SELECT id FROM changed_directories
        UNION SELECT n.id FROM nodes n JOIN subtree p ON n.parent=p.id
            WHERE n.volume=? AND n.is_dir=1 AND n.id<>n.parent)
        SELECT n.id,n.parent,n.name,n.allocated FROM nodes n JOIN subtree p ON n.parent=p.id
        WHERE n.volume=? AND n.is_dir=0 AND n.id NOT IN (SELECT id FROM staging)";

    fn rows(db:&Connection, sql:&str, volume:&str)->rusqlite::Result<Vec<(i64,i64,String,i64)>> {
        let mut stmt=db.prepare(sql)?;
        let mut rows=stmt.query_map([volume,volume],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.sort(); Ok(rows)
    }

    fn fixture(directories:i64)->Connection {
        let db=Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE nodes(volume TEXT,id INTEGER,parent INTEGER,name TEXT,is_dir INTEGER,allocated INTEGER);
            CREATE INDEX nodes_parent ON nodes(volume,parent);
            CREATE INDEX nodes_directories ON nodes(volume,id,parent,name) WHERE is_dir=1;
            CREATE TABLE staging(id INTEGER PRIMARY KEY);
            CREATE TEMP TABLE changed_directories(id INTEGER PRIMARY KEY);
            INSERT INTO nodes VALUES('v',5,5,'root',1,0),('other',90000,10,'another volume',0,123);
            INSERT INTO changed_directories VALUES(10),(11),(99999);
            INSERT INTO staging VALUES(100011);
            BEGIN;").unwrap();
        for id in 10..directories+10 {
            let parent=if id==10 {5} else {10+(id-11)/4};
            db.execute("INSERT INTO nodes VALUES('v',?,?,?,1,0)",params![id,parent,format!("dir-{id}")]).unwrap();
            db.execute("INSERT INTO nodes VALUES('v',?,?,?,0,123)",params![100000+id,id,format!("file-{id}.log")]).unwrap();
        }
        db.execute_batch("COMMIT;").unwrap();db
    }

    #[test]
    fn parent_index_traversal_preserves_nested_changes_and_staging_exclusion() {
        let db=fixture(120);
        let expected=rows(&db,ORIGINAL,"v").unwrap();
        assert_eq!(rows(&db,FILES,"v").unwrap(),expected);
        assert_eq!(expected.len(),119);
    }

    #[test]
    fn recursive_and_file_steps_both_seek_the_parent_index() {
        let db=fixture(6000);
        let mut stmt=db.prepare(&format!("EXPLAIN QUERY PLAN {FILES}")).unwrap();
        let plan=stmt.query_map(["v","v"],|r|r.get::<_,String>(3)).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap().join("\n");
        assert_eq!(plan.matches("nodes_parent (volume=? AND parent=?)").count(),2,"{plan}");
        assert_eq!(rows(&db,FILES,"v").unwrap().len(),5999);
    }

    #[test]
    fn production_subtree_benchmark_when_requested() {
        let Ok(source)=std::env::var("VOLUMETRAIL_SUBTREE_BENCH_SOURCE") else {return;};
        let id:i64=std::env::var("VOLUMETRAIL_SUBTREE_BENCH_ID").unwrap().parse().unwrap();
        let db=Connection::open_with_flags(source,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let volume:String=db.query_row("SELECT volume FROM nodes WHERE id=? AND is_dir=1",[id],|r|r.get(0)).unwrap();
        db.execute_batch("CREATE TEMP TABLE changed_directories(id INTEGER PRIMARY KEY);").unwrap();
        db.execute("INSERT INTO changed_directories VALUES(?)",[id]).unwrap();
        let start=Instant::now();
        db.progress_handler(10000,Some(move || start.elapsed()>Duration::from_secs(3)));
        let old=rows(&db,ORIGINAL,&volume); let old_ms=start.elapsed().as_millis();
        db.progress_handler(0,None::<fn()->bool>);
        let start=Instant::now();let new=rows(&db,FILES,&volume).unwrap();
        let new_ms=start.elapsed().as_millis();
        if let Ok(old)=&old {assert_eq!(old,&new);}
        println!("bundled SQLite {}; original={}ms completed={}; parent-index={}ms rows={}",
            rusqlite::version(),old_ms,old.is_ok(),new_ms,new.len());
    }
}
