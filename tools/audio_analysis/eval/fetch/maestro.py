"""Fetch a ~20-track MAESTRO v3 selection, per
docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §3/P3 ("~20 individually addressable
performances (real piano + aligned MIDI)").

DEVIATION FROM THE BRIEF'S LITERAL WORDING (flagged per P3's own "if the doc
contradicts reality, stop and report" clause) — verified 2026-07-17:
MAESTRO v3's real recorded piano audio is NOT individually addressable. The
GCS bucket (storage.googleapis.com/magentadata/datasets/maestro/v3.0.0/) was
listed directly and contains exactly:
  - maestro-v3.0.0.zip            108.4 GB  (full audio+MIDI, one blob)
  - maestro-v3.0.0-midi.zip         58 MB   (MIDI only)
  - maestro-v3.0.0.csv/.json               (metadata)
  - *_ns_wav_{train,test}.tfrecord-NNNNN-of-00025  (sharded NoteSequence
    protobufs with embedded audio; smallest shard is still ~97MB and holds
    MULTIPLE performances bundled together, and parsing requires either
    `tensorflow` or `note_seq` — NEITHER is installed in the bundled
    runtime, and installing either is a real new-dependency decision this
    phase's brief did not authorize (P3 is measurement-only)).
There is no per-performance audio URL. Given this, the pragmatic in-scope
choice: fetch the tiny MIDI-only zip (real MAESTRO MIDI, genuine ground
truth) and render audio ourselves via eval/midi_synth.py (pretty_midi's pure
-numpy additive synth — no new dependency, no fluidsynth binary needed).
This trades "real recorded piano timbre" for "exact reproducible ground
truth we can regenerate any time" — acceptable for P4's stated use (tuning
basic_pitch's post-processing against sustained polyphony), NOT acceptable
if a future phase needs to test basic_pitch against REAL piano recording
artifacts (mic bleed, room tone, pedal noise) — that would need the full
108GB zip or the tfrecord+note_seq route, flagged here for whoever picks
that up.

License: MAESTRO v3 MIDI is CC BY-NC-SA 4.0 — eval-only per D8 (never
redistributed, never trains shipped weights).

Selection: ~20 tracks from the 'test' split (177 available, already a
disjoint held-back split by MAESTRO's own curators), picked for diversity
across composer and duration (a deterministic, sorted-then-strided pick —
not random, so re-running this script is reproducible).

Usage:
    python -m eval.fetch.maestro --out-dir eval/data/maestro_v3 --n-tracks 20
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import sys
import zipfile
from pathlib import Path
from typing import Dict, List

MIDI_ZIP_URL = "https://storage.googleapis.com/magentadata/datasets/maestro/v3.0.0/maestro-v3.0.0-midi.zip"
METADATA_CSV_URL = "https://storage.googleapis.com/magentadata/datasets/maestro/v3.0.0/maestro-v3.0.0.csv"
EXPECTED_MIDI_ZIP_SIZE = 58416533
LICENSE = "CC BY-NC-SA 4.0 (MAESTRO v3, magenta.withgoogle.com/datasets/maestro) — eval-only per D8"


def download(url: str, dest: Path) -> None:
    import urllib.request

    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists() and dest.stat().st_size > 0:
        print(f"[maestro] already downloaded: {dest}")
        return
    print(f"[maestro] downloading {url} -> {dest}")
    tmp = dest.with_suffix(dest.suffix + ".part")
    urllib.request.urlretrieve(url, tmp)
    tmp.rename(dest)


def load_metadata(csv_path: Path) -> List[Dict[str, str]]:
    with open(csv_path, newline="", encoding="utf-8") as f:
        return list(csv.DictReader(f))


def select_diverse(rows: List[Dict[str, str]], split: str, n: int) -> List[Dict[str, str]]:
    """Deterministic diversity pick: sort candidates by (composer, duration),
    then take an even stride across the sorted list so both composer and
    duration range are spread rather than clustered."""
    candidates = [r for r in rows if r["split"] == split]
    candidates.sort(key=lambda r: (r["canonical_composer"], float(r["duration"])))
    if len(candidates) <= n:
        return candidates
    stride = len(candidates) / n
    picked = []
    for i in range(n):
        idx = int(round(i * stride))
        idx = min(idx, len(candidates) - 1)
        picked.append(candidates[idx])
    # De-dup (stride rounding can collide on small n) while preserving order.
    seen = set()
    out = []
    for r in picked:
        key = r["midi_filename"]
        if key not in seen:
            seen.add(key)
            out.append(r)
    return out


def extract_selected(zip_path: Path, out_dir: Path, selected: List[Dict[str, str]]) -> List[Path]:
    out_dir.mkdir(parents=True, exist_ok=True)
    written: List[Path] = []
    with zipfile.ZipFile(zip_path) as zf:
        names = set(zf.namelist())
        for row in selected:
            midi_name = row["midi_filename"]
            # midi zip stores paths as "<year>/<file>.midi" possibly with a
            # top-level "maestro-v3.0.0-midi/" prefix depending on packaging.
            candidates = [midi_name, f"maestro-v3.0.0-midi/{midi_name}"]
            match = next((c for c in candidates if c in names), None)
            if match is None:
                # fallback: suffix match
                match = next((n for n in names if n.endswith(midi_name)), None)
            if match is None:
                print(f"[maestro] WARNING: {midi_name} not found in midi zip", file=sys.stderr)
                continue
            dest = out_dir / Path(midi_name).name
            with zf.open(match) as src, open(dest, "wb") as dst:
                dst.write(src.read())
            written.append(dest)
    return written


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=Path("eval/data/maestro_v3"))
    parser.add_argument("--n-tracks", type=int, default=20)
    parser.add_argument("--split", type=str, default="test")
    args = parser.parse_args(argv)

    zip_path = args.out_dir / "maestro-v3.0.0-midi.zip"
    csv_path = args.out_dir / "maestro-v3.0.0.csv"
    download(MIDI_ZIP_URL, zip_path)
    download(METADATA_CSV_URL, csv_path)

    rows = load_metadata(csv_path)
    selected = select_diverse(rows, args.split, args.n_tracks)
    midi_dir = args.out_dir / "midi"
    written = extract_selected(zip_path, midi_dir, selected)

    manifest = [
        {
            "composer": r["canonical_composer"],
            "title": r["canonical_title"],
            "year": r["year"],
            "duration_sec": float(r["duration"]),
            "midi_filename": r["midi_filename"],
        }
        for r in selected
    ]
    (args.out_dir / "selection_manifest.json").write_text(json.dumps(manifest, indent=2))

    print(f"[maestro] selected {len(selected)} tracks from split={args.split!r}, extracted {len(written)} MIDI files")
    print(f"[maestro] license: {LICENSE}")
    print("[maestro] NOTE: real recorded audio not fetched (not individually addressable — see module docstring); "
          "render with eval.midi_synth.synthesize_midi_file before scoring")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
