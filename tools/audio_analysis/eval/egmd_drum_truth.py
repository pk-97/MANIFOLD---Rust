"""E-GMD truth extraction for the ADTOF bake-off (phase B1,
docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md 2026-07-18 addendum). Thin wrapper
around the manifest eval/fetch/egmd.py already wrote: E-GMD audio IS ALREADY
an isolated drum-kit recording (no demucs separation needed -- there is no
other instrument in the mix to remove), paired with an aligned MIDI capture
of every hit (kick/snare/hat/tom/etc, extended Roland e-kit GM-ish pitches).

Reuses eval.baseline_scoreboard_p3's GM_TO_CLASS 4-class mapping (kick/
snare/hat/perc), same as babyslakh and Slakh -- no new taxonomy. Coverage
gap, disclosed rather than silently patched: a handful of Roland-extended
pitches E-GMD's MIDI captures that sit OUTSIDE the standard GM drum map
(e.g. 22/26, hi-hat edge-click articulations) aren't in GM_TO_CLASS and are
silently uncounted, same as any out-of-map pitch already is for babyslakh --
measured at ~3% of notes in a 15-file sample, 2026-07-18.

Domain: "other" (per the design doc's dev_2026-07-18 note: "E-GMD = acoustic
kits mostly -> tag honestly, likely 'other'") -- these are real acoustic/
electronic-kit-SAMPLE recordings across 43 kit patches, not EDM-genre
material; only the self_render EDM-kit fixture and the manifold_own kick
fixtures are domain=electronic in this bake-off's dev corpus.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Dict, List

import pretty_midi

from eval.baseline_scoreboard_p3 import CLASSES, GM_TO_CLASS

from eval.paths import DATA_ROOT

EGMD_ROOT = DATA_ROOT / "egmd"


def load_manifest(root: Path = EGMD_ROOT) -> Dict[str, object]:
    manifest_path = root / "manifest.json"
    if not manifest_path.exists():
        return {"rows": [], "license": None}
    return json.loads(manifest_path.read_text())


def available_rows(root: Path = EGMD_ROOT, split: str = "dev") -> List[Dict[str, object]]:
    """Rows from the fetched manifest matching `split` ("dev" or "heldout"),
    with resolved absolute paths -- HELDOUT rows are only ever consumed by
    the orchestrator's own acceptance script, never by a dev-only sweep."""
    manifest = load_manifest(root)
    out = []
    for row in manifest.get("rows", []):
        if row["our_split"] != split:
            continue
        out.append({
            **row,
            "domain": "other",
            "audio_path": str(root / row["audio_path"]),
            "midi_path": str(root / row["midi_path"]),
        })
    return out


def load_drum_truth(midi_path: Path) -> Dict[str, List[float]]:
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
    dev = available_rows(split="dev")
    heldout = available_rows(split="heldout")
    print(f"[egmd_drum_truth] {len(dev)} dev rows, {len(heldout)} heldout rows")
    for row in dev[:5]:
        truth = load_drum_truth(Path(row["midi_path"]))
        counts = {c: len(v) for c, v in truth.items()}
        print(f"  {row['id']} ({row['drummer']}, {row['kit_name']}, {row['bpm']}bpm): {counts}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
