use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, io::ErrorKind, path::Path};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub drives: Vec<char>,
    pub history_budget_mib: u64,
    pub close_after_manual_scan: bool,
    pub trend_days: u16,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self { drives: vec!['C'], history_budget_mib: 512, close_after_manual_scan: false, trend_days: 30 }
    }
}

impl AppConfig {
    pub fn load(data: &Path) -> Result<Self> {
        let bytes = match fs::read(data.join("config.json")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error).context("Read VolumeTrail configuration"),
        };
        let config: Self = serde_json::from_slice(&bytes).context("Parse VolumeTrail configuration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, data: &Path) -> Result<()> {
        self.validate()?;
        fs::write(data.join("config.json"), serde_json::to_vec_pretty(self)?)
            .context("Save VolumeTrail configuration")?;
        Ok(())
    }

    pub fn budget_bytes(&self) -> u64 { self.history_budget_mib * 1024 * 1024 }

    fn validate(&self) -> Result<()> {
        ensure!((64..=4096).contains(&self.history_budget_mib), "History budget must be 64-4096 MiB");
        ensure!([0, 7, 30, 365].contains(&self.trend_days), "Trend range must be 7, 30, 365 days or all time");
        ensure!(!self.drives.is_empty(), "Choose at least one drive");
        let mut seen = HashSet::new();
        for letter in &self.drives {
            ensure!(letter.is_ascii_uppercase() && seen.insert(*letter), "Drive letters must be unique uppercase A-Z");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_persists_drive_selection_and_budget() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(AppConfig::load(dir.path()).unwrap(), AppConfig::default());
        let config = AppConfig { drives: vec!['C', 'E'], history_budget_mib: 768, close_after_manual_scan: true, trend_days: 365 };
        config.save(dir.path()).unwrap();
        assert_eq!(AppConfig::load(dir.path()).unwrap(), config);
        assert!(AppConfig { drives: vec!['C', 'C'], ..config }.save(dir.path()).is_err());
    }

    #[test]
    fn older_config_defaults_to_keeping_the_window_open() {
        let config: AppConfig = serde_json::from_str(r#"{"drives":["C"],"history_budget_mib":512}"#).unwrap();
        assert!(!config.close_after_manual_scan);
    }
}
