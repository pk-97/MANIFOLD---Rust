#!/usr/bin/env bash
#
# Build tv-led-mirror and wrap the binary in TVLEDMirror.app so macOS attributes
# the Screen Recording TCC prompt to *this app's* bundle identifier instead of
# whichever terminal launched it. The grant then lives in
# System Settings → Privacy & Security → Screen Recording under
# "TVLEDMirror" — independent of Terminal/iTerm/etc.
#
# Re-run this whenever you change the source. The ad-hoc signature uses a
# stable identifier (com.latentspace.tv-led-mirror) so the TCC grant survives
# rebuilds in practice for personal use.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/TVLEDMirror.app"
BIN="$ROOT/target/release/tv-led-mirror"

echo "→ Building release binary"
cargo build --release --manifest-path "$ROOT/Cargo.toml"

echo "→ Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
mkdir -p "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/tv-led-mirror"
cp "$ROOT/Info.plist" "$APP/Contents/Info.plist"

echo "→ Ad-hoc signing"
# `--sign -` = ad-hoc signature; --identifier locks the bundle identity so
# TCC tracks the grant by name rather than by file path.
codesign --force --sign - \
    --identifier com.latentspace.tv-led-mirror \
    "$APP/Contents/MacOS/tv-led-mirror"
codesign --force --sign - \
    --identifier com.latentspace.tv-led-mirror \
    "$APP"

echo
echo "Done."
echo "  $APP"
echo
echo "Run with:"
echo "  $APP/Contents/MacOS/tv-led-mirror --display 5"
echo
echo "First launch will trigger a Screen Recording prompt for TVLEDMirror"
echo "(NOT for Terminal). Approve once, then re-launch."
