use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub parent: u64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    pub parent: u64,
    pub name: String,
    pub is_dir: bool,
    pub logical: u64,
    pub allocated: u64,
    pub modified: i64,
    pub attributes: u32,
    pub links: Vec<Link>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub volume_id: String,
    pub journal_id: u64,
    pub next_usn: i64,
    pub root_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanMode {
    Full,
    Incremental,
}

#[derive(Clone, Debug)]
pub enum Mutation {
    Upsert(Entry),
    Delete(u64),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanOutcome {
    pub checkpoint: Checkpoint,
    pub mode: ScanMode,
    pub total: u64,
    pub free: u64,
    pub sampled_at: String,
    pub pending: Vec<u64>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScanProgress {
    pub phase: String,
    pub processed: u64,
    pub expected: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScanCounters {
    pub journal_records: u64,
    pub unique_changed_files: u64,
    pub entry_reads: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScanPerformance {
    pub duration_ms: u64,
    pub read_ms: Option<u64>,
    pub commit_ms: Option<u64>,
    pub maintenance_ms: Option<u64>,
    pub paused_ms: u64,
    pub counters: ScanCounters,
    pub cpu_time_ms: Option<u64>,
    pub average_cpu_percent: Option<f64>,
    pub process_peak_working_set_bytes: Option<u64>,
}
