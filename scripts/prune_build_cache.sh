#!/bin/sh
# Deletes incremental build caches that no build has touched for a day.
#
# Every crate, profile and feature combination gets its own folder under
# target/**/incremental, and cargo never removes folders for variants it stops
# building. With several sessions building in parallel that grows by about
# 50 GB a day. Deleting a folder only costs one slower compile of that crate the
# next time it changes; folders in use by current builds are newer than a day
# and stay. Run daily by ~/Library/LaunchAgents/com.manifold.prune-build-cache.plist.
set -eu

target="$(cd "$(dirname "$0")/.." && pwd)/target"
/usr/bin/find "$target" -maxdepth 3 -type d -name incremental | while IFS= read -r dir; do
    /usr/bin/find "$dir" -mindepth 1 -maxdepth 1 -type d -mtime +0 -exec rm -rf {} +
done
