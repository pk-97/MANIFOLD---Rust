"""Fetch MUSDB18 (compressed edition, ~4.4GB) — true stems (vocals/drums/
bass/other) for vocal-region ground truth (D13) and demucs/gating eval.

Per docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §3: license is research/NC —
eval-only per D8 ("internal evaluation and parameter tuning against
NC-licensed datasets is acceptable ... never redistribute dataset audio,
never train shipped model weights on NC data"). Zenodo record 1117372,
license verified 2026-07-17 ('other-nc').

Files are 7z-free .mp4 stem containers read via the `stempeg` package
(not installed by default — decoding is P3/P4's concern; this script's job
is download + checksum + extract only).

Usage:
    python -m eval.fetch.musdb18 --out-dir eval/data/musdb18
"""

from __future__ import annotations

import argparse
import hashlib
import sys
import urllib.request
import zipfile
from pathlib import Path

from eval.paths import DATA_ROOT

ZENODO_RECORD = 1117372
FILENAME = "musdb18.zip"
URL = f"https://zenodo.org/record/{ZENODO_RECORD}/files/{FILENAME}?download=1"
EXPECTED_MD5 = "af06762477334799bfc5abf237648207"
EXPECTED_SIZE = 4684228845
LICENSE = "research/non-commercial (Zenodo record 1117372, 'other-nc') — eval-only per D8, never shipped"


def _md5sum(path: Path, chunk_mb: int = 8) -> str:
    h = hashlib.md5()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(chunk_mb << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download(dest_zip: Path) -> None:
    dest_zip.parent.mkdir(parents=True, exist_ok=True)
    if dest_zip.exists() and dest_zip.stat().st_size == EXPECTED_SIZE:
        print(f"[musdb18] already downloaded: {dest_zip}")
        return
    print(f"[musdb18] downloading {URL} -> {dest_zip} ({EXPECTED_SIZE / 1e9:.1f} GB, this is slow)")
    tmp = dest_zip.with_suffix(dest_zip.suffix + ".part")
    urllib.request.urlretrieve(URL, tmp)
    tmp.rename(dest_zip)


def verify(dest_zip: Path) -> bool:
    got = _md5sum(dest_zip)
    ok = got == EXPECTED_MD5
    print(f"[musdb18] md5 {'OK' if ok else 'MISMATCH'}: expected={EXPECTED_MD5} got={got}")
    return ok


def extract(dest_zip: Path, out_dir: Path) -> None:
    print(f"[musdb18] extracting -> {out_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(dest_zip) as zf:
        zf.extractall(out_dir)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=DATA_ROOT / "musdb18")
    parser.add_argument("--keep-archive", action="store_true")
    args = parser.parse_args(argv)

    data_root = args.out_dir.parent
    zip_path = data_root / FILENAME
    try:
        download(zip_path)
    except Exception as exc:
        print(f"[musdb18] download failed (network/size): {exc}", file=sys.stderr)
        return 1
    if not verify(zip_path):
        print("[musdb18] checksum mismatch — refusing to extract", file=sys.stderr)
        return 1
    extract(zip_path, args.out_dir)
    if not args.keep_archive:
        zip_path.unlink()
        print(f"[musdb18] deleted bulk archive: {zip_path}")
    print(f"[musdb18] license: {LICENSE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
