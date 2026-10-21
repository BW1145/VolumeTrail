//! One read-only overview for the desktop and automation clients.
use crate::store::{FolderGrowth, Report, Store};
use anyhow::Result;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Interval {
    pub from: i64,
    pub to: i64,
    pub from_observed_at: String,
    pub to_observed_at: String,
    pub used_delta_bytes: i128,
    pub coverage: (u64, u64, u64, u64),
}

#[derive(Clone, Debug, Serialize)]
pub struct CurrentIndex {pub indexed_bytes:u64,pub total_bytes:u64,pub free_bytes:u64,pub observed_at:String}

#[derive(Clone, Debug, Serialize)]
pub struct Overview {
    pub drive: String,
    pub volume: Option<String>,
    pub latest: Option<Report>,
    pub pre_cleanup_scan: Option<i64>,
    pub verification: String,
    pub interval: Option<Interval>,
    pub growth: Vec<FolderGrowth>,
    pub reductions: Vec<FolderGrowth>,
    pub trend: Vec<Report>,
    pub depth: usize,
    pub latest_cleanup: Option<crate::cleanup::Run>,
    pub current_index: Option<CurrentIndex>,
    pub last_operation: Option<crate::worker::ScanStatus>,
}

pub fn load(store: &Store, root: &str) -> Result<Overview> {
    let volume = store.checkpoint(root)?.map(|cp| cp.volume_id);
    let reports: Vec<_> = store.reports()?.into_iter().filter(|r| Some(&r.volume) == volume.as_ref()).collect();
    let latest = reports.first().cloned();
    let current_index=volume.as_deref().map(|v|store.index_state(v)).transpose()?.flatten()
        .map(|(indexed_bytes,total_bytes,free_bytes,observed_at)|CurrentIndex {indexed_bytes,total_bytes,free_bytes,observed_at});
    let current_point_present=latest.as_ref().zip(current_index.as_ref()).is_some_and(|(r,i)|r.sampled==i.observed_at);
    let pre = if current_point_present {latest.as_ref().map(|r| store.operation_pre(r.id)).transpose()?} else {None};
    let verification = pre.map(|id| store.verification(id)).transpose()?.unwrap_or_else(|| if current_index.is_some() {"history_point_missing"} else {"no_scan"}.into());
    let interval = if current_point_present {store.default_interval(&reports)?} else {None}.map(|(from,to)| -> Result<_> {
        let a = reports.iter().find(|r| r.id == from).unwrap();
        let b = reports.iter().find(|r| r.id == to).unwrap();
        Ok(Interval { from, to, from_observed_at: a.sampled.clone(), to_observed_at: b.sampled.clone(),
            used_delta_bytes: i128::from(b.total.saturating_sub(b.free)) - i128::from(a.total.saturating_sub(a.free)),
            coverage: store.coverage(&b.volume,from,to)? })
    }).transpose()?;
    let depth = 3;
    let items = interval.as_ref().map(|i| store.folder_growth(volume.as_deref().unwrap(),i.from,i.to,depth))
        .transpose()?.unwrap_or_default();
    let growth = items.iter().filter(|i| i.allocated_delta > 0).cloned().collect();
    let reductions = items.into_iter().rev().filter(|i| i.allocated_delta < 0).collect();
    let latest_cleanup=if let Some(volume)=volume.as_deref() {store.cleanup_runs(Some(volume),1,0)?.1.into_iter().next()} else {None};
    let last_operation=store.source_directory().and_then(|data|root.chars().next().and_then(|letter|crate::worker::read_status(data,letter)));
    Ok(Overview { drive: root.into(), volume, latest, pre_cleanup_scan: pre, verification,
        interval, growth, reductions, trend: reports, depth,latest_cleanup,current_index,last_operation })
}
