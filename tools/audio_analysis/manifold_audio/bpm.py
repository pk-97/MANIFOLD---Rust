"""BPM estimation (madmom + autocorrelation) and beat grid construction."""

from __future__ import annotations

import math
from typing import TYPE_CHECKING, List, Optional, Sequence, Tuple

if TYPE_CHECKING:
    from manifold_audio.models import AnalysisConfig

import numpy as np

from manifold_audio.math_utils import (
    _clamp,
    _local_peak_value,
    _safe_div,
    robust_threshold,
)
from manifold_audio.models import BeatGrid, ScoredEvent

try:
    import madmom  # noqa: F401
    from madmom.features.beats import (
        DBNBeatTrackingProcessor,
        RNNBeatProcessor,
    )
    from madmom.features.downbeats import (
        DBNDownBeatTrackingProcessor,
        RNNDownBeatProcessor,
    )
    from madmom.features.tempo import (
        CombFilterTempoHistogramProcessor,
        TempoEstimationProcessor,
    )

    _HAS_MADMOM = True
except ImportError:
    _HAS_MADMOM = False


def estimate_bpm(
    global_onset: np.ndarray,
    hop_time: float,
    min_bpm: float = 70.0,
    max_bpm: float = 180.0,
) -> Optional[float]:
    if global_onset.size < 16:
        return None

    x = global_onset.astype(np.float64)
    x = x - np.mean(x)
    if float(np.max(np.abs(x))) < 1e-12:
        return None

    autocorr = np.correlate(x, x, mode="full")[len(x) - 1 :]

    lo_bpm = max(20.0, min_bpm)
    hi_bpm = min(300.0, max_bpm)
    min_lag = max(1, int(round((60.0 / hi_bpm) / hop_time)))
    max_lag = max(min_lag + 1, int(round((60.0 / lo_bpm) / hop_time)))

    if max_lag >= len(autocorr):
        return None

    roi = autocorr[min_lag:max_lag]
    if roi.size == 0:
        return None

    best_offset = int(np.argmax(roi))
    best_lag = min_lag + best_offset
    if best_lag <= 0:
        return None

    bpm = 60.0 / (best_lag * hop_time)

    while bpm < lo_bpm:
        bpm *= 2.0
    while bpm > hi_bpm:
        bpm *= 0.5

    return float(bpm)


def _fractional_autocorr(x: np.ndarray, lag: float) -> float:
    """Autocorrelation at a fractional lag using linear interpolation."""
    n = len(x)
    lag_int = int(lag)
    lag_frac = lag - lag_int

    if lag_int + 1 >= n:
        return 0.0

    if lag_frac < 1e-12:
        return float(np.dot(x[: n - lag_int], x[lag_int:]))

    usable = n - lag_int - 1
    if usable <= 0:
        return 0.0
    shifted = (1.0 - lag_frac) * x[lag_int: lag_int + usable] + lag_frac * x[lag_int + 1: lag_int + 1 + usable]
    return float(np.dot(x[:usable], shifted))


def _refine_bpm_via_autocorrelation(
    candidate_bpm: float,
    global_onset: np.ndarray,
    hop_time: float,
    search_half_range: int = 4,
    margin_threshold: float = 0.01,
) -> float:
    """Refine BPM by testing nearby integer BPMs via fractional-lag autocorrelation.

    Electronic music is produced at integer BPMs.  This function tests each
    integer BPM within ±search_half_range of the candidate using linear-
    interpolated autocorrelation on the onset envelope.  The candidate's own
    (non-integer) BPM also competes; the highest-scoring value wins.

    Fractional-lag autocorrelation avoids the resolution limit of integer-
    lag autocorrelation (where adjacent BPMs map to the same lag index at
    typical hop sizes).
    """
    if global_onset.size < 16 or candidate_bpm <= 0 or hop_time <= 0:
        return candidate_bpm

    x = global_onset.astype(np.float64)
    x = x - np.mean(x)
    energy = float(np.dot(x, x))
    if energy < 1e-12:
        return candidate_bpm

    base_int = round(candidate_bpm)

    # Score the original (non-integer) candidate.
    candidate_lag = 60.0 / (candidate_bpm * hop_time)
    candidate_score = _fractional_autocorr(x, candidate_lag)

    best_int_bpm = base_int
    best_int_score = -1e30

    # Test nearby integer BPMs.
    for test_bpm in range(base_int - search_half_range, base_int + search_half_range + 1):
        if test_bpm <= 0:
            continue
        lag = 60.0 / (float(test_bpm) * hop_time)
        score = _fractional_autocorr(x, lag)
        if score > best_int_score:
            best_int_score = score
            best_int_bpm = test_bpm

    # Prefer integer unless the non-integer candidate is *significantly*
    # better.  Electronic music is produced at integer BPMs; small
    # autocorrelation advantages for non-integer values are noise.
    if best_int_score > 0:
        if candidate_score <= best_int_score:
            return float(best_int_bpm)
        margin = (candidate_score - best_int_score) / max(abs(candidate_score), 1e-12)
        if margin < margin_threshold:
            return float(best_int_bpm)

    return candidate_bpm


def _normalize_bpm(
    bpm: Optional[float],
    min_bpm: float = 70.0,
    max_bpm: float = 180.0,
) -> Optional[float]:
    if bpm is None or not np.isfinite(bpm) or bpm <= 0.0:
        return None

    lo = max(20.0, min_bpm)
    hi = min(300.0, max_bpm)
    normalized = float(bpm)
    while normalized < lo:
        normalized *= 2.0
    while normalized > hi:
        normalized *= 0.5
    return float(_clamp(normalized, 20.0, 300.0))


def _estimate_madmom_beats(
    audio_path: str,
    min_bpm: float = 55.0,
    max_bpm: float = 215.0,
    ffmpeg_bin: Optional[str] = None,
) -> Tuple[Optional[float], List[float], List[Tuple[float, float]]]:
    """Estimate beat times, BPM, and tempo hypotheses using madmom RNN+DBN.

    Returns (bpm, beat_times, tempo_hypotheses) or (None, [], []) on failure.
    tempo_hypotheses is a list of (bpm, strength) sorted descending by strength.
    """
    if not _HAS_MADMOM:
        return None, [], []

    try:
        import os
        # madmom loads audio via pysoundfile or ffmpeg subprocess.
        # Ensure ffmpeg is on PATH so madmom can decode compressed formats.
        if ffmpeg_bin and os.path.isfile(ffmpeg_bin):
            ffmpeg_dir = os.path.dirname(os.path.abspath(ffmpeg_bin))
            current_path = os.environ.get("PATH", "")
            if ffmpeg_dir not in current_path.split(os.pathsep):
                os.environ["PATH"] = ffmpeg_dir + os.pathsep + current_path

        proc = RNNBeatProcessor()
        activations = proc(audio_path)

        beat_tracker = DBNBeatTrackingProcessor(
            min_bpm=min_bpm,
            max_bpm=max_bpm,
            fps=100,
        )
        raw_beats = beat_tracker.process_offline(activations)

        tempo_proc = TempoEstimationProcessor(
            method=None,
            fps=100,
            histogram_processor=CombFilterTempoHistogramProcessor(
                min_bpm=min_bpm,
                max_bpm=max_bpm,
                fps=100,
            ),
        )
        raw_tempos = tempo_proc(activations)

        # Convert beat times to sorted, deduplicated list.
        beat_times: List[float] = sorted(
            float(t)
            for t in np.asarray(raw_beats, dtype=np.float64)
            if np.isfinite(t) and t >= 0.0
        )
        deduped: List[float] = []
        for t in beat_times:
            if not deduped or abs(t - deduped[-1]) > 1e-4:
                deduped.append(t)
        beat_times = deduped

        # Derive BPM from median inter-beat interval.
        bpm: Optional[float] = None
        if len(beat_times) >= 2:
            intervals = np.diff(np.asarray(beat_times, dtype=np.float64))
            intervals = intervals[intervals > 1e-6]
            if intervals.size > 0:
                bpm = 60.0 / float(np.median(intervals))

        # Convert tempo hypotheses to (bpm, strength) tuples.
        # NOTE: must build tempo_hypotheses before the refinement step below.
        tempo_hypotheses: List[Tuple[float, float]] = []
        if raw_tempos is not None:
            arr = np.atleast_2d(raw_tempos)
            for row_idx in range(arr.shape[0]):
                t_bpm = float(arr[row_idx, 0])
                t_str = float(arr[row_idx, 1]) if arr.shape[1] > 1 else 0.0
                if np.isfinite(t_bpm) and t_bpm > 0.0:
                    tempo_hypotheses.append((t_bpm, t_str))
            tempo_hypotheses.sort(key=lambda x: x[1], reverse=True)

        # Prefer comb-filter tempo over beat-interval median when close.
        # The comb filter operates on the full activation function (global
        # spectral analysis) and is far more stable than median(diff(beats))
        # which suffers from individual beat position jitter in complex
        # rhythms (e.g. DnB breakbeats with 5-15ms per-beat drift).
        if bpm is not None and bpm > 0 and tempo_hypotheses:
            best_hyp_bpm: Optional[float] = None
            best_hyp_strength = 0.0
            for t_bpm, t_strength in tempo_hypotheses:
                if abs(t_bpm - bpm) / bpm < 0.05 and t_strength > best_hyp_strength:
                    best_hyp_bpm = t_bpm
                    best_hyp_strength = t_strength
            if best_hyp_bpm is not None:
                bpm = best_hyp_bpm

        return bpm, beat_times, tempo_hypotheses
    except Exception as exc:
        import sys
        import traceback
        print(f"[bpm] madmom beat estimation failed: {exc}", file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        return None, [], []


def _detect_madmom_downbeat_phase(
    audio_path: str,
    beat_times: Sequence[float],
    beats_per_bar: int = 4,
    ffmpeg_bin: Optional[str] = None,
    detection_config: Optional["AnalysisConfig"] = None,
) -> Optional[int]:
    """Detect downbeat phase offset using madmom's RNN downbeat tracker.

    Returns the phase offset (0 .. beats_per_bar-1) into *beat_times* that
    best matches madmom's downbeat predictions, or None on failure.
    """
    if not _HAS_MADMOM or not audio_path or len(beat_times) < beats_per_bar:
        return None

    try:
        import os

        if ffmpeg_bin and os.path.isfile(ffmpeg_bin):
            ffmpeg_dir = os.path.dirname(os.path.abspath(ffmpeg_bin))
            current_path = os.environ.get("PATH", "")
            if ffmpeg_dir not in current_path.split(os.pathsep):
                os.environ["PATH"] = ffmpeg_dir + os.pathsep + current_path

        proc = RNNDownBeatProcessor()
        activations = proc(audio_path)

        dbn = DBNDownBeatTrackingProcessor(
            beats_per_bar=[beats_per_bar],
            fps=100,
        )
        result = dbn(activations)

        if result is None or len(result) == 0:
            return None

        # result is Nx2: [time, beat_position] where beat_position==1 means downbeat.
        result = np.atleast_2d(result)
        downbeat_times = [
            float(row[0]) for row in result
            if row.shape[0] >= 2 and int(round(row[1])) == 1
        ]

        if not downbeat_times:
            return None

        # Map each madmom downbeat to the nearest beat in our grid and
        # accumulate votes for each phase offset.
        cfg = detection_config
        bt_arr = np.asarray(beat_times, dtype=np.float64)
        tolerance = cfg.downbeat_tolerance if cfg and cfg.downbeat_tolerance is not None else 0.120
        min_agreement = cfg.downbeat_min_agreement if cfg and cfg.downbeat_min_agreement is not None else 0.40
        votes = [0] * beats_per_bar

        for db_time in downbeat_times:
            diffs = np.abs(bt_arr - db_time)
            nearest_idx = int(np.argmin(diffs))
            if diffs[nearest_idx] <= tolerance:
                phase = nearest_idx % beats_per_bar
                votes[phase] += 1

        total_votes = sum(votes)
        if total_votes == 0:
            return None

        best_phase = int(np.argmax(votes))
        best_count = votes[best_phase]

        if best_count / total_votes < min_agreement:
            return None

        import sys
        print(
            f"[bpm] madmom downbeat phase: {best_phase} "
            f"(votes={votes}, total={total_votes})",
            file=sys.stderr,
        )
        return best_phase

    except Exception as exc:
        import sys
        print(f"[bpm] madmom downbeat detection failed: {exc}", file=sys.stderr)
        return None


def _score_octave_hypotheses(
    base_bpm: float,
    beat_times: List[float],
    kick_events: Sequence[ScoredEvent],
    snare_events: Sequence[ScoredEvent],
    global_onset: np.ndarray,
    hop_time: float,
    duration_sec: float,
    min_bpm: float = 55.0,
    max_bpm: float = 215.0,
    tempo_hypotheses: Optional[List[Tuple[float, float]]] = None,
    detection_config: Optional["AnalysisConfig"] = None,
) -> Tuple[float, List[float]]:
    """Score octave-related BPM hypotheses and return the best BPM with adjusted beat times.

    Tests base_bpm * {0.5, 1.0, 2.0} against kick/snare onsets and autocorrelation.
    Returns (resolved_bpm, resolved_beat_times).
    """
    if base_bpm is None or base_bpm <= 0.0 or duration_sec <= 0.0:
        return (base_bpm or 120.0), list(beat_times)

    # Build candidate list filtered to [min_bpm, max_bpm].
    factors = [0.5, 1.0, 2.0]
    candidates: List[Tuple[float, float]] = []  # (bpm, factor)
    for f in factors:
        c = base_bpm * f
        if min_bpm <= c <= max_bpm:
            candidates.append((c, f))

    if not candidates:
        return base_bpm, list(beat_times)
    if len(candidates) == 1:
        bpm_out, factor = candidates[0]
        return bpm_out, _adjust_beat_times(beat_times, factor, global_onset, hop_time)

    has_tempo_prior = tempo_hypotheses is not None and len(tempo_hypotheses) > 0

    # Weight distribution — overridable via config.
    cfg = detection_config
    if has_tempo_prior:
        w_kick = cfg.octave_kick_weight if cfg and cfg.octave_kick_weight is not None else 0.35
        w_snare = cfg.octave_snare_weight if cfg and cfg.octave_snare_weight is not None else 0.25
        w_onset = cfg.octave_onset_weight if cfg and cfg.octave_onset_weight is not None else 0.20
        w_prior = cfg.octave_prior_weight if cfg and cfg.octave_prior_weight is not None else 0.20
    else:
        w_kick = cfg.octave_kick_weight_no_prior if cfg and cfg.octave_kick_weight_no_prior is not None else 0.43
        w_snare = cfg.octave_snare_weight_no_prior if cfg and cfg.octave_snare_weight_no_prior is not None else 0.31
        w_onset = cfg.octave_onset_weight_no_prior if cfg and cfg.octave_onset_weight_no_prior is not None else 0.26
        w_prior = 0.0

    tol_factor = cfg.octave_tolerance if cfg and cfg.octave_tolerance is not None else 0.15
    tie_break = cfg.octave_tie_break_margin if cfg and cfg.octave_tie_break_margin is not None else 0.05

    kick_times = [float(e.time) for e in kick_events]
    snare_times = [float(e.time) for e in snare_events]

    best_score = -1e9
    best_bpm = base_bpm
    best_factor = 1.0
    scores: List[Tuple[float, float, float]] = []  # (bpm, factor, score)

    for c_bpm, c_factor in candidates:
        spb = 60.0 / c_bpm
        tolerance = spb * tol_factor

        # --- Kick alignment ---
        kick_score = _onset_alignment_score(kick_times, spb, tolerance, duration_sec)

        # --- Snare backbeat ---
        snare_score = _snare_backbeat_score(snare_times, spb, tolerance, duration_sec)

        # --- Onset autocorrelation at candidate SPB lag ---
        onset_score = _onset_autocorrelation_score(global_onset, spb, hop_time)

        # --- Tempo hypothesis prior ---
        prior_score = 0.0
        if has_tempo_prior:
            prior_score = _tempo_prior_score(c_bpm, tempo_hypotheses)

        total = (
            w_kick * kick_score
            + w_snare * snare_score
            + w_onset * onset_score
            + w_prior * prior_score
        )
        scores.append((c_bpm, c_factor, total))

        if total > best_score:
            best_score = total
            best_bpm = c_bpm
            best_factor = c_factor

    # Tie-break: if top two are within tie_break, prefer factor 1.0 (unmodified).
    scores.sort(key=lambda x: x[2], reverse=True)
    if len(scores) >= 2 and (scores[0][2] - scores[1][2]) < tie_break:
        for s_bpm, s_factor, s_score in scores:
            if abs(s_factor - 1.0) < 0.01:
                best_bpm = s_bpm
                best_factor = s_factor
                break

    return best_bpm, _adjust_beat_times(beat_times, best_factor, global_onset, hop_time)


def _onset_alignment_score(
    onset_times: List[float],
    spb: float,
    tolerance: float,
    duration_sec: float,
) -> float:
    """Fraction of onsets that land within tolerance of a beat grid position."""
    if not onset_times or spb <= 0.0:
        return 0.0

    aligned = 0
    for t in onset_times:
        # Distance to nearest beat position.
        phase = (t % spb)
        dist = min(phase, spb - phase)
        if dist <= tolerance:
            aligned += 1

    return aligned / max(1, len(onset_times))


def _snare_backbeat_score(
    snare_times: List[float],
    spb: float,
    tolerance: float,
    duration_sec: float,
) -> float:
    """Score how well snares land on backbeat positions (beats 2 and 4 in 4/4).

    In 4/4 time, the bar period is 4 * spb. Backbeats are at offsets 1*spb and 3*spb.
    """
    if not snare_times or spb <= 0.0:
        return 0.0

    bar_period = 4.0 * spb
    backbeat_offsets = [1.0 * spb, 3.0 * spb]
    backbeat_count = 0

    for t in snare_times:
        phase = t % bar_period
        for offset in backbeat_offsets:
            dist = abs(phase - offset)
            dist = min(dist, bar_period - dist)
            if dist <= tolerance:
                backbeat_count += 1
                break

    return backbeat_count / max(1, len(snare_times))


def _onset_autocorrelation_score(
    global_onset: np.ndarray,
    spb: float,
    hop_time: float,
) -> float:
    """Normalized autocorrelation at the lag corresponding to the candidate SPB."""
    if global_onset.size < 16 or spb <= 0.0 or hop_time <= 0.0:
        return 0.0

    lag = int(round(spb / hop_time))
    if lag <= 0 or lag >= global_onset.size:
        return 0.0

    x = global_onset.astype(np.float64)
    x = x - np.mean(x)
    norm = float(np.dot(x, x))
    if norm < 1e-12:
        return 0.0

    # Autocorrelation at the specific lag, normalized by zero-lag (energy).
    shifted = np.roll(x, -lag)[: len(x) - lag]
    original = x[: len(x) - lag]
    corr = float(np.dot(original, shifted))
    score = corr / norm

    return float(_clamp(score, 0.0, 1.0))


def _tempo_prior_score(
    candidate_bpm: float,
    tempo_hypotheses: Optional[List[Tuple[float, float]]],
) -> float:
    """Score from madmom's tempo hypotheses. Returns the strength of the closest match."""
    if not tempo_hypotheses:
        return 0.0

    best = 0.0
    for t_bpm, t_strength in tempo_hypotheses:
        # Match if within 3% of candidate.
        if abs(t_bpm - candidate_bpm) / max(1.0, candidate_bpm) < 0.03:
            best = max(best, t_strength)

    return float(_clamp(best, 0.0, 1.0))


def _adjust_beat_times(
    beat_times: List[float],
    factor: float,
    global_onset: np.ndarray,
    hop_time: float,
) -> List[float]:
    """Adjust beat times based on octave factor.

    factor ~2.0: was half-tempo → remove every other beat (keep higher-energy ones).
    factor ~0.5: was double-tempo → insert interpolated midpoints.
    factor ~1.0: unchanged.
    """
    if not beat_times or abs(factor - 1.0) < 0.01:
        return list(beat_times)

    if abs(factor - 2.0) < 0.01:
        # Double BPM: remove every other beat, keeping the higher-onset ones.
        if len(beat_times) < 2:
            return list(beat_times)
        kept: List[float] = []
        for i in range(0, len(beat_times) - 1, 2):
            t_a = beat_times[i]
            t_b = beat_times[i + 1]
            e_a = _onset_energy_at(global_onset, t_a, hop_time)
            e_b = _onset_energy_at(global_onset, t_b, hop_time)
            kept.append(t_a if e_a >= e_b else t_b)
        if len(beat_times) % 2 == 1:
            kept.append(beat_times[-1])
        return kept

    if abs(factor - 0.5) < 0.01:
        # Half BPM: insert midpoints between consecutive beats.
        if len(beat_times) < 2:
            return list(beat_times)
        expanded: List[float] = [beat_times[0]]
        for i in range(1, len(beat_times)):
            midpoint = (beat_times[i - 1] + beat_times[i]) * 0.5
            expanded.append(midpoint)
            expanded.append(beat_times[i])
        return expanded

    return list(beat_times)


def _onset_energy_at(global_onset: np.ndarray, time_sec: float, hop_time: float) -> float:
    """Sample the onset envelope at a given time."""
    if global_onset.size == 0 or hop_time <= 0.0:
        return 0.0
    idx = int(round(time_sec / hop_time))
    if idx < 0:
        idx = 0
    elif idx >= global_onset.size:
        idx = global_onset.size - 1
    return float(global_onset[idx])



def _infer_downbeat_indices(
    beat_times: Sequence[float],
    kick_events: Sequence[ScoredEvent],
    detection_config: Optional["AnalysisConfig"] = None,
) -> List[int]:
    if len(beat_times) < 4:
        return [0] if beat_times else []

    if not kick_events:
        return [i for i in range(0, len(beat_times), 4)]

    cfg = detection_config
    tolerance = cfg.downbeat_tolerance if cfg and cfg.downbeat_tolerance is not None else 0.080
    non_db_weight = cfg.non_downbeat_weight if cfg and cfg.non_downbeat_weight is not None else -0.18
    kick_times = [float(e.time) for e in kick_events]
    kick_strengths = [max(0.0, float(e.confidence)) for e in kick_events]

    best_offset = 0
    best_score = -1e9

    for offset in range(4):
        score = 0.0
        for i, beat_time in enumerate(beat_times):
            target_weight = 1.0 if (i % 4) == offset else non_db_weight
            nearest_score = 0.0
            for k_time, k_strength in zip(kick_times, kick_strengths):
                dt = abs(k_time - beat_time)
                if dt > tolerance:
                    continue
                proximity = 1.0 - (dt / tolerance)
                nearest_score = max(nearest_score, proximity * (0.4 + (0.6 * k_strength)))
            score += target_weight * nearest_score

        if score > best_score:
            best_score = score
            best_offset = offset

    return [i for i in range(best_offset, len(beat_times), 4)]


def _estimate_grid_confidence(
    beat_times: Sequence[float],
    global_onset: np.ndarray,
    hop_time: float,
    kick_events: Sequence[ScoredEvent],
    duration_sec: float,
    detection_config: Optional["AnalysisConfig"] = None,
) -> float:
    if len(beat_times) < 2:
        return 0.0

    intervals = np.diff(np.asarray(beat_times, dtype=np.float64))
    intervals = intervals[intervals > 1e-6]
    if intervals.size == 0:
        return 0.0

    cfg = detection_config
    w_stability = cfg.grid_stability_weight if cfg and cfg.grid_stability_weight is not None else 0.36
    w_onset_align = cfg.grid_onset_align_weight if cfg and cfg.grid_onset_align_weight is not None else 0.25
    w_event_density = cfg.grid_event_density_weight if cfg and cfg.grid_event_density_weight is not None else 0.14
    w_kick_bonus = cfg.grid_kick_bonus_weight if cfg and cfg.grid_kick_bonus_weight is not None else 0.10
    w_edge_coverage = cfg.grid_edge_coverage_weight if cfg and cfg.grid_edge_coverage_weight is not None else 0.15
    rel_var_scale = cfg.grid_rel_var_scale if cfg and cfg.grid_rel_var_scale is not None else 5.5

    median_interval = float(np.median(intervals))
    mad_interval = float(np.median(np.abs(intervals - median_interval)))
    rel_var = _safe_div(mad_interval, median_interval)
    stability = _clamp(1.0 - (rel_var_scale * rel_var), 0.0, 1.0)

    event_density = _clamp(len(beat_times) / 64.0, 0.0, 1.0)

    onset_ref = robust_threshold(global_onset, 1.0) + 1e-6
    local_scores: List[float] = []
    if global_onset.size > 0:
        half_window = 1
        for beat_time in beat_times:
            idx = int(round(beat_time / hop_time))
            local = _local_peak_value(global_onset, idx, half_window)
            local_scores.append(local / onset_ref)
    onset_alignment = _clamp((_safe_div(float(np.mean(local_scores)) if local_scores else 0.0, 1.15)), 0.0, 1.0)

    kick_bonus = 0.0
    if kick_events:
        kick_bonus = _clamp(len(kick_events) / max(1.0, len(beat_times) * 0.35), 0.0, 1.0)

    edge_coverage = 1.0
    if duration_sec > 1e-3:
        first_gap = max(0.0, float(beat_times[0]))
        last_gap = max(0.0, duration_sec - float(beat_times[-1]))
        edge_coverage = _clamp(1.0 - _safe_div(first_gap + last_gap, duration_sec), 0.0, 1.0)

    confidence = (
        (w_stability * stability)
        + (w_onset_align * onset_alignment)
        + (w_event_density * event_density)
        + (w_kick_bonus * kick_bonus)
        + (w_edge_coverage * edge_coverage)
    )
    return float(_clamp(confidence, 0.0, 1.0))


def _build_regular_beat_grid(
    duration_sec: float,
    bpm: Optional[float],
    anchor_sec: float,
) -> List[float]:
    if bpm is None or bpm <= 1e-6 or duration_sec <= 0.0:
        return []

    spb = 60.0 / bpm
    if spb <= 1e-6:
        return []

    start_index = int(math.floor((0.0 - anchor_sec) / spb))
    end_index = int(math.ceil((duration_sec - anchor_sec) / spb))

    beats: List[float] = []
    for idx in range(start_index, end_index + 1):
        t = anchor_sec + (idx * spb)
        if t < -0.06 or t > (duration_sec + 0.06):
            continue
        beats.append(float(max(0.0, min(duration_sec, t))))

    beats.sort()
    deduped: List[float] = []
    for t in beats:
        if not deduped or abs(t - deduped[-1]) > 1e-4:
            deduped.append(t)
    return deduped


def _extend_beat_times_coverage(
    beat_times: Sequence[float],
    duration_sec: float,
    fallback_bpm: Optional[float],
) -> List[float]:
    if len(beat_times) < 2:
        return [float(t) for t in beat_times]

    sorted_beats = sorted(float(t) for t in beat_times if np.isfinite(t) and t >= 0.0)
    if len(sorted_beats) < 2:
        return sorted_beats

    intervals = np.diff(np.asarray(sorted_beats, dtype=np.float64))
    intervals = intervals[intervals > 1e-6]
    if intervals.size == 0:
        return sorted_beats

    interval = float(np.median(intervals))
    if fallback_bpm is not None and fallback_bpm > 1e-6:
        spb = 60.0 / fallback_bpm
        # Keep interval in a musically plausible octave around SPB.
        while interval > (spb * 1.8):
            interval *= 0.5
        while interval < (spb * 0.55):
            interval *= 2.0

    if interval <= 1e-6:
        return sorted_beats

    expanded: List[float] = list(sorted_beats)

    while expanded[0] > (interval * 0.5):
        expanded.insert(0, expanded[0] - interval)

    if duration_sec > 0.0:
        while expanded[-1] < (duration_sec - (interval * 0.5)):
            expanded.append(expanded[-1] + interval)

    clipped: List[float] = []
    for t in expanded:
        if t < -0.06 or (duration_sec > 0.0 and t > duration_sec + 0.06):
            continue
        tt = max(0.0, min(duration_sec, t)) if duration_sec > 0.0 else max(0.0, t)
        if not clipped or abs(tt - clipped[-1]) > 1e-4:
            clipped.append(float(tt))

    return clipped


def _build_beat_grid(
    duration_sec: float,
    bpm: Optional[float],
    global_onset: np.ndarray,
    hop_time: float,
    kick_events: Sequence[ScoredEvent],
    phase_offset_sec: float,
    tracker_beat_times: Sequence[float],
    mode_override: Optional[str] = None,
    min_bpm: float = 70.0,
    max_bpm: float = 180.0,
    madmom_downbeat_phase: Optional[int] = None,
    onset_to_peak_sec: float = 0.0,
    detection_config: Optional["AnalysisConfig"] = None,
) -> Optional[BeatGrid]:
    candidate_bpm = _normalize_bpm(bpm, min_bpm, max_bpm)

    beat_times: List[float] = [float(t) for t in tracker_beat_times if np.isfinite(t) and t >= 0.0]
    mode = mode_override if mode_override else "tracker"

    if len(beat_times) < 2:
        mode = "synthetic"
        anchor_sec = phase_offset_sec
        if kick_events:
            anchor_sec = min(anchor_sec, min(float(e.time) for e in kick_events))
        beat_times = _build_regular_beat_grid(duration_sec=duration_sec, bpm=candidate_bpm, anchor_sec=anchor_sec)

    if len(beat_times) < 2:
        return None

    beat_times = _extend_beat_times_coverage(
        beat_times=beat_times,
        duration_sec=duration_sec,
        fallback_bpm=candidate_bpm,
    )
    if len(beat_times) < 2:
        return None

    intervals = np.diff(np.asarray(beat_times, dtype=np.float64))
    intervals = intervals[intervals > 1e-6]
    if intervals.size > 0:
        bpm_from_grid = _normalize_bpm(60.0 / float(np.median(intervals)), min_bpm, max_bpm)
        if bpm_from_grid is not None:
            # Only override input BPM if grid-derived differs significantly.
            # Small differences (<2%) come from beat position jitter, not
            # actual tempo error — trust the upstream estimate in that case.
            if candidate_bpm is None or abs(bpm_from_grid - candidate_bpm) / max(1.0, candidate_bpm) > 0.02:
                candidate_bpm = bpm_from_grid

    # Prefer madmom's ML-based downbeat phase when available; fall back
    # to the kick-correlation heuristic which can't distinguish beat 1
    # from beat 3 in electronic music.
    if madmom_downbeat_phase is not None and 0 <= madmom_downbeat_phase < 4:
        downbeat_indices = [i for i in range(madmom_downbeat_phase, len(beat_times), 4)]
    else:
        downbeat_indices = _infer_downbeat_indices(beat_times, kick_events, detection_config)
    confidence = _estimate_grid_confidence(
        beat_times=beat_times,
        global_onset=global_onset,
        hop_time=hop_time,
        kick_events=kick_events,
        duration_sec=duration_sec,
        detection_config=detection_config,
    )
    cfg = detection_config
    synth_penalty = cfg.synthetic_grid_penalty if cfg and cfg.synthetic_grid_penalty is not None else 0.72
    if mode == "synthetic":
        confidence *= synth_penalty

    return BeatGrid(
        mode=mode,
        beat_times=[round(float(t), 4) for t in beat_times],
        downbeat_indices=downbeat_indices,
        bpm_derived=candidate_bpm,
        confidence=round(_clamp(confidence, 0.0, 1.0), 4),
        onset_to_peak_sec=round(float(max(0.0, onset_to_peak_sec)), 4),
    )
