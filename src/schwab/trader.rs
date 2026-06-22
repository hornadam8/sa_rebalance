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
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..2u32 {
            if attempt > 0 {
                eprintln!(
                    "token refresh attempt {} failed, sleeping 300s and retrying once...",
                    attempt
                );
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            }
            match auth::refresh(&self.http, &self.client_id, &self.client_secret, &self.tokens)
                .await
            {
                Ok(refreshed) => {
                    auth::save_tokens(&refreshed)?;
                    self.tokens = refreshed;
                    return Ok(());
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap())
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

    pub async fn quotes(&self, symbols: &[String]) -> Result<HashMap<String, Quote>> {
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
            if let Some(q) = extract_quote(body) {
                out.insert(sym.clone(), q);
            }
        }
        Ok(out)
    }

    pub async fn place_limit_order(
        &self,
        account_hash: &str,
        symbol: &str,
        instruction: &str,
        quantity: u32,
        limit_price: f64,
    ) -> Result<String> {
        let body = serde_json::json!({
            "orderType": "LIMIT",
            "session": "NORMAL",
            "duration": "DAY",
            "orderStrategyType": "SINGLE",
            "price": format!("{:.4}", limit_price),
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
            anyhow::bail!(
                "place limit {} {} {} @ {} returned {}: {}",
                instruction,
                quantity,
                symbol,
                limit_price,
                status,
                body
            );
        }
        let location = resp
            .headers()
            .get("Location")
            .and_then(|v| v.to_str().ok())
            .context("missing Location header on order placement")?;
        Ok(location
            .rsplit('/')
            .next()
            .context("malformed Location header")?
            .to_string())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Quote {
    pub bid: f64,
    pub ask: f64,
    pub mark: f64,
}

impl Quote {
    pub fn price(&self) -> f64 {
        if self.mark > 0.0 {
            self.mark
        } else if self.bid > 0.0 && self.ask > 0.0 {
            (self.bid + self.ask) / 2.0
        } else if self.ask > 0.0 {
            self.ask
        } else if self.bid > 0.0 {
            self.bid
        } else {
            0.0
        }
    }
}

fn extract_quote(body: &Value) -> Option<Quote> {
    let q = body.get("quote")?;
    let mark = q.get("mark").and_then(Value::as_f64)
        .or_else(|| q.get("lastPrice").and_then(Value::as_f64))
        .unwrap_or(0.0);
    let bid = q.get("bidPrice").and_then(Value::as_f64).unwrap_or(0.0);
    let ask = q.get("askPrice").and_then(Value::as_f64).unwrap_or(0.0);
    if mark <= 0.0 && bid <= 0.0 && ask <= 0.0 {
        return None;
    }
    Some(Quote { bid, ask, mark })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quote_price_prefers_mark() {
        let q = Quote { bid: 9.0, ask: 11.0, mark: 10.0 };
        assert_eq!(q.price(), 10.0);
    }

    #[test]
    fn quote_price_falls_back_to_bid_ask_mid() {
        let q = Quote { bid: 100.0, ask: 102.0, mark: 0.0 };
        assert_eq!(q.price(), 101.0);
    }

    #[test]
    fn quote_price_uses_ask_when_only_ask() {
        let q = Quote { bid: 0.0, ask: 50.0, mark: 0.0 };
        assert_eq!(q.price(), 50.0);
    }

    #[test]
    fn quote_price_zero_when_all_zero() {
        let q = Quote { bid: 0.0, ask: 0.0, mark: 0.0 };
        assert_eq!(q.price(), 0.0);
    }

    #[test]
    fn extract_quote_reads_all_three_fields() {
        let body = json!({
            "quote": { "bidPrice": 9.5, "askPrice": 10.5, "mark": 10.0 }
        });
        let q = extract_quote(&body).expect("should parse");
        assert_eq!(q.bid, 9.5);
        assert_eq!(q.ask, 10.5);
        assert_eq!(q.mark, 10.0);
    }

    #[test]
    fn extract_quote_falls_back_to_last_price() {
        let body = json!({ "quote": { "lastPrice": 42.0 } });
        let q = extract_quote(&body).expect("should parse");
        assert_eq!(q.mark, 42.0);
    }

    #[test]
    fn extract_quote_returns_none_for_empty() {
        let body = json!({ "quote": {} });
        assert!(extract_quote(&body).is_none());
    }

    #[test]
    fn parse_accounts_extracts_basic_account_data() {
        let numbers = json!([
            {"accountNumber": "12345", "hashValue": "HASH123"}
        ]);
        let accounts = json!([{
            "securitiesAccount": {
                "type": "MARGIN",
                "accountNumber": "12345",
                "currentBalances": {
                    "liquidationValue": 50000.0,
                    "cashBalance": 1000.0,
                },
                "positions": [{
                    "instrument": {"assetType": "EQUITY", "symbol": "AAPL"},
                    "longQuantity": 10.0,
                    "shortQuantity": 0.0,
                    "marketValue": 1500.0,
                    "averagePrice": 150.0,
                }]
            }
        }]);
        let parsed = parse_accounts(&numbers, &accounts).expect("should parse");
        assert_eq!(parsed.len(), 1);
        let a = &parsed[0];
        assert_eq!(a.account_number, "12345");
        assert_eq!(a.account_hash, "HASH123");
        assert_eq!(a.equity, 50000.0);
        assert_eq!(a.cash, 1000.0);
        assert_eq!(a.positions.len(), 1);
        assert_eq!(a.positions[0].symbol, "AAPL");
        assert_eq!(a.positions[0].quantity, 10.0);
    }

    #[test]
    fn parse_accounts_skips_non_equity_positions() {
        let numbers = json!([{"accountNumber": "12345", "hashValue": "HASH"}]);
        let accounts = json!([{
            "securitiesAccount": {
                "type": "MARGIN",
                "accountNumber": "12345",
                "currentBalances": {"liquidationValue": 1000.0, "cashBalance": 100.0},
                "positions": [
                    {"instrument": {"assetType": "EQUITY", "symbol": "AAPL"}, "longQuantity": 5.0, "shortQuantity": 0.0, "marketValue": 750.0, "averagePrice": 150.0},
                    {"instrument": {"assetType": "OPTION", "symbol": "AAPL  240517P00150000"}, "longQuantity": 1.0, "shortQuantity": 0.0, "marketValue": 100.0, "averagePrice": 100.0},
                ]
            }
        }]);
        let parsed = parse_accounts(&numbers, &accounts).expect("should parse");
        assert_eq!(parsed[0].positions.len(), 1);
        assert_eq!(parsed[0].positions[0].symbol, "AAPL");
    }

    #[test]
    fn parse_accounts_falls_back_to_derived_equity() {
        let numbers = json!([{"accountNumber": "X", "hashValue": "Y"}]);
        let accounts = json!([{
            "securitiesAccount": {
                "type": "CASH",
                "accountNumber": "X",
                "currentBalances": {
                    "cashBalance": 500.0,
                    "longMarketValue": 1500.0,
                },
                "positions": []
            }
        }]);
        let parsed = parse_accounts(&numbers, &accounts).expect("should parse");
        // cash + longMarketValue - shortMarketValue = 500 + 1500 = 2000
        assert_eq!(parsed[0].equity, 2000.0);
    }
}
