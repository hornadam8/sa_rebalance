use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;

use crate::config::Env;
use crate::schwab::auth::{self, Tokens};

const TRADER_BASE: &str = "https://api.schwabapi.com/trader/v1";
const MARKETDATA_BASE: &str = "https://api.schwabapi.com/marketdata/v1";

pub struct Client {
    http: reqwest::Client,
    tokens: Tokens,
    client_id: String,
    client_secret: String,
}

#[derive(Debug)]
pub struct Account {
    pub account_number: String,
    pub account_hash: String,
    pub account_type: String,
    pub equity: f64,
    pub cash: f64,
    pub positions: Vec<Position>,
}

#[derive(Debug)]
pub struct Position {
    pub symbol: String,
    pub quantity: f64,
    pub market_value: f64,
    pub average_price: f64,
}

impl Client {
    pub async fn new(env: &Env) -> Result<Self> {
        let mut c = Self {
            http: reqwest::Client::new(),
            tokens: auth::load_tokens()?,
            client_id: env.schwab_client_id.clone(),
            client_secret: env.schwab_client_secret.clone(),
        };
        c.ensure_fresh().await?;
        Ok(c)
    }

    async fn ensure_fresh(&mut self) -> Result<()> {
        let now = auth::now_secs();
        if self.tokens.access_expires_at > now + 30 {
            return Ok(());
        }
        if self.tokens.refresh_expires_at <= now {
            anyhow::bail!("Schwab refresh token expired — re-run `sa_rebalance auth`");
        }
        let refreshed =
            auth::refresh(&self.http, &self.client_id, &self.client_secret, &self.tokens).await?;
        auth::save_tokens(&refreshed)?;
        self.tokens = refreshed;
        Ok(())
    }

    pub fn days_until_reauth(&self) -> i64 {
        let now = auth::now_secs() as i64;
        (self.tokens.refresh_expires_at as i64 - now) / 86400
    }

    async fn get_url(&self, url: &str) -> Result<Value> {
        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.tokens.access_token)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {} returned {}: {}", url, status, body);
        }
        Ok(resp.json().await?)
    }

    pub async fn account_numbers_raw(&self) -> Result<Value> {
        self.get_url(&format!("{TRADER_BASE}/accounts/accountNumbers"))
            .await
    }

    pub async fn accounts_with_positions_raw(&self) -> Result<Value> {
        self.get_url(&format!("{TRADER_BASE}/accounts?fields=positions"))
            .await
    }

    pub async fn is_equity_market_open(&self) -> Result<bool> {
        let url = format!("{MARKETDATA_BASE}/markets?markets=equity");
        let json = self.get_url(&url).await?;
        let equity = json.get("equity").context("no equity in markets response")?;
        for (_key, market) in equity.as_object().context("equity is not object")? {
            if market
                .get("isOpen")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn place_market_order(
        &self,
        account_hash: &str,
        symbol: &str,
        instruction: &str,
        quantity: u32,
    ) -> Result<String> {
        let body = serde_json::json!({
            "orderType": "MARKET",
            "session": "NORMAL",
            "duration": "DAY",
            "orderStrategyType": "SINGLE",
            "orderLegCollection": [{
                "instruction": instruction,
                "quantity": quantity,
                "instrument": {
                    "symbol": symbol,
                    "assetType": "EQUITY",
                }
            }]
        });
        let url = format!("{TRADER_BASE}/accounts/{account_hash}/orders");
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.tokens.access_token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("place order {} {} returned {}: {}", instruction, symbol, status, body);
        }
        let location = resp
            .headers()
            .get("Location")
            .and_then(|v| v.to_str().ok())
            .context("missing Location header on order placement")?;
        let order_id = location
            .rsplit('/')
            .next()
            .context("malformed Location header")?
            .to_string();
        Ok(order_id)
    }

    pub async fn get_order(&self, account_hash: &str, order_id: &str) -> Result<Value> {
        let url = format!("{TRADER_BASE}/accounts/{account_hash}/orders/{order_id}");
        self.get_url(&url).await
    }

    pub async fn await_filled(
        &self,
        account_hash: &str,
        order_id: &str,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let start = std::time::Instant::now();
        loop {
            let order = self.get_order(account_hash, order_id).await?;
            let status = order
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("UNKNOWN");
            match status {
                "FILLED" => return Ok(order),
                "REJECTED" | "CANCELED" | "EXPIRED" => {
                    anyhow::bail!("order {} ended with status {}", order_id, status);
                }
                _ => {}
            }
            if start.elapsed() > timeout {
                anyhow::bail!("order {} timed out in status {}", order_id, status);
            }
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
    }

    pub async fn quotes(&self, symbols: &[String]) -> Result<HashMap<String, f64>> {
        if symbols.is_empty() {
            return Ok(HashMap::new());
        }
        let url = format!(
            "{MARKETDATA_BASE}/quotes?symbols={}&fields=quote",
            symbols.join(","),
        );
        let json = self.get_url(&url).await?;
        let obj = json
            .as_object()
            .context("quotes response is not an object")?;
        let mut out = HashMap::new();
        for (sym, body) in obj {
            if let Some(price) = extract_price(body) {
                out.insert(sym.clone(), price);
            }
        }
        Ok(out)
    }
}

fn extract_price(body: &Value) -> Option<f64> {
    let q = body.get("quote")?;
    q.get("mark")
        .and_then(Value::as_f64)
        .or_else(|| q.get("lastPrice").and_then(Value::as_f64))
        .or_else(|| {
            let bid = q.get("bidPrice").and_then(Value::as_f64)?;
            let ask = q.get("askPrice").and_then(Value::as_f64)?;
            Some((bid + ask) / 2.0)
        })
}

pub fn parse_accounts(numbers: &Value, accounts: &Value) -> Result<Vec<Account>> {
    let hash_for = |num: &str| -> Option<String> {
        numbers.as_array()?.iter().find_map(|item| {
            if item.get("accountNumber").and_then(Value::as_str)? == num {
                item.get("hashValue")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            } else {
                None
            }
        })
    };

    let arr = accounts
        .as_array()
        .context("accounts response is not an array")?;
    let mut out = Vec::new();
    for item in arr {
        let sa = &item["securitiesAccount"];
        let account_number = sa["accountNumber"]
            .as_str()
            .context("missing accountNumber")?
            .to_string();
        let account_hash = hash_for(&account_number)
            .with_context(|| format!("no hash for account {account_number}"))?;
        let account_type = sa["type"].as_str().unwrap_or("").to_string();
        let bal = &sa["currentBalances"];
        let equity = bal["liquidationValue"]
            .as_f64()
            .or_else(|| bal["equity"].as_f64())
            .or_else(|| {
                let cash = bal["cashBalance"].as_f64()?;
                let lmv = bal["longMarketValue"].as_f64().unwrap_or(0.0);
                let smv = bal["shortMarketValue"].as_f64().unwrap_or(0.0);
                Some(cash + lmv - smv)
            })
            .context("could not derive equity from currentBalances")?;
        let cash = bal["cashBalance"]
            .as_f64()
            .or_else(|| bal["cashAvailableForTrading"].as_f64())
            .unwrap_or(0.0);

        let mut positions = Vec::new();
        if let Some(pos_arr) = sa["positions"].as_array() {
            for p in pos_arr {
                let inst = &p["instrument"];
                if inst["assetType"].as_str() != Some("EQUITY") {
                    continue;
                }
                let long_q = p["longQuantity"].as_f64().unwrap_or(0.0);
                let short_q = p["shortQuantity"].as_f64().unwrap_or(0.0);
                positions.push(Position {
                    symbol: inst["symbol"].as_str().unwrap_or("").to_string(),
                    quantity: long_q - short_q,
                    market_value: p["marketValue"].as_f64().unwrap_or(0.0),
                    average_price: p["averagePrice"].as_f64().unwrap_or(0.0),
                });
            }
        }
        positions.sort_by(|a, b| b.market_value.partial_cmp(&a.market_value).unwrap());

        out.push(Account {
            account_number,
            account_hash,
            account_type,
            equity,
            cash,
            positions,
        });
    }
    Ok(out)
}
