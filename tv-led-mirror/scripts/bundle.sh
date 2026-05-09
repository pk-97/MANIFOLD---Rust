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
cp "$ROOT/launch.sh" "$APP/Contents/MacOS/launch.sh"
chmod +x "$APP/Contents/MacOS/launch.sh"
cp "$ROOT/Info.plist" "$APP/Contents/Info.plist"
# PkgInfo helps LaunchServices treat the directory as a real app for TCC.
printf 'APPL????' > "$APP/Contents/PkgInfo"

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
echo "  $ROOT/run.sh --display 5            # from terminal, with logs"
echo "  open '$APP'                         # from terminal, no logs"
echo "  (or just double-click the .app)     # from Finder/Dock"
echo
echo "To persist default flags for double-click / Dock launches, write a"
echo "single line of CLI flags into:"
echo "  ~/Library/Application Support/TVLEDMirror/flags.conf"
echo
echo "Example:"
echo "  mkdir -p \"\$HOME/Library/Application Support/TVLEDMirror\""
echo "  echo '--display 5 --luminance-floor 0.05 --luminance-knee 0.1' \\"
echo "      > \"\$HOME/Library/Application Support/TVLEDMirror/flags.conf\""
