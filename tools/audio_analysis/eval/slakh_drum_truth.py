"""Slakh2100 drum-stem truth extraction for the ADTOF bake-off (phase B1,
docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md 2026-07-18 addendum).

Slakh's per-track `is_drum` stem IS already an isolated drum recording (no
demucs separation needed to get a "drum stem" here -- Slakh renders each
instrument to its own stem file by construction), paired with an aligned
MIDI file giving EXACT onset ground truth per GM drum pitch. Reuses
eval.baseline_scoreboard_p3's own GM_TO_CLASS 4-class mapping (kick/snare/
hat/perc) so scoring lines up with ADTOF's own vocabulary -- no new truth
taxonomy invented here.

`eval/data/slakh2100_test` is filled by a STREAMING background fetch
(eval/fetch/slakh2100.py) that may be mid-flight at any given time -- this
module works with whatever tracks currently have BOTH a complete drum stem
audio file and its MIDI on disk (`available_drum_tracks`), per the bake-off
brief's "leave it; use whatever tracks it has landed so far."

Dev/heldout split
-------------------
A track's split assignment is a DETERMINISTIC hash of its track id (not a
random draw at extraction time, not order-of-arrival) -- D9's "heldout is
untouchable" extends here to "heldout membership doesn't get reshuffled as
more of the stream lands": whichever tracks happen to hash into heldout stay
heldout for good, regardless of how many more tracks the still-running fetch
adds later. ~80% dev / ~20% heldout (`_is_heldout`).
"""

from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Dict, List

import pretty_midi
import yaml

from eval.baseline_scoreboard_p3 import CLASSES, GM_TO_CLASS

from eval.paths import DATA_ROOT

SLAKH_TEST_ROOT = DATA_ROOT / "slakh2100_test"
HELDOUT_HASH_MODULUS = 5  # 1-in-5 -> heldout (~20%), deterministic per track id


def _is_heldout(track_id: str) -> bool:
    h = int(hashlib.sha1(track_id.encode("utf-8")).hexdigest(), 16)
    return (h % HELDOUT_HASH_MODULUS) == 0


def available_drum_tracks(root: Path = SLAKH_TEST_ROOT) -> List[Dict[str, object]]:
    """Scan the (possibly still-streaming) Slakh test-split directory for
    tracks with a COMPLETE drum stem (metadata.yaml says is_drum, and both
    the stems/<id>.flac audio and MIDI/<id>.mid file actually exist on disk
    -- the fetch writes files as it encounters them, so most tracks are
    partial at any given moment). Returns one row per usable drum stem
    (a track could in principle have >1 is_drum stem; Slakh in practice has
    exactly one per track, but this doesn't assume that)."""
    if not root.is_dir():
        return []
    out: List[Dict[str, object]] = []
    for track_dir in sorted(p for p in root.iterdir() if p.is_dir()):
        meta_path = track_dir / "metadata.yaml"
        if not meta_path.exists():
            continue
        try:
            meta = yaml.safe_load(meta_path.read_text())
        except Exception:
            continue
        stems = (meta or {}).get("stems") or {}
        for stem_id, stem_meta in stems.items():
            if not stem_meta.get("is_drum"):
                continue
            audio_path = track_dir / "stems" / f"{stem_id}.flac"
            midi_path = track_dir / "MIDI" / f"{stem_id}.mid"
            if audio_path.exists() and midi_path.exists():
                track_id = f"{track_dir.name}_{stem_id}"
                out.append({
                    "id": track_id,
                    "track_dir": track_dir.name,
                    "stem_id": stem_id,
                    "audio_path": str(audio_path),
                    "midi_path": str(midi_path),
                    "split": "heldout" if _is_heldout(track_id) else "dev",
                })
    return out


def load_drum_truth(midi_path: Path) -> Dict[str, List[float]]:
    """Per-class (kick/snare/hat/perc) onset truth from the drum stem's own
    aligned MIDI -- identical GM_TO_CLASS mapping eval.baseline_scoreboard_p3
    already uses for babyslakh, so Slakh-test scores on the same vocabulary."""
    truth: Dict[str, List[float]] = {c: [] for c in CLASSES}
    pm = pretty_midi.PrettyMIDI(str(midi_path))
    for inst in pm.instruments:
        for note in inst.notes:
            cls = GM_TO_CLASS.get(note.pitch)
            if cls is not None:
                truth[cls].append(note.start)
    for c in truth:
        truth[c].sort()
    return truth


def main() -> int:
    import json

    tracks = available_drum_tracks()
    print(f"[slakh_drum_truth] {len(tracks)} usable drum-stem tracks currently on disk "
          f"(fetch may still be streaming -- see eval/data/_fetch_progress.json)")
    n_dev = sum(1 for t in tracks if t["split"] == "dev")
    n_heldout = len(tracks) - n_dev
    print(f"[slakh_drum_truth] split: {n_dev} dev / {n_heldout} heldout")
    for t in tracks:
        truth = load_drum_truth(Path(t["midi_path"]))
        counts = {c: len(v) for c, v in truth.items()}
        print(f"  {t['id']} ({t['split']}): {counts}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
