"""Audio loading, decoding, resampling, and framing."""

from __future__ import annotations

import json
import subprocess
import tempfile
import wave
from pathlib import Path
from typing import Dict, Optional, Tuple

import numpy as np

from manifold_audio.external_tools import _resolve_ffmpeg_path

# D14 (docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md): per-format decode-stage
# alignment correction, measured by eval/click_track.py against known-
# position click fixtures and written to decoder_alignment.json beside this
# file. Absent file or absent key = zero correction (today's behavior,
# unchanged) — this is additive, never a behavior change for a format that
# hasn't been measured yet.
_DECODER_ALIGNMENT_PATH = Path(__file__).resolve().parent / "decoder_alignment.json"
_decoder_alignment_cache: Optional[Dict[str, float]] = None


def _load_decoder_alignment_table() -> Dict[str, float]:
    global _decoder_alignment_cache
    if _decoder_alignment_cache is not None:
        return _decoder_alignment_cache
    table: Dict[str, float] = {}
    if _DECODER_ALIGNMENT_PATH.exists():
        try:
            payload = json.loads(_DECODER_ALIGNMENT_PATH.read_text())
            table = {k: float(v) for k, v in payload.get("correction_sec_by_suffix", {}).items()}
        except Exception:
            table = {}
    _decoder_alignment_cache = table
    return table


def _shift_audio_by_correction(audio: np.ndarray, sr: int, correction_sec: float) -> np.ndarray:
    """Pure sample-shift logic, split out from the table lookup so it's
    directly testable (eval/tests/test_click_track.py locks this sign
    convention down — the single most dangerous place for D14 to silently
    do the wrong thing). correction_sec is the raw measured offset
    (decoded_time - truth_time): > 0 means this format's decode arrives late
    relative to truth -> advance (trim leading samples); < 0 means early ->
    delay (prepend zeros). See eval/click_track.py
    (write_decoder_alignment_table) for how this is measured — the sign
    convention there must match exactly."""
    if correction_sec == 0.0:
        return audio
    shift_samples = int(round(correction_sec * sr))
    if shift_samples > 0:
        return np.ascontiguousarray(audio[shift_samples:], dtype=np.float32)
    if shift_samples < 0:
        pad = np.zeros(-shift_samples, dtype=np.float32)
        return np.ascontiguousarray(np.concatenate([pad, audio]), dtype=np.float32)
    return audio


def _apply_decode_stage_correction(audio: np.ndarray, sr: int, suffix: str) -> np.ndarray:
    """Looks up the measured per-format correction (D14) and applies it via
    _shift_audio_by_correction."""
    table = _load_decoder_alignment_table()
    correction_sec = table.get(suffix.lower().lstrip("."), 0.0)
    return _shift_audio_by_correction(audio, sr, correction_sec)


def _read_wav_to_mono_float(path: Path) -> Tuple[np.ndarray, int]:
    with wave.open(str(path), "rb") as wf:
        channels = wf.getnchannels()
        sample_rate = wf.getframerate()
        sample_width = wf.getsampwidth()
        frame_count = wf.getnframes()
        raw = wf.readframes(frame_count)

    if sample_width == 1:
        data = np.frombuffer(raw, dtype=np.uint8).astype(np.float32)
        data = (data - 128.0) / 128.0
    elif sample_width == 2:
        data = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    elif sample_width == 3:
        bytes_u8 = np.frombuffer(raw, dtype=np.uint8)
        triplets = bytes_u8.reshape(-1, 3)
        ints = (
            triplets[:, 0].astype(np.int32)
            | (triplets[:, 1].astype(np.int32) << 8)
            | (triplets[:, 2].astype(np.int32) << 16)
        )
        sign_mask = 1 << 23
        ints = (ints ^ sign_mask) - sign_mask
        data = ints.astype(np.float32) / float(1 << 23)
    elif sample_width == 4:
        data = np.frombuffer(raw, dtype=np.int32).astype(np.float32) / float(1 << 31)
    else:
        raise ValueError(f"Unsupported WAV sample width: {sample_width}")

    if channels > 1:
        data = data.reshape(-1, channels).mean(axis=1)

    data = np.ascontiguousarray(data, dtype=np.float32)
    return data, sample_rate


def _resample_linear(signal: np.ndarray, from_sr: int, to_sr: int) -> np.ndarray:
    if from_sr == to_sr:
        return signal

    duration = len(signal) / float(from_sr)
    target_len = max(1, int(round(duration * to_sr)))
    x_old = np.linspace(0.0, duration, num=len(signal), endpoint=False)
    x_new = np.linspace(0.0, duration, num=target_len, endpoint=False)
    out = np.interp(x_new, x_old, signal).astype(np.float32)
    return np.ascontiguousarray(out)


def _decode_with_ffmpeg(path: Path, sample_rate: int, ffmpeg_bin: Optional[str]) -> Tuple[np.ndarray, int]:
    ffmpeg_path = _resolve_ffmpeg_path(ffmpeg_bin)
    if not ffmpeg_path:
        raise RuntimeError(
            "ffmpeg is required for non-wav input but was not found. "
            "Set FFMPEG_PATH or pass --ffmpeg-bin /absolute/path/to/ffmpeg."
        )

    with tempfile.NamedTemporaryFile(suffix=".wav", delete=True) as tmp:
        cmd = [
            ffmpeg_path,
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            str(path),
            "-ac",
            "1",
            "-ar",
            str(sample_rate),
            "-f",
            "wav",
            tmp.name,
        ]
        try:
            subprocess.run(cmd, check=True)
        except subprocess.CalledProcessError as exc:
            raise RuntimeError(f"ffmpeg failed to decode '{path}' using '{ffmpeg_path}'.") from exc

        audio, sr = _read_wav_to_mono_float(Path(tmp.name))
        return audio, sr


def load_audio_mono(path: Path, target_sr: int, ffmpeg_bin: Optional[str]) -> Tuple[np.ndarray, int]:
    if not path.exists():
        raise FileNotFoundError(f"Audio file not found: {path}")

    if path.suffix.lower() == ".wav":
        audio, sr = _read_wav_to_mono_float(path)
    else:
        audio, sr = _decode_with_ffmpeg(path, target_sr, ffmpeg_bin)

    if sr != target_sr:
        audio = _resample_linear(audio, sr, target_sr)
        sr = target_sr

    # D14: correct measured per-format decode-stage skew at the seam where
    # audio enters analysis (this function), before anything downstream sees
    # a sample. No-op for formats with no measured entry (default zero).
    audio = _apply_decode_stage_correction(audio, sr, path.suffix)

    if len(audio) == 0:
        raise RuntimeError("Decoded audio is empty.")

    # Peak normalize for stable thresholds.
    peak = float(np.max(np.abs(audio)))
    if peak > 1e-6:
        audio = audio / peak

    return np.ascontiguousarray(audio, dtype=np.float32), sr


def frame_signal(signal: np.ndarray, frame_size: int, hop_size: int) -> np.ndarray:
    if len(signal) < frame_size:
        pad = frame_size - len(signal)
        signal = np.pad(signal, (0, pad), mode="constant")

    frame_count = 1 + (len(signal) - frame_size) // hop_size
    if frame_count <= 0:
        frame_count = 1

    stride = signal.strides[0]
    frames = np.lib.stride_tricks.as_strided(
        signal,
        shape=(frame_count, frame_size),
        strides=(hop_size * stride, stride),
        writeable=False,
    )
    return np.ascontiguousarray(frames)
