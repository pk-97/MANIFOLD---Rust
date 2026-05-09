#!/usr/bin/env bash
#
# Bundle launcher — runs as TVLEDMirror.app's CFBundleExecutable so that
# Finder double-click and Dock launches go through here. Reads optional
# default CLI flags from
#
#     ~/Library/Application Support/TVLEDMirror/flags.conf
#
# (one single line, space-separated, e.g.
#     --display 5 --luminance-floor 0.05 --luminance-knee 0.1)
#
# and forwards them to the binary. Any flags passed via `open --args` come
# AFTER and override — clap keeps the last value for repeated flags. When
# the file is missing or empty, the binary just runs with its built-in
# defaults.
#
# This script is the responsible process for TCC purposes; the binary it
# execs inherits TVLEDMirror.app's Screen Recording grant via the bundle.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/tv-led-mirror"

CONFIG="$HOME/Library/Application Support/TVLEDMirror/flags.conf"
DEFAULT_FLAGS=()
if [[ -r "$CONFIG" ]]; then
    # Word-split the single config line into argv tokens. Comments (#) and
    # blank lines are ignored.
    while IFS= read -r line || [[ -n "$line" ]]; do
        line="${line%%#*}"               # strip trailing comment
        line="${line#"${line%%[![:space:]]*}"}"  # ltrim
        [[ -z "$line" ]] && continue
        # shellcheck disable=SC2206
        DEFAULT_FLAGS+=( $line )
    done < "$CONFIG"
fi

exec "$BIN" "${DEFAULT_FLAGS[@]}" "$@"
