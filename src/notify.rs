use anyhow::{Context, Result};
use lettre::message::{MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

use crate::config::Env;
use crate::execute::{AccountExecutionReport, Fill, OrderFailure};
use crate::rebalance::{AccountPlan, Side};

pub struct Report {
    pub run_at: OffsetDateTime,
    pub schwab_days_remaining: i64,
    pub sa_cookie_age_days: Option<i64>,
    pub top_used: Vec<String>,
    pub blocked: Vec<String>,
    pub accounts: Vec<AccountReport>,
}

pub struct AccountReport {
    pub plan: AccountPlan,
    pub execution: AccountExecutionReport,
    pub prev_equity: Option<f64>,
    pub prev_ts_unix: Option<i64>,
    pub post_residual_cash: f64,
    pub sanity_warning: Option<String>,
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

    pub fn body_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "Run at: {}", self.run_at);
        let _ = writeln!(out, "Schwab re-auth in: {} days", self.schwab_days_remaining);
        if let Some(d) = self.sa_cookie_age_days {
            let _ = writeln!(
                out,
                "SA cookie last refreshed: {d} day{} ago",
                if d == 1 { "" } else { "s" }
            );
        }
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
                "  Post-trade cash: ${:.2}",
                a.post_residual_cash
            );
            if let Some(w) = &a.sanity_warning {
                let _ = writeln!(out, "  WARNING: {w}");
            }
        }
        out
    }

    pub fn body_html(&self) -> String {
        let date = self
            .run_at
            .format(format_description!("[year]-[month]-[day]"))
            .unwrap_or_default();
        let time = self
            .run_at
            .format(format_description!("[hour]:[minute]:[second]"))
            .unwrap_or_default();
        let total_fills: usize = self.accounts.iter().map(|a| a.execution.fills.len()).sum();
        let total_failures: usize = self
            .accounts
            .iter()
            .map(|a| a.execution.failures.len())
            .sum();

        let reauth_color = if self.schwab_days_remaining <= 0 {
            "#dc2626"
        } else if self.schwab_days_remaining <= 2 {
            "#d97706"
        } else {
            "#4a5568"
        };
        let reauth_weight = if self.schwab_days_remaining <= 2 {
            "700"
        } else {
            "500"
        };

        let mut s = String::new();
        let _ = write!(s, r#"<!doctype html><html><body style="margin:0;padding:0;background:#edf2f7;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#1a202c;">"#);
        let _ = write!(s, r#"<table role="presentation" cellpadding="0" cellspacing="0" width="100%" style="background:#edf2f7;padding:24px 12px;"><tr><td align="center">"#);
        let _ = write!(s, r#"<table role="presentation" cellpadding="0" cellspacing="0" width="100%" style="max-width:720px;background:#ffffff;border-radius:12px;box-shadow:0 1px 3px rgba(0,0,0,0.06);overflow:hidden;"><tr><td style="padding:24px 28px;">"#);

        let _ = write!(
            s,
            r#"<div style="font-size:12px;letter-spacing:0.08em;text-transform:uppercase;color:#718096;font-weight:600;">SA rebalance</div>"#
        );
        let _ = write!(
            s,
            r#"<div style="font-size:24px;font-weight:700;color:#1a365d;margin-top:4px;">{date}</div>"#
        );
        let _ = write!(
            s,
            r#"<div style="font-size:14px;color:#4a5568;margin-top:6px;">Run at {time} local · {} accounts · {total_fills} fills{}</div>"#,
            self.accounts.len(),
            if total_failures > 0 {
                format!(
                    r#" · <span style="color:#c53030;font-weight:600;">{total_failures} failures</span>"#
                )
            } else {
                String::new()
            }
        );
        let _ = write!(
            s,
            r#"<div style="margin-top:14px;font-size:14px;color:{reauth_color};font-weight:{reauth_weight};">Schwab re-auth in {} days</div>"#,
            self.schwab_days_remaining
        );
        if let Some(d) = self.sa_cookie_age_days {
            let _ = write!(
                s,
                r#"<div style="margin-top:4px;font-size:13px;color:#718096;">SA cookie last refreshed {d} day{} ago</div>"#,
                if d == 1 { "" } else { "s" }
            );
        }

        let _ = write!(
            s,
            r#"<div style="margin-top:24px;font-size:12px;letter-spacing:0.06em;text-transform:uppercase;color:#718096;font-weight:600;">Top 20 used</div>"#
        );
        let _ = write!(s, r#"<div style="margin-top:8px;">"#);
        for sym in &self.top_used {
            let _ = write!(s, r#"<span style="display:inline-block;padding:4px 10px;margin:3px 4px 3px 0;background:#edf2f7;color:#2d3748;border-radius:12px;font-family:Menlo,Consolas,monospace;font-size:12px;font-weight:600;">{sym}</span>"#);
        }
        let _ = write!(s, r#"</div>"#);
        if !self.blocked.is_empty() {
            let _ = write!(s, r#"<div style="margin-top:10px;font-size:13px;color:#718096;">Blocked: <span style="font-family:Menlo,Consolas,monospace;color:#c53030;">{}</span></div>"#, self.blocked.join(", "));
        }

        for a in &self.accounts {
            self.append_account_html(&mut s, a);
        }

        let _ = write!(
            s,
            r#"<div style="margin-top:28px;font-size:11px;color:#a0aec0;border-top:1px solid #edf2f7;padding-top:14px;">Generated by sa_rebalance · audit copy saved to ~/.local/state/sa_rebalance/runs/</div>"#
        );

        let _ = write!(s, r#"</td></tr></table></td></tr></table></body></html>"#);
        s
    }

    fn append_account_html(&self, s: &mut String, a: &AccountReport) {
        let (sells, buys): (Vec<&Fill>, Vec<&Fill>) = a
            .execution
            .fills
            .iter()
            .partition(|f| matches!(f.trade.side, Side::Sell));

        let _ = write!(s, r#"<div style="margin-top:24px;border:1px solid #e2e8f0;border-radius:10px;overflow:hidden;">"#);
        let _ = write!(
            s,
            r#"<div style="padding:14px 18px;background:#f7fafc;border-bottom:1px solid #e2e8f0;">"#
        );
        let _ = write!(
            s,
            r#"<div style="font-size:11px;letter-spacing:0.08em;text-transform:uppercase;color:#718096;font-weight:600;">Account</div>"#
        );
        let _ = write!(
            s,
            r#"<div style="font-family:Menlo,Consolas,monospace;font-size:18px;font-weight:700;color:#1a365d;margin-top:2px;">{}</div>"#,
            a.plan.account_number
        );
        let _ = write!(s, r#"<table role="presentation" cellpadding="0" cellspacing="0" style="margin-top:10px;font-size:13px;color:#4a5568;"><tr>"#);
        let _ = write!(
            s,
            r#"<td style="padding-right:24px;"><div style="color:#718096;font-size:11px;text-transform:uppercase;letter-spacing:0.06em;">Equity</div><div style="font-variant-numeric:tabular-nums;font-weight:600;color:#1a202c;font-size:15px;">{}</div>{}</td>"#,
            fmt_money(a.plan.equity),
            equity_change_html(a.plan.equity, a.prev_equity, a.prev_ts_unix),
        );
        let _ = write!(
            s,
            r#"<td style="padding-right:24px;"><div style="color:#718096;font-size:11px;text-transform:uppercase;letter-spacing:0.06em;">Cash</div><div style="font-variant-numeric:tabular-nums;font-weight:600;color:#1a202c;font-size:15px;">{}</div></td>"#,
            fmt_money(a.plan.cash)
        );
        let _ = write!(
            s,
            r#"<td style="padding-right:24px;"><div style="color:#718096;font-size:11px;text-transform:uppercase;letter-spacing:0.06em;">Target / name</div><div style="font-variant-numeric:tabular-nums;font-weight:600;color:#1a202c;font-size:15px;">{} · {} names</div></td>"#,
            fmt_money(a.plan.target_per_name),
            a.plan.subset_size
        );
        let _ = write!(s, r#"</tr></table>"#);
        if !a.plan.skipped_unaffordable.is_empty() {
            let _ = write!(s, r#"<div style="margin-top:10px;font-size:13px;color:#718096;">Skipped (won't fit): <span style="font-family:Menlo,Consolas,monospace;color:#4a5568;">{}</span></div>"#, a.plan.skipped_unaffordable.join(", "));
        }
        let _ = write!(s, r#"</div>"#);

        let _ = write!(s, r#"<div style="padding:18px;">"#);

        if !a.execution.failures.is_empty() {
            self.append_failures_html(s, &a.execution.failures);
        }
        if !sells.is_empty() {
            self.append_fills_table_html(s, "Sells filled", &sells, "#c53030");
        }
        if !buys.is_empty() {
            self.append_fills_table_html(s, "Buys filled", &buys, "#2f855a");
        }
        if a.execution.fills.is_empty() && a.execution.failures.is_empty() {
            let _ = write!(
                s,
                r#"<div style="font-size:13px;color:#718096;font-style:italic;">No trades executed.</div>"#
            );
        }

        let _ = write!(
            s,
            r#"<div style="margin-top:14px;padding-top:12px;border-top:1px solid #edf2f7;font-size:13px;color:#4a5568;">Post-trade cash: <span style="font-variant-numeric:tabular-nums;font-weight:600;color:#1a202c;">{}</span></div>"#,
            fmt_money(a.post_residual_cash)
        );
        if let Some(warning) = &a.sanity_warning {
            let _ = write!(
                s,
                r#"<div style="margin-top:10px;padding:10px 12px;background:#fffaf0;border:1px solid #f6ad55;border-radius:6px;font-size:13px;color:#7b341e;">⚠ {}</div>"#,
                html_escape(warning)
            );
        }

        let _ = write!(s, r#"</div></div>"#);
    }

    fn append_fills_table_html(&self, s: &mut String, title: &str, fills: &[&Fill], color: &str) {
        let _ = write!(s, r#"<div style="margin-bottom:14px;">"#);
        let _ = write!(s, r#"<div style="font-size:12px;letter-spacing:0.06em;text-transform:uppercase;color:{color};font-weight:700;margin-bottom:6px;">{title} ({})</div>"#, fills.len());
        let _ = write!(s, r#"<table role="presentation" cellpadding="0" cellspacing="0" width="100%" style="border-collapse:collapse;font-size:13px;">"#);
        let _ = write!(s, r#"<thead><tr style="background:#f7fafc;color:#718096;text-transform:uppercase;font-size:11px;letter-spacing:0.06em;"><th align="left" style="padding:8px 10px;border-bottom:1px solid #e2e8f0;">Symbol</th><th align="right" style="padding:8px 10px;border-bottom:1px solid #e2e8f0;">Qty</th><th align="right" style="padding:8px 10px;border-bottom:1px solid #e2e8f0;">Fill</th><th align="right" style="padding:8px 10px;border-bottom:1px solid #e2e8f0;">Total</th></tr></thead><tbody>"#);
        for f in fills {
            let total = f.avg_price * f.filled_quantity as f64;
            let _ = write!(s, r#"<tr><td style="padding:8px 10px;border-bottom:1px solid #f0f4f8;font-family:Menlo,Consolas,monospace;font-weight:600;color:#1a365d;">{}</td><td align="right" style="padding:8px 10px;border-bottom:1px solid #f0f4f8;font-variant-numeric:tabular-nums;">{}</td><td align="right" style="padding:8px 10px;border-bottom:1px solid #f0f4f8;font-variant-numeric:tabular-nums;color:#4a5568;">{}</td><td align="right" style="padding:8px 10px;border-bottom:1px solid #f0f4f8;font-variant-numeric:tabular-nums;font-weight:600;">{}</td></tr>"#,
                f.trade.symbol,
                f.filled_quantity,
                fmt_price(f.avg_price),
                fmt_money(total),
            );
        }
        let _ = write!(s, r#"</tbody></table></div>"#);
    }

    fn append_failures_html(&self, s: &mut String, failures: &[OrderFailure]) {
        let _ = write!(s, r#"<div style="margin-bottom:14px;padding:12px 14px;background:#fff5f5;border:1px solid #fed7d7;border-radius:8px;">"#);
        let _ = write!(s, r#"<div style="font-size:12px;letter-spacing:0.06em;text-transform:uppercase;color:#c53030;font-weight:700;margin-bottom:8px;">Failures ({})</div>"#, failures.len());
        for f in failures {
            let side = match f.trade.side {
                Side::Buy => "BUY",
                Side::Sell => "SELL",
            };
            let _ = write!(s, r#"<div style="font-size:13px;color:#2d3748;margin-bottom:6px;"><span style="font-family:Menlo,Consolas,monospace;font-weight:600;color:#c53030;">{side} {} × {}</span><div style="font-size:12px;color:#718096;margin-top:2px;">{}</div></div>"#,
                f.trade.symbol, f.trade.shares, html_escape(&f.reason)
            );
        }
        let _ = write!(s, r#"</div>"#);
    }
}

fn equity_change_html(curr: f64, prev: Option<f64>, prev_ts_unix: Option<i64>) -> String {
    let Some(p) = prev else {
        return String::new();
    };
    if p <= 0.0 {
        return String::new();
    }
    let delta = curr - p;
    let pct = delta / p * 100.0;
    let (color, sign) = if delta >= 0.0 {
        ("#2f855a", "+")
    } else {
        ("#c53030", "−")
    };
    let abs_delta = delta.abs();
    let abs_pct = pct.abs();
    let since = prev_ts_unix.and_then(|ts| {
        OffsetDateTime::from_unix_timestamp(ts).ok().map(|t| {
            t.to_offset(UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
                .format(format_description!("[year]-[month]-[day]"))
                .unwrap_or_default()
        })
    });
    let since_html = match since {
        Some(d) if !d.is_empty() => format!(r#" <span style="color:#a0aec0;">since {d}</span>"#),
        _ => String::new(),
    };
    format!(
        r#"<div style="margin-top:2px;font-variant-numeric:tabular-nums;font-size:12px;color:{color};font-weight:600;">{sign}{} ({sign}{:.2}%){since_html}</div>"#,
        fmt_money(abs_delta),
        abs_pct,
    )
}

fn fmt_money(amount: f64) -> String {
    let cents = (amount.abs() * 100.0).round() as i64;
    let dollars = cents / 100;
    let cents_part = cents % 100;
    let dollars_with_commas = with_thousands(dollars);
    let sign = if amount < 0.0 { "-" } else { "" };
    format!("{sign}${dollars_with_commas}.{cents_part:02}")
}

fn fmt_price(amount: f64) -> String {
    let raw = format!("{:.4}", amount);
    let (int_part, dec_part) = raw.split_once('.').unwrap_or((&raw, ""));
    let trimmed = dec_part.trim_end_matches('0');
    let dec = if trimmed.len() < 2 {
        format!("{trimmed:0<2}")
    } else {
        trimmed.to_string()
    };
    format!("${int_part}.{dec}")
}

fn with_thousands(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
    let body = format!("{}\n\n{}", report.subject(), report.body_text());
    fs::write(&path, body)?;
    Ok(path)
}

pub async fn send_failure_email(env: &Env, error: &str) -> Result<()> {
    let date = now_local()
        .format(format_description!("[year]-[month]-[day]"))
        .unwrap_or_default();
    let body = format!(
        "sa_rebalance run failed before placing trades:\n\n{error}\n\n\
         No orders were placed. Check Schwab tokens, SA cookies, and Schwab's status page."
    );
    let email = Message::builder()
        .from(env.gmail_user.parse()?)
        .to(env.notify_to.parse()?)
        .subject(format!("FAILURES: SA rebalance — {date} — run aborted"))
        .header(lettre::message::header::ContentType::TEXT_PLAIN)
        .body(body)?;

    let creds = Credentials::new(env.gmail_user.clone(), env.gmail_app_password.clone());
    let mailer: AsyncSmtpTransport<Tokio1Executor> =
        AsyncSmtpTransport::<Tokio1Executor>::relay("smtp.gmail.com")?
            .credentials(creds)
            .build();
    mailer.send(email).await.context("sending failure email")?;
    Ok(())
}

pub async fn send_email(env: &Env, report: &Report) -> Result<()> {
    let email = Message::builder()
        .from(env.gmail_user.parse()?)
        .to(env.notify_to.parse()?)
        .subject(report.subject())
        .multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::plain(report.body_text()))
                .singlepart(SinglePart::html(report.body_html())),
        )?;

    let creds = Credentials::new(env.gmail_user.clone(), env.gmail_app_password.clone());
    let mailer: AsyncSmtpTransport<Tokio1Executor> =
        AsyncSmtpTransport::<Tokio1Executor>::relay("smtp.gmail.com")?
            .credentials(creds)
            .build();
    mailer.send(email).await.context("sending email")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_money_inserts_thousands_separators() {
        assert_eq!(fmt_money(1234567.89), "$1,234,567.89");
        assert_eq!(fmt_money(1000.0), "$1,000.00");
        assert_eq!(fmt_money(999.99), "$999.99");
    }

    #[test]
    fn fmt_money_handles_negative() {
        assert_eq!(fmt_money(-1234.56), "-$1,234.56");
    }

    #[test]
    fn fmt_money_handles_zero() {
        assert_eq!(fmt_money(0.0), "$0.00");
    }

    #[test]
    fn fmt_price_trims_trailing_zeros() {
        assert_eq!(fmt_price(161.4200), "$161.42");
        assert_eq!(fmt_price(40.5800), "$40.58");
    }

    #[test]
    fn fmt_price_keeps_significant_decimals() {
        assert_eq!(fmt_price(14.8537), "$14.8537");
        assert_eq!(fmt_price(161.4250), "$161.425");
    }

    #[test]
    fn fmt_price_pads_to_two_decimals_minimum() {
        assert_eq!(fmt_price(100.0), "$100.00");
        assert_eq!(fmt_price(50.5), "$50.50");
    }

    #[test]
    fn with_thousands_handles_small_numbers() {
        assert_eq!(with_thousands(0), "0");
        assert_eq!(with_thousands(42), "42");
        assert_eq!(with_thousands(999), "999");
    }

    #[test]
    fn with_thousands_inserts_commas() {
        assert_eq!(with_thousands(1000), "1,000");
        assert_eq!(with_thousands(1234567), "1,234,567");
    }

    #[test]
    fn equity_change_html_empty_when_no_prior() {
        let html = equity_change_html(1000.0, None, None);
        assert!(html.is_empty());
    }

    #[test]
    fn equity_change_html_shows_positive_delta_in_green() {
        let html = equity_change_html(1100.0, Some(1000.0), None);
        assert!(html.contains("+"));
        assert!(html.contains("10.00%"));
        assert!(html.contains("#2f855a"));
    }

    #[test]
    fn equity_change_html_shows_negative_delta_in_red() {
        let html = equity_change_html(900.0, Some(1000.0), None);
        assert!(html.contains("10.00%"));
        assert!(html.contains("#c53030"));
    }

    #[test]
    fn html_escape_protects_against_injection() {
        let escaped = html_escape(r#"<script>alert("x")</script>"#);
        assert!(!escaped.contains("<script>"));
        assert!(escaped.contains("&lt;script&gt;"));
        assert!(escaped.contains("&quot;"));
    }
}
