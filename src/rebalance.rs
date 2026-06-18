use std::collections::{HashMap, HashSet};

use crate::sa::Ticker;
use crate::schwab::trader::Account;

#[derive(Debug, Clone, Copy)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct Trade {
    pub symbol: String,
    pub side: Side,
    pub shares: u32,
    pub indicative_price: f64,
}

#[derive(Debug, Clone)]
pub struct AccountPlan {
    pub account_number: String,
    pub account_hash: String,
    pub equity: f64,
    pub cash: f64,
    pub target_per_name: f64,
    pub subset_size: usize,
    pub sells: Vec<Trade>,
    pub buys: Vec<Trade>,
    pub skipped_unaffordable: Vec<String>,
    pub missing_quotes: Vec<String>,
    pub estimated_residual_cash: f64,
}

pub fn plan_account(
    account: &Account,
    top_n: &[Ticker],
    prices: &HashMap<String, f64>,
) -> AccountPlan {
    let missing_quotes: Vec<String> = top_n
        .iter()
        .filter(|t| prices.get(&t.symbol).copied().unwrap_or(0.0) <= 0.0)
        .map(|t| t.symbol.clone())
        .collect();

    let quoted: Vec<&Ticker> = top_n
        .iter()
        .filter(|t| prices.get(&t.symbol).copied().unwrap_or(0.0) > 0.0)
        .collect();

    let planning_equity = account.cash
        + account
            .positions
            .iter()
            .map(|p| {
                let px = prices.get(&p.symbol).copied().unwrap_or_else(|| {
                    if p.quantity > 0.0 {
                        p.market_value / p.quantity
                    } else {
                        0.0
                    }
                });
                px * p.quantity
            })
            .sum::<f64>();

    let subset = affordable_subset(&quoted, prices, planning_equity);
    let subset_syms: HashSet<&str> = subset.iter().map(|t| t.symbol.as_str()).collect();
    let skipped_unaffordable: Vec<String> = quoted
        .iter()
        .filter(|t| !subset_syms.contains(t.symbol.as_str()))
        .map(|t| t.symbol.clone())
        .collect();

    let target_shares = allocate_shares(&subset, prices, planning_equity);
    let target_per_name = if subset.is_empty() {
        0.0
    } else {
        planning_equity / subset.len() as f64
    };

    let mut sells: Vec<Trade> = Vec::new();
    let mut buys: Vec<Trade> = Vec::new();

    for pos in &account.positions {
        if subset_syms.contains(pos.symbol.as_str()) || pos.quantity <= 0.0 {
            continue;
        }
        let price = prices
            .get(&pos.symbol)
            .copied()
            .unwrap_or_else(|| pos.market_value / pos.quantity.max(1.0));
        sells.push(Trade {
            symbol: pos.symbol.clone(),
            side: Side::Sell,
            shares: pos.quantity as u32,
            indicative_price: price,
        });
    }

    for ticker in &subset {
        let price = prices[&ticker.symbol];
        let target = *target_shares.get(&ticker.symbol).unwrap_or(&0) as i64;
        let current = account
            .positions
            .iter()
            .find(|p| p.symbol == ticker.symbol)
            .map(|p| p.quantity as i64)
            .unwrap_or(0);
        let delta = target - current;
        if delta > 0 {
            buys.push(Trade {
                symbol: ticker.symbol.clone(),
                side: Side::Buy,
                shares: delta as u32,
                indicative_price: price,
            });
        } else if delta < 0 {
            sells.push(Trade {
                symbol: ticker.symbol.clone(),
                side: Side::Sell,
                shares: (-delta) as u32,
                indicative_price: price,
            });
        }
    }

    let sell_proceeds: f64 = sells.iter().map(|t| t.indicative_price * t.shares as f64).sum();
    let buy_cost: f64 = buys.iter().map(|t| t.indicative_price * t.shares as f64).sum();
    let estimated_residual_cash = account.cash + sell_proceeds - buy_cost;

    AccountPlan {
        account_number: account.account_number.clone(),
        account_hash: account.account_hash.clone(),
        equity: account.equity,
        cash: account.cash,
        target_per_name,
        subset_size: subset.len(),
        sells,
        buys,
        skipped_unaffordable,
        missing_quotes,
        estimated_residual_cash,
    }
}

fn affordable_subset<'a>(
    quoted: &[&'a Ticker],
    prices: &HashMap<String, f64>,
    equity: f64,
) -> Vec<&'a Ticker> {
    let mut subset: Vec<&Ticker> = quoted.to_vec();
    loop {
        let n = subset.len();
        if n == 0 {
            return subset;
        }
        let target = equity / n as f64;
        let before = subset.len();
        subset.retain(|t| prices[&t.symbol] <= target);
        if subset.len() == before {
            return subset;
        }
    }
}

fn allocate_shares(
    subset: &[&Ticker],
    prices: &HashMap<String, f64>,
    equity: f64,
) -> HashMap<String, u32> {
    let n = subset.len();
    if n == 0 {
        return HashMap::new();
    }
    let target = equity / n as f64;
    let mut alloc: HashMap<String, u32> = HashMap::new();
    let mut invested = 0.0;
    for t in subset {
        let p = prices[&t.symbol];
        let shares = (target / p).floor() as u32;
        alloc.insert(t.symbol.clone(), shares);
        invested += shares as f64 * p;
    }
    let mut available = equity - invested;

    loop {
        let mut best: Option<(String, f64, f64)> = None;
        for t in subset {
            let p = prices[&t.symbol];
            if p > available {
                continue;
            }
            let current_value = *alloc.get(&t.symbol).unwrap() as f64 * p;
            let underflow = target - current_value;
            if 2.0 * underflow <= p {
                continue;
            }
            let score = p * (2.0 * underflow - p);
            match &best {
                None => best = Some((t.symbol.clone(), p, score)),
                Some((_, _, s)) if score > *s => best = Some((t.symbol.clone(), p, score)),
                _ => {}
            }
        }
        match best {
            Some((sym, price, _)) => {
                *alloc.get_mut(&sym).unwrap() += 1;
                available -= price;
            }
            None => break,
        }
    }

    alloc
}
