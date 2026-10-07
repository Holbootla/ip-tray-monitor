//! Persistent settings stored as JSON in `%APPDATA%\IpTrayMonitor\config.json`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::{fs, io};

pub const MIN_INTERVAL_SECS: u64 = 10;
pub const MAX_INTERVAL_SECS: u64 = 86_400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    /// Expected public IP(s): comma-separated addresses and/or CIDR networks.
    /// Empty means "just show the IP, don't compare".
    pub target_ip: String,
    /// How often to query the public IP, in seconds.
    pub check_interval_secs: u64,
    /// Show a Windows notification when the IP does not match the target.
    pub notifications: bool,
    /// Optional custom list of "what is my IP" URLs (plain-text response).
    /// Empty = built-in list. Not shown in the Settings window.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            target_ip: String::new(),
            check_interval_secs: 60,
            notifications: true,
            providers: Vec::new(),
        }
    }
}

impl Config {
    pub fn interval_secs(&self) -> u64 {
        self.check_interval_secs.clamp(MIN_INTERVAL_SECS, MAX_INTERVAL_SECS)
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("IpTrayMonitor")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub enum LoadResult {
    Loaded(Config),
    /// No config file yet — first launch.
    FirstRun(Config),
    /// The file exists but could not be parsed; defaults are used.
    Invalid(Config, String),
}

pub fn load() -> LoadResult {
    let path = config_path();
    match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Config>(&text) {
            Ok(cfg) => LoadResult::Loaded(cfg),
            Err(e) => LoadResult::Invalid(Config::default(), e.to_string()),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => LoadResult::FirstRun(Config::default()),
        Err(e) => LoadResult::Invalid(Config::default(), e.to_string()),
    }
}

pub fn save(cfg: &Config) -> io::Result<()> {
    fs::create_dir_all(config_dir())?;
    let json = serde_json::to_string_pretty(cfg).map_err(io::Error::other)?;
    // Write to a temp file first so a crash never leaves a half-written config.
    let tmp = config_dir().join("config.json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, config_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_use_defaults() {
        let cfg: Config = serde_json::from_str(r#"{"target_ip":"1.2.3.4"}"#).unwrap();
        assert_eq!(cfg.target_ip, "1.2.3.4");
        assert_eq!(cfg.check_interval_secs, 60);
        assert!(cfg.notifications);
    }

    #[test]
    fn interval_is_clamped() {
        let cfg = Config {
            check_interval_secs: 1,
            ..Default::default()
        };
        assert_eq!(cfg.interval_secs(), MIN_INTERVAL_SECS);
    }
}
