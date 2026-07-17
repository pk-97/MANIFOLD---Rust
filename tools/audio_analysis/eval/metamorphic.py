"""Metamorphic invariant suite — label-free, runs on any audio (§3).

Four checks, per docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §3:
    gain            +-6 dB          -> event times/counts unchanged
    time_stretch    +-5%            -> event times scale, grid BPM scales
    stem_injection  known stem @ known offset -> events appear there
    noise_floor     -40 dB noise added         -> no new events

"Violations are bugs, not tuning targets" — this suite is not a scoreboard
metric to optimize; run_metamorphic_suite returns pass/fail + detail, and a
failure blocks the P1/P4 gates outright rather than feeding a score.

Detector-agnostic by construction (Deferred #4: "we should use these datasets
to also improve the real-time detectors in the future") — every check takes
a `detect_fn(audio: np.ndarray, sr: int) -> DetectorOutput` callback, so the
same suite runs over the offline pipeline today and a causal/realtime
detector later, replayed over fixture audio via the offline path.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Callable, List, Optional, Sequence

import numpy as np

try:
    import librosa
except ImportError:  # pragma: no cover
    librosa = None  # type: ignore[assignment]


@dataclass(frozen=True)
class DetectorOutput:
    """Minimal detector output shape the metamorphic suite needs. A real
    detector wrapper (run.py) adapts analyze_percussion()'s richer output
    down to this."""

    event_times_sec: List[float] = field(default_factory=list)
    bpm: Optional[float] = None


DetectFn = Callable[[np.ndarray, int], DetectorOutput]


@dataclass(frozen=True)
class MetamorphicResult:
    name: str
    passed: bool
    detail: str


def _apply_gain_db(audio: np.ndarray, gain_db: float) -> np.ndarray:
    gain = 10.0 ** (gain_db / 20.0)
    return np.clip(audio * gain, -1.0, 1.0).astype(np.float32)


def check_gain_invariance(
    audio: np.ndarray,
    sr: int,
    detect_fn: DetectFn,
    gain_db: float = 6.0,
    time_tolerance_sec: float = 0.021,  # >1 madmom onset-activation frame (10ms hop) — single-frame quantization jitter under gain change is expected, not a bug
    count_tolerance: Optional[int] = None,
    count_tolerance_fraction: float = 0.05,
) -> MetamorphicResult:
    """+-gain_db should not move event times beyond decode/framing jitter or
    change event counts beyond a small slop (envelope-follower threshold
    crossings can shift by a frame at extreme gain). count_tolerance, if
    given, is an absolute cap; otherwise it's max(1, 5% of base event count)
    — real musical material has borderline onsets near any threshold, so a
    fixed +-1 is too strict on a 60-90 event track and too loose on a
    3-event synthetic one."""
    base = detect_fn(audio, sr)
    effective_tolerance = count_tolerance if count_tolerance is not None else max(1, round(count_tolerance_fraction * len(base.event_times_sec)))
    for sign, label in ((1.0, "up"), (-1.0, "down")):
        shifted_audio = _apply_gain_db(audio, sign * gain_db)
        shifted = detect_fn(shifted_audio, sr)
        if abs(len(shifted.event_times_sec) - len(base.event_times_sec)) > effective_tolerance:
            return MetamorphicResult(
                f"gain_invariance_{label}",
                False,
                f"event count changed by "
                f"{len(shifted.event_times_sec) - len(base.event_times_sec)} "
                f"(base={len(base.event_times_sec)}, shifted={len(shifted.event_times_sec)})",
            )
        max_shift = _max_matched_time_shift(base.event_times_sec, shifted.event_times_sec)
        if max_shift is not None and max_shift > time_tolerance_sec:
            return MetamorphicResult(
                f"gain_invariance_{label}",
                False,
                f"max matched event-time shift {max_shift * 1000:.1f}ms > "
                f"{time_tolerance_sec * 1000:.1f}ms tolerance",
            )
    return MetamorphicResult("gain_invariance", True, f"stable under +-{gain_db}dB")


def _max_matched_time_shift(a: Sequence[float], b: Sequence[float], max_pair_distance_sec: float = 0.25) -> Optional[float]:
    """Nearest-neighbor time shift between two event lists, used as a sanity
    distance (not a P/R match) — greedy pairing, BOUNDED: a pair further
    apart than max_pair_distance_sec is not a "shift", it's two different
    events (list-length mismatch, a different signal already caught by the
    caller's own count/bpm checks) and is excluded rather than inflating the
    reported shift with a meaningless force-match."""
    if not a or not b:
        return None
    a_sorted = sorted(a)
    b_remaining = sorted(b)
    shifts: List[float] = []
    for t in a_sorted:
        if not b_remaining:
            break
        nearest = min(b_remaining, key=lambda x: abs(x - t))
        dist = abs(nearest - t)
        if dist <= max_pair_distance_sec:
            shifts.append(dist)
            b_remaining.remove(nearest)
    return max(shifts) if shifts else None


def check_time_stretch_invariance(
    audio: np.ndarray,
    sr: int,
    detect_fn: DetectFn,
    stretch_fraction: float = 0.05,
    time_tolerance_sec: float = 0.020,
    bpm_tolerance_fraction: float = 0.02,
) -> MetamorphicResult:
    """+-stretch_fraction time-stretch should scale event times and BPM by the
    same factor (librosa.effects.time_stretch preserves pitch, changes
    duration by 1/rate — rate>1 shortens)."""
    if librosa is None:
        return MetamorphicResult("time_stretch_invariance", False, "librosa not importable")
    base = detect_fn(audio, sr)
    if base.bpm is None or not base.event_times_sec:
        return MetamorphicResult(
            "time_stretch_invariance", True, "no base events/bpm to check (vacuously passes)"
        )
    for direction, rate in ((1.0 + stretch_fraction, "faster"), (1.0 - stretch_fraction, "slower")):
        stretched_audio = librosa.effects.time_stretch(audio.astype(np.float32), rate=direction)
        stretched = detect_fn(stretched_audio, sr)
        if stretched.bpm is None:
            return MetamorphicResult(
                f"time_stretch_invariance_{rate}", False, "detector returned no bpm on stretched audio"
            )
        expected_bpm = base.bpm * direction
        bpm_err = abs(stretched.bpm - expected_bpm) / expected_bpm
        if bpm_err > bpm_tolerance_fraction:
            return MetamorphicResult(
                f"time_stretch_invariance_{rate}",
                False,
                f"bpm scaled wrong: base={base.bpm:.2f} expected={expected_bpm:.2f} "
                f"got={stretched.bpm:.2f} ({bpm_err * 100:.1f}% err)",
            )
        expected_times = [t / direction for t in base.event_times_sec]
        # Two distinct failure modes, checked separately: the event SET
        # changing size (a different invariant than timing precision — a
        # detector can scale its surviving events perfectly and still be
        # unstable about which events survive) vs the timing of events that
        # DO match. A count mismatch alone isn't fatal here (stretching can
        # legitimately reveal/hide a borderline onset) — it's reported so a
        # human can judge, not gated.
        count_delta = len(stretched.event_times_sec) - len(expected_times)
        max_shift = _max_matched_time_shift(expected_times, stretched.event_times_sec)
        if max_shift is not None and max_shift > time_tolerance_sec:
            return MetamorphicResult(
                f"time_stretch_invariance_{rate}",
                False,
                f"event times didn't scale with stretch: max matched-pair shift "
                f"{max_shift * 1000:.1f}ms > {time_tolerance_sec * 1000:.1f}ms "
                f"(event count delta: {count_delta:+d})",
            )
    return MetamorphicResult("time_stretch_invariance", True, f"scales correctly under +-{stretch_fraction * 100:.0f}%")


def check_stem_injection(
    audio: np.ndarray,
    sr: int,
    stem_audio: np.ndarray,
    offset_sec: float,
    detect_fn: DetectFn,
    time_tolerance_sec: float = 0.050,
    stem_gain: float = 1.0,
) -> MetamorphicResult:
    """Mixing a known stem in at a known offset should produce a new event
    near that offset that wasn't there before (a floor for recall: injecting
    an unambiguous transient must be detectable, or the detector is broken
    in a way P/R against real fixtures might not isolate)."""
    base = detect_fn(audio, sr)
    offset_samples = int(round(offset_sec * sr))
    mixed = audio.copy().astype(np.float32)
    end = min(len(mixed), offset_samples + len(stem_audio))
    if offset_samples >= len(mixed):
        # Pad if the injection point is past the end.
        pad = np.zeros(offset_samples + len(stem_audio) - len(mixed), dtype=np.float32)
        mixed = np.concatenate([mixed, pad])
        end = len(mixed)
    mixed[offset_samples:end] += stem_gain * stem_audio[: end - offset_samples].astype(np.float32)
    peak = float(np.max(np.abs(mixed)))
    if peak > 1e-6:
        mixed = mixed / peak
    injected = detect_fn(mixed, sr)
    new_events = [t for t in injected.event_times_sec if not _near_any(t, base.event_times_sec, time_tolerance_sec)]
    if not _near_any(offset_sec, new_events, time_tolerance_sec):
        return MetamorphicResult(
            "stem_injection",
            False,
            f"no new event within {time_tolerance_sec * 1000:.0f}ms of injected offset "
            f"{offset_sec:.3f}s (new events: {new_events[:10]})",
        )
    return MetamorphicResult("stem_injection", True, f"detected injected event at {offset_sec:.3f}s")


def _near_any(t: float, candidates: Sequence[float], tolerance_sec: float) -> bool:
    return any(abs(t - c) <= tolerance_sec for c in candidates)


def check_noise_floor_silence(
    audio: np.ndarray,
    sr: int,
    detect_fn: DetectFn,
    noise_db: float = -40.0,
    seed: int = 0,
) -> MetamorphicResult:
    """Adding noise at noise_db (relative to full scale) to an otherwise
    silent/near-silent excerpt should not create new events. Uses the
    quietest 2-second window of the given audio as the "silence" base —
    callers that want a guaranteed-silent base should pass a dedicated
    silent clip instead."""
    window_len = min(len(audio), int(2.0 * sr))
    if window_len <= 0:
        return MetamorphicResult("noise_floor_silence", True, "no audio to test (vacuous pass)")
    hop = max(1, window_len // 4)
    best_start = 0
    best_rms = float("inf")
    for start in range(0, max(1, len(audio) - window_len), hop):
        window = audio[start : start + window_len]
        rms = float(np.sqrt(np.mean(window.astype(np.float64) ** 2)))
        if rms < best_rms:
            best_rms = rms
            best_start = start
    quiet = audio[best_start : best_start + window_len].astype(np.float32)
    base = detect_fn(quiet, sr)
    rng = np.random.default_rng(seed)
    noise_amp = 10.0 ** (noise_db / 20.0)  # dBFS: -40dB -> 0.01 absolute amplitude
    noisy = quiet + (noise_amp * rng.standard_normal(len(quiet))).astype(np.float32)
    # Deliberately NOT peak-renormalized: this check's whole point is "is
    # noise_db of absolute-scale noise on top of near-silent content
    # detectable" — renormalizing a near-zero signal back up to full scale
    # would amplify the added noise right back to 0dBFS and guarantee a
    # false failure. Only clip to avoid wraparound distortion.
    noisy = np.clip(noisy, -1.0, 1.0)
    noised = detect_fn(noisy, sr)
    new_events = [t for t in noised.event_times_sec if not _near_any(t, base.event_times_sec, 0.050)]
    if new_events:
        return MetamorphicResult(
            "noise_floor_silence",
            False,
            f"{len(new_events)} new event(s) appeared after adding {noise_db}dB noise "
            f"to the quietest window (base rms={best_rms:.2e})",
        )
    return MetamorphicResult("noise_floor_silence", True, f"no new events under {noise_db}dB noise")


def run_metamorphic_suite(
    audio: np.ndarray,
    sr: int,
    detect_fn: DetectFn,
    injection_stem_audio: Optional[np.ndarray] = None,
    injection_offset_sec: float = 3.0,
) -> List[MetamorphicResult]:
    """Runs all applicable checks. stem_injection is skipped (not failed) if
    no injection_stem_audio is supplied — the caller (run.py) is expected to
    pass one from a different track's stem when available."""
    results = [
        check_gain_invariance(audio, sr, detect_fn),
        check_time_stretch_invariance(audio, sr, detect_fn),
        check_noise_floor_silence(audio, sr, detect_fn),
    ]
    if injection_stem_audio is not None:
        results.append(
            check_stem_injection(audio, sr, injection_stem_audio, injection_offset_sec, detect_fn)
        )
    return results
