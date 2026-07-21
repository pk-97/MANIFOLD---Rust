#!/bin/bash
# One heavy cargo/flow run at a time, machine-wide (D-14, .claude/orchestration/decisions.md).
# Two GUI lockups on 2026-07-21 correlate with concurrent full sweeps; this makes the
# serialization rule mechanical instead of disciplinary.
#
# Usage: .claude/scripts/with-build-lock.sh <command...>
# Blocks (polling) until the lock is free, runs the command, releases. mkdir-based
# because macOS ships no flock(1). Stale locks (dead pid) are reclaimed automatically.

set -u
LOCK_DIR="${TMPDIR:-/tmp}/manifold-build-lock"
WAITED=0

while ! mkdir "$LOCK_DIR" 2>/dev/null; do
    HOLDER=$(cat "$LOCK_DIR/pid" 2>/dev/null || echo "")
    if [ -n "$HOLDER" ] && ! kill -0 "$HOLDER" 2>/dev/null; then
        rm -rf "$LOCK_DIR"   # stale: holder is dead
        continue
    fi
    if [ $((WAITED % 60)) -eq 0 ]; then
        echo "with-build-lock: waiting for build lock (held by pid ${HOLDER:-unknown}, ${WAITED}s)" >&2
    fi
    sleep 5
    WAITED=$((WAITED + 5))
done
echo $$ > "$LOCK_DIR/pid"
trap 'rm -rf "$LOCK_DIR"' EXIT INT TERM

"$@"
