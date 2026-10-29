#!/bin/sh
# Deletes incremental build caches that no build has touched for a day.
#
# Every crate, profile and feature combination gets its own folder under
# target/**/incremental, and cargo never removes folders for variants it stops
# building. With several sessions building in parallel that grows by about
# 50 GB a day. Deleting a folder only costs one slower compile of that crate the
# next time it changes. Run daily at 4:00 by
# ~/Library/LaunchAgents/com.manifold.cache-cleanup.plist.
#
# Safe alongside running agents: it waits until no compiler is running, then
# re-checks each folder's age and moves it out of target/ in one atomic rename
# before deleting it, so a compile starting meanwhile gets a fresh folder.
set -eu

target="$(cd "$(dirname "$0")/.." && pwd)/target"

# Wait for a gap between compiles, up to three hours; otherwise try tomorrow.
waited=0
while pgrep -qx 'rustc|clippy-driver'; do
    if [ "$waited" -ge 10800 ]; then
        echo "$(date): compiles kept running, skipped" >&2
        exit 0
    fi
    sleep 30
    waited=$((waited + 30))
done

rm -rf "$target"/.cache-cleanup.*
trash="$(mktemp -d "$target/.cache-cleanup.XXXXXX")"
/usr/bin/find "$target" -maxdepth 3 -type d -name incremental | while IFS= read -r dir; do
    bin="$(mktemp -d "$trash/XXXXXX")"
    /usr/bin/find "$dir" -mindepth 1 -maxdepth 1 -type d -mtime +0 | while IFS= read -r crate; do
        if [ -n "$(/usr/bin/find "$crate" -maxdepth 0 -mtime +0)" ]; then
            mv "$crate" "$bin/"
        fi
    done
done
rm -rf "$trash"
