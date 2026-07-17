"""D14 — absolute alignment by measurement, not fudge.

Click-track truth fixtures: bursts rendered at exactly known sample
positions, exported as wav + mp3 + AAC, run through the full pipeline. The
measured per-stage, per-format offsets are applied once at the seam where
audio enters analysis (manifold_audio.audio_io.load_audio_mono) and stamped
so they're re-measured automatically when ffmpeg/model versions change.

Two stages measured, per format:
    decode   — does load_audio_mono() itself introduce a sample-position
               shift (ffmpeg priming delay for mp3/AAC; should be ~0 for wav,
               which is read directly with no ffmpeg round-trip)
    detector — does the CURRENT onset detector (madmom CNN, D2's last
               pre-P6 arm) additionally bias attack-vs-truth on top of decode

D14's end state: onset_compensation_seconds (Rust, percussion_settings.rs)
defaults to zero and remains only as an artistic offset — that's P5's Rust
half. This module's job is measuring the numbers and applying the
decode-stage correction at the Python seam (the one component that's
stage-independent and doesn't require any detector swap to be meaningful;
the detector-stage number is reported per format but not "corrected away"
here since P2/P6 change the detector itself — see the P1 landing report for
the explicit deviation note).
"""

from __future__ import annotations

import json
import subprocess
import sys
import wave
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Dict, List, Optional

import numpy as np
from scipy import signal as sp_signal

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from manifold_audio.audio_io import load_audio_mono  # noqa: E402
from manifold_audio.external_tools import _resolve_ffmpeg_path  # noqa: E402

SAMPLE_RATE = 44100
BURST_FREQ_HZ = 3000.0
BURST_DURATION_SEC = 0.0015  # ~1.5ms, 3-ish cycles at 3kHz — concentrated energy, lossy-codec survivable
CLICK_TIMES_SEC = [round(1.0 + i * 1.7, 4) for i in range(12)]  # 12 clicks over ~20s, never on a round second

DECODER_ALIGNMENT_PATH = Path(__file__).resolve().parent.parent / "manifold_audio" / "decoder_alignment.json"


def _burst_waveform(sr: int) -> np.ndarray:
    n = int(round(BURST_DURATION_SEC * sr))
    t = np.arange(n) / sr
    window = np.hanning(n)
    tone = np.sin(2 * np.pi * BURST_FREQ_HZ * t)
    return (tone * window).astype(np.float32)


def generate_click_track(sr: int = SAMPLE_RATE, click_times_sec: Optional[List[float]] = None) -> np.ndarray:
    """Silence except for a Hann-windowed tone burst CENTERED on each exact
    sample position in click_times_sec (the Hann envelope's peak — where
    _envelope_peaks() below actually locates it — lands exactly on the truth
    time, not its start). Truth time t -> envelope peak at sample
    round(t*sr), sample-accurate."""
    times = click_times_sec if click_times_sec is not None else CLICK_TIMES_SEC
    total_len = int(round((max(times) + 2.0) * sr))
    audio = np.zeros(total_len, dtype=np.float32)
    burst = _burst_waveform(sr)
    half = len(burst) // 2
    for t in times:
        center = int(round(t * sr))
        start = max(0, center - half)
        burst_start = start - (center - half)  # 0 unless clipped at the array edge
        end = min(total_len, start + (len(burst) - burst_start))
        audio[start:end] += burst[burst_start : burst_start + (end - start)]
    peak = float(np.max(np.abs(audio)))
    if peak > 0:
        audio = audio / peak * 0.9
    return audio


def write_wav(path: Path, audio: np.ndarray, sr: int = SAMPLE_RATE) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    pcm16 = np.clip(audio * 32767.0, -32768, 32767).astype(np.int16)
    with wave.open(str(path), "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(sr)
        wf.writeframes(pcm16.tobytes())


def encode_lossy(wav_path: Path, out_path: Path, fmt: str, ffmpeg_bin: Optional[str] = None) -> None:
    """fmt: 'mp3' or 'aac' (written as .m4a container, the format Manifold
    exports/imports actually see)."""
    ffmpeg = _resolve_ffmpeg_path(ffmpeg_bin)
    if not ffmpeg:
        raise RuntimeError("ffmpeg not found — required to render mp3/AAC click fixtures")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    if fmt == "mp3":
        cmd = [ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-i", str(wav_path), "-codec:a", "libmp3lame", "-b:a", "192k", str(out_path)]
    elif fmt == "aac":
        cmd = [ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-i", str(wav_path), "-codec:a", "aac", "-b:a", "192k", str(out_path)]
    else:
        raise ValueError(f"unknown lossy format {fmt!r}")
    subprocess.run(cmd, check=True)


def build_click_fixtures(out_dir: Path, ffmpeg_bin: Optional[str] = None) -> Dict[str, Path]:
    """Generates the wav and renders mp3 + AAC from it. Returns {format: path}."""
    audio = generate_click_track()
    wav_path = out_dir / "click_track.wav"
    write_wav(wav_path, audio)
    mp3_path = out_dir / "click_track.mp3"
    aac_path = out_dir / "click_track.m4a"
    encode_lossy(wav_path, mp3_path, "mp3", ffmpeg_bin)
    encode_lossy(wav_path, aac_path, "aac", ffmpeg_bin)
    return {"wav": wav_path, "mp3": mp3_path, "aac": aac_path}


def _envelope_peaks(audio: np.ndarray, sr: int, search_times_sec: List[float], search_window_sec: float = 0.15) -> List[Optional[float]]:
    """Bandpass around BURST_FREQ_HZ, Hilbert envelope, sub-sample parabolic
    peak near each expected truth time. Returns detected time (sec) per
    truth click, or None if nothing crosses a floor within the window."""
    nyq = sr / 2.0
    low = max(1.0, BURST_FREQ_HZ - 800.0) / nyq
    high = min(nyq - 1.0, BURST_FREQ_HZ + 800.0) / nyq
    sos = sp_signal.butter(4, [low, high], btype="bandpass", output="sos")
    filtered = sp_signal.sosfiltfilt(sos, audio.astype(np.float64))
    envelope = np.abs(sp_signal.hilbert(filtered))
    floor = float(np.median(envelope)) * 5.0 + 1e-9

    detected: List[Optional[float]] = []
    for t in search_times_sec:
        center = int(round(t * sr))
        half = int(round(search_window_sec * sr))
        lo = max(0, center - half)
        hi = min(len(envelope), center + half)
        if hi <= lo:
            detected.append(None)
            continue
        window = envelope[lo:hi]
        peak_idx = int(np.argmax(window))
        if window[peak_idx] < floor:
            detected.append(None)
            continue
        # Parabolic sub-sample refinement.
        i = lo + peak_idx
        if 0 < i < len(envelope) - 1:
            y0, y1, y2 = envelope[i - 1], envelope[i], envelope[i + 1]
            denom = (y0 - 2 * y1 + y2)
            delta = 0.5 * (y0 - y2) / denom if abs(denom) > 1e-12 else 0.0
            delta = float(np.clip(delta, -1.0, 1.0))
        else:
            delta = 0.0
        detected.append((i + delta) / sr)
    return detected


@dataclass
class StageOffset:
    n_truth: int
    n_matched: int
    median_offset_ms: Optional[float]
    mean_offset_ms: Optional[float]
    max_abs_offset_ms: Optional[float]


def measure_decode_stage_offset(audio_path: Path, truth_times_sec: List[float], ffmpeg_bin: Optional[str] = None) -> StageOffset:
    """decoded_time - truth_time, per click, via load_audio_mono (the
    analysis-input seam) — this is the offset a raw decode-then-analyze path
    would carry before any detector runs at all."""
    audio, sr = load_audio_mono(audio_path, target_sr=SAMPLE_RATE, ffmpeg_bin=ffmpeg_bin)
    detected = _envelope_peaks(audio, sr, truth_times_sec)
    offsets_ms = [
        (d - t) * 1000.0 for d, t in zip(detected, truth_times_sec) if d is not None
    ]
    if not offsets_ms:
        return StageOffset(len(truth_times_sec), 0, None, None, None)
    return StageOffset(
        n_truth=len(truth_times_sec),
        n_matched=len(offsets_ms),
        median_offset_ms=float(np.median(offsets_ms)),
        mean_offset_ms=float(np.mean(offsets_ms)),
        max_abs_offset_ms=float(np.max(np.abs(offsets_ms))),
    )


def measure_detector_stage_offset(audio_path: Path, truth_times_sec: List[float], ffmpeg_bin: Optional[str] = None) -> StageOffset:
    """Runs the CURRENT onset detector (madmom CNN — the live pre-P6 arm)
    directly on the file and matches its picked onsets to truth clicks. This
    captures model hop-quantization + attack-vs-onset bias ON TOP of decode."""
    from manifold_audio.onset_detection import detect_madmom_onsets

    result = detect_madmom_onsets(str(audio_path), method="cnn", ffmpeg_bin=ffmpeg_bin)
    if result is None:
        return StageOffset(len(truth_times_sec), 0, None, None, None)
    _activation, onset_times = result
    offsets_ms: List[float] = []
    onset_list = list(onset_times)
    for t in truth_times_sec:
        if not onset_list:
            break
        nearest = min(onset_list, key=lambda x: abs(x - t))
        if abs(nearest - t) <= 0.15:
            offsets_ms.append((nearest - t) * 1000.0)
    if not offsets_ms:
        return StageOffset(len(truth_times_sec), 0, None, None, None)
    return StageOffset(
        n_truth=len(truth_times_sec),
        n_matched=len(offsets_ms),
        median_offset_ms=float(np.median(offsets_ms)),
        mean_offset_ms=float(np.mean(offsets_ms)),
        max_abs_offset_ms=float(np.max(np.abs(offsets_ms))),
    )


def build_alignment_report(out_dir: Path, ffmpeg_bin: Optional[str] = None) -> Dict[str, Dict]:
    fixtures = build_click_fixtures(out_dir, ffmpeg_bin)
    report: Dict[str, Dict] = {}
    for fmt, path in fixtures.items():
        decode = measure_decode_stage_offset(path, CLICK_TIMES_SEC, ffmpeg_bin)
        detector = measure_detector_stage_offset(path, CLICK_TIMES_SEC, ffmpeg_bin)
        report[fmt] = {"decode_stage": asdict(decode), "detector_stage": asdict(detector)}
    return report


# build_click_fixtures names the AAC render click_track.m4a (the container
# Manifold actually imports) — the correction table must be keyed by real
# file suffix (what load_audio_mono sees via Path.suffix), not codec name.
_FORMAT_TO_SUFFIX = {"wav": "wav", "mp3": "mp3", "aac": "m4a"}


def write_decoder_alignment_table(report: Dict[str, Dict], path: Path = DECODER_ALIGNMENT_PATH) -> None:
    """Writes the correction table load_audio_mono() reads (audio_io.py).

    Sign convention (must match _apply_decode_stage_correction in
    audio_io.py exactly): correction_sec is the RAW measured offset
    (decoded_time - truth_time), unmodified. Positive = this format's decode
    delivers content late -> the consumer trims that many leading samples
    (advances). Negative = early -> the consumer pads that many leading
    zero samples (delays). Missing/zero entries are safe no-ops (additive,
    load-bearing for the 'wav unaffected' invariant)."""
    correction_sec: Dict[str, float] = {}
    for fmt, stages in report.items():
        suffix = _FORMAT_TO_SUFFIX.get(fmt, fmt)
        decode = stages["decode_stage"]
        if decode["median_offset_ms"] is not None:
            correction_sec[suffix] = decode["median_offset_ms"] / 1000.0
        else:
            correction_sec[suffix] = 0.0
    payload = {
        "_comment": (
            "D14 decode-stage correction, applied in manifold_audio/audio_io.py "
            "load_audio_mono(). Measured by eval/click_track.py against known-position "
            "click fixtures. Re-measure whenever ffmpeg or the audio_io decode path "
            "changes — this file is a measurement artifact, not a hand-tuned constant."
        ),
        "correction_sec_by_suffix": correction_sec,
    }
    path.write_text(json.dumps(payload, indent=2))
