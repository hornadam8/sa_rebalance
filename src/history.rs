use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub equity: f64,
    pub unix_ts: i64,
}

pub type Snapshots = HashMap<String, Snapshot>;

fn snapshots_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home.join(".local/state/sa_rebalance/snapshots.json"))
}

pub fn load() -> Snapshots {
    let Ok(path) = snapshots_path() else {
        return HashMap::new();
    };
    let Ok(body) = fs::read_to_string(&path) else {
        return HashMap::new();
    };
    serde_json::from_str(&body).unwrap_or_default()
}

pub fn save(snaps: &Snapshots) -> Result<()> {
    let path = snapshots_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(snaps)?;
    fs::write(&path, body)?;
    Ok(())
}
