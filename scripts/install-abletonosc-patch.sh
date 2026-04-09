#!/bin/sh
#
# install-abletonosc-patch.sh
#
# Adds two new OSC endpoints to the user-installed AbletonOSC remote script:
#
#   /live/track/get/arrangement_clips/end_time
#   /live/track/get/arrangement_clips/muted
#
# These are needed by MANIFOLD's perform-mode HUD to correctly display which
# tracks are supposed to be playing at the current Ableton playhead position
# (looped clips break the existing arrangement_clips/length-based heuristic).
#
# The patch is purely additive — no existing endpoint is modified.
# A backup of the original file is saved alongside the patched file as
# `track.py.bak`. Re-running the script is a safe no-op (idempotent).
#
# After patching, restart Ableton Live so it reloads the remote script.
#
# See assets/abletonosc-patches/README.md for full background.

set -eu

DEFAULT_PATH="$HOME/Library/CloudStorage/Dropbox/Music Production/Ableton/User Library/Remote Scripts/AbletonOSC/abletonosc/track.py"
TARGET="${ABLETONOSC_PATH:-$DEFAULT_PATH}"

if [ ! -f "$TARGET" ]; then
    echo "[abletonosc-patch] ERROR: track.py not found at:" >&2
    echo "  $TARGET" >&2
    echo "" >&2
    echo "Set ABLETONOSC_PATH to your AbletonOSC track.py if it lives elsewhere." >&2
    exit 1
fi

echo "[abletonosc-patch] Target: $TARGET"

python3 - "$TARGET" <<'PYEOF'
import sys, os, shutil

target = sys.argv[1]

with open(target, "r", encoding="utf-8") as f:
    src = f.read()

# Idempotency check — if the new endpoints are already present, do nothing.
if "arrangement_clips/end_time" in src and "arrangement_clips/muted" in src:
    print("[abletonosc-patch] Already patched — no changes needed.")
    sys.exit(0)

# ── Anchor 1: insert the two new function definitions right after the existing
#    track_get_arrangement_clip_start_times function.
fn_anchor = (
    "        def track_get_arrangement_clip_start_times(track, _):\n"
    "            return tuple(clip.start_time for clip in track.arrangement_clips)\n"
)
fn_insert = (
    "\n"
    "        def track_get_arrangement_clip_end_times(track, _):\n"
    "            return tuple(clip.end_time for clip in track.arrangement_clips)\n"
    "\n"
    "        def track_get_arrangement_clip_muted(track, _):\n"
    "            return tuple(clip.muted for clip in track.arrangement_clips)\n"
)
if fn_anchor not in src:
    print("[abletonosc-patch] ERROR: could not find function anchor in track.py.", file=sys.stderr)
    print("[abletonosc-patch] Your AbletonOSC version may be too new or modified.", file=sys.stderr)
    sys.exit(2)

# ── Anchor 2: insert the two new add_handler calls right after the existing
#    arrangement_clips/start_time handler registration.
handler_anchor = (
    '        self.osc_server.add_handler("/live/track/get/arrangement_clips/start_time", '
    "create_track_callback(track_get_arrangement_clip_start_times))\n"
)
handler_insert = (
    '        self.osc_server.add_handler("/live/track/get/arrangement_clips/end_time", '
    "create_track_callback(track_get_arrangement_clip_end_times))\n"
    '        self.osc_server.add_handler("/live/track/get/arrangement_clips/muted", '
    "create_track_callback(track_get_arrangement_clip_muted))\n"
)
if handler_anchor not in src:
    print("[abletonosc-patch] ERROR: could not find handler anchor in track.py.", file=sys.stderr)
    print("[abletonosc-patch] Your AbletonOSC version may be too new or modified.", file=sys.stderr)
    sys.exit(3)

patched = src.replace(fn_anchor, fn_anchor + fn_insert, 1)
patched = patched.replace(handler_anchor, handler_anchor + handler_insert, 1)

# Backup the original — only if a backup doesn't already exist.
backup = target + ".bak"
if not os.path.exists(backup):
    shutil.copy2(target, backup)
    print(f"[abletonosc-patch] Backup written: {backup}")
else:
    print(f"[abletonosc-patch] Backup already exists (preserved): {backup}")

# Write the patched file.
with open(target, "w", encoding="utf-8") as f:
    f.write(patched)
print("[abletonosc-patch] Patched.")
print("[abletonosc-patch] Restart Ableton Live to load the new endpoints.")
PYEOF
