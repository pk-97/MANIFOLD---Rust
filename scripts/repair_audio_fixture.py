#!/usr/bin/env python3
"""Reconstruct the mix in the bad_guy audio fixture from its four stems.

The command is intentionally conservative: it only accepts the fixture layout
used by the audio evaluation corpus and never resamples, stretches, or
normalizes a stem.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import struct
import sys
import tempfile
import wave
from pathlib import Path
from typing import Iterable


STEM_NAMES = ("bass", "drums", "others", "vocals")
MIX_NAME = "mix.wav"
REPORT_NAME = "provenance.json"


class RepairError(RuntimeError):
    """A refusal to inspect or mutate an invalid fixture."""


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _read_wav(path: Path) -> tuple[dict[str, int | str], bytes]:
    try:
        with wave.open(str(path), "rb") as stream:
            params = stream.getparams()
            if stream.getcomptype() != "NONE":
                raise RepairError(f"{path.name}: compressed WAV is not supported")
            frames = stream.readframes(params.nframes)
            if len(frames) != params.nframes * params.nchannels * params.sampwidth:
                raise RepairError(f"{path.name}: truncated WAV frame data")
            info: dict[str, int | str] = {
                "sample_rate": params.framerate,
                "channels": params.nchannels,
                "sample_width": params.sampwidth,
                "frames": params.nframes,
                "format": "PCM" if params.sampwidth else "unknown",
            }
            return info, frames
    except (wave.Error, EOFError) as exc:
        raise RepairError(f"{path.name}: invalid WAV ({exc})") from exc


def _require_stem_format(name: str, info: dict[str, int | str], expected: dict[str, int]) -> None:
    if info["sample_width"] != 2:
        raise RepairError(f"{name}.wav: expected 16-bit PCM, got {info['sample_width'] * 8}-bit")
    if info["channels"] != 2:
        raise RepairError(f"{name}.wav: expected stereo, got {info['channels']} channels")
    for field in ("sample_rate", "frames"):
        if info[field] != expected[field]:
            raise RepairError(
                f"{name}.wav: {field} {info[field]} does not match stems ({expected[field]})"
            )


def _quantize_24(sample: int) -> int:
    """Map an in-range signed 16-bit sum to signed 24-bit PCM.

    Signed 16-bit PCM uses 32768 as its scale on both sides. The positive
    endpoint can therefore be reached by summing stems; map it to the highest
    representable signed 24-bit code.
    """

    return 8_388_607 if sample == 32_768 else sample * 256


def _encode_mix(stem_frames: Iterable[bytes], frame_count: int) -> tuple[bytes, float]:
    unpacked = [struct.iter_unpack("<hh", frames) for frames in stem_frames]
    output = bytearray(frame_count * 2 * 3)
    max_abs = 0
    offset = 0
    for _ in range(frame_count):
        frame_samples = [next(samples) for samples in unpacked]
        for channel in range(2):
            summed = sum(samples[channel] for samples in frame_samples)
            if summed > 32_768 or summed < -32_768:
                raise RepairError(
                    f"stem sum exceeds full scale at frame {_}, channel {channel}: {summed}"
                )
            max_abs = max(max_abs, abs(summed))
            encoded = _quantize_24(summed)
            output[offset : offset + 3] = (encoded & 0xFFFFFF).to_bytes(3, "little")
            offset += 3
    return bytes(output), max_abs / 32_768


def _wav_bytes(info: dict[str, int | str], frames: bytes, sample_width: int) -> bytes:
    with tempfile.TemporaryFile() as temporary:
        with wave.open(temporary, "wb") as stream:
            stream.setnchannels(2)
            stream.setsampwidth(sample_width)
            stream.setframerate(int(info["sample_rate"]))
            stream.writeframes(frames)
        temporary.seek(0)
        return temporary.read()


def reconstruct(fixture_dir: Path, backup_dir: Path, apply: bool = False) -> dict[str, object]:
    if not fixture_dir.is_dir():
        raise RepairError(f"fixture directory does not exist: {fixture_dir}")
    if fixture_dir.resolve() == backup_dir.resolve():
        raise RepairError("backup directory must be separate from the fixture directory")
    mix_path = fixture_dir / MIX_NAME
    if not mix_path.is_file():
        raise RepairError(f"missing {MIX_NAME} in {fixture_dir}")

    mix_info, mix_frames = _read_wav(mix_path)
    stem_infos: dict[str, dict[str, int | str]] = {}
    stem_frames: list[bytes] = []
    stem_hashes: dict[str, str] = {}
    expected: dict[str, int] | None = None
    for name in STEM_NAMES:
        path = fixture_dir / f"{name}.wav"
        if not path.is_file():
            raise RepairError(f"missing {path.name} in {fixture_dir}")
        info, frames = _read_wav(path)
        if expected is None:
            expected = {"sample_rate": int(info["sample_rate"]), "frames": int(info["frames"])}
        _require_stem_format(name, info, expected)
        stem_infos[name] = info
        stem_frames.append(frames)
        stem_hashes[name] = sha256(path.read_bytes())

    assert expected is not None
    reconstructed_frames, peak = _encode_mix(stem_frames, expected["frames"])
    output_bytes = _wav_bytes(expected | {"channels": 2}, reconstructed_frames, 3)

    stem_records = [
        {
            "name": f"{name}.wav",
            "sample_rate": int(stem_infos[name]["sample_rate"]),
            "channels": int(stem_infos[name]["channels"]),
            "sample_width": int(stem_infos[name]["sample_width"]),
            "frames": int(stem_infos[name]["frames"]),
            "sha256": stem_hashes[name],
        }
        for name, frames in zip(STEM_NAMES, stem_frames)
    ]
    original_mix_bytes = mix_path.read_bytes()
    source_sha256 = sha256(original_mix_bytes)
    source_mix = {
        "name": MIX_NAME,
        "sample_rate": int(mix_info["sample_rate"]),
        "channels": int(mix_info["channels"]),
        "sample_width": int(mix_info["sample_width"]),
        "frames": int(mix_info["frames"]),
    }
    existing_report: dict[str, object] | None = None
    existing_report_path = backup_dir / REPORT_NAME
    if existing_report_path.is_file():
        try:
            loaded = json.loads(existing_report_path.read_text(encoding="utf-8"))
            if isinstance(loaded, dict):
                existing_report = loaded
        except (OSError, json.JSONDecodeError):
            existing_report = None
    if existing_report is not None:
        existing_mix = existing_report.get("mix")
        if isinstance(existing_mix, dict) and existing_mix.get("output_sha256") == source_sha256:
            # A rerun sees the already repaired mix. Keep provenance anchored
            # to the original source that was copied into the backup.
            prior_source = existing_mix.get("source_sha256")
            if isinstance(prior_source, str):
                source_sha256 = prior_source
            prior_format = existing_report.get("source_mix")
            if isinstance(prior_format, dict):
                source_mix = prior_format

    report: dict[str, object] = {
        "schema": 1,
        "source_mix": source_mix,
        "mix": {
            "name": MIX_NAME,
            "source_sha256": source_sha256,
            "output_sha256": sha256(output_bytes),
            "sample_rate": expected["sample_rate"],
            "channels": 2,
            "sample_width": 3,
            "frames": expected["frames"],
            "peak": peak,
        },
        "stems": stem_records,
    }

    if apply:
        backup_path = backup_dir / MIX_NAME
        original_bytes = original_mix_bytes
        repaired_report = (
            existing_report is not None
            and isinstance(existing_report.get("mix"), dict)
            and existing_report["mix"].get("output_sha256") == sha256(original_bytes)
        )
        if repaired_report and not backup_path.exists():
            raise RepairError(f"refusing rerun with missing original backup: {backup_path}")
        if backup_path.exists():
            backup_matches_source = backup_path.is_file() and backup_path.read_bytes() == original_bytes
            already_repaired = (
                existing_report is not None
                and isinstance(existing_report.get("mix"), dict)
                and existing_report["mix"].get("output_sha256") == sha256(original_bytes)
                and existing_report["mix"].get("source_sha256") == sha256(backup_path.read_bytes())
            )
            if not backup_matches_source and not already_repaired:
                raise RepairError(f"refusing to overwrite conflicting backup: {backup_path}")
        else:
            backup_dir.mkdir(parents=True, exist_ok=True)
            backup_path.write_bytes(original_bytes)
        report_path = backup_dir / REPORT_NAME
        report_bytes = (json.dumps(report, indent=2, sort_keys=True) + "\n").encode("utf-8")
        report_path.write_bytes(report_bytes)
        if mix_path.read_bytes() != output_bytes:
            temporary_path = mix_path.with_suffix(".wav.tmp")
            temporary_path.write_bytes(output_bytes)
            os.replace(temporary_path, mix_path)
        report["backup"] = str(Path(MIX_NAME))
        report["report"] = str(Path(REPORT_NAME))
        # Rewrite with the stable backup/report fields after applying.
        report_path.write_bytes((json.dumps(report, indent=2, sort_keys=True) + "\n").encode("utf-8"))
    return report


def _print_report(report: dict[str, object], applied: bool) -> None:
    source = report["source_mix"]
    mix = report["mix"]
    assert isinstance(source, dict)
    assert isinstance(mix, dict)
    print(
        f"source mix: {source['sample_width'] * 8}-bit PCM, {source['channels']} channels, "
        f"{source['sample_rate']} Hz, {source['frames']} frames"
    )
    print(
        f"output mix: {mix['sample_width'] * 8}-bit PCM, {mix['channels']} channels, "
        f"{mix['sample_rate']} Hz, {mix['frames']} frames"
    )
    for stem in report["stems"]:
        print(
            f"{stem['name']}: {stem['sample_width'] * 8}-bit PCM, {stem['channels']} channels, "
            f"{stem['sample_rate']} Hz, {stem['frames']} frames"
        )
    print(f"peak: {mix['peak']:.9f}")
    print(f"source_sha256: {mix['source_sha256']}")
    print(f"output_sha256: {mix['output_sha256']}")
    if applied:
        print("applied: yes")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-dir", required=True, type=Path)
    parser.add_argument("--backup-dir", required=True, type=Path)
    parser.add_argument("--apply", action="store_true", help="write the backup, report, and reconstructed mix")
    args = parser.parse_args(argv)
    try:
        report = reconstruct(args.fixture_dir, args.backup_dir, apply=args.apply)
    except RepairError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    _print_report(report, args.apply)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
