#!/usr/bin/env bash
#
# Launch TVLEDMirror.app via LaunchServices so macOS attributes the Screen
# Recording TCC request to *this* app's bundle identifier rather than the
# parent terminal. Forwards the app's stdout/stderr back to this terminal so
# logs stay visible, and forwards Ctrl+C so the LED pipeline gets a chance to
# send a final blackout.
#
# Why we need this: when a binary is exec'd directly from a shell (e.g.
# `./TVLEDMirror.app/Contents/MacOS/tv-led-mirror`), the kernel's "responsible
# process" walk still attributes TCC requests to the shell's terminal app —
# even if the binary lives inside an .app bundle. Launching via `open`
# forces LaunchServices to spawn the app as its own LS-tracked process, which
# is what TCC actually checks against the Screen Recording grant list.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
APP="$ROOT/target/TVLEDMirror.app"
BIN_NAME="tv-led-mirror"

if [[ ! -d "$APP" ]]; then
    echo "Bundle not found at $APP" >&2
    echo "Run: $ROOT/scripts/bundle.sh" >&2
    exit 1
fi

# `open --wait-apps` blocks until the app exits but doesn't forward signals
# to it. Trap Ctrl+C and send SIGINT to the binary directly so its handler
# fires and the LEDs get a clean blackout instead of freezing on the last frame.
on_int() {
    pkill -INT "$BIN_NAME" 2>/dev/null || true
    sleep 0.3
}
trap on_int INT TERM

# /dev/tty pipes the app's stdout/stderr back into this terminal so you see
# the per-second "captured N fps" logs.
exec open --wait-apps \
    --stdout /dev/tty \
    --stderr /dev/tty \
    -a "$APP" \
    --args "$@"
