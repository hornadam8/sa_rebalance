mod config;
mod execute;
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
    let env = config::Env::load()?;
    let report = notify::Report {
        run_at: notify::now_local(),
        schwab_days_remaining: 7,
        top_used: vec!["SNDK".into(), "CNC".into(), "SEZL".into()],
        blocked: vec!["SHIP".into()],
        accounts: vec![],
    };
    println!("Subject: {}", report.subject());
    notify::send_email(&env, &report).await?;
    println!("Sent test email to {}", env.notify_to);
    Ok(())
}

async fn execute_cmd(yes: bool, force: bool) -> Result<()> {
    use std::collections::HashSet;

    if !yes {
        eprintln!("`execute` will place real orders. Pass --yes to confirm.");
        std::process::exit(2);
    }

    let env = config::Env::load()?;
    let allowlist: HashSet<String> = env.schwab_rebalance_accounts.iter().cloned().collect();
    if allowlist.is_empty() {
        anyhow::bail!("SCHWAB_REBALANCE_ACCOUNTS is empty");
    }

    let blocklist = config::load_blocklist(&config::blocklist_path())?;
    let raw_sa = sa::fetch_top_rated(&env.sa_cookie).await?;
    let top_20: Vec<sa::Ticker> = raw_sa
        .into_iter()
        .filter(|t| !blocklist.contains(&t.symbol.to_ascii_uppercase()))
        .take(20)
        .collect();

    let client = schwab::trader::Client::new(&env).await?;

    if !force && !client.is_equity_market_open().await? {
        anyhow::bail!("US equity market is closed (pass --force to override)");
    }

    let numbers = client.account_numbers_raw().await?;
    let accounts_raw = client.accounts_with_positions_raw().await?;
    let accounts = schwab::trader::parse_accounts(&numbers, &accounts_raw)?;

    let mut symbol_set: HashSet<String> = top_20.iter().map(|t| t.symbol.clone()).collect();
    for acc in &accounts {
        if !allowlist.contains(&acc.account_number) {
            continue;
        }
        for p in &acc.positions {
            symbol_set.insert(p.symbol.clone());
        }
    }
    let symbols: Vec<String> = symbol_set.into_iter().collect();
    let prices = client.quotes(&symbols).await?;

    let plans: Vec<rebalance::AccountPlan> = accounts
        .iter()
        .filter(|a| allowlist.contains(&a.account_number))
        .map(|a| rebalance::plan_account(a, &top_20, &prices))
        .collect();

    let days_remaining = client.days_until_reauth();
    println!("Schwab re-auth in {} days", days_remaining);
    let exec_reports = execute::run_execute(&client, &plans).await?;

    let account_reports: Vec<notify::AccountReport> = plans
        .into_iter()
        .zip(exec_reports.into_iter())
        .map(|(plan, execution)| notify::AccountReport { plan, execution })
        .collect();

    let blocked: Vec<String> = blocklist.iter().cloned().collect();
    let report = notify::Report {
        run_at: notify::now_local(),
        schwab_days_remaining: days_remaining,
        top_used: top_20.iter().map(|t| t.symbol.clone()).collect(),
        blocked,
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
    let raw_sa = sa::fetch_top_rated(&env.sa_cookie).await?;
    let top_20: Vec<sa::Ticker> = raw_sa
        .into_iter()
        .filter(|t| !blocklist.contains(&t.symbol.to_ascii_uppercase()))
        .take(20)
        .collect();

    let client = schwab::trader::Client::new(&env).await?;
    let numbers = client.account_numbers_raw().await?;
    let accounts_raw = client.accounts_with_positions_raw().await?;
    let accounts = schwab::trader::parse_accounts(&numbers, &accounts_raw)?;

    let mut symbol_set: HashSet<String> =
        top_20.iter().map(|t| t.symbol.clone()).collect();
    for acc in &accounts {
        if !allowlist.contains(&acc.account_number) {
            continue;
        }
        for p in &acc.positions {
            symbol_set.insert(p.symbol.clone());
        }
    }
    let symbols: Vec<String> = symbol_set.into_iter().collect();
    let prices = client.quotes(&symbols).await?;

    println!("Top 20 used:");
    for (i, t) in top_20.iter().enumerate() {
        let px = prices.get(&t.symbol).copied().unwrap_or(0.0);
        println!("  {:>2}. {:<8} ${:>8.2}", i + 1, t.symbol, px);
    }
    println!("\nSchwab re-auth in {} days\n", client.days_until_reauth());

    for acc in &accounts {
        if !allowlist.contains(&acc.account_number) {
            continue;
        }
        let plan = rebalance::plan_account(acc, &top_20, &prices);
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

async fn screen(top: usize) -> Result<()> {
    let env = config::Env::load()?;
    let blocklist = config::load_blocklist(&config::blocklist_path())?;

    let tickers = sa::fetch_top_rated(&env.sa_cookie).await?;
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
