use anyhow::Result;
use serde_json::Value;
use std::time::Duration;

use crate::rebalance::{AccountPlan, Side, Trade};
use crate::schwab::trader::Client;

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

pub async fn execute_plan(client: &Client, plan: &AccountPlan) -> AccountExecutionReport {
    let mut report = AccountExecutionReport {
        account_number: plan.account_number.clone(),
        ..Default::default()
    };

    place_and_collect(client, &plan.account_hash, &plan.sells, &mut report).await;
    place_and_collect(client, &plan.account_hash, &plan.buys, &mut report).await;

    report
}

async fn place_and_collect(
    client: &Client,
    account_hash: &str,
    trades: &[Trade],
    report: &mut AccountExecutionReport,
) {
    let mut pending: Vec<(Trade, String)> = Vec::new();
    for trade in trades {
        let instruction = match trade.side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };
        match client
            .place_market_order(account_hash, &trade.symbol, instruction, trade.shares)
            .await
        {
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
        let report = execute_plan(client, plan).await;
        println!(
            "[{}] {} fills, {} failures",
            plan.account_number,
            report.fills.len(),
            report.failures.len()
        );
        reports.push(report);
    }
    Ok(reports)
}
