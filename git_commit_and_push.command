#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"

# Remove stale git lock if present
rm -f .git/index.lock

git config user.email "hornadam8@gmail.com"
git config user.name "Adam Horn"

git add -A

git commit -m "Fix cancel_and_check to handle PARTIALLY_FILLED orders

A timed-out limit order can partially fill before the cancel lands.
Previously cancel_and_check only checked for FILLED status, so a
PARTIALLY_FILLED order was treated as cancelled -- the shares landed
in the account unrecorded (causing the LNVGY \$1118 vs \$9945 discrepancy).

Fix: treat PARTIALLY_FILLED the same as FILLED in cancel_and_check,
so extract_fill returns the partial quantity and it shows up in fills.

Also includes earlier session changes:
- absorb_residual_cash: buy floor(cash/price) shares not hardcoded 1
- place_and_collect: cancel timed-out orders before proceeding
- cancel_order / cancel_and_check added to Client
- promote_missing: auto-promote quoted spare when top-20 has no quote
- notify: surface promoted swaps and missing_quotes in reports
- blocklist: add PBR.A
- .env: add account 34264879

Tests: add cancel_and_check_{filled,partially_filled,cancelled} cases"

git push

echo ""
echo "=== Committed and pushed ==="
git log --oneline -3
