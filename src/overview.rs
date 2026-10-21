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
}

pub fn load(store: &Store, root: &str) -> Result<Overview> {
    let volume = store.checkpoint(root)?.map(|cp| cp.volume_id);
    let reports: Vec<_> = store.reports()?.into_iter().filter(|r| Some(&r.volume) == volume.as_ref()).collect();
    let latest = reports.first().cloned();
    let pre = latest.as_ref().map(|r| store.operation_pre(r.id)).transpose()?;
    let verification = pre.map(|id| store.verification(id)).transpose()?.unwrap_or_else(|| "no_scan".into());
    let interval = store.default_interval(&reports)?.map(|(from,to)| -> Result<_> {
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
    Ok(Overview { drive: root.into(), volume, latest, pre_cleanup_scan: pre, verification,
        interval, growth, reductions, trend: reports, depth })
}
