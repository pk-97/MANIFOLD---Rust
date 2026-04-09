#!/bin/sh
#
# uninstall-abletonosc-patch.sh
#
# Restores the AbletonOSC track.py from the backup created by
# install-abletonosc-patch.sh. After running, restart Ableton Live.

set -eu

DEFAULT_PATH="$HOME/Library/CloudStorage/Dropbox/Music Production/Ableton/User Library/Remote Scripts/AbletonOSC/abletonosc/track.py"
TARGET="${ABLETONOSC_PATH:-$DEFAULT_PATH}"
BACKUP="$TARGET.bak"

if [ ! -f "$BACKUP" ]; then
    echo "[abletonosc-patch] No backup found at:" >&2
    echo "  $BACKUP" >&2
    echo "Nothing to uninstall." >&2
    exit 1
fi

cp "$BACKUP" "$TARGET"
rm "$BACKUP"
echo "[abletonosc-patch] Restored $TARGET from backup."
echo "[abletonosc-patch] Restart Ableton Live to reload the unpatched script."
