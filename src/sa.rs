use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

const SCREENER_URL: &str = "https://seekingalpha.com/api/v3/screener_results";
const REFERER: &str = "https://seekingalpha.com/screeners/96793299-Top-Rated-Stocks";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticker {
    pub symbol: String,
    pub company: String,
    pub exchange: String,
}

#[derive(Deserialize)]
struct ScreenerResponse {
    data: Vec<ScreenerItem>,
}

#[derive(Deserialize)]
struct ScreenerItem {
    attributes: ScreenerAttrs,
}

#[derive(Deserialize)]
struct ScreenerAttrs {
    name: String,
    company: String,
    exchange: String,
}

fn filter_body() -> serde_json::Value {
    let grade_in = json!({"in": ["A+", "A", "A-", "B+", "B", "B-"]});
    let rating_in = json!({"in": ["strong_buy", "buy"]});
    json!({
        "filter": {
            "quant_rating": rating_in,
            "authors_rating": rating_in,
            "sell_side_rating": rating_in,
            "value_category": grade_in,
            "growth_category": grade_in,
            "profitability_category": grade_in,
            "momentum_category": grade_in,
            "eps_revisions_category": grade_in,
        },
        "page": 1,
        "per_page": 100,
        "sort": "-quant_rating",
        "total_count": true,
        "type": "stock",
    })
}

pub async fn fetch_top_rated(sa_cookie: &str) -> Result<(Vec<Ticker>, String)> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("building http client")?;

    let resp = client
        .post(SCREENER_URL)
        .header("accept", "application/json")
        .header("accept-language", "en-US,en;q=0.8")
        .header("content-type", "application/json")
        .header("cookie", sa_cookie)
        .header("origin", "https://seekingalpha.com")
        .header("referer", REFERER)
        .json(&filter_body())
        .send()
        .await
        .context("sending SA screener request")?;

    let status = resp.status();
    let updated_cookies = merge_set_cookies(sa_cookie, resp.headers());

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("SA screener returned {}: {}", status, body);
    }

    let parsed: ScreenerResponse = resp.json().await.context("parsing SA response")?;
    let tickers = parsed
        .data
        .into_iter()
        .map(|item| Ticker {
            symbol: item.attributes.name,
            company: item.attributes.company,
            exchange: item.attributes.exchange,
        })
        .collect();
    Ok((tickers, updated_cookies))
}

fn merge_set_cookies(current: &str, headers: &reqwest::header::HeaderMap) -> String {
    let set_cookies: Vec<String> = headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(String::from))
        .collect();
    merge_set_cookies_raw(current, &set_cookies)
}

fn merge_set_cookies_raw(current: &str, set_cookies: &[String]) -> String {
    use std::collections::BTreeMap;
    let mut jar: BTreeMap<String, String> = current
        .split(';')
        .filter_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            Some((name.to_string(), value.to_string()))
        })
        .collect();

    for s in set_cookies {
        let main_part = s.split(';').next().unwrap_or("");
        if let Some((name, value)) = main_part.split_once('=') {
            jar.insert(name.trim().to_string(), value.trim().to_string());
        }
    }

    jar.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_adds_new_cookie() {
        let current = "session_id=ABC";
        let set = vec!["new_cookie=XYZ; Path=/; HttpOnly".to_string()];
        let merged = merge_set_cookies_raw(current, &set);
        assert!(merged.contains("session_id=ABC"));
        assert!(merged.contains("new_cookie=XYZ"));
    }

    #[test]
    fn merge_replaces_existing_value() {
        let current = "session_id=OLD; user_id=42";
        let set = vec!["session_id=NEW; Path=/".to_string()];
        let merged = merge_set_cookies_raw(current, &set);
        assert!(merged.contains("session_id=NEW"));
        assert!(!merged.contains("session_id=OLD"));
        assert!(merged.contains("user_id=42"));
    }

    #[test]
    fn merge_strips_set_cookie_attributes() {
        let set = vec!["foo=bar; Expires=Wed, 01 Jan 2027 00:00:00 GMT; Path=/; HttpOnly; Secure".to_string()];
        let merged = merge_set_cookies_raw("", &set);
        assert_eq!(merged.trim(), "foo=bar");
    }

    #[test]
    fn merge_with_empty_input_returns_only_new() {
        let set = vec!["a=1".to_string(), "b=2".to_string()];
        let merged = merge_set_cookies_raw("", &set);
        assert!(merged.contains("a=1"));
        assert!(merged.contains("b=2"));
    }

    #[test]
    fn merge_with_no_set_cookies_returns_current() {
        let current = "x=1; y=2";
        let merged = merge_set_cookies_raw(current, &[]);
        assert!(merged.contains("x=1"));
        assert!(merged.contains("y=2"));
    }
}
