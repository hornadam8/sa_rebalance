mod config;
mod execute;
mod history;
mod notify;
mod rebalance;
mod sa;
mod schwab;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "sa_rebalance", about = "Equal-weight Schwab accounts across SA top-rated stocks")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run Schwab OAuth flow and store tokens
    Auth,
    /// Print today's top-rated tickers from SA (blocklist applied)
    Screen {
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Print Schwab accounts, balances, and positions (read-only)
    Accounts {
        /// Dump raw JSON from Schwab instead of parsed summary
        #[arg(long)]
        raw: bool,
    },
    /// Print rebalance plan per account (no orders placed)
    Plan,
    /// Place rebalance orders. Requires --yes.
    Execute {
        #[arg(long)]
        r#yes: bool,
        #[arg(long)]
        force: bool,
    },
    /// Send a sample email to verify SMTP setup
    NotifyTest,
    /// Store a fresh SA cookie (paste cURL or raw cookie string, end with Ctrl+D)
    SetCookie,
    /// Read iCloud-resident files to trigger TCC permission prompts (run during install)
    Warmup,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Screen { top } => screen(top).await,
        Cmd::Auth => auth().await,
        Cmd::Accounts { raw } => accounts(raw).await,
        Cmd::Plan => plan().await,
        Cmd::Execute { r#yes, force } => execute_cmd(r#yes, force).await,
        Cmd::NotifyTest => notify_test().await,
        Cmd::SetCookie => set_cookie().await,
        Cmd::Warmup => warmup().await,
    }
}

async fn auth() -> Result<()> {
    let env = config::Env::load()?;
    schwab::auth::run_auth_flow(
        &env.schwab_client_id,
        &env.schwab_client_secret,
        &env.schwab_redirect_uri,
    )
    .await
}

async fn accounts(raw: bool) -> Result<()> {
    let env = config::Env::load()?;
    let client = schwab::trader::Client::new(&env).await?;
    let numbers = client.account_numbers_raw().await?;
    let accounts_raw = client.accounts_with_positions_raw().await?;

    if raw {
        println!("# /accounts/accountNumbers");
        println!("{}", serde_json::to_string_pretty(&numbers)?);
        println!("\n# /accounts?fields=positions");
        println!("{}", serde_json::to_string_pretty(&accounts_raw)?);
        return Ok(());
    }

    let parsed = schwab::trader::parse_accounts(&numbers, &accounts_raw)?;
    println!("Schwab re-auth in {} days\n", client.days_until_reauth());
    for acc in &parsed {
        println!("Account {} ({})", acc.account_number, acc.account_type);
        println!("  Equity: ${:>12.2}", acc.equity);
        println!("  Cash:   ${:>12.2}", acc.cash);
        println!("  Positions ({}):", acc.positions.len());
        for p in &acc.positions {
            println!(
                "    {:<8} qty={:>8.0}  mv=${:>12.2}  avg=${:>8.2}",
                p.symbol, p.quantity, p.market_value, p.average_price,
            );
        }
        println!();
    }
    Ok(())
}

async fn notify_test() -> Result<()> {
    use execute::{AccountExecutionReport, Fill, OrderFailure};
    use rebalance::{AccountPlan, Side, Trade};

    let env = config::Env::load()?;

    let sell = |sym: &str, qty: u32, px: f64| Trade {
        symbol: sym.into(),
        side: Side::Sell,
        shares: qty,
        indicative_price: px,
    };
    let buy = |sym: &str, qty: u32, px: f64| Trade {
        symbol: sym.into(),
        side: Side::Buy,
        shares: qty,
        indicative_price: px,
    };
    let fill = |t: Trade| Fill {
        filled_quantity: t.shares,
        avg_price: t.indicative_price,
        trade: t,
    };

    let big_plan = AccountPlan {
        account_number: "29006453".into(),
        account_hash: "<hash>".into(),
        equity: 201535.60,
        cash: 6.00,
        target_per_name: 10076.78,
        subset_size: 20,
        sells: vec![
            sell("SEZL", 1, 161.36),
            sell("FRO", 6, 40.60),
            sell("THG", 1, 197.72),
            sell("JAZZ", 1, 224.08),
            sell("ALVOF", 11, 6.18),
        ],
        buys: vec![
            buy("CNC", 1, 61.16),
            buy("PBR", 1, 16.62),
            buy("CSTM", 1, 34.13),
            buy("CENX", 5, 51.77),
            buy("GM", 2, 79.18),
            buy("SBLK", 4, 25.58),
            buy("DKILY", 6, 14.83),
        ],
        skipped_unaffordable: vec![],
        missing_quotes: vec![],
        estimated_residual_cash: 2.96,
        pre_trade_holdings: std::collections::HashSet::new(),
    };
    let big_exec = AccountExecutionReport {
        account_number: "29006453".into(),
        fills: vec![
            fill(sell("SEZL", 1, 161.42)),
            fill(sell("FRO", 6, 40.58)),
            fill(sell("THG", 1, 197.55)),
            fill(sell("JAZZ", 1, 223.91)),
            fill(sell("ALVOF", 11, 6.19)),
            fill(buy("CNC", 1, 61.21)),
            fill(buy("PBR", 1, 16.65)),
            fill(buy("CSTM", 1, 34.10)),
            fill(buy("CENX", 5, 51.82)),
            fill(buy("GM", 2, 79.24)),
            fill(buy("SBLK", 4, 25.61)),
            fill(buy("DKILY", 6, 14.85)),
        ],
        failures: vec![],
    };

    let small_plan = AccountPlan {
        account_number: "57136195".into(),
        account_hash: "<hash>".into(),
        equity: 8734.67,
        cash: 9.41,
        target_per_name: 459.76,
        subset_size: 19,
        sells: vec![
            sell("CNC", 9, 61.16),
            sell("PBR", 27, 16.62),
            sell("FRO", 16, 40.60),
            sell("REPYY", 19, 24.71),
        ],
        buys: vec![
            buy("SBLK", 18, 25.58),
            buy("UNCRY", 10, 46.17),
            buy("THG", 2, 197.72),
            buy("ALVOF", 74, 6.18),
            buy("DKILY", 31, 14.83),
        ],
        skipped_unaffordable: vec!["SNDK".into()],
        missing_quotes: vec![],
        estimated_residual_cash: 19.63,
        pre_trade_holdings: std::collections::HashSet::new(),
    };
    let small_exec = AccountExecutionReport {
        account_number: "57136195".into(),
        fills: vec![
            fill(sell("CNC", 9, 61.20)),
            fill(sell("PBR", 27, 16.59)),
            fill(sell("FRO", 16, 40.62)),
            fill(buy("SBLK", 18, 25.55)),
            fill(buy("UNCRY", 10, 46.21)),
            fill(buy("THG", 2, 197.85)),
            fill(buy("ALVOF", 74, 6.21)),
            fill(buy("DKILY", 31, 14.86)),
        ],
        failures: vec![OrderFailure {
            trade: sell("REPYY", 19, 24.71),
            reason: "place order SELL REPYY returned 422 Unprocessable Entity: market closed for symbol"
                .into(),
        }],
    };

    let report = notify::Report {
        run_at: notify::now_local(),
        schwab_days_remaining: 4,
        sa_cookie_age_days: Some(12),
        top_used: vec![
            "SNDK", "CNC", "SEZL", "PBR", "CSTM", "FRO", "CENX", "GM", "SBLK", "UNCRY", "FMX",
            "INDV", "REPYY", "DINO", "THG", "JAZZ", "PKX", "SM", "ALVOF", "DKILY",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        blocked: vec!["SHIP".into(), "PBR.A".into()],
        promoted: vec![("PBR.A".into(), "JAZZ".into())],
        accounts: vec![
            notify::AccountReport {
                prev_equity: Some(200012.34),
                prev_ts_unix: Some(notify::now_local().unix_timestamp() - 86400),
                post_residual_cash: 9.04,
                sanity_warning: None,
                plan: big_plan,
                execution: big_exec,
            },
            notify::AccountReport {
                prev_equity: Some(8801.50),
                prev_ts_unix: Some(notify::now_local().unix_timestamp() - 86400),
                post_residual_cash: 1230.45,
                sanity_warning: Some(
                    "Post-trade cash $1230.45 exceeds ~one position size ($459.76). Rebalance likely incomplete — see failures above.".into()
                ),
                plan: small_plan,
                execution: small_exec,
            },
        ],
    };

    println!("Subject: {}", report.subject());
    let path = notify::write_local(&report)?;
    println!("Wrote sample report to {}", path.display());
    notify::send_email(&env, &report).await?;
    println!("Sent sample email to {}", env.notify_to);
    Ok(())
}

async fn execute_cmd(yes: bool, force: bool) -> Result<()> {
    if !yes {
        eprintln!("`execute` will place real orders. Pass --yes to confirm.");
        std::process::exit(2);
    }

    let env = config::Env::load()?;
    let result = execute_cmd_inner(&env, force).await;
    if let Err(e) = &result {
        let msg = format!("{e:#}");
        eprintln!("Run failed: {msg}");
        if let Err(send_err) = notify::send_failure_email(&env, &msg).await {
            eprintln!("Also failed to send failure email: {send_err:#}");
        }
    }
    result
}

/// Replace any top-20 slots with no Schwab quote with the next quoted spare.
/// Returns (updated_top, remaining_spares, swaps) where swaps is (dropped, replacement).
fn promote_missing(
    top_20: &[sa::Ticker],
    spares: &[sa::Ticker],
    prices: &std::collections::HashMap<String, f64>,
) -> (Vec<sa::Ticker>, Vec<sa::Ticker>, Vec<(String, String)>) {
    let mut effective = top_20.to_vec();
    let mut promoted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut swaps: Vec<(String, String)> = Vec::new();
    let mut spare_iter = spares.iter();

    for slot in effective.iter_mut() {
        if prices.get(&slot.symbol).copied().unwrap_or(0.0) > 0.0 {
            continue;
        }
        // Advance through spares to find the next one with a valid quote.
        loop {
            match spare_iter.next() {
                None => break,
                Some(spare) => {
                    if prices.get(&spare.symbol).copied().unwrap_or(0.0) > 0.0 {
                        swaps.push((slot.symbol.clone(), spare.symbol.clone()));
                        promoted.insert(spare.symbol.clone());
                        *slot = spare.clone();
                        break;
                    }
                    // spare also has no quote; skip it
                }
            }
        }
    }

    let remaining_spares: Vec<sa::Ticker> = spares
        .iter()
        .filter(|s| !promoted.contains(&s.symbol))
        .cloned()
        .collect();

    (effective, remaining_spares, swaps)
}

async fn execute_cmd_inner(env: &config::Env, force: bool) -> Result<()> {
    use std::collections::HashSet;

    let allowlist: HashSet<String> = env.schwab_rebalance_accounts.iter().cloned().collect();
    if allowlist.is_empty() {
        anyhow::bail!("SCHWAB_REBALANCE_ACCOUNTS is empty");
    }

    let blocklist = config::load_blocklist(&config::blocklist_path())?;
    let (top_20, spares) = get_or_fetch_top_lists(env, &blocklist).await?;

    let client = schwab::trader::Client::new(&env).await?;

    if !force && !client.is_equity_market_open().await? {
        anyhow::bail!("US equity market is closed (pass --force to override)");
    }

    let numbers = client.account_numbers_raw().await?;
    let accounts_raw = client.accounts_with_positions_raw().await?;
    let accounts = schwab::trader::parse_accounts(&numbers, &accounts_raw)?;

    let mut symbol_set: HashSet<String> = top_20.iter().map(|t| t.symbol.clone()).collect();
    for t in &spares {
        symbol_set.insert(t.symbol.clone());
    }
    for acc in &accounts {
        if !allowlist.contains(&acc.account_number) {
            continue;
        }
        for p in &acc.positions {
            symbol_set.insert(p.symbol.clone());
        }
    }
    let symbols: Vec<String> = symbol_set.into_iter().collect();
    let quotes_map = client.quotes(&symbols).await?;
    let prices: std::collections::HashMap<String, f64> = quotes_map
        .iter()
        .map(|(k, q)| (k.clone(), q.price()))
        .collect();

    // Promote quoted spares to fill any top-20 slots that have no Schwab quote.
    let (effective_top, effective_spares, promotions) =
        promote_missing(&top_20, &spares, &prices);

    let mut exchanges: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for t in &top_20 {
        exchanges.insert(t.symbol.clone(), t.exchange.clone());
    }
    for t in &spares {
        exchanges.insert(t.symbol.clone(), t.exchange.clone());
    }

    let plans: Vec<rebalance::AccountPlan> = accounts
        .iter()
        .filter(|a| allowlist.contains(&a.account_number))
        .map(|a| rebalance::plan_account(a, &effective_top, &prices))
        .collect();

    let days_remaining = client.days_until_reauth();
    println!("Schwab re-auth in {} days", days_remaining);
    let exec_reports = execute::run_execute(&client, &plans, &effective_top, &effective_spares, &quotes_map, &exchanges).await?;

    let prev_snapshots = history::load();
    let mut new_snapshots = prev_snapshots.clone();
    let today = today_local_str();
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let account_reports: Vec<notify::AccountReport> = plans
        .into_iter()
        .zip(exec_reports.into_iter())
        .map(|(plan, execution)| {
            let prev = history::most_recent_prior(&prev_snapshots, &plan.account_number, &today);
            let post_residual = execute::compute_residual_cash(&plan, &execution);
            let warning = execute::sanity_warning(&plan, post_residual);
            let report = notify::AccountReport {
                prev_equity: prev.map(|p| p.equity),
                prev_ts_unix: prev.map(|p| p.unix_ts),
                post_residual_cash: post_residual,
                sanity_warning: warning,
                plan: plan.clone(),
                execution,
            };
            history::record_today(
                &mut new_snapshots,
                plan.account_number.clone(),
                today.clone(),
                history::Snapshot {
                    equity: plan.equity,
                    unix_ts: now_unix,
                },
            );
            report
        })
        .collect();
    let _ = history::save(&new_snapshots);

    let blocked: Vec<String> = blocklist.iter().cloned().collect();
    let report = notify::Report {
        run_at: notify::now_local(),
        schwab_days_remaining: days_remaining,
        sa_cookie_age_days: sa_cookie_age_days(&env),
        top_used: effective_top.iter().map(|t| t.symbol.clone()).collect(),
        blocked,
        promoted: promotions,
        accounts: account_reports,
    };

    let path = notify::write_local(&report)?;
    println!("Wrote report to {}", path.display());

    match notify::send_email(&env, &report).await {
        Ok(_) => println!("Emailed report to {}", env.notify_to),
        Err(e) => eprintln!("Email send failed: {e}"),
    }

    Ok(())
}

async fn plan() -> Result<()> {
    use std::collections::HashSet;

    let env = config::Env::load()?;
    let allowlist: HashSet<String> = env.schwab_rebalance_accounts.iter().cloned().collect();
    if allowlist.is_empty() {
        anyhow::bail!("SCHWAB_REBALANCE_ACCOUNTS is empty");
    }

    let blocklist = config::load_blocklist(&config::blocklist_path())?;
    let (top_20, spares) = get_or_fetch_top_lists(&env, &blocklist).await?;

    let client = schwab::trader::Client::new(&env).await?;
    let numbers = client.account_numbers_raw().await?;
    let accounts_raw = client.accounts_with_positions_raw().await?;
    let accounts = schwab::trader::parse_accounts(&numbers, &accounts_raw)?;

    let mut symbol_set: HashSet<String> =
        top_20.iter().chain(spares.iter()).map(|t| t.symbol.clone()).collect();
    for acc in &accounts {
        if !allowlist.contains(&acc.account_number) {
            continue;
        }
        for p in &acc.positions {
            symbol_set.insert(p.symbol.clone());
        }
    }
    let symbols: Vec<String> = symbol_set.into_iter().collect();
    let quotes_map = client.quotes(&symbols).await?;
    let prices: std::collections::HashMap<String, f64> = quotes_map
        .iter()
        .map(|(k, q)| (k.clone(), q.price()))
        .collect();

    let (effective_top, _effective_spares, promotions) =
        promote_missing(&top_20, &spares, &prices);

    println!("Top 20 used:");
    for (i, t) in effective_top.iter().enumerate() {
        let px = prices.get(&t.symbol).copied().unwrap_or(0.0);
        println!("  {:>2}. {:<8} ${:>8.2}", i + 1, t.symbol, px);
    }
    for (dropped, replacement) in &promotions {
        println!("  [promoted] {replacement} replaces {dropped} (no quote)");
    }
    println!("\nSchwab re-auth in {} days\n", client.days_until_reauth());

    for acc in &accounts {
        if !allowlist.contains(&acc.account_number) {
            continue;
        }
        let plan = rebalance::plan_account(acc, &effective_top, &prices);
        print_plan(&plan);
    }
    Ok(())
}

fn print_plan(p: &rebalance::AccountPlan) {
    println!("Account {} — equity ${:.2}, cash ${:.2}", p.account_number, p.equity, p.cash);
    println!(
        "  subset: {} names, target/name: ${:.2}",
        p.subset_size, p.target_per_name
    );
    if !p.skipped_unaffordable.is_empty() {
        println!("  skipped (won't fit subset): {}", p.skipped_unaffordable.join(", "));
    }
    if !p.missing_quotes.is_empty() {
        println!("  no quote available: {}", p.missing_quotes.join(", "));
    }
    if p.sells.is_empty() {
        println!("  sells: (none)");
    } else {
        println!("  sells:");
        for t in &p.sells {
            println!(
                "    {:<8} {:>5}  @ ~${:>8.2}  = ${:>10.2}",
                t.symbol,
                t.shares,
                t.indicative_price,
                t.indicative_price * t.shares as f64,
            );
        }
    }
    if p.buys.is_empty() {
        println!("  buys:  (none)");
    } else {
        println!("  buys:");
        for t in &p.buys {
            println!(
                "    {:<8} {:>5}  @ ~${:>8.2}  = ${:>10.2}",
                t.symbol,
                t.shares,
                t.indicative_price,
                t.indicative_price * t.shares as f64,
            );
        }
    }
    println!("  estimated residual cash: ${:.2}\n", p.estimated_residual_cash);
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TopListCache {
    date: String,
    top_20: Vec<sa::Ticker>,
    spares: Vec<sa::Ticker>,
}

fn sa_cache_path() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".local/state/sa_rebalance/sa_top_list.json"))
}

fn today_local_str() -> String {
    let now = notify::now_local();
    now.format(time::macros::format_description!("[year]-[month]-[day]"))
        .unwrap_or_default()
}

fn cache_is_fresh(cache: &TopListCache, today: &str) -> bool {
    cache.date == today
}

fn load_top_list_cache() -> Option<TopListCache> {
    let path = sa_cache_path()?;
    let body = std::fs::read_to_string(&path).ok()?;
    let cache: TopListCache = serde_json::from_str(&body).ok()?;
    if cache_is_fresh(&cache, &today_local_str()) {
        Some(cache)
    } else {
        None
    }
}

fn save_top_list_cache(cache: &TopListCache) {
    let Some(path) = sa_cache_path() else { return };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(body) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(&path, body);
    }
}

async fn get_or_fetch_top_lists(
    env: &config::Env,
    blocklist: &std::collections::HashSet<String>,
) -> Result<(Vec<sa::Ticker>, Vec<sa::Ticker>)> {
    if let Some(cache) = load_top_list_cache() {
        eprintln!("using cached SA top-list from {}", cache.date);
        return Ok((cache.top_20, cache.spares));
    }
    let cookie = load_sa_cookie(env);
    let (raw_sa, rotated) = sa::fetch_top_rated(&cookie).await?;
    if rotated != cookie {
        save_sa_cookie(env, &rotated);
    }
    let filtered: Vec<sa::Ticker> = raw_sa
        .into_iter()
        .filter(|t| !blocklist.contains(&t.symbol.to_ascii_uppercase()))
        .collect();
    let split = 20.min(filtered.len());
    let top_20 = filtered[..split].to_vec();
    let spares = filtered[split..].to_vec();
    save_top_list_cache(&TopListCache {
        date: today_local_str(),
        top_20: top_20.clone(),
        spares: spares.clone(),
    });
    Ok((top_20, spares))
}

fn resolve_path(s: &str) -> std::path::PathBuf {
    if let Some(stripped) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    std::path::PathBuf::from(s)
}

fn load_sa_cookie(env: &config::Env) -> String {
    if let Some(p) = &env.sa_cookie_path {
        let path = resolve_path(p);
        if let Ok(s) = std::fs::read_to_string(&path) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    env.sa_cookie.clone()
}

fn sa_cookie_meta_path(env: &config::Env) -> Option<std::path::PathBuf> {
    let p = env.sa_cookie_path.as_ref()?;
    let resolved = resolve_path(p);
    resolved.parent().map(|d| d.join("sa_cookie_meta.json"))
}

fn save_sa_cookie_bootstrap_now(env: &config::Env) {
    let Some(path) = sa_cookie_meta_path(env) else {
        return;
    };
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = std::fs::write(
        &path,
        format!(r#"{{"bootstrapped_at_unix":{}}}"#, now.as_secs()),
    );
}

fn sa_cookie_age_days(env: &config::Env) -> Option<i64> {
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    if let Some(path) = sa_cookie_meta_path(env) {
        if let Ok(body) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(unix) = v.get("bootstrapped_at_unix").and_then(|x| x.as_i64()) {
                    return Some((now_unix - unix).max(0) / 86400);
                }
            }
        }
    }
    let cookie_path = env.sa_cookie_path.as_ref()?;
    let resolved = resolve_path(cookie_path);
    let metadata = std::fs::metadata(&resolved).ok()?;
    let mtime = metadata.modified().ok()?;
    let mtime_unix = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some((now_unix - mtime_unix).max(0) / 86400)
}

fn save_sa_cookie(env: &config::Env, cookie: &str) {
    let Some(p) = &env.sa_cookie_path else {
        return;
    };
    let path = resolve_path(p);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if std::fs::write(&path, cookie).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

async fn warmup() -> Result<()> {
    let env = config::Env::load()?;
    eprintln!("Warming up TCC permissions (approve any prompts that appear)...");
    let paths = iCloud_paths_to_warm(&env);
    let mut touched = 0;
    for path in &paths {
        match std::fs::metadata(path) {
            Ok(_) => {
                let _ = std::fs::read(path);
                eprintln!("  ✓ {}", path.display());
                touched += 1;
            }
            Err(e) => {
                eprintln!("  - skipped {} ({e})", path.display());
            }
        }
    }
    eprintln!("Warmup done — touched {} file(s).", touched);
    eprintln!("If a TCC prompt appeared and you approved, future launchd runs will not re-prompt for this binary.");
    Ok(())
}

#[allow(non_snake_case)]
fn iCloud_paths_to_warm(env: &config::Env) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = &env.sa_cookie_path {
        out.push(resolve_path(p));
    }
    if let Ok(tp) = schwab::auth::tokens_path() {
        out.push(tp);
    }
    out
}

async fn set_cookie() -> Result<()> {
    let env = config::Env::load()?;
    if env.sa_cookie_path.is_none() {
        anyhow::bail!("SA_COOKIE_PATH not set in .env — can't persist a cookie without it");
    }

    eprintln!("Paste cURL command from DevTools (or raw Cookie header value), then Ctrl+D:");
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let cookie = extract_cookie_value(&input)?;

    save_sa_cookie(&env, &cookie);
    save_sa_cookie_bootstrap_now(&env);
    eprintln!("Stored cookie. Verifying with SA…");
    let (tickers, rotated) = sa::fetch_top_rated(&cookie).await?;
    if rotated != cookie {
        save_sa_cookie(&env, &rotated);
    }
    println!("OK — fetched {} tickers from SA.", tickers.len());
    Ok(())
}

fn extract_cookie_value(input: &str) -> Result<String> {
    if let Some(start) = input.find("-b '") {
        let after = &input[start + 4..];
        if let Some(end) = after.find('\'') {
            return Ok(after[..end].to_string());
        }
    }
    if let Some(start) = input.find("-b \"") {
        let after = &input[start + 4..];
        if let Some(end) = after.find('"') {
            return Ok(after[..end].to_string());
        }
    }
    let trimmed = input.trim().trim_matches(|c| c == '\'' || c == '"');
    if trimmed.contains('=') {
        return Ok(trimmed.to_string());
    }
    anyhow::bail!("could not find a cookie in input — expected cURL with -b or raw cookie header")
}

async fn screen(top: usize) -> Result<()> {
    let env = config::Env::load()?;
    let blocklist = config::load_blocklist(&config::blocklist_path())?;

    let cookie = load_sa_cookie(&env);
    let (tickers, rotated) = sa::fetch_top_rated(&cookie).await?;
    if rotated != cookie {
        save_sa_cookie(&env, &rotated);
    }
    let total = tickers.len();

    let filtered: Vec<sa::Ticker> = tickers
        .into_iter()
        .filter(|t| !blocklist.contains(&t.symbol.to_ascii_uppercase()))
        .take(top)
        .collect();

    println!("SA top {} (from {} matches, {} blocked):", filtered.len(), total, blocklist.len());
    for (i, t) in filtered.iter().enumerate() {
        println!("{:>3}. {:<8} {:<10} {}", i + 1, t.symbol, t.exchange, t.company);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_expands_tilde() {
        let home = dirs::home_dir().unwrap();
        let resolved = resolve_path("~/foo/bar");
        assert_eq!(resolved, home.join("foo/bar"));
    }

    #[test]
    fn resolve_path_passes_absolute_through() {
        let resolved = resolve_path("/tmp/foo");
        assert_eq!(resolved.to_str().unwrap(), "/tmp/foo");
    }

    #[test]
    fn resolve_path_handles_relative() {
        let resolved = resolve_path("foo/bar");
        assert_eq!(resolved.to_str().unwrap(), "foo/bar");
    }

    #[test]
    fn extract_cookie_value_from_curl_single_quoted() {
        let input = "curl 'https://example.com' -H 'accept: json' -b 'session=abc; user=42' -H 'x: y'";
        let result = extract_cookie_value(input).unwrap();
        assert_eq!(result, "session=abc; user=42");
    }

    #[test]
    fn extract_cookie_value_from_curl_double_quoted() {
        let input = r#"curl "url" -b "k1=v1; k2=v2" -H "h: v""#;
        let result = extract_cookie_value(input).unwrap();
        assert_eq!(result, "k1=v1; k2=v2");
    }

    #[test]
    fn extract_cookie_value_accepts_raw_cookie_string() {
        let result = extract_cookie_value("k1=v1; k2=v2").unwrap();
        assert_eq!(result, "k1=v1; k2=v2");
    }

    #[test]
    fn extract_cookie_value_strips_surrounding_quotes() {
        let result = extract_cookie_value("'k=v'").unwrap();
        assert_eq!(result, "k=v");
    }

    #[test]
    fn extract_cookie_value_rejects_input_without_equals() {
        let result = extract_cookie_value("just some text no kv pairs");
        assert!(result.is_err());
    }

    #[test]
    fn cache_is_fresh_when_dates_match() {
        let cache = TopListCache {
            date: "2026-06-22".into(),
            top_20: vec![],
            spares: vec![],
        };
        assert!(cache_is_fresh(&cache, "2026-06-22"));
    }

    #[test]
    fn cache_is_stale_when_dates_differ() {
        let cache = TopListCache {
            date: "2026-06-21".into(),
            top_20: vec![],
            spares: vec![],
        };
        assert!(!cache_is_fresh(&cache, "2026-06-22"));
    }
}
