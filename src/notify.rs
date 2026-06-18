use anyhow::{Context, Result};
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

use crate::config::Env;
use crate::execute::AccountExecutionReport;
use crate::rebalance::{AccountPlan, Side};

pub struct Report {
    pub run_at: OffsetDateTime,
    pub schwab_days_remaining: i64,
    pub top_used: Vec<String>,
    pub blocked: Vec<String>,
    pub accounts: Vec<AccountReport>,
}

pub struct AccountReport {
    pub plan: AccountPlan,
    pub execution: AccountExecutionReport,
}

impl Report {
    pub fn subject(&self) -> String {
        let date = self
            .run_at
            .format(format_description!("[year]-[month]-[day]"))
            .unwrap_or_default();
        let trades: usize = self
            .accounts
            .iter()
            .map(|a| a.execution.fills.len())
            .sum();
        let failures: usize = self
            .accounts
            .iter()
            .map(|a| a.execution.failures.len())
            .sum();
        let mut prefix = String::new();
        if failures > 0 {
            prefix.push_str("FAILURES: ");
        }
        if self.schwab_days_remaining <= 0 {
            prefix.push_str("RE-AUTH EXPIRED: ");
        } else if self.schwab_days_remaining <= 2 {
            prefix.push_str(&format!("RE-AUTH IN {}D: ", self.schwab_days_remaining));
        }
        format!(
            "{prefix}SA rebalance — {date} — {} accounts, {} trades",
            self.accounts.len(),
            trades
        )
    }

    pub fn body(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "Run at: {}", self.run_at);
        let _ = writeln!(out, "Schwab re-auth in: {} days", self.schwab_days_remaining);
        let _ = writeln!(out);
        let _ = writeln!(out, "Top 20 used:");
        for (i, sym) in self.top_used.iter().enumerate() {
            let _ = writeln!(out, "  {:>2}. {}", i + 1, sym);
        }
        if !self.blocked.is_empty() {
            let _ = writeln!(out, "Blocked (skipped from list): {}", self.blocked.join(", "));
        }
        for a in &self.accounts {
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "Account {} — equity ${:.2}, cash ${:.2}",
                a.plan.account_number, a.plan.equity, a.plan.cash
            );
            let _ = writeln!(
                out,
                "  subset: {} names, target/name: ${:.2}",
                a.plan.subset_size, a.plan.target_per_name
            );
            if !a.plan.skipped_unaffordable.is_empty() {
                let _ = writeln!(
                    out,
                    "  skipped (won't fit): {}",
                    a.plan.skipped_unaffordable.join(", ")
                );
            }
            let (sells, buys): (Vec<_>, Vec<_>) = a
                .execution
                .fills
                .iter()
                .partition(|f| matches!(f.trade.side, Side::Sell));
            if !sells.is_empty() {
                let _ = writeln!(out, "  Sells filled:");
                for f in &sells {
                    let _ = writeln!(
                        out,
                        "    {:<8} {:>5} @ ${:.4}  = ${:.2}",
                        f.trade.symbol,
                        f.filled_quantity,
                        f.avg_price,
                        f.avg_price * f.filled_quantity as f64
                    );
                }
            }
            if !buys.is_empty() {
                let _ = writeln!(out, "  Buys filled:");
                for f in &buys {
                    let _ = writeln!(
                        out,
                        "    {:<8} {:>5} @ ${:.4}  = ${:.2}",
                        f.trade.symbol,
                        f.filled_quantity,
                        f.avg_price,
                        f.avg_price * f.filled_quantity as f64
                    );
                }
            }
            if !a.execution.failures.is_empty() {
                let _ = writeln!(out, "  Failures:");
                for x in &a.execution.failures {
                    let _ = writeln!(
                        out,
                        "    {:?} {:<8} {:>5} — {}",
                        x.trade.side, x.trade.symbol, x.trade.shares, x.reason
                    );
                }
            }
            let _ = writeln!(
                out,
                "  Estimated residual cash: ${:.2}",
                a.plan.estimated_residual_cash
            );
        }
        out
    }
}

pub fn now_local() -> OffsetDateTime {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    OffsetDateTime::now_utc().to_offset(offset)
}

pub fn write_local(report: &Report) -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    let dir = home.join(".local/state/sa_rebalance/runs");
    fs::create_dir_all(&dir)?;
    let fname = report
        .run_at
        .format(format_description!("[year]-[month]-[day]_[hour]-[minute]-[second]"))
        .unwrap_or_else(|_| "run".into());
    let path = dir.join(format!("{fname}.txt"));
    let body = format!("{}\n\n{}", report.subject(), report.body());
    fs::write(&path, body)?;
    Ok(path)
}

pub async fn send_email(env: &Env, report: &Report) -> Result<()> {
    let email = Message::builder()
        .from(env.gmail_user.parse()?)
        .to(env.notify_to.parse()?)
        .subject(report.subject())
        .header(ContentType::TEXT_PLAIN)
        .body(report.body())?;

    let creds = Credentials::new(env.gmail_user.clone(), env.gmail_app_password.clone());
    let mailer: AsyncSmtpTransport<Tokio1Executor> =
        AsyncSmtpTransport::<Tokio1Executor>::relay("smtp.gmail.com")?
            .credentials(creds)
            .build();
    mailer.send(email).await.context("sending email")?;
    Ok(())
}
