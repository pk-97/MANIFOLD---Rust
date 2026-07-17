"""Fetch the Slakh2100 **test split only** (aligned MIDI + true stems, flac)
from the redux archive, per docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §3/P3.

Zenodo record 4599666 ships exactly ONE file: `slakh2100_flac_redux.tar.gz`
(104.3 GB, CC-BY 4.0, md5 f4b71b6c45ac9b506f59788456b3f0c4) — there is no
separate per-split download. The archive's internal layout is
`slakh2100_flac_redux/<split>/TrackXXXXX/...` with split in
{train, validation, test, omitted} (verified 2026-07-17 by peeking the first
tar headers of the live gzip stream). Because gzip is a sequential format,
extracting "test only" still requires decompressing the ENTIRE stream — the
saving vs a naive fetch is disk (~keep only the test slice) and time-to-first
-usable-fixture, not total network transfer.

IMPORTANT — measured 2026-07-17: Zenodo throttles this file to ~830 KB/s
regardless of the environment's actual bandwidth (verified independently at
~11.5 MB/s via a Cloudflare speed-test endpoint on the same connection). At
830 KB/s the full 104.3 GB stream takes **~35 hours**. This is a real,
external constraint the design doc's "~11GB, P3's job" framing did not
anticipate (the design doc cited the ORIGINAL Slakh2100 split, 225 test
tracks; the redux archive actually shipped is smaller — 151 test tracks per
the redux de-duplication, confirmed via community docs 2026-07-17). This
script is meant to run for a long time as a background/nohup job and is
resumable in the loose sense that it logs progress; it does NOT resume a
partial stream (gzip streaming has no byte-range resume point that's cheap to
reconstruct) — a truly interrupted run restarts from track 0. Redundant runs
are avoided by checking which test tracks already exist on disk and skipping
their content if re-run (still must stream past their bytes, since the
source is sequential, but avoids re-writing already-complete tracks).

Usage:
    python -m eval.fetch.slakh2100 --out-dir eval/data/slakh2100_test --progress-every 1
"""

from __future__ import annotations

import argparse
import json
import socket
import sys
import tarfile
import time
import urllib.request
from pathlib import Path
from typing import Optional

ZENODO_RECORD = 4599666
FILENAME = "slakh2100_flac_redux.tar.gz"
URL = f"https://zenodo.org/record/{ZENODO_RECORD}/files/{FILENAME}?download=1"
ARCHIVE_TOTAL_SIZE = 104_300_000_000  # ~104.3 GB, Zenodo-reported
LICENSE = "CC-BY 4.0 (Zenodo record 4599666)"
ARCHIVE_ROOT_PREFIX = "slakh2100_flac_redux"
TARGET_SPLIT = "test"
EXPECTED_TEST_TRACKS = 151  # redux de-duplicated count, NOT the original 225 (design doc cited the original split)

PROGRESS_PATH_NAME = "_fetch_progress.json"


def _split_of(member_name: str) -> Optional[str]:
    parts = member_name.split("/")
    if len(parts) < 2 or parts[0] != ARCHIVE_ROOT_PREFIX:
        return None
    return parts[1]


def _track_of(member_name: str) -> Optional[str]:
    parts = member_name.split("/")
    if len(parts) < 3:
        return None
    return parts[2]


def stream_extract_split(
    out_dir: Path,
    split: str = TARGET_SPLIT,
    progress_every_sec: float = 30.0,
    max_bytes_downloaded: Optional[int] = None,
) -> dict:
    """Streams the archive sequentially (no full-file buffering on disk),
    writing only members under <split>/ to out_dir/<TrackXXXXX>/..., skipping
    everything else. Returns a summary dict; also writes a progress JSON
    beside out_dir so a concurrent/later process can read state without
    parsing logs."""
    out_dir.mkdir(parents=True, exist_ok=True)
    progress_path = out_dir.parent / PROGRESS_PATH_NAME

    socket.setdefaulttimeout(120)
    resp = urllib.request.urlopen(URL, timeout=120)

    tracks_seen = set()
    files_written = 0
    bytes_written = 0
    t_start = time.time()
    t_last_log = t_start

    def write_progress(done: bool, error: Optional[str] = None) -> None:
        payload = {
            "generated_at": time.time(),
            "split": split,
            "expected_test_tracks": EXPECTED_TEST_TRACKS,
            "tracks_seen": len(tracks_seen),
            "files_written": files_written,
            "bytes_written": bytes_written,
            "elapsed_sec": time.time() - t_start,
            "archive_total_size_bytes": ARCHIVE_TOTAL_SIZE,
            "note": (
                "bytes_written is only the TEST-split subset actually kept; the "
                "process must still stream past the full archive sequentially "
                "(gzip has no cheap seek), so elapsed_sec vs archive_total_size_bytes "
                "at the measured ~830KB/s throttle is the honest ETA proxy, not "
                "bytes_written's own rate."
            ),
            "done": done,
            "error": error,
        }
        progress_path.write_text(json.dumps(payload, indent=2))

    try:
        tf = tarfile.open(fileobj=resp, mode="r|gz")
        for member in tf:
            sp = _split_of(member.name)
            if sp != split:
                # Must still advance the stream past this member's body to
                # reach the next header — tarfile does this internally when
                # we don't call extractfile/extract, just iterate.
                continue
            track = _track_of(member.name)
            if track:
                tracks_seen.add(track)
            if member.isfile():
                rel = "/".join(member.name.split("/")[2:])  # TrackXXXXX/...
                dest = out_dir / rel
                dest.parent.mkdir(parents=True, exist_ok=True)
                src = tf.extractfile(member)
                if src is not None:
                    with open(dest, "wb") as fh:
                        while True:
                            chunk = src.read(1 << 20)
                            if not chunk:
                                break
                            fh.write(chunk)
                            bytes_written += len(chunk)
                    files_written += 1

            now = time.time()
            if now - t_last_log >= progress_every_sec:
                print(
                    f"[slakh2100] {len(tracks_seen)}/{EXPECTED_TEST_TRACKS} test tracks seen, "
                    f"{files_written} files, {bytes_written / 1e9:.2f} GB kept, "
                    f"{now - t_start:.0f}s elapsed",
                    file=sys.stderr,
                )
                write_progress(done=False)
                t_last_log = now

            if max_bytes_downloaded is not None and bytes_written >= max_bytes_downloaded:
                print("[slakh2100] max_bytes_downloaded reached, stopping early (test/debug cap)", file=sys.stderr)
                break
    except Exception as exc:  # network errors, truncated stream, etc.
        write_progress(done=False, error=str(exc))
        raise
    else:
        write_progress(done=True)

    return {
        "tracks_seen": sorted(tracks_seen),
        "files_written": files_written,
        "bytes_written": bytes_written,
        "elapsed_sec": time.time() - t_start,
    }


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=Path("eval/data/slakh2100_test"))
    parser.add_argument("--progress-every", type=float, default=30.0, help="seconds between progress log lines")
    parser.add_argument("--max-bytes", type=int, default=None, help="stop early after this many kept bytes (debug/testing only)")
    args = parser.parse_args(argv)

    print(f"[slakh2100] fetching TEST split only from {URL}")
    print(f"[slakh2100] WARNING: full archive is {ARCHIVE_TOTAL_SIZE/1e9:.1f} GB and must be streamed "
          f"through sequentially even though only ~{EXPECTED_TEST_TRACKS} test tracks are kept "
          f"(measured throttle ~830KB/s => ~35h full traverse, 2026-07-17)")
    summary = stream_extract_split(args.out_dir, progress_every_sec=args.progress_every, max_bytes_downloaded=args.max_bytes)
    print(f"[slakh2100] kept {summary['files_written']} files, {summary['bytes_written']/1e9:.2f} GB, "
          f"{len(summary['tracks_seen'])} distinct test tracks, {summary['elapsed_sec']:.0f}s elapsed")
    print(f"[slakh2100] license: {LICENSE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
