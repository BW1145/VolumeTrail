//! Rules, planning and execution are shared by the GUI, CLI and login worker.
pub mod windows;
mod service;
pub mod cli;
pub use service::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::{Path,PathBuf}};

pub const MAX_CANDIDATES: usize = 10_000;
pub const MAX_DETAILS: usize = 200;
pub const PLAN_SECONDS: i64 = 15 * 60;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub revision: u64,
    pub name: String,
    pub volume_id: String,
    pub directory: String,
    pub patterns: Vec<String>,
    pub recursive: bool,
    pub min_age_days: u32,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity { pub volume: u64, pub id: [u8;16] }
impl Identity { pub fn volume_id(&self) -> String { format!("ntfs:{:016x}",self.volume) } }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub identity: Identity,
    pub parents: Vec<Identity>,
    pub modified_ticks: i64,
    pub change_ticks: i64,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub attributes: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate { pub path: String, pub snapshot: Snapshot, pub rule_ids: Vec<String> }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Skip { pub path: String, pub reason: String }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub plan_id: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub rules: Vec<Rule>,
    pub deletion_method: String,
    pub candidate_count: usize,
    pub allocated_bytes_estimate: u64,
    pub candidates: Vec<Candidate>,
    pub skipped_count: u64,
    pub skip_reasons: BTreeMap<String,u64>,
    pub skipped: Vec<Skip>,
    pub truncated: bool,
    pub skip_details_truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub run_id: String,
    pub request_id: String,
    pub plan_id: String,
    pub rules: Vec<Rule>,
    pub volume_id: String,
    pub pre_scan: Option<i64>,
    pub post_scan: Option<i64>,
    pub automatic_date: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub state: String,
    pub verification: String,
    pub deleted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub unknown: u64,
    pub allocated_bytes_estimate: u64,
    pub free_before: Option<u64>,
    pub free_after: Option<u64>,
    pub free_observed_at: Option<String>,
    pub details_trimmed: bool,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunItem { pub path: String, pub state: String, pub reason: Option<String>, pub allocated_bytes: u64 }

#[derive(Clone)]
pub struct Safety { pub protected: Vec<PathBuf> }
impl Safety {
    pub fn for_data(data: &Path) -> Result<Self> {
        let mut protected = vec![data.to_path_buf(),std::env::current_exe()?.parent().unwrap().to_path_buf()];
        for key in ["SystemRoot","ProgramFiles","ProgramFiles(x86)","ProgramW6432","ProgramData"] {
            if let Some(value) = std::env::var_os(key) { protected.push(value.into()); }
        }
        Ok(Self {protected})
    }

    pub fn validate(&self, rule: &Rule) -> Result<()> {
        ensure!(!rule.name.trim().is_empty() && rule.name.chars().count() <= 100,"Rule name must contain 1-100 characters");
        ensure!(rule.volume_id.starts_with("ntfs:") && rule.volume_id.len() == 21,"Bind the NTFS volume identity");
        ensure!(rule.min_age_days <= 36500,"Age must be 0-36500 days");
        ensure!(!rule.patterns.is_empty() && rule.patterns.len() <= 32,"Provide 1-32 filename patterns");
        for pattern in &rule.patterns {
            ensure!(!pattern.is_empty() && pattern.chars().count() <= 255 && !pattern.contains(['\\','/',':','\0'])
                && !pattern.contains(['[',']']),"Use filename wildcards * and ?, without paths or regular expressions");
        }
        let target = normal_path(&rule.directory)?;
        ensure!(target.len()>3,"Volume roots are protected");
        let target_guard=windows::DirectoryGuard::open(&rule.directory)?;
        for path in &self.protected {
            let value = path.to_string_lossy();
            let canonical = windows::canonical_directory(&value).unwrap_or_else(|_| value.into_owned());
            let protected = normal_path(&canonical)?;
            ensure!(!within(&target,&protected) && !within(&protected,&target),"Protected directory overlaps cleanup scope: {canonical}");
            if let Ok(guard)=windows::DirectoryGuard::open(&canonical) {
                let target_id=target_guard.identities.last().unwrap();
                let protected_id=guard.identities.last().unwrap();
                ensure!(!target_guard.identities.contains(protected_id) && !guard.identities.contains(target_id),
                    "Protected directory identity overlaps cleanup scope: {canonical}");
            }
        }
        Ok(())
    }
}

pub fn normal_path(value: &str) -> Result<String> {
    let value = value.strip_prefix(r"\\?\").unwrap_or(value);
    let value = if value.len()==3 {value} else {value.trim_end_matches('\\')};
    ensure!(value.len()>=3 && value.as_bytes()[0].is_ascii_alphabetic() && &value.as_bytes()[1..3]==b":\\",
        "Use an absolute local drive path");
    for part in value[3..].split('\\') {
        if part.is_empty() && value.len()==3 {continue;}
        ensure!(!part.is_empty() && part!="." && part!=".." && !part.ends_with(['.',' '])
            && !part.contains(['/',':','*','?','\0']),"Ambiguous or non-local path component");
    }
    Ok(value.to_lowercase())
}

pub fn within(path: &str, directory: &str) -> bool {
    path==directory || path.strip_prefix(directory).is_some_and(|tail| tail.starts_with('\\'))
}

// Small bounded wildcard matcher. Unicode scalar values, case insensitive, no regex.
pub fn filename_matches(name: &str, pattern: &str) -> bool {
    let n: Vec<_> = name.to_lowercase().chars().collect();
    let p: Vec<_> = pattern.to_lowercase().chars().collect();
    let (mut i,mut j,mut star,mut retry)=(0,0,None,0);
    while i<n.len() {
        if j<p.len() && (p[j]=='?' || p[j]==n[i]) {i+=1;j+=1;}
        else if j<p.len() && p[j]=='*' {star=Some(j);j+=1;retry=i;}
        else if let Some(s)=star {retry+=1;i=retry;j=s+1;}
        else {return false;}
    }
    while j<p.len() && p[j]=='*' {j+=1;}
    j==p.len()
}

pub fn old_enough(modified_ticks: i64, days: u32, now: i64) -> bool {
    days==0 || i128::from(modified_ticks) <= (i128::from(now)-i128::from(days)*86400+11644473600)*10_000_000
}

pub fn new_id() -> String { format!("{:?}",unsafe {::windows::Win32::System::Com::CoCreateGuid()}.expect("Windows GUID")) }

#[derive(Debug)]
pub struct Fault { pub code: &'static str, pub message: String }
impl std::fmt::Display for Fault {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str(&self.message)}}
impl std::error::Error for Fault {}
pub fn fault(code: &'static str,message: impl Into<String>)->anyhow::Error { Fault {code,message:message.into()}.into() }
