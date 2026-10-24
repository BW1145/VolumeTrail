//! Lossless storage: one copy of indexed fields and zlib-compressed history chunks.
//! Public queries use the same SQL views for live and archived changes.
use super::*;
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use rusqlite::{Row, functions::FunctionFlags, types::Type};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub(super) const ENTRY_COLUMNS: &str = "id,parent,name,is_dir,logical,allocated,payload";

#[derive(Serialize, Deserialize)]
struct EntryExtra {
    modified: i64,
    attributes: u32,
    links: Vec<Link>,
    #[serde(default)]
    primary_link: bool,
}

pub(super) fn entry_payload(entry: &Entry) -> Result<String> {
    let primary_link = entry.links.first().is_some_and(|link| link.parent == entry.parent && link.name == entry.name);
    Ok(serde_json::to_string(&EntryExtra {
        modified: entry.modified, attributes: entry.attributes,
        links: entry.links[usize::from(primary_link)..].to_vec(), primary_link,
    })?)
}

pub(super) fn read_entry(row: &Row<'_>) -> rusqlite::Result<Entry> {
    // Legacy full-entry JSON also has these fields, so read-only old databases work.
    let mut extra: EntryExtra = serde_json::from_str(&row.get::<_, String>(6)?)
        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(6, Type::Text, Box::new(error)))?;
    let parent = row.get::<_, i64>(1)? as u64;
    let name: String = row.get(2)?;
    if extra.primary_link { extra.links.insert(0, Link { parent, name: name.clone() }); }
    Ok(Entry {
        id: row.get::<_, i64>(0)? as u64, parent,
        name, is_dir: row.get(3)?, logical: row.get::<_, i64>(4)? as u64,
        allocated: row.get::<_, i64>(5)? as u64,
        modified: extra.modified, attributes: extra.attributes, links: extra.links,
    })
}

type HistoryRow=(i64,String,String,i64,i64,String);
pub(super) type IdHistoryRow=(i64,i64,i64,i64,i64,String);

#[derive(Serialize,Deserialize)]
#[serde(untagged)]
enum PathRef {Text(String),Id(usize)}
#[derive(Serialize,Deserialize)]
struct SharedPaths {rows:Vec<(i64,PathRef,PathRef,i64,i64,String)>}

fn deflate(json:&str)->std::io::Result<Vec<u8>> {
    let mut encoder=ZlibEncoder::new(Vec::new(),Compression::default());
    encoder.write_all(json.as_bytes())?;encoder.finish()
}

#[cfg(test)]
fn history_blob(rows:&[HistoryRow])->Result<Vec<u8>> {
    let original=serde_json::to_string(rows)?;
    let plain=deflate(&original)?;
    let mut ids=HashMap::new();
    let mut path=|text:&String| {
        if let Some(&id)=ids.get(text) {PathRef::Id(id)} else {
            let id=ids.len();ids.insert(text.clone(),id);PathRef::Text(text.clone())
        }
    };
    let shared=SharedPaths {rows:rows.iter().map(|(seq,old,new,logical,allocated,kind)|
        (*seq,path(old),path(new),*logical,*allocated,kind.clone())).collect()};
    let pooled=deflate(&serde_json::to_string(&shared)?)?;
    // Some blocks already compress repeated strings better than numeric references.
    let data=if pooled.len()<plain.len() {pooled} else {plain};
    ensure!(inflate(&data)?==original,"Compressed history verification failed");Ok(data)
}

fn inflate(bytes: &[u8]) -> std::io::Result<String> {
    let mut output = inflate_raw(bytes)?;
    if output.starts_with('{') {
        let shared:SharedPaths=serde_json::from_str(&output)?;
        let mut paths=Vec::new();
        let mut resolve=|path|->std::io::Result<String> {match path {
            PathRef::Text(text)=>{paths.push(text.clone());Ok(text)},
            PathRef::Id(id)=>paths.get(id).cloned().ok_or_else(||std::io::Error::new(std::io::ErrorKind::InvalidData,"Missing history path ID")),
        }};
        let mut rows=Vec::with_capacity(shared.rows.len());
        for (seq,old,new,logical,allocated,kind) in shared.rows {
            rows.push((seq,resolve(old)?,resolve(new)?,logical,allocated,kind));
        }
        output=serde_json::to_string(&rows)?;
    }
    Ok(output)
}

fn inflate_raw(bytes: &[u8]) -> std::io::Result<String> {
    let mut output = String::new();
    ZlibDecoder::new(bytes).read_to_string(&mut output)?;
    Ok(output)
}

pub(super) fn decode_legacy(bytes: &[u8]) -> Result<Vec<HistoryRow>> {
    Ok(serde_json::from_str(&inflate(bytes)?)?)
}

pub(super) fn decode_ids(bytes: &[u8]) -> Result<Vec<IdHistoryRow>> {
    Ok(serde_json::from_str(&inflate_raw(bytes)?)?)
}

pub(super) fn encode_ids(rows: &[IdHistoryRow]) -> Result<Vec<u8>> {
    let data = deflate(&serde_json::to_string(rows)?)?;
    ensure!(decode_ids(&data)? == rows, "Compressed history verification failed");
    Ok(data)
}

pub(super) fn configure(db: &Connection) -> Result<()> {
    let version = db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?;
    let shared = version >= 14;
    db.create_scalar_function("volumetrail_inflate", 1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |context| (if shared { inflate_raw(context.get_raw(0).as_blob()?) }
            else { inflate(context.get_raw(0).as_blob()?) })
            .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error))))?;
    let archived = version >= 12;
    let live_rows = if shared {
        "SELECT seq,scan,old_path_id AS old_path,new_path_id AS new_path,logical_delta,allocated_delta,kind FROM changes"
    } else { "SELECT rowid AS seq,scan,old_path,new_path,logical_delta,allocated_delta,kind FROM changes" };
    let archive_rows = if archived { " UNION ALL
        SELECT json_extract(j.value,'$[0]'),a.scan,
            json_extract(j.value,'$[1]'),json_extract(j.value,'$[2]'),
            json_extract(j.value,'$[3]'),json_extract(j.value,'$[4]'),json_extract(j.value,'$[5]')
        FROM change_archives a,json_each(volumetrail_inflate(a.data)) j" } else { "" };
    let archive_counts = if archived {
        " UNION ALL SELECT scan,SUM(entries) FROM change_archives GROUP BY scan"
    } else { "" };
    db.execute_batch(&format!("CREATE TEMP VIEW change_history AS
        {live_rows}{archive_rows};
        CREATE TEMP VIEW change_counts AS SELECT scan,COUNT(*) AS entries FROM changes GROUP BY scan{archive_counts};"))?;
    Ok(())
}

impl Store {
    pub(super) fn migrate_compact(db: &Connection) -> Result<()> {
        if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? >= 12 { return Ok(()); }
        let tx = db.unchecked_transaction()?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS change_archives(
            scan INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
            part INTEGER NOT NULL,entries INTEGER NOT NULL,data BLOB NOT NULL,
            PRIMARY KEY(scan,part));
            UPDATE nodes SET payload=json_remove(payload,'$.id','$.parent','$.name',
                '$.is_dir','$.logical','$.allocated');
            UPDATE nodes SET payload=json_set(json_remove(payload,'$.links[0]'),'$.primary_link',json('true'))
                WHERE COALESCE(json_extract(payload,'$.primary_link'),0)=0
                AND json_extract(payload,'$.links[0].parent')=parent
                AND json_extract(payload,'$.links[0].name')=name;
            PRAGMA user_version=12;")?;
        tx.commit()?;
        Ok(())
    }

    /// Keep all summaries and details. Only the newest scan on each volume stays live.
    pub fn compact_history(&mut self) -> Result<u64> {
        ensure!(!self.staging, "Cannot compact during a scan");
        let scans = {
            let mut stmt = self.db.prepare("SELECT DISTINCT c.scan FROM changes c JOIN scans s ON s.id=c.scan
                WHERE c.scan < (SELECT MAX(recent.id) FROM scans recent WHERE recent.volume=s.volume)
                ORDER BY c.scan")?;
            stmt.query_map([], |row| row.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for scan in scans {
            self.check_cancel()?;
            let tx = self.db.unchecked_transaction()?;
            let mut part: i64 = tx.query_row("SELECT COALESCE(MAX(part)+1,0) FROM change_archives WHERE scan=?", [scan], |r| r.get(0))?;
            loop {
                self.check_cancel()?;
                let rows = {
                    let mut stmt = tx.prepare("SELECT seq,old_path_id,new_path_id,logical_delta,allocated_delta,kind
                        FROM changes WHERE scan=? ORDER BY seq LIMIT 256")?;
                    stmt.query_map([scan], |r| Ok((r.get::<_, i64>(0)?,r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,r.get::<_, i64>(3)?,r.get::<_, i64>(4)?,r.get::<_, String>(5)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                let Some(last) = rows.last().map(|row| row.0) else { break; };
                let data=encode_ids(&rows)?;
                tx.execute("INSERT INTO change_archives VALUES(?,?,?,?)", params![scan,part,rows.len() as i64,data])?;
                tx.execute("DELETE FROM changes WHERE scan=? AND seq<=?", params![scan,last])?;
                part += 1;
            }
            self.check_cancel()?;
            tx.commit()?;
        }
        self.reclaim_free_pages()?;
        self.used_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_history_path_ids_round_trip_unicode_empty_paths_and_signed_deltas() {
        let data=deflate(r#"{"rows":[[1,"C:\\日志 😀.txt",0,-7,8,"modified"],[2,"",1,0,-9,"deleted"]]}"#).unwrap();
        let expected:Vec<HistoryRow>=vec![(1,"C:\\日志 😀.txt".into(),"C:\\日志 😀.txt".into(),-7,8,"modified".into()),
            (2,"".into(),"".into(),0,-9,"deleted".into())];
        assert_eq!(inflate(&data).unwrap(),serde_json::to_string(&expected).unwrap());
        let compressed=history_blob(&expected).unwrap();
        assert!(compressed.len()<=deflate(&serde_json::to_string(&expected).unwrap()).unwrap().len());
        assert_eq!(inflate(&compressed).unwrap(),serde_json::to_string(&expected).unwrap());
    }
    #[test]
    fn missing_history_path_id_reports_corrupt_data() {
        let data=deflate(r#"{"rows":[[1,7,"",0,0,"modified"]]}"#).unwrap();
        assert!(inflate(&data).is_err());
    }
}
