"""Fetch babyslakh_16k — the small (~880MB, 20-track) Slakh2100 bring-up
subset (aligned MIDI + true stems), per docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md
§3/P1. License CC-BY 4.0 (Zenodo record 4603870, verified 2026-07-17).

The full Slakh2100 test split (P3's job, 225 tracks / ~11GB) is a separate,
larger fetch — not this script.

Usage:
    python -m eval.fetch.babyslakh --out-dir eval/data/babyslakh_16k
"""

from __future__ import annotations

import argparse
import hashlib
import shutil
import sys
import tarfile
import urllib.request
from pathlib import Path

ZENODO_RECORD = 4603870
FILENAME = "babyslakh_16k.tar.gz"
URL = f"https://zenodo.org/record/{ZENODO_RECORD}/files/{FILENAME}?download=1"
EXPECTED_MD5 = "311096dc2bde7d61c97e930edbfc7f78"
EXPECTED_SIZE = 882818115
LICENSE = "CC-BY 4.0 (Zenodo record 4603870)"


def _md5sum(path: Path) -> str:
    h = hashlib.md5()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download(dest_tar: Path) -> None:
    dest_tar.parent.mkdir(parents=True, exist_ok=True)
    if dest_tar.exists() and dest_tar.stat().st_size == EXPECTED_SIZE:
        print(f"[babyslakh] already downloaded: {dest_tar}")
        return
    print(f"[babyslakh] downloading {URL} -> {dest_tar}")
    tmp = dest_tar.with_suffix(dest_tar.suffix + ".part")
    urllib.request.urlretrieve(URL, tmp)
    tmp.rename(dest_tar)


def verify(dest_tar: Path) -> bool:
    got = _md5sum(dest_tar)
    ok = got == EXPECTED_MD5
    print(f"[babyslakh] md5 {'OK' if ok else 'MISMATCH'}: expected={EXPECTED_MD5} got={got}")
    return ok


def extract(dest_tar: Path, out_dir: Path) -> None:
    print(f"[babyslakh] extracting -> {out_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)
    with tarfile.open(dest_tar, "r:gz") as tf:
        tf.extractall(out_dir)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=Path("eval/data/babyslakh_16k"))
    parser.add_argument("--keep-archive", action="store_true", help="don't delete the .tar.gz after extraction")
    args = parser.parse_args(argv)

    data_root = args.out_dir.parent
    tar_path = data_root / FILENAME
    download(tar_path)
    if not verify(tar_path):
        print("[babyslakh] checksum mismatch — refusing to extract a corrupted/tampered archive", file=sys.stderr)
        return 1
    extract(tar_path, args.out_dir)
    if not args.keep_archive:
        tar_path.unlink()
        print(f"[babyslakh] deleted bulk archive: {tar_path}")

    track_dirs = sorted(p for p in args.out_dir.iterdir() if p.is_dir()) if args.out_dir.exists() else []
    # babyslakh_16k.tar.gz extracts to a top-level babyslakh_16k/ dir.
    nested = args.out_dir / "babyslakh_16k"
    if nested.is_dir() and not any(p.name.startswith("Track") for p in track_dirs):
        track_dirs = sorted(p for p in nested.iterdir() if p.is_dir())
    print(f"[babyslakh] {len(track_dirs)} track dirs present under {args.out_dir}")
    print(f"[babyslakh] license: {LICENSE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
