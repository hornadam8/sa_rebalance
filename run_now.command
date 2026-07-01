#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
echo "=== Building sa_rebalance (release) ==="
cargo build --release 2>&1
echo ""
echo "=== Running execute ==="
./target/release/sa_rebalance execute --yes
