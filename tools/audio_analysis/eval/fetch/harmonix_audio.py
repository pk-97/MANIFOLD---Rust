"""Harmonix Set — per-track AUDIO matching (P3), incremental + best-effort.

`eval/fetch/harmonix.py` (P1) pulled only the annotation subtree (beats,
downbeats, segments, metadata.csv, youtube_urls.csv). Harmonix does not
redistribute audio; per docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §3, matching
real audio to annotated tracks is P3's job, done incrementally and
best-effort, logging unmatched tracks rather than failing the run. Per the
design doc's domain directive, the ELECTRONIC slice (Genre == "Dance/Electronic"
in metadata.csv, 129/912 tracks) is prioritized first.

Mechanism: youtube_urls.csv gives a YouTube URL per track id; this script uses
yt-dlp (⚠ NEW eval-only dependency, pip-installed 2026-07-17 — never added to
requirements.runtime.mac.txt, the app's shipped runtime; same "eval-tool
exemption, not a fallback" precedent as madmom post-P6) to fetch best-effort
audio-only downloads, trims/normalizes via ffmpeg, and writes
eval/data/harmonixset_audio/<track_id>.wav. Every failure (unavailable,
region-blocked, taken down, network) is logged to
eval/data/harmonixset_audio/_unmatched.json — the run continues past
individual failures by construction.

License note: Harmonix's own annotation LICENSE (CC0-style) covers the beat/
segment labels only; matched audio inherits whatever license the original
YouTube upload carries (typically all-rights-reserved commercial music) —
eval-only per D8 (never redistributed, never shipped, never trains shipped
weights). This script writes a per-track note recording that stance beside
the fetched audio.

Usage:
    python -m eval.fetch.harmonix_audio --genre "Dance/Electronic" --limit 129 \
        --out-dir eval/data/harmonixset_audio --annotations-dir eval/data/harmonixset/dataset
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
import time
from pathlib import Path
from typing import Dict, List, Optional

try:
    import yt_dlp
except ImportError:
    yt_dlp = None  # checked explicitly in main() with a clear error


def load_metadata(annotations_dir: Path) -> Dict[str, Dict[str, str]]:
    out: Dict[str, Dict[str, str]] = {}
    with open(annotations_dir / "metadata.csv", newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            out[row["File"]] = row
    return out


def load_urls(annotations_dir: Path) -> Dict[str, str]:
    out: Dict[str, str] = {}
    with open(annotations_dir / "youtube_urls.csv", newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            out[row["File"]] = row["URL"]
    return out


def select_tracks(
    metadata: Dict[str, Dict[str, str]],
    urls: Dict[str, str],
    genre: Optional[str],
    limit: Optional[int],
) -> List[str]:
    ids = sorted(metadata.keys())
    if genre is not None:
        ids = [i for i in ids if metadata[i].get("Genre") == genre]
    ids = [i for i in ids if i in urls]
    if limit is not None:
        ids = ids[:limit]
    return ids


def fetch_one(track_id: str, url: str, out_dir: Path, ffmpeg_bin: Optional[str]) -> Optional[str]:
    """Returns None on success, an error string on failure. Writes
    out_dir/<track_id>.wav (mono 44.1kHz, via yt-dlp's ffmpeg postprocessor)."""
    dest_wav = out_dir / f"{track_id}.wav"
    if dest_wav.exists() and dest_wav.stat().st_size > 0:
        return None  # already matched — incremental re-run skips it

    ydl_opts = {
        "format": "bestaudio/best",
        "outtmpl": str(out_dir / f"{track_id}.%(ext)s"),
        "postprocessors": [
            {
                "key": "FFmpegExtractAudio",
                "preferredcodec": "wav",
                "preferredquality": "192",
            }
        ],
        "quiet": True,
        "no_warnings": True,
        "noprogress": True,
        "socket_timeout": 30,
        "retries": 2,
    }
    if ffmpeg_bin:
        ydl_opts["ffmpeg_location"] = ffmpeg_bin

    try:
        with yt_dlp.YoutubeDL(ydl_opts) as ydl:
            ydl.download([url])
    except Exception as exc:  # noqa: BLE001 — best-effort per-track, must not abort the run
        return str(exc)[:400]

    if not dest_wav.exists():
        return "post-processing did not produce expected .wav"
    return None


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--annotations-dir", type=Path, default=Path("eval/data/harmonixset/dataset"))
    parser.add_argument("--out-dir", type=Path, default=Path("eval/data/harmonixset_audio"))
    parser.add_argument("--genre", type=str, default="Dance/Electronic", help="metadata.csv Genre filter, or empty string for all")
    parser.add_argument("--limit", type=int, default=None)
    parser.add_argument("--ffmpeg-bin", type=str, default=None)
    parser.add_argument("--sleep-between", type=float, default=1.0, help="seconds between downloads, politeness")
    args = parser.parse_args(argv)

    if yt_dlp is None:
        print("[harmonix_audio] yt-dlp not installed — this is an eval-only fetch dependency, "
              "install with the bundled python's pip (never add to requirements.runtime.mac.txt)", file=sys.stderr)
        return 1

    args.out_dir.mkdir(parents=True, exist_ok=True)
    metadata = load_metadata(args.annotations_dir)
    urls = load_urls(args.annotations_dir)
    genre = args.genre if args.genre else None
    track_ids = select_tracks(metadata, urls, genre, args.limit)

    print(f"[harmonix_audio] {len(track_ids)} candidate tracks (genre={genre!r})", file=sys.stderr)

    unmatched: Dict[str, str] = {}
    matched: List[str] = []
    unmatched_path = args.out_dir / "_unmatched.json"
    if unmatched_path.exists():
        try:
            unmatched = json.loads(unmatched_path.read_text())
        except Exception:
            unmatched = {}

    for i, track_id in enumerate(track_ids):
        dest_wav = args.out_dir / f"{track_id}.wav"
        if dest_wav.exists() and dest_wav.stat().st_size > 0:
            matched.append(track_id)
            continue
        print(f"[harmonix_audio] ({i+1}/{len(track_ids)}) {track_id} ...", file=sys.stderr)
        err = fetch_one(track_id, urls[track_id], args.out_dir, args.ffmpeg_bin)
        if err is None:
            matched.append(track_id)
            unmatched.pop(track_id, None)
        else:
            unmatched[track_id] = err
            print(f"[harmonix_audio]   FAILED: {err}", file=sys.stderr)
        unmatched_path.write_text(json.dumps(unmatched, indent=2))
        # Progress snapshot after every track — a concurrent reader (or this
        # session's own report step) never has to wait for the whole run.
        progress = {
            "generated_at": time.time(),
            "genre": genre,
            "n_candidates": len(track_ids),
            "n_matched": len(matched),
            "n_unmatched": len(unmatched),
            "match_rate": len(matched) / len(track_ids) if track_ids else None,
        }
        (args.out_dir / "_progress.json").write_text(json.dumps(progress, indent=2))
        time.sleep(args.sleep_between)

    print(f"[harmonix_audio] done: {len(matched)}/{len(track_ids)} matched, {len(unmatched)} unmatched")
    print(f"[harmonix_audio] license note: matched audio is eval-only (D8) — never redistributed/shipped")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
