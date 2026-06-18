use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct Env {
    pub sa_cookie: String,
    pub schwab_client_id: String,
    pub schwab_client_secret: String,
    pub schwab_redirect_uri: String,
    pub schwab_rebalance_accounts: Vec<String>,
    pub gmail_user: String,
    pub gmail_app_password: String,
    pub notify_to: String,
}

impl Env {
    pub fn load() -> Result<Self> {
        let _ = dotenvy::dotenv();
        let csv = var("SCHWAB_REBALANCE_ACCOUNTS")?;
        let accounts: Vec<String> = csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        Ok(Self {
            sa_cookie: var("SA_COOKIE")?,
            schwab_client_id: var("SCHWAB_CLIENT_ID")?,
            schwab_client_secret: var("SCHWAB_CLIENT_SECRET")?,
            schwab_redirect_uri: var("SCHWAB_REDIRECT_URI")?,
            schwab_rebalance_accounts: accounts,
            gmail_user: var("GMAIL_USER")?,
            gmail_app_password: var("GMAIL_APP_PASSWORD")?,
            notify_to: var("NOTIFY_TO")?,
        })
    }
}

fn var(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{key} not set (see .env.example)"))
}

pub fn blocklist_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config/blocklist.txt")
}

pub fn load_blocklist(path: &Path) -> Result<HashSet<String>> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("reading blocklist at {}", path.display()))?;
    Ok(body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_ascii_uppercase())
        .collect())
}
