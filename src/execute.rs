use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;

use crate::rebalance::{AccountPlan, Side, Trade};
use crate::sa::Ticker;
use crate::schwab::trader::{Client, Quote};

async fn place_order_routed(
    client: &Client,
    account_hash: &str,
    symbol: &str,
    side: Side,
    shares: u32,
    quote: Option<&Quote>,
    exchange: Option<&str>,
) -> Result<String> {
    let instruction = match side {
        Side::Buy => "BUY",
        Side::Sell => "SELL",
    };
    let use_limit = matches!(side, Side::Buy) && exchange == Some("OTCMKTS");
    if use_limit {
        let limit_price = quote
            .and_then(|q| {
                if q.ask > 0.0 {
                    Some(q.ask)
                } else if q.mark > 0.0 {
                    Some(q.mark * 1.005)
                } else {
                    None
                }
            })
            .unwrap_or(0.0);
        if limit_price <= 0.0 {
            anyhow::bail!("no ask/mark price available to set limit for OTC buy {symbol}");
        }
        client
            .place_limit_order(account_hash, symbol, instruction, shares, limit_price)
            .await
    } else {
        client
            .place_market_order(account_hash, symbol, instruction, shares)
            .await
    }
}

const FILL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct Fill {
    pub trade: Trade,
    pub filled_quantity: u32,
    pub avg_price: f64,
}

#[derive(Debug, Clone)]
pub struct OrderFailure {
    pub trade: Trade,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct AccountExecutionReport {
    pub account_number: String,
    pub fills: Vec<Fill>,
    pub failures: Vec<OrderFailure>,
}

pub async fn execute_plan(
    client: &Client,
    plan: &AccountPlan,
    quotes: &HashMap<String, Quote>,
    exchanges: &HashMap<String, String>,
) -> AccountExecutionReport {
    let mut report = AccountExecutionReport {
        account_number: plan.account_number.clone(),
        ..Default::default()
    };

    place_and_collect(client, &plan.account_hash, &plan.sells, quotes, exchanges, &mut report).await;
    place_and_collect(client, &plan.account_hash, &plan.buys, quotes, exchanges, &mut report).await;

    report
}

async fn place_and_collect(
    client: &Client,
    account_hash: &str,
    trades: &[Trade],
    quotes: &HashMap<String, Quote>,
    exchanges: &HashMap<String, String>,
    report: &mut AccountExecutionReport,
) {
    let mut pending: Vec<(Trade, String)> = Vec::new();
    for trade in trades {
        let q = quotes.get(&trade.symbol);
        let ex = exchanges.get(&trade.symbol).map(String::as_str);
        match place_order_routed(client, account_hash, &trade.symbol, trade.side, trade.shares, q, ex).await {
            Ok(order_id) => pending.push((trade.clone(), order_id)),
            Err(e) => report.failures.push(OrderFailure {
                trade: trade.clone(),
                reason: e.to_string(),
            }),
        }
    }

    for (trade, order_id) in pending {
        match client.await_filled(account_hash, &order_id, FILL_TIMEOUT).await {
            Ok(order) => {
                let (qty, price) = extract_fill(&order);
                report.fills.push(Fill {
                    trade,
                    filled_quantity: qty,
                    avg_price: price,
                });
            }
            Err(e) => report.failures.push(OrderFailure {
                trade,
                reason: e.to_string(),
            }),
        }
    }
}

fn extract_fill(order: &Value) -> (u32, f64) {
    let qty = order
        .get("filledQuantity")
        .and_then(Value::as_f64)
        .unwrap_or(0.0) as u32;
    let mut total_value = 0.0;
    let mut total_qty = 0.0;
    if let Some(activities) = order.get("orderActivityCollection").and_then(Value::as_array) {
        for activity in activities {
            if let Some(legs) = activity.get("executionLegs").and_then(Value::as_array) {
                for leg in legs {
                    let q = leg.get("quantity").and_then(Value::as_f64).unwrap_or(0.0);
                    let p = leg.get("price").and_then(Value::as_f64).unwrap_or(0.0);
                    total_value += q * p;
                    total_qty += q;
                }
            }
        }
    }
    let avg_price = if total_qty > 0.0 {
        total_value / total_qty
    } else {
        0.0
    };
    (qty, avg_price)
}

pub async fn run_execute(
    client: &Client,
    plans: &[AccountPlan],
    spares: &[Ticker],
    quotes: &HashMap<String, Quote>,
    exchanges: &HashMap<String, String>,
) -> Result<Vec<AccountExecutionReport>> {
    let mut reports = Vec::new();
    for plan in plans {
        if plan.sells.is_empty() && plan.buys.is_empty() {
            reports.push(AccountExecutionReport {
                account_number: plan.account_number.clone(),
                ..Default::default()
            });
            continue;
        }
        println!(
            "[{}] placing {} sells, then {} buys",
            plan.account_number,
            plan.sells.len(),
            plan.buys.len()
        );
        let mut report = execute_plan(client, plan, quotes, exchanges).await;
        println!(
            "[{}] {} fills, {} failures (pre-substitution)",
            plan.account_number,
            report.fills.len(),
            report.failures.len()
        );
        let subs = run_substitutions(client, plan, &mut report, spares, quotes, exchanges).await;
        if subs > 0 {
            println!("[{}] placed {} substitution buys", plan.account_number, subs);
        }
        reports.push(report);
    }
    Ok(reports)
}

async fn run_substitutions(
    client: &Client,
    plan: &AccountPlan,
    report: &mut AccountExecutionReport,
    spares: &[Ticker],
    quotes: &HashMap<String, Quote>,
    exchanges: &HashMap<String, String>,
) -> usize {
    let failed_buys: Vec<OrderFailure> = report
        .failures
        .iter()
        .filter(|f| matches!(f.trade.side, Side::Buy))
        .filter(|f| !plan.pre_trade_holdings.contains(&f.trade.symbol))
        .cloned()
        .collect();
    if failed_buys.is_empty() {
        return 0;
    }

    let excluded: HashSet<String> = plan
        .sells
        .iter()
        .chain(plan.buys.iter())
        .map(|t| t.symbol.clone())
        .collect();
    let mut tried: HashSet<String> = failed_buys.iter().map(|f| f.trade.symbol.clone()).collect();
    let mut placed = 0usize;

    for failed in &failed_buys {
        let target_value = failed.trade.indicative_price * failed.trade.shares as f64;
        if target_value < 1.0 {
            continue;
        }

        for spare in spares {
            if excluded.contains(&spare.symbol) || tried.contains(&spare.symbol) {
                continue;
            }
            let Some(q) = quotes.get(&spare.symbol) else { continue };
            let price = q.price();
            if price <= 0.0 || price > target_value {
                continue;
            }
            tried.insert(spare.symbol.clone());

            let shares = (target_value / price).floor() as u32;
            if shares == 0 {
                continue;
            }
            let trade = Trade {
                symbol: spare.symbol.clone(),
                side: Side::Buy,
                shares,
                indicative_price: price,
            };
            let ex = exchanges.get(&spare.symbol).map(String::as_str);

            let place_result =
                place_order_routed(client, &plan.account_hash, &spare.symbol, Side::Buy, shares, Some(q), ex).await;
            match place_result {
                Ok(order_id) => match client
                    .await_filled(&plan.account_hash, &order_id, FILL_TIMEOUT)
                    .await
                {
                    Ok(order) => {
                        let (qty, p) = extract_fill(&order);
                        report.fills.push(Fill {
                            trade,
                            filled_quantity: qty,
                            avg_price: p,
                        });
                        placed += 1;
                        break;
                    }
                    Err(e) => report.failures.push(OrderFailure {
                        trade,
                        reason: format!("substitute fill: {e}"),
                    }),
                },
                Err(e) => report.failures.push(OrderFailure {
                    trade,
                    reason: format!("substitute place: {e}"),
                }),
            }
        }
    }

    placed
}

pub fn compute_residual_cash(plan: &AccountPlan, report: &AccountExecutionReport) -> f64 {
    let buy_cost: f64 = report
        .fills
        .iter()
        .filter(|f| matches!(f.trade.side, Side::Buy))
        .map(|f| f.avg_price * f.filled_quantity as f64)
        .sum();
    let sell_proceeds: f64 = report
        .fills
        .iter()
        .filter(|f| matches!(f.trade.side, Side::Sell))
        .map(|f| f.avg_price * f.filled_quantity as f64)
        .sum();
    plan.cash + sell_proceeds - buy_cost
}

pub fn sanity_warning(plan: &AccountPlan, residual: f64) -> Option<String> {
    let threshold = plan.target_per_name.max(plan.equity * 0.01);
    if residual.abs() > threshold {
        Some(format!(
            "Post-trade cash ${:.2} exceeds ~one position size (${:.2}). Rebalance likely incomplete — see failures above.",
            residual, threshold,
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rebalance::Side;

    fn plan(equity: f64, cash: f64, target: f64) -> AccountPlan {
        AccountPlan {
            account_number: "1".into(),
            account_hash: "H".into(),
            equity,
            cash,
            target_per_name: target,
            subset_size: 20,
            sells: vec![],
            buys: vec![],
            skipped_unaffordable: vec![],
            missing_quotes: vec![],
            estimated_residual_cash: 0.0,
            pre_trade_holdings: HashSet::new(),
        }
    }

    fn trade(symbol: &str, side: Side, shares: u32, px: f64) -> Trade {
        Trade {
            symbol: symbol.into(),
            side,
            shares,
            indicative_price: px,
        }
    }

    fn fill(t: Trade) -> Fill {
        Fill {
            filled_quantity: t.shares,
            avg_price: t.indicative_price,
            trade: t,
        }
    }

    fn report_with(fills: Vec<Fill>) -> AccountExecutionReport {
        AccountExecutionReport {
            account_number: "1".into(),
            fills,
            failures: vec![],
        }
    }

    #[test]
    fn residual_zero_when_buys_equal_sells_plus_cash() {
        let p = plan(1000.0, 10.0, 100.0);
        let r = report_with(vec![
            fill(trade("A", Side::Sell, 10, 50.0)),
            fill(trade("B", Side::Buy, 51, 10.0)),
        ]);
        let residual = compute_residual_cash(&p, &r);
        assert!((residual - 0.0).abs() < 0.001);
    }

    #[test]
    fn residual_positive_when_sells_exceed_buys() {
        let p = plan(1000.0, 0.0, 100.0);
        let r = report_with(vec![
            fill(trade("A", Side::Sell, 10, 100.0)),
            fill(trade("B", Side::Buy, 5, 100.0)),
        ]);
        let residual = compute_residual_cash(&p, &r);
        assert!((residual - 500.0).abs() < 0.001);
    }

    #[test]
    fn sanity_warning_fires_when_residual_above_position_size() {
        let p = plan(10000.0, 0.0, 500.0);
        let warning = sanity_warning(&p, 800.0);
        assert!(warning.is_some());
    }

    #[test]
    fn sanity_warning_silent_when_residual_below_threshold() {
        let p = plan(10000.0, 0.0, 500.0);
        let warning = sanity_warning(&p, 50.0);
        assert!(warning.is_none());
    }

    #[test]
    fn sanity_warning_uses_one_percent_floor_for_small_targets() {
        let p = plan(100000.0, 0.0, 1.0);
        // 1% of equity = 1000. residual 500 < 1000 → silent.
        let warning = sanity_warning(&p, 500.0);
        assert!(warning.is_none());
        // residual 2000 > 1000 → warns.
        let warning = sanity_warning(&p, 2000.0);
        assert!(warning.is_some());
    }
}
