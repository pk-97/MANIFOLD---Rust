#!/usr/bin/env bash
# Build and install the Manifold Analyzer VST3.
#
# Runs `cargo xtask bundle manifold-analyzer-plugin --release`, then
# replaces the installed copy at ~/Library/Audio/Plug-Ins/VST3/.
# Ableton (and any other host) needs to rescan / reload to pick up
# the new binary.
#
# Flags:
#   --debug   Build without --release (faster rebuild, slower DSP)
#   --no-install  Build only; skip the copy into the VST3 directory

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGINS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUNDLE_NAME="manifold-analyzer-plugin.vst3"
VST3_DIR="$HOME/Library/Audio/Plug-Ins/VST3"

PROFILE_FLAG="--release"
PROFILE_DIR="bundled"
INSTALL=1

for arg in "$@"; do
    case "$arg" in
        --debug)
            PROFILE_FLAG=""
            PROFILE_DIR="bundled-debug"
            ;;
        --no-install)
            INSTALL=0
            ;;
        -h|--help)
            sed -n '2,12p' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *)
            echo "unknown flag: $arg" >&2
            exit 2
            ;;
    esac
done

cd "$PLUGINS_DIR"

echo "==> Bundling manifold-analyzer-plugin ($PROFILE_DIR)"
# shellcheck disable=SC2086
cargo xtask bundle manifold-analyzer-plugin $PROFILE_FLAG

BUILT_BUNDLE="$PLUGINS_DIR/target/$PROFILE_DIR/$BUNDLE_NAME"
if [[ ! -d "$BUILT_BUNDLE" ]]; then
    echo "build succeeded but bundle not found at $BUILT_BUNDLE" >&2
    exit 1
fi

if [[ "$INSTALL" == "0" ]]; then
    echo "==> Skipping install (--no-install). Bundle at: $BUILT_BUNDLE"
    exit 0
fi

mkdir -p "$VST3_DIR"
echo "==> Installing to $VST3_DIR/$BUNDLE_NAME"
rm -rf "$VST3_DIR/$BUNDLE_NAME"
cp -R "$BUILT_BUNDLE" "$VST3_DIR/"

echo "==> Done. Reload / rescan VST3 in your DAW to pick up the new build."
