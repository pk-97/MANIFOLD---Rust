#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <unity-file-or-symbol>"
  exit 1
fi

query="$1"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

echo "== PORT STATUS =="
rg -n -i --color never "$query" "$root/docs/PORT_STATUS.md" || true
echo

echo "== KNOWN DIVERGENCES =="
rg -n -i --color never "$query" "$root/docs/KNOWN_DIVERGENCES.md" || true
echo

echo "== PARITY TRACKER =="
rg -n -i --color never "$query" "$root/docs/parity_tracker.json" || true
echo

echo "== INLINE COPY RISK (app/ui bridge) =="
rg -n -i --color never "$query" \
  "$root/crates/manifold-app/src/app.rs" \
  "$root/crates/manifold-app/src/ui_bridge.rs" || true
echo

echo "== RUST CANDIDATES =="
rg -n -i --color never "$query" "$root/crates" --glob '!**/target/**' || true
