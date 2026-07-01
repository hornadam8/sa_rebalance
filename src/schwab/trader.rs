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
    trader_base: String,
    marketdata_base: String,
    token_url: String,
    refresh_retry_delay: std::time::Duration,
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
            trader_base: TRADER_BASE.to_string(),
            marketdata_base: MARKETDATA_BASE.to_string(),
            token_url: auth::TOKEN_URL.to_string(),
            refresh_retry_delay: std::time::Duration::from_secs(300),
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
                    "token refresh attempt {} failed, sleeping {}s and retrying once...",
                    attempt,
                    self.refresh_retry_delay.as_secs()
                );
                tokio::time::sleep(self.refresh_retry_delay).await;
            }
            match auth::refresh(
                &self.http,
                &self.client_id,
                &self.client_secret,
                &self.token_url,
                &self.tokens,
            )
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
        self.get_url(&format!("{}/accounts/accountNumbers", self.trader_base))
            .await
    }

    pub async fn accounts_with_positions_raw(&self) -> Result<Value> {
        self.get_url(&format!("{}/accounts?fields=positions", self.trader_base))
            .await
    }

    pub async fn is_equity_market_open(&self) -> Result<bool> {
        let url = format!("{}/markets?markets=equity", self.marketdata_base);
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
        let url = format!("{}/accounts/{account_hash}/orders", self.trader_base);
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
        let url = format!("{}/accounts/{account_hash}/orders/{order_id}", self.trader_base);
        self.get_url(&url).await
    }

    pub async fn cancel_order(&self, account_hash: &str, order_id: &str) -> Result<()> {
        let url = format!("{}/accounts/{account_hash}/orders/{order_id}", self.trader_base);
        let resp = self
            .http
            .delete(&url)
            .bearer_auth(&self.tokens.access_token)
            .send()
            .await
            .with_context(|| format!("DELETE order {order_id}"))?;
        let status = resp.status();
        // 404 means it already completed (filled or already cancelled) — treat as ok
        if !status.is_success() && status.as_u16() != 404 {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("cancel order {order_id} returned {status}: {body}");
        }
        Ok(())
    }

    /// Cancel a live order and check whether it managed to fill before the cancel landed.
    /// Returns `Some(order)` if the order filled (record as a fill),
    /// or `None` if it was cancelled (buying power freed).
    pub async fn cancel_and_check(
        &self,
        account_hash: &str,
        order_id: &str,
    ) -> Result<Option<Value>> {
        // Best-effort cancel; ignore errors (order may already be terminal)
        let _ = self.cancel_order(account_hash, order_id).await;
        // Give Schwab time to process the cancel
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let order = self.get_order(account_hash, order_id).await?;
        let status = order
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN");
        if status == "FILLED" || status == "PARTIALLY_FILLED" {
            // Raced to a fill (full or partial) just before the cancel — record whatever filled
            Ok(Some(order))
        } else {
            // Cancelled/expired/rejected — buying power freed
            Ok(None)
        }
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
                    let reason = extract_rejection_reason(&order);
                    eprintln!(
                        "order {order_id} {status} — full order JSON: {}",
                        serde_json::to_string(&order).unwrap_or_default()
                    );
                    if let Some(r) = reason {
                        anyhow::bail!("order {} {}: {}", order_id, status, r);
                    } else {
                        anyhow::bail!("order {} {} (no reason field; see launchd.err.log)", order_id, status);
                    }
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
            "{}/quotes?symbols={}&fields=quote",
            self.marketdata_base,
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
        let url = format!("{}/accounts/{account_hash}/orders", self.trader_base);
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

fn extract_rejection_reason(order: &Value) -> Option<String> {
    let str_field = |obj: &Value, key: &str| -> Option<String> {
        obj.get(key).and_then(Value::as_str).map(str::to_string)
    };
    for key in &["statusDescription", "rejectReason", "rejectionReason", "cancelTime"] {
        if let Some(s) = str_field(order, key) {
            return Some(s);
        }
    }
    if let Some(activities) = order.get("orderActivityCollection").and_then(Value::as_array) {
        for a in activities {
            for key in &["rejectionReason", "statusDescription", "rejectReason"] {
                if let Some(s) = str_field(a, key) {
                    return Some(s);
                }
            }
        }
    }
    None
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
    fn extract_rejection_reason_finds_status_description() {
        let order = serde_json::json!({
            "status": "REJECTED",
            "statusDescription": "Symbol restricted from electronic trading"
        });
        assert_eq!(
            extract_rejection_reason(&order).as_deref(),
            Some("Symbol restricted from electronic trading")
        );
    }

    #[test]
    fn extract_rejection_reason_finds_reject_reason() {
        let order = serde_json::json!({
            "status": "REJECTED",
            "rejectReason": "Insufficient buying power"
        });
        assert_eq!(
            extract_rejection_reason(&order).as_deref(),
            Some("Insufficient buying power")
        );
    }

    #[test]
    fn extract_rejection_reason_finds_nested_activity_reason() {
        let order = serde_json::json!({
            "status": "REJECTED",
            "orderActivityCollection": [{
                "activityType": "EXECUTION",
                "rejectionReason": "Order would create wash sale"
            }]
        });
        assert_eq!(
            extract_rejection_reason(&order).as_deref(),
            Some("Order would create wash sale")
        );
    }

    #[test]
    fn extract_rejection_reason_returns_none_when_no_fields_present() {
        let order = serde_json::json!({"status": "REJECTED"});
        assert!(extract_rejection_reason(&order).is_none());
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

    fn test_tokens_valid() -> Tokens {
        Tokens {
            access_token: "valid_access".into(),
            refresh_token: "valid_refresh".into(),
            access_expires_at: u64::MAX,
            refresh_expires_at: u64::MAX,
        }
    }

    fn test_tokens_expired_access() -> Tokens {
        Tokens {
            access_token: "expired_access".into(),
            refresh_token: "valid_refresh".into(),
            access_expires_at: 1,
            refresh_expires_at: u64::MAX,
        }
    }

    fn test_client(server_uri: &str, tokens: Tokens) -> Client {
        Client {
            http: reqwest::Client::new(),
            tokens,
            client_id: "test_id".into(),
            client_secret: "test_secret".into(),
            trader_base: format!("{}/trader/v1", server_uri),
            marketdata_base: format!("{}/marketdata/v1", server_uri),
            token_url: format!("{}/v1/oauth/token", server_uri),
            refresh_retry_delay: std::time::Duration::from_millis(5),
        }
    }

    fn point_tokens_path_at_tempfile() {
        std::env::set_var("TOKENS_PATH", "/tmp/sa_rebalance_http_test_tokens.json");
    }

    #[tokio::test]
    async fn ensure_fresh_retries_after_transient_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        point_tokens_path_at_tempfile();
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(400).set_body_string("transient"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh_access",
                "refresh_token": "fresh_refresh",
                "expires_in": 1800,
                "refresh_token_expires_in": 604800,
            })))
            .mount(&server)
            .await;

        let mut client = test_client(&server.uri(), test_tokens_expired_access());
        client.ensure_fresh().await.expect("should succeed via retry");
        assert_eq!(client.tokens.access_token, "fresh_access");
    }

    #[tokio::test]
    async fn ensure_fresh_bails_when_both_attempts_fail() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        point_tokens_path_at_tempfile();
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server error"))
            .mount(&server)
            .await;

        let mut client = test_client(&server.uri(), test_tokens_expired_access());
        let result = client.ensure_fresh().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn ensure_fresh_no_refresh_when_access_still_valid() {
        use wiremock::MockServer;

        point_tokens_path_at_tempfile();
        let server = MockServer::start().await;

        let mut client = test_client(&server.uri(), test_tokens_valid());
        let original_access = client.tokens.access_token.clone();
        client.ensure_fresh().await.expect("no-op should succeed");
        assert_eq!(client.tokens.access_token, original_access);
    }

    #[tokio::test]
    async fn quotes_parses_multi_symbol_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketdata/v1/quotes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "AAPL": {"quote": {"bidPrice": 150.0, "askPrice": 151.0, "mark": 150.5}},
                "MSFT": {"quote": {"lastPrice": 300.0}}
            })))
            .mount(&server)
            .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        let q = client.quotes(&["AAPL".into(), "MSFT".into()]).await.unwrap();
        assert_eq!(q["AAPL"].mark, 150.5);
        assert_eq!(q["AAPL"].ask, 151.0);
        assert_eq!(q["MSFT"].mark, 300.0);
    }

    #[tokio::test]
    async fn place_market_order_extracts_order_id_from_location() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/trader/v1/accounts/HASH/orders"))
            .respond_with(
                ResponseTemplate::new(201).insert_header(
                    "Location",
                    "https://api.schwabapi.com/trader/v1/accounts/HASH/orders/1234567890",
                ),
            )
            .mount(&server)
            .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        let id = client
            .place_market_order("HASH", "AAPL", "BUY", 10)
            .await
            .unwrap();
        assert_eq!(id, "1234567890");
    }

    #[tokio::test]
    async fn place_market_order_propagates_schwab_rejection() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/trader/v1/accounts/HASH/orders"))
            .respond_with(ResponseTemplate::new(422).set_body_string("symbol restricted"))
            .mount(&server)
            .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        let result = client.place_market_order("HASH", "SHIP", "BUY", 10).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("422"), "expected 422 in error: {msg}");
        assert!(msg.contains("symbol restricted"));
    }

    #[tokio::test]
    async fn place_limit_order_sends_limit_type_and_price() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/trader/v1/accounts/HASH/orders"))
            .respond_with(
                ResponseTemplate::new(201).insert_header("Location", "/orders/777"),
            )
            .mount(&server)
            .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        let id = client
            .place_limit_order("HASH", "OTCSYM", "BUY", 5, 45.67)
            .await
            .unwrap();
        assert_eq!(id, "777");

        let recorded = server.received_requests().await.expect("requests recorded");
        let body: serde_json::Value =
            serde_json::from_slice(&recorded[0].body).expect("json body");
        assert_eq!(body["orderType"], "LIMIT");
        assert_eq!(body["price"], "45.6700");
        assert_eq!(body["orderLegCollection"][0]["instruction"], "BUY");
        assert_eq!(body["orderLegCollection"][0]["quantity"], 5);
    }

    #[tokio::test]
    async fn is_equity_market_open_returns_false_when_closed() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketdata/v1/markets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "equity": {"EQ": {"isOpen": false}}
            })))
            .mount(&server)
            .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        assert!(!client.is_equity_market_open().await.unwrap());
    }

    #[tokio::test]
    async fn is_equity_market_open_returns_true_when_any_sub_market_open() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketdata/v1/markets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "equity": {"EQ": {"isOpen": true}}
            })))
            .mount(&server)
            .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        assert!(client.is_equity_market_open().await.unwrap());
    }

    #[tokio::test]
    async fn await_filled_returns_on_filled_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/trader/v1/accounts/HASH/orders/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "FILLED",
                "filledQuantity": 10,
                "orderActivityCollection": [{
                    "executionLegs": [{"quantity": 10, "price": 150.25}]
                }]
            })))
            .mount(&server)
            .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        let order = client
            .await_filled("HASH", "42", std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(order["status"], "FILLED");
    }

    #[tokio::test]
    async fn await_filled_bails_on_rejected_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/trader/v1/accounts/HASH/orders/99"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "REJECTED"
            })))
            .mount(&server)
            .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        let result = client
            .await_filled("HASH", "99", std::time::Duration::from_secs(1))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("REJECTED"));
    }

    // --- cancel_and_check tests ---
    // Helper: build a mock server with a DELETE endpoint for cancel and a GET endpoint for order status.
    async fn cancel_and_check_server(
        order_id: &str,
        order_json: serde_json::Value,
    ) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // DELETE — cancel succeeds
        Mock::given(method("DELETE"))
            .and(path(format!("/trader/v1/accounts/HASH/orders/{order_id}")))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        // GET — return the provided order state
        Mock::given(method("GET"))
            .and(path(format!("/trader/v1/accounts/HASH/orders/{order_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(order_json))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn cancel_and_check_returns_some_when_filled() {
        let server = cancel_and_check_server(
            "99",
            serde_json::json!({
                "status": "FILLED",
                "filledQuantity": 170,
                "orderActivityCollection": [{
                    "executionLegs": [{"quantity": 170, "price": 58.85}]
                }]
            }),
        )
        .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        let result = client.cancel_and_check("HASH", "99").await.unwrap();
        assert!(result.is_some(), "FILLED order should return Some(order)");
        assert_eq!(result.unwrap()["status"], "FILLED");
    }

    #[tokio::test]
    async fn cancel_and_check_returns_some_when_partially_filled() {
        // This is the LNVGY scenario: 19 of 170 shares filled before the cancel landed.
        let server = cancel_and_check_server(
            "100",
            serde_json::json!({
                "status": "PARTIALLY_FILLED",
                "filledQuantity": 19,
                "orderActivityCollection": [{
                    "executionLegs": [{"quantity": 19, "price": 58.85}]
                }]
            }),
        )
        .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        let result = client.cancel_and_check("HASH", "100").await.unwrap();
        assert!(result.is_some(), "PARTIALLY_FILLED order should return Some(order)");
        let order = result.unwrap();
        assert_eq!(order["status"], "PARTIALLY_FILLED");
        assert_eq!(order["filledQuantity"], 19);
    }

    #[tokio::test]
    async fn cancel_and_check_returns_none_when_cancelled() {
        let server = cancel_and_check_server(
            "101",
            serde_json::json!({"status": "CANCELED"}),
        )
        .await;

        let client = test_client(&server.uri(), test_tokens_valid());
        let result = client.cancel_and_check("HASH", "101").await.unwrap();
        assert!(result.is_none(), "CANCELED order should return None");
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
