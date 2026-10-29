#!/usr/bin/env bash
# Sets up the detector lab on a Linux box with an NVIDIA card. Run it there, from a checkout of this repo:
#   tools/audio_analysis/lab/linux_setup.sh [MAC_HOST]
# 1. Mac paths resolve here. songs.json, the label files, the offset proofs and the catalogue store Mac paths, so
#    /Users/peterkiemann links to this home, the Mac's Dropbox folder to ~/Dropbox (the Linux Dropbox app's folder,
#    which must sync Music Production for labelling and for the song bounces) and ~/MANIFOLD - Rust to this checkout.
# 2. A Python venv at ~/.venvs/manifold-lab with the lab's packages (the default Linux torch wheel ships CUDA).
# 3. With MAC_HOST (ssh to the Mac, Remote Login on), copies the lab caches and label files, about 52 GB.
# It then lists any song file still missing; preflight.py checks the card trains a net.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
VENV="$HOME/.venvs/manifold-lab"

link() {   # link TARGET NAME: NAME points at TARGET; an existing NAME that is anything else stops the script
    if [ -L "$2" ] && [ "$(readlink "$2")" = "$1" ]; then return; fi
    if [ -e "$2" ] || [ -L "$2" ]; then echo "$2 exists and is not a link to $1; move it and rerun" >&2; exit 1; fi
    ${3:-} ln -s "$1" "$2"
    echo "linked $2 -> $1"
}
[ -d /Users ] || sudo mkdir /Users
link "$HOME" /Users/peterkiemann sudo
mkdir -p "$HOME/Library/CloudStorage"
link "$HOME/Dropbox" "$HOME/Library/CloudStorage/Dropbox"
[ "$REPO" = "$HOME/MANIFOLD - Rust" ] || link "$REPO" "$HOME/MANIFOLD - Rust"
[ -d "$HOME/Dropbox/Music Production" ] || echo "note: ~/Dropbox/Music Production is not here yet; install Dropbox and let it sync"

[ -d "$VENV" ] || python3 -m venv "$VENV"
"$VENV/bin/pip" install -q --upgrade pip
"$VENV/bin/pip" install -q torch numpy scipy scikit-learn soundfile

if [ -n "${1:-}" ]; then
    mkdir -p "$HOME/.cache/manifold"
    for d in detector-lab kick-goal-2026-10-10 ableton; do
        rsync -a --info=progress2 "$1:.cache/manifold/$d" "$HOME/.cache/manifold/"
    done
fi

"$VENV/bin/python" - <<'EOF'
import json, os
songs = json.load(open(os.path.expanduser('~/MANIFOLD - Rust/tools/audio_analysis/lab/songs.json')))
missing = sorted({p for s in songs for p in s['source'].get('paths', []) if not os.path.exists(p)})
print(f'{len(missing)} song file(s) missing' + ''.join(f'\n  {p}' for p in missing))
EOF
echo "next: source $VENV/bin/activate, then tools/audio_analysis/lab/preflight.py tools/audio_analysis/lab/plans/<plan>.json"
