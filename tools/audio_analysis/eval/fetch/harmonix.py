"""Fetch Harmonix Set annotations (github.com/urinieto/harmonixset) — beats,
downbeats, and functional segment labels for 912 pop/EDM tracks.

Per docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §3: annotations are tiny (git);
audio is NOT redistributed by Harmonix and must be user-matched per track,
incrementally — that matching is P3's job (`youtube_alignment_scores.csv` /
`youtube_urls.csv`, already part of the annotation pull below, are the
matching aids). This script only pulls the annotation repo's `dataset/`
subtree.

License: annotations are open (CC0-style; LICENSE file inside the pull).

Usage:
    python -m eval.fetch.harmonix --out-dir eval/data/harmonixset
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

REPO_ZIP_URL = "https://github.com/urinieto/harmonixset/archive/refs/heads/master.zip"
KEEP_SUBPATHS = ("dataset/beats_and_downbeats", "dataset/segments", "dataset/metadata.csv", "dataset/youtube_urls.csv", "dataset/youtube_alignment_scores.csv", "LICENSE")


def fetch_and_extract(out_dir: Path) -> int:
    out_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        zip_path = Path(tmp) / "harmonixset.zip"
        print(f"[harmonix] downloading {REPO_ZIP_URL}")
        try:
            urllib.request.urlretrieve(REPO_ZIP_URL, zip_path)
        except Exception as exc:
            print(f"[harmonix] download failed: {exc}", file=sys.stderr)
            return 1

        with zipfile.ZipFile(zip_path) as zf:
            names = zf.namelist()
            if not names:
                print("[harmonix] empty archive", file=sys.stderr)
                return 1
            root_prefix = names[0].split("/")[0]  # "harmonixset-master"
            n_extracted = 0
            for name in names:
                rel = name[len(root_prefix) + 1 :]
                if not rel:
                    continue
                if any(rel == keep or rel.startswith(keep + "/") for keep in KEEP_SUBPATHS):
                    dest = out_dir / rel
                    if name.endswith("/"):
                        dest.mkdir(parents=True, exist_ok=True)
                        continue
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    with zf.open(name) as src, open(dest, "wb") as dst:
                        shutil.copyfileobj(src, dst)
                    n_extracted += 1
    print(f"[harmonix] extracted {n_extracted} annotation files -> {out_dir}")
    return 0 if n_extracted > 0 else 1


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=Path("eval/data/harmonixset"))
    args = parser.parse_args(argv)
    rc = fetch_and_extract(args.out_dir)
    if rc == 0:
        segments_dir = args.out_dir / "dataset" / "segments"
        n_tracks = len(list(segments_dir.glob("*.txt"))) if segments_dir.exists() else 0
        print(f"[harmonix] {n_tracks} tracks with segment annotations")
        print("[harmonix] audio NOT included — P3 does incremental audio matching against youtube_urls.csv")
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
