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
    pub pre_trade_holdings: HashSet<String>,
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

    let pre_trade_holdings: HashSet<String> = account
        .positions
        .iter()
        .filter(|p| p.quantity > 0.0)
        .map(|p| p.symbol.clone())
        .collect();

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
        pre_trade_holdings,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schwab::trader::{Account, Position};

    fn ticker(symbol: &str) -> Ticker {
        Ticker {
            symbol: symbol.to_string(),
            company: format!("{symbol} Co."),
            exchange: "NYSE".to_string(),
        }
    }

    fn price_map(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn pos(symbol: &str, qty: f64, price: f64) -> Position {
        Position {
            symbol: symbol.to_string(),
            quantity: qty,
            market_value: qty * price,
            average_price: price,
        }
    }

    fn account(equity: f64, cash: f64, positions: Vec<Position>) -> Account {
        Account {
            account_number: "1".into(),
            account_hash: "H".into(),
            account_type: "MARGIN".into(),
            equity,
            cash,
            positions,
        }
    }

    #[test]
    fn affordable_subset_keeps_all_when_target_above_all_prices() {
        let tickers = vec![ticker("A"), ticker("B"), ticker("C")];
        let refs: Vec<&Ticker> = tickers.iter().collect();
        let prices = price_map(&[("A", 100.0), ("B", 200.0), ("C", 300.0)]);
        let subset = affordable_subset(&refs, &prices, 3000.0);
        assert_eq!(subset.len(), 3);
    }

    #[test]
    fn affordable_subset_drops_unaffordable_until_stable() {
        let tickers = vec![ticker("CHEAP"), ticker("MID"), ticker("EXPENSIVE")];
        let refs: Vec<&Ticker> = tickers.iter().collect();
        let prices = price_map(&[("CHEAP", 5.0), ("MID", 50.0), ("EXPENSIVE", 2000.0)]);
        let subset = affordable_subset(&refs, &prices, 1000.0);
        let syms: Vec<&str> = subset.iter().map(|t| t.symbol.as_str()).collect();
        assert_eq!(syms, vec!["CHEAP", "MID"]);
    }

    #[test]
    fn affordable_subset_handles_cascading_drops() {
        let tickers = vec![ticker("A"), ticker("B"), ticker("C")];
        let refs: Vec<&Ticker> = tickers.iter().collect();
        let prices = price_map(&[("A", 10.0), ("B", 400.0), ("C", 500.0)]);
        let subset = affordable_subset(&refs, &prices, 600.0);
        let syms: Vec<&str> = subset.iter().map(|t| t.symbol.as_str()).collect();
        assert_eq!(syms, vec!["A"]);
    }

    #[test]
    fn allocate_shares_floors_initial_allocation() {
        let tickers = vec![ticker("A"), ticker("B")];
        let refs: Vec<&Ticker> = tickers.iter().collect();
        let prices = price_map(&[("A", 100.0), ("B", 200.0)]);
        let alloc = allocate_shares(&refs, &prices, 1000.0);
        assert!(alloc["A"] >= 5);
        assert!(alloc["B"] >= 2);
    }

    #[test]
    fn allocate_shares_fully_invests_until_no_share_affordable() {
        let tickers = vec![ticker("CHEAP"), ticker("MID")];
        let refs: Vec<&Ticker> = tickers.iter().collect();
        let prices = price_map(&[("CHEAP", 10.0), ("MID", 50.0)]);
        let equity = 1000.0;
        let alloc = allocate_shares(&refs, &prices, equity);
        let invested: f64 = alloc
            .iter()
            .map(|(s, n)| *n as f64 * prices[s])
            .sum();
        let residual = equity - invested;
        let min_price = prices.values().cloned().fold(f64::INFINITY, f64::min);
        assert!(
            residual < min_price,
            "residual {residual} should be below cheapest share {min_price}"
        );
    }

    #[test]
    fn allocate_shares_empty_subset_yields_empty_alloc() {
        let prices = price_map(&[]);
        let alloc = allocate_shares(&[], &prices, 1000.0);
        assert!(alloc.is_empty());
    }

    #[test]
    fn plan_account_sells_positions_outside_subset() {
        let top = vec![ticker("A"), ticker("B")];
        let prices = price_map(&[("A", 100.0), ("B", 50.0), ("OLD", 25.0)]);
        let acc = account(
            2000.0,
            0.0,
            vec![pos("A", 10.0, 100.0), pos("OLD", 40.0, 25.0)],
        );
        let plan = plan_account(&acc, &top, &prices);
        assert!(plan.sells.iter().any(|t| t.symbol == "OLD" && t.shares == 40));
    }

    #[test]
    fn plan_account_buys_to_reach_target() {
        let top = vec![ticker("A"), ticker("B")];
        let prices = price_map(&[("A", 100.0), ("B", 50.0)]);
        let acc = account(2000.0, 2000.0, vec![]);
        let plan = plan_account(&acc, &top, &prices);
        let a_buy: u32 = plan
            .buys
            .iter()
            .filter(|t| t.symbol == "A")
            .map(|t| t.shares)
            .sum();
        let b_buy: u32 = plan
            .buys
            .iter()
            .filter(|t| t.symbol == "B")
            .map(|t| t.shares)
            .sum();
        assert!(a_buy >= 9, "should buy ~10 shares of A, got {a_buy}");
        assert!(b_buy >= 19, "should buy ~20 shares of B, got {b_buy}");
    }

    #[test]
    fn plan_account_records_skipped_unaffordable() {
        let top = vec![ticker("CHEAP"), ticker("PRICEY")];
        let prices = price_map(&[("CHEAP", 10.0), ("PRICEY", 5000.0)]);
        let acc = account(1000.0, 1000.0, vec![]);
        let plan = plan_account(&acc, &top, &prices);
        assert_eq!(plan.skipped_unaffordable, vec!["PRICEY"]);
        assert_eq!(plan.subset_size, 1);
    }

    #[test]
    fn plan_account_records_missing_quotes() {
        let top = vec![ticker("HAS_QUOTE"), ticker("NO_QUOTE")];
        let prices = price_map(&[("HAS_QUOTE", 50.0)]);
        let acc = account(1000.0, 1000.0, vec![]);
        let plan = plan_account(&acc, &top, &prices);
        assert_eq!(plan.missing_quotes, vec!["NO_QUOTE"]);
    }

    #[test]
    fn plan_account_captures_pre_trade_holdings() {
        let top = vec![ticker("A")];
        let prices = price_map(&[("A", 100.0), ("OLD", 25.0)]);
        let acc = account(
            1000.0,
            0.0,
            vec![pos("A", 5.0, 100.0), pos("OLD", 20.0, 25.0)],
        );
        let plan = plan_account(&acc, &top, &prices);
        assert!(plan.pre_trade_holdings.contains("A"));
        assert!(plan.pre_trade_holdings.contains("OLD"));
    }

    #[test]
    fn plan_account_residual_cash_estimate_is_nonnegative() {
        let top = vec![ticker("A"), ticker("B"), ticker("C")];
        let prices = price_map(&[("A", 17.0), ("B", 23.0), ("C", 41.0)]);
        let acc = account(1000.0, 1000.0, vec![]);
        let plan = plan_account(&acc, &top, &prices);
        assert!(plan.estimated_residual_cash >= 0.0);
    }
}
