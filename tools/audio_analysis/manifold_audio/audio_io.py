"""Audio loading, decoding, resampling, and framing."""

from __future__ import annotations

import subprocess
import tempfile
import wave
from pathlib import Path
from typing import Optional, Tuple

import numpy as np

from manifold_audio.external_tools import _resolve_ffmpeg_path


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
