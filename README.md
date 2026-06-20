# sa_rebalance

Equal-weight Schwab IRA accounts across Seeking Alpha's *Top Rated Stocks* every market morning. Pulls the SA screener, computes per-account rebalance plans, places Schwab orders, emails a styled report.

## Components

- **Rust binary** (`cargo run -- <subcommand>`) — all the logic
- **launchd plist** — runs `execute --yes` weekdays at 7:30 MT (= 9:30 ET)
- **iCloud Drive** — shared Schwab tokens + SA cookies, so re-auth works from any of your Macs
- **Gmail SMTP** — styled HTML email after each run, plain-text audit copy in `~/.local/state/sa_rebalance/runs/`

## Subcommands

| Command | What it does |
|---|---|
| `auth` | Schwab OAuth dance — refreshes the 7-day refresh token |
| `screen` | Prints today's top 20 from SA, blocklist applied |
| `accounts [--raw]` | Prints Schwab balances + positions (read-only) |
| `plan` | Computes rebalance plan (no orders placed) |
| `execute --yes [--force]` | Places market orders, emails report |
| `notify-test` | Sends a sample email with dummy trades |
| `set-cookie` | Stores a fresh SA cookie (paste cURL, end with Ctrl+D) |

## Re-auth from another machine (laptop)

Both Schwab tokens and the SA cookie live in iCloud Drive — any Mac signed into the same Apple ID can refresh them.

### One-time laptop setup (~5 min)

```bash
# 1. Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. Clone the repo
gh repo clone hornadam8/sa_rebalance   # or: git clone https://github.com/hornadam8/sa_rebalance.git
cd sa_rebalance

# 3. Get .env from the desktop (AirDrop is easiest — don't commit it)
#    On desktop: Finder → ~/repos/personal/sa_rebalance/ → ⌘⇧. to show hidden → right-click .env → Share → AirDrop

# 4. Confirm iCloud Drive is synced (and signed in to same Apple ID)
ls -la "$HOME/Library/Mobile Documents/com~apple~CloudDocs/sa_rebalance/"
# should show tokens.json and sa_cookie.txt; if you see *.icloud stubs, run:
#   brctl download "$HOME/Library/Mobile Documents/com~apple~CloudDocs/sa_rebalance/tokens.json"

# 5. Smoke test
cargo run --quiet -- accounts
```

### Re-auth Schwab tokens (every ~6–7 days)

You'll see `RE-AUTH IN 2D:` in the morning email subject when it's time.

```bash
cd sa_rebalance
cargo run --quiet -- auth
```

1. Click the printed URL.
2. Log into Schwab, approve the app.
3. Browser redirects to `https://127.0.0.1/?code=...`. **The page fails to load — that's expected.** The auth code is in the address bar.
4. Copy the *full* URL from the address bar.
5. Paste at the `>` prompt, hit Enter.
6. You'll see `Saved tokens to .../iCloud.../tokens.json` and `Re-auth required in ~7 days.`

iCloud syncs the file to all your other Macs within ~60 seconds. The desktop's next scheduled run uses the fresh tokens.

### Re-auth SA cookies (rare — months at a time)

The app automatically captures rotated session cookies from SA's `Set-Cookie` headers after each run, so the `_sapi_session_id` and friends refresh themselves. You only need to manually re-bootstrap when the long-lived `user_remember_token` invalidates (typically months, or when you log out of SA in your browser).

You'll know because the morning email will land with a `FAILURES:` subject and `SA screener returned 401` in the body.

```bash
cd sa_rebalance
cargo run --quiet -- set-cookie
```

1. Log into seekingalpha.com in your browser.
2. Visit the screener: https://seekingalpha.com/screeners/96793299-Top-Rated-Stocks
3. DevTools → Network → Fetch/XHR → reload → find `screener_results` → right-click → **Copy → Copy as cURL**.
4. Paste the entire cURL command into the terminal. Press Ctrl+D.
5. The tool extracts the `-b '...'` cookie, writes it to iCloud, and verifies by hitting SA.

## Files written by the app

| Path | What |
|---|---|
| `~/Library/Mobile Documents/com~apple~CloudDocs/sa_rebalance/tokens.json` | Schwab access + refresh tokens (mode 600) |
| `~/Library/Mobile Documents/com~apple~CloudDocs/sa_rebalance/sa_cookie.txt` | Rotating SA cookies (mode 600) |
| `~/.local/state/sa_rebalance/snapshots.json` | Per-account equity from last run (for delta tracking) |
| `~/.local/state/sa_rebalance/runs/YYYY-MM-DD_HH-MM-SS.txt` | Plain-text audit of each run |
| `~/.local/state/sa_rebalance/launchd.{out,err}.log` | launchd job stdout/stderr |

`~/.local/state/` is per-machine (not shared). iCloud paths roam.

## Editing the blocklist

`config/blocklist.txt` lists symbols Schwab won't let us trade. One per line, `#` for comments. Seeded with `SHIP` (broker-call-required). Add more by hand when Schwab rejects an order — the email's `Failures` section will tell you which symbol and why.

## Scheduling (install / uninstall)

```bash
./scripts/install.sh         # builds release binary, installs launchd job, runs Mon–Fri at 7:30 MT
launchctl unload ~/Library/LaunchAgents/com.sa_rebalance.daily.plist   # disable
rm ~/Library/LaunchAgents/com.sa_rebalance.daily.plist                 # remove entirely
```

## Sanity checks

```bash
cargo run --quiet -- screen        # SA cookie still works?
cargo run --quiet -- accounts      # Schwab tokens still work?
cargo run --quiet -- plan          # What would today's rebalance look like?
cargo run --quiet -- notify-test   # Email pipeline working?
```

## Environment variables (.env)

See `.env.example` for the full list. The ones you'll touch most:

- `SA_COOKIE` — bootstrap cookie. Only consulted if `SA_COOKIE_PATH` file is missing/empty.
- `SA_COOKIE_PATH` — preferred cookie store. App reads from here and writes rotated cookies back. Point at iCloud to roam.
- `TOKENS_PATH` — Schwab tokens.json. Point at iCloud to roam.
- `SCHWAB_REBALANCE_ACCOUNTS` — comma-separated account numbers to include. Anything not listed is ignored.
- `GMAIL_USER`, `GMAIL_APP_PASSWORD`, `NOTIFY_TO` — Gmail SMTP for the report email. App password (with or without spaces) from https://myaccount.google.com/apppasswords.

`.env` is gitignored — never commit it. Move it between machines via AirDrop.
