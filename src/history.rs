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

pub type Snapshots = HashMap<String, HashMap<String, Snapshot>>;

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

pub fn most_recent_prior<'a>(
    snaps: &'a Snapshots,
    account: &str,
    today: &str,
) -> Option<&'a Snapshot> {
    let by_date = snaps.get(account)?;
    by_date
        .iter()
        .filter(|(d, _)| d.as_str() < today)
        .max_by_key(|(d, _)| d.as_str().to_string())
        .map(|(_, s)| s)
}

pub fn record_today(snaps: &mut Snapshots, account: String, today: String, snap: Snapshot) {
    snaps.entry(account).or_default().insert(today, snap);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(equity: f64, ts: i64) -> Snapshot {
        Snapshot {
            equity,
            unix_ts: ts,
        }
    }

    #[test]
    fn most_recent_prior_returns_yesterday_when_available() {
        let mut snaps = Snapshots::new();
        let mut by_date = HashMap::new();
        by_date.insert("2026-06-21".to_string(), snap(1000.0, 100));
        by_date.insert("2026-06-22".to_string(), snap(1100.0, 200));
        snaps.insert("ACC1".to_string(), by_date);
        let prior = most_recent_prior(&snaps, "ACC1", "2026-06-23").expect("should find");
        assert_eq!(prior.equity, 1100.0);
    }

    #[test]
    fn most_recent_prior_skips_today_and_returns_yesterday() {
        let mut snaps = Snapshots::new();
        let mut by_date = HashMap::new();
        by_date.insert("2026-06-22".to_string(), snap(1100.0, 200));
        by_date.insert("2026-06-23".to_string(), snap(1150.0, 300));
        snaps.insert("ACC1".to_string(), by_date);
        let prior = most_recent_prior(&snaps, "ACC1", "2026-06-23").expect("should find");
        assert_eq!(prior.equity, 1100.0);
    }

    #[test]
    fn most_recent_prior_returns_friday_after_weekend() {
        let mut snaps = Snapshots::new();
        let mut by_date = HashMap::new();
        by_date.insert("2026-06-19".to_string(), snap(900.0, 50)); // Friday
        snaps.insert("ACC1".to_string(), by_date);
        // Monday 2026-06-22
        let prior = most_recent_prior(&snaps, "ACC1", "2026-06-22").expect("should find");
        assert_eq!(prior.equity, 900.0);
    }

    #[test]
    fn most_recent_prior_returns_none_when_no_prior_data() {
        let mut snaps = Snapshots::new();
        snaps.insert("ACC1".to_string(), HashMap::new());
        let prior = most_recent_prior(&snaps, "ACC1", "2026-06-22");
        assert!(prior.is_none());
    }

    #[test]
    fn most_recent_prior_returns_none_for_unknown_account() {
        let snaps = Snapshots::new();
        let prior = most_recent_prior(&snaps, "MISSING", "2026-06-22");
        assert!(prior.is_none());
    }

    #[test]
    fn record_today_overwrites_existing_entry_for_same_date() {
        let mut snaps = Snapshots::new();
        record_today(&mut snaps, "ACC1".into(), "2026-06-22".into(), snap(1000.0, 100));
        record_today(&mut snaps, "ACC1".into(), "2026-06-22".into(), snap(1050.0, 200));
        let by_date = snaps.get("ACC1").unwrap();
        assert_eq!(by_date.len(), 1);
        assert_eq!(by_date["2026-06-22"].equity, 1050.0);
    }

    #[test]
    fn record_today_preserves_prior_day_entries() {
        let mut snaps = Snapshots::new();
        record_today(&mut snaps, "ACC1".into(), "2026-06-22".into(), snap(1000.0, 100));
        record_today(&mut snaps, "ACC1".into(), "2026-06-23".into(), snap(1100.0, 200));
        let by_date = snaps.get("ACC1").unwrap();
        assert_eq!(by_date.len(), 2);
        assert_eq!(by_date["2026-06-22"].equity, 1000.0);
        assert_eq!(by_date["2026-06-23"].equity, 1100.0);
    }
}
