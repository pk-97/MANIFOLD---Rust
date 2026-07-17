"""Fetch a REPRESENTATIVE SUBSET of the Expanded Groove MIDI Dataset (E-GMD),
per the 2026-07-18 ADTOF bake-off addendum (docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md,
Deferred #1 §7.1, phase B1): "E-GMD is ~110GB full — fetch a REPRESENTATIVE
subset only (<=8GB: sample across drummers/kits/tempi; document the sampling
rule), license VERIFY-AT-FETCH."

License (VERIFIED AT FETCH, 2026-07-18): the archive's own bundled
`e-gmd-v1.0.0/LICENSE` file (read via HTTP range request from inside the zip
itself, NOT just the magenta.tensorflow.org marketing page) reads verbatim:
"This work is licensed under the Creative Commons Attribution 4.0
International License." -- CC BY 4.0, permissive. Matches the design doc's
"expected CC-BY 4.0" guess, confirmed against the primary source.

Why a selective fetch is possible at all
------------------------------------------
Unlike Slakh2100's tar.gz (sequential-only, eval/fetch/slakh2100.py), E-GMD
ships as a genuine ZIP (`e-gmd-v1.0.0.zip`, 96.4GB, DEFLATE-compressed
entries). ZIP's central directory sits at the END of the file and lists every
entry's name/size/local-header-offset, so `zipfile.ZipFile` can open a
*seekable* file-like object that answers reads via small HTTP Range requests
and lazily discover exactly which byte ranges hold the entries we actually
want -- no need to stream the full 96.4GB. `HTTPRangeFile` below implements
that file-like object (a small, self-contained version of the "remote zip"
pattern; no new dependency, just `requests` + `io.RawIOBase` + stdlib
`zipfile`). Network cost per selected entry is its `compress_size` (DEFLATE),
not the raw PCM size.

Sampling rule (documented per the brief's "document the sampling rule")
-------------------------------------------------------------------------
E-GMD's metadata CSV (45,537 rows, one per (performance, kit) rendering) is
downloaded whole first (~4MB, cheap, needed to plan the sample). Then:

  1. Compute GLOBAL bpm tertile edges from the full CSV (33rd/66th
     percentile) -> 3 tempo buckets (slow/mid/fast), same bucket edges used
     for every drummer so a "fast" clip means the same thing across drummers.
  2. Group rows by (drummer, tempo_bucket). Within each cell, greedily pick
     up to MAX_PER_CELL performances (`id` column, i.e. one specific
     performance+kit rendering), preferring:
       a. a `kit_name` not yet chosen for THIS drummer (maximizes kit
          coverage per drummer before allowing repeats),
       b. duration inside [MIN_DURATION_SEC, MAX_DURATION_SEC] (drops both
          the near-zero fragments and the multi-minute full-song outliers
          the CSV's own duration column shows up to 611s -- keeps the fetch
          small and each clip a meaningfully-sized onset-detection unit).
     If a cell has fewer eligible rows than MAX_PER_CELL, take what exists
     (no padding/repeats forced).
  3. dev/heldout split reuses E-GMD's OWN `split` column (train/validation ->
     our dev; test -> our heldout) rather than inventing a new one -- this
     preserves whatever drummer/session-disjointness the dataset's own
     authors built into that column (same D9 "heldout untouchable" spirit,
     inherited rather than re-derived).
  4. Budget guard: before downloading, sum `compress_size` (from the ZIP
     central directory, i.e. actual network bytes) over the full selection;
     if it exceeds MAX_TOTAL_BYTES (8GB), trim the lowest-priority rows
     (last-picked per cell) until under budget. At MAX_PER_CELL=6 this sample
     comes in far under the cap by construction (verified empirically before
     landing this script: ~9 drummers x 3 buckets x 6 x ~2 short files
     ~= a few hundred MB) -- the cap is a safety rail, not a target to fill.

Usage:
    python -m eval.fetch.egmd --out-dir eval/data/egmd
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import io
import json
import random
import sys
from collections import defaultdict
from pathlib import Path

from eval.paths import DATA_ROOT
from typing import Any, Dict, List, Optional, Tuple

import requests

ZIP_URL = "https://storage.googleapis.com/magentadata/datasets/e-gmd/v1.0.0/e-gmd-v1.0.0.zip"
CSV_URL = "https://storage.googleapis.com/magentadata/datasets/e-gmd/v1.0.0/e-gmd-v1.0.0.csv"
ZIP_ROOT_PREFIX = "e-gmd-v1.0.0"
LICENSE_ENTRY = f"{ZIP_ROOT_PREFIX}/LICENSE"
EXPECTED_LICENSE_SUBSTRING = "Creative Commons Attribution 4.0 International"

MAX_TOTAL_BYTES = 8 * 1_000_000_000  # 8GB cap, brief's ceiling (not a target)
MAX_PER_CELL = 6  # performances per (drummer, tempo_bucket)
MIN_DURATION_SEC = 3.0
MAX_DURATION_SEC = 20.0
SAMPLE_SEED = 20260718  # deterministic shuffle -- E-GMD re-records the SAME
# performance `id` through many (up to 43) different kits, so sorting
# candidates by kit_name alphabetically (first draft of this script) always
# picked the same handful of alphabetically-early kits across every
# drummer/tempo cell (measured: 6 distinct kits used out of 43 available).
# A fixed-seed shuffle before the greedy round-robin fixes this while
# staying reproducible.

PROGRESS_EVERY = 20


class HTTPRangeFile(io.RawIOBase):
    """Minimal seekable file-like object over HTTP Range requests, just
    enough for zipfile.ZipFile to lazily read the central directory + any
    entries we .read() by name. No dependency beyond `requests`."""

    def __init__(self, url: str, session: Optional[requests.Session] = None):
        self.url = url
        self.pos = 0
        self._session = session or requests.Session()
        head = self._session.head(url, allow_redirects=True, timeout=30)
        head.raise_for_status()
        self.size = int(head.headers["Content-Length"])

    def readable(self) -> bool:
        return True

    def seekable(self) -> bool:
        return True

    def seek(self, offset: int, whence: int = io.SEEK_SET) -> int:
        if whence == io.SEEK_SET:
            self.pos = offset
        elif whence == io.SEEK_CUR:
            self.pos += offset
        elif whence == io.SEEK_END:
            self.pos = self.size + offset
        else:
            raise ValueError(f"bad whence {whence}")
        return self.pos

    def tell(self) -> int:
        return self.pos

    def readinto(self, b) -> int:
        n = len(b)
        if n == 0:
            return 0
        end = min(self.pos + n, self.size) - 1
        if end < self.pos:
            return 0
        r = self._session.get(self.url, headers={"Range": f"bytes={self.pos}-{end}"}, timeout=60)
        r.raise_for_status()
        data = r.content
        b[: len(data)] = data
        self.pos += len(data)
        return len(data)


def fetch_metadata_csv(out_dir: Path, csv_url: str = CSV_URL) -> Path:
    out_path = out_dir / "e-gmd-v1.0.0.csv"
    if out_path.exists() and out_path.stat().st_size > 0:
        return out_path
    out_dir.mkdir(parents=True, exist_ok=True)
    r = requests.get(csv_url, timeout=120)
    r.raise_for_status()
    out_path.write_bytes(r.content)
    return out_path


def _tempo_buckets(rows: List[Dict[str, Any]]) -> Tuple[float, float]:
    bpms = sorted(float(r["bpm"]) for r in rows)
    n = len(bpms)
    lo = bpms[int(n * 0.33)]
    hi = bpms[int(n * 0.66)]
    return lo, hi


def _bucket_of(bpm: float, lo: float, hi: float) -> str:
    if bpm < lo:
        return "slow"
    if bpm > hi:
        return "fast"
    return "mid"


def select_sample(rows: List[Dict[str, Any]], max_per_cell: int = MAX_PER_CELL) -> List[Dict[str, Any]]:
    """Stratified (drummer x tempo_bucket) selection -- see module docstring.
    One row per selected `id` (one specific kit rendering of that
    performance); kit diversity preferred within a cell before repeats."""
    lo, hi = _tempo_buckets(rows)
    by_cell: Dict[Tuple[str, str], List[Dict[str, Any]]] = defaultdict(list)
    for r in rows:
        dur = float(r["duration"])
        if not (MIN_DURATION_SEC <= dur <= MAX_DURATION_SEC):
            continue
        bucket = _bucket_of(float(r["bpm"]), lo, hi)
        by_cell[(r["drummer"], bucket)].append(r)

    selected: List[Dict[str, Any]] = []
    for cell_idx, ((drummer, bucket), cell_rows) in enumerate(sorted(by_cell.items())):
        seen_ids: set = set()
        used_kits: set = set()
        # Deterministic per-cell shuffle (not a kit_name sort -- E-GMD
        # re-records the SAME performance id through up to 43 different
        # kits, so sorting by kit_name would pick the same alphabetically-
        # early kits in every cell; see SAMPLE_SEED comment above).
        cell_rows_shuffled = list(cell_rows)
        random.Random(SAMPLE_SEED + cell_idx).shuffle(cell_rows_shuffled)
        chosen: List[Dict[str, Any]] = []
        # First pass: prefer an unseen kit_name for this drummer/bucket cell.
        for r in cell_rows_shuffled:
            if len(chosen) >= max_per_cell:
                break
            if r["id"] in seen_ids:
                continue
            if r["kit_name"] in used_kits:
                continue
            seen_ids.add(r["id"])
            used_kits.add(r["kit_name"])
            chosen.append(r)
        # Second pass: fill remaining slots allowing kit repeats.
        if len(chosen) < max_per_cell:
            for r in cell_rows_shuffled:
                if len(chosen) >= max_per_cell:
                    break
                if r["id"] in seen_ids:
                    continue
                seen_ids.add(r["id"])
                chosen.append(r)
        for r in chosen:
            row = dict(r)
            row["tempo_bucket"] = bucket
            selected.append(row)
    return selected


def verify_license(zf, session_url: str = ZIP_URL) -> Dict[str, Any]:
    text = zf.read(LICENSE_ENTRY).decode("utf-8", errors="replace")
    permissive = EXPECTED_LICENSE_SUBSTRING in text
    return {"license_text": text.strip(), "permissive": permissive, "expected_substring": EXPECTED_LICENSE_SUBSTRING}


def fetch_selected(
    selected: List[Dict[str, Any]],
    out_dir: Path,
    max_total_bytes: int = MAX_TOTAL_BYTES,
) -> Dict[str, Any]:
    import zipfile

    out_dir.mkdir(parents=True, exist_ok=True)
    f = HTTPRangeFile(ZIP_URL)
    zf = zipfile.ZipFile(f)
    print(f"[egmd] zip opened, {len(zf.namelist())} entries in central directory", file=sys.stderr)

    license_info = verify_license(zf)
    print(f"[egmd] LICENSE (verified at fetch, read from inside the archive): {license_info['license_text']!r}", file=sys.stderr)
    if not license_info["permissive"]:
        print("[egmd] STOP: license is NOT the expected permissive CC-BY text -- do not proceed with E-GMD.", file=sys.stderr)
        return {"license": license_info, "stopped": True, "rows": []}

    name_to_info = {info.filename: info for info in zf.infolist()}

    # Budget: sum compress_size (actual network bytes) over the selection,
    # trim lowest-priority (last-picked-per-cell) rows if over cap.
    def _entry_names(row: Dict[str, Any]) -> Tuple[str, str]:
        midi_name = f"{ZIP_ROOT_PREFIX}/{row['midi_filename']}"
        audio_name = f"{ZIP_ROOT_PREFIX}/{row['audio_filename']}"
        return midi_name, audio_name

    priced: List[Tuple[Dict[str, Any], int]] = []
    for row in selected:
        midi_name, audio_name = _entry_names(row)
        midi_info = name_to_info.get(midi_name)
        audio_info = name_to_info.get(audio_name)
        if midi_info is None or audio_info is None:
            print(f"[egmd] WARNING: entry missing in zip, skipping row id={row['id']}", file=sys.stderr)
            continue
        priced.append((row, midi_info.compress_size + audio_info.compress_size))

    total = sum(sz for _, sz in priced)
    kept = priced
    if total > max_total_bytes:
        print(f"[egmd] selection {total/1e9:.2f}GB exceeds cap {max_total_bytes/1e9:.2f}GB, trimming", file=sys.stderr)
        kept = []
        running = 0
        for row, sz in priced:
            if running + sz > max_total_bytes:
                continue
            kept.append((row, sz))
            running += sz
        total = running

    print(f"[egmd] downloading {len(kept)} performances, ~{total/1e6:.1f}MB compressed", file=sys.stderr)

    written_rows: List[Dict[str, Any]] = []
    bytes_written = 0
    for i, (row, _sz) in enumerate(kept):
        midi_name, audio_name = _entry_names(row)
        rel_midi = row["midi_filename"]
        rel_audio = row["audio_filename"]
        dest_midi = out_dir / rel_midi
        dest_audio = out_dir / rel_audio
        dest_midi.parent.mkdir(parents=True, exist_ok=True)
        dest_audio.parent.mkdir(parents=True, exist_ok=True)
        dest_midi.write_bytes(zf.read(midi_name))
        audio_bytes = zf.read(audio_name)
        dest_audio.write_bytes(audio_bytes)
        bytes_written += len(audio_bytes)

        our_split = "heldout" if row["split"] == "test" else "dev"
        written_rows.append({
            "id": row["id"],
            "drummer": row["drummer"],
            "kit_name": row["kit_name"],
            "style": row["style"],
            "bpm": float(row["bpm"]),
            "tempo_bucket": row["tempo_bucket"],
            "duration_sec": float(row["duration"]),
            "egmd_split": row["split"],
            "our_split": our_split,
            "midi_path": rel_midi,
            "audio_path": rel_audio,
        })
        if (i + 1) % PROGRESS_EVERY == 0:
            print(f"[egmd] {i + 1}/{len(kept)} fetched, {bytes_written/1e6:.1f}MB audio written so far", file=sys.stderr)

    return {
        "license": license_info,
        "stopped": False,
        "rows": written_rows,
        "bytes_written_audio": bytes_written,
        "n_dev": sum(1 for r in written_rows if r["our_split"] == "dev"),
        "n_heldout": sum(1 for r in written_rows if r["our_split"] == "heldout"),
    }


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out-dir", type=Path, default=DATA_ROOT / "egmd")
    parser.add_argument("--max-per-cell", type=int, default=MAX_PER_CELL)
    parser.add_argument("--max-total-bytes", type=int, default=MAX_TOTAL_BYTES)
    args = parser.parse_args(argv)

    args.out_dir.mkdir(parents=True, exist_ok=True)
    csv_path = fetch_metadata_csv(args.out_dir)
    print(f"[egmd] metadata CSV -> {csv_path}", file=sys.stderr)
    rows = list(csv.DictReader(csv_path.open()))
    print(f"[egmd] {len(rows)} total (performance, kit) rows in metadata", file=sys.stderr)

    selected = select_sample(rows, max_per_cell=args.max_per_cell)
    print(f"[egmd] stratified sample: {len(selected)} performances selected before budget trim", file=sys.stderr)

    result = fetch_selected(selected, args.out_dir, max_total_bytes=args.max_total_bytes)
    if result["stopped"]:
        manifest = {
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "stopped_on_license": True,
            "license": result["license"],
        }
        (args.out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2))
        print("[egmd] STOPPED: license not permissive, see manifest.json", file=sys.stderr)
        return 1

    manifest = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "source": "E-GMD v1.0.0 (Magenta), https://magenta.tensorflow.org/datasets/e-gmd",
        "license": result["license"],
        "sampling_rule": (
            f"stratified by (drummer x global-bpm-tertile), up to {args.max_per_cell} performances per cell, "
            f"kit_name diversity preferred before repeats within a cell, duration filtered to "
            f"[{MIN_DURATION_SEC}, {MAX_DURATION_SEC}]s, budget cap {args.max_total_bytes/1e9:.1f}GB "
            "(computed from ZIP compress_size, the real network cost of a selective range-fetch)."
        ),
        "dev_heldout_split_rule": "E-GMD's own `split` column: train/validation -> our dev, test -> our heldout (inherits the dataset's own disjointness, not re-derived).",
        "n_rows": len(result["rows"]),
        "n_dev": result["n_dev"],
        "n_heldout": result["n_heldout"],
        "bytes_written_audio": result["bytes_written_audio"],
        "rows": result["rows"],
    }
    (args.out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2))
    print(f"[egmd] wrote {len(result['rows'])} performances ({result['n_dev']} dev / {result['n_heldout']} heldout), "
          f"{result['bytes_written_audio']/1e6:.1f}MB audio -> {args.out_dir}", file=sys.stderr)
    print(f"[egmd] license: {result['license']['license_text']!r} (permissive={result['license']['permissive']})", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
