#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

if [[ $# -eq 0 ]]; then
  cargo check
  exit 0
fi

package="$1"
shift

if [[ "${1:-}" == "--tests" ]]; then
  cargo test -p "$package"
  exit 0
fi

cargo check -p "$package" "$@"
