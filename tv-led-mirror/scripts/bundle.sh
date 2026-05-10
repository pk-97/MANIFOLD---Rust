#!/usr/bin/env bash
#
# Build tv-led-mirror and wrap the binary in TVLEDMirror.app so macOS attributes
# the Screen Recording TCC prompt to *this app's* bundle identifier instead of
# whichever terminal launched it. The grant then lives in
# System Settings → Privacy & Security → Screen Recording under
# "TVLEDMirror" — independent of Terminal/iTerm/etc.
#
# Signing strategy:
#   - PREFERRED: a self-signed cert "TVLEDMirror Code Signing" in the login
#     keychain. macOS keys TCC grants on the cert's designated requirement,
#     which stays the same across rebuilds, so Screen Recording permission
#     persists. Run scripts/create-signing-cert.sh once to install it.
#   - FALLBACK: ad-hoc (`--sign -`). Each build gets a fresh cdhash and you
#     have to delete + re-add the TCC grant for every rebuild.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/TVLEDMirror.app"
BIN="$ROOT/target/release/tv-led-mirror"
CERT_NAME="TVLEDMirror Code Signing"

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

# Pick a signing identity. Match by CN against `security find-identity`'s
# output; that command prints lines like
#   1) AB12CD34...  "TVLEDMirror Code Signing"
# We grab the SHA-1 hash so codesign matches even when multiple certs share
# the same CN (e.g. you re-ran the cert script).
SIGN_ID=$(security find-identity -v -p codesigning 2>/dev/null \
    | awk -v cn="$CERT_NAME" '$0 ~ cn {print $2; exit}')

if [ -n "$SIGN_ID" ]; then
    echo "→ Signing with '$CERT_NAME' ($SIGN_ID) — TCC grants will persist across rebuilds"
    SIGN_ARG="$SIGN_ID"
else
    echo "→ Ad-hoc signing (fallback)"
    echo "  Note: you'll have to re-grant Screen Recording in System Settings"
    echo "  on every rebuild. To fix this once and for all, run:"
    echo "    scripts/create-signing-cert.sh"
    SIGN_ARG="-"
fi

# --identifier locks the bundle identity so TCC tracks the grant by bundle
# ID. Combined with a stable code-signing cert, the grant survives rebuilds.
codesign --force --sign "$SIGN_ARG" \
    --identifier com.latentspace.tv-led-mirror \
    "$APP/Contents/MacOS/tv-led-mirror"
codesign --force --sign "$SIGN_ARG" \
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
