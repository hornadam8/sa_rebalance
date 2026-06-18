use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const AUTHORIZE_URL: &str = "https://api.schwabapi.com/v1/oauth/authorize";
pub(crate) const TOKEN_URL: &str = "https://api.schwabapi.com/v1/oauth/token";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_at: u64,
    pub refresh_expires_at: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    #[serde(default = "default_refresh_expiry")]
    refresh_token_expires_in: u64,
}

fn default_refresh_expiry() -> u64 {
    7 * 24 * 60 * 60
}

pub fn tokens_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not resolve home dir")?;
    match std::env::var("TOKENS_PATH").ok() {
        Some(s) if s.starts_with("~/") => Ok(home.join(&s[2..])),
        Some(s) if !s.is_empty() => Ok(PathBuf::from(s)),
        _ => Ok(home.join(".config/sa_rebalance/tokens.json")),
    }
}

pub fn save_tokens(tokens: &Tokens) -> Result<()> {
    let path = tokens_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(tokens)?;
    fs::write(&path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn load_tokens() -> Result<Tokens> {
    let path = tokens_path()?;
    let body = fs::read_to_string(&path)
        .with_context(|| format!("reading tokens at {} — have you run `auth`?", path.display()))?;
    Ok(serde_json::from_str(&body)?)
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

pub(crate) async fn refresh(
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    current: &Tokens,
) -> Result<Tokens> {
    let resp = http
        .post(TOKEN_URL)
        .basic_auth(client_id, Some(client_secret))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &current.refresh_token),
        ])
        .send()
        .await
        .context("refreshing Schwab access token")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Schwab refresh returned {}: {}", status, body);
    }

    let tr: TokenResponse = resp.json().await?;
    let now = now_secs();
    Ok(Tokens {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token,
        access_expires_at: now + tr.expires_in,
        refresh_expires_at: current.refresh_expires_at,
    })
}

pub fn authorize_url(client_id: &str, redirect_uri: &str) -> String {
    let mut url = url::Url::parse(AUTHORIZE_URL).expect("static URL");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri);
    url.into()
}

fn extract_code(pasted: &str) -> Option<String> {
    let url = url::Url::parse(pasted.trim()).ok()?;
    url.query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
}

pub async fn run_auth_flow(
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
) -> Result<()> {
    let url = authorize_url(client_id, redirect_uri);
    println!("Open this URL, log into Schwab, approve the app:\n");
    println!("  {url}\n");
    println!(
        "Your browser will be redirected to a URL starting with {redirect_uri} (the page won't \
         load — that's fine). Copy the FULL URL from the address bar and paste below:\n"
    );
    print!("> ");
    io::stdout().flush().ok();

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let code = extract_code(&line)
        .context("could not find `code=` parameter in the pasted URL")?;

    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .basic_auth(client_id, Some(client_secret))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await
        .context("posting to Schwab token endpoint")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Schwab token endpoint returned {}: {}", status, body);
    }

    let tr: TokenResponse = resp.json().await.context("parsing token response")?;
    let now = now_secs();
    let tokens = Tokens {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token,
        access_expires_at: now + tr.expires_in,
        refresh_expires_at: now + tr.refresh_token_expires_in,
    };
    save_tokens(&tokens)?;

    let path = tokens_path()?;
    println!("\nSaved tokens to {}", path.display());
    println!(
        "Re-auth required in ~{} days.",
        tr.refresh_token_expires_in / 86400
    );
    Ok(())
}
