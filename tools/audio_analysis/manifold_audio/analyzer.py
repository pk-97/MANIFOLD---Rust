"""Main analysis orchestrator and JSON output builder."""

from __future__ import annotations

import sys
from concurrent.futures import Future, ProcessPoolExecutor
from typing import Callable, Dict, FrozenSet, List, Optional, Sequence, Tuple

# (progress_01, message) — wired to emit_progress in cli.py.
ProgressCallback = Optional[Callable[[float, str], None]]

import numpy as np

from manifold_audio.bpm import (
    _build_beat_grid,
    _detect_madmom_downbeat_phase,
    _estimate_madmom_beats,
    _refine_bpm_via_autocorrelation,
    _score_octave_hypotheses,
    estimate_bpm,
)
from manifold_audio.conflict_resolution import (
    _count_event_types,
    _overlap_rate,
)
from manifold_audio.adtof_detection import detect_drums_adtof
from manifold_audio.basic_pitch_detection import (
    classify_synth_notes,
    detect_notes_basic_pitch,
    split_bass_by_duration,
)
from manifold_audio.gestures import analyze_vocal_gestures
from manifold_audio.math_utils import _clamp
from manifold_audio.models import (
    AnalysisConfig,
    BeatGrid,
    DetectionMetrics,
    Event,
    ScoredEvent,
)
from manifold_audio.onset_detection import detect_madmom_onsets
from manifold_audio.profiles import _pick_profile
from manifold_audio.spectral import compute_band_onsets


def analyze_percussion(
    audio: np.ndarray,
    sample_rate: int,
    frame_size: int,
    hop_size: int,
    profile_name: str,
    emit_bass: bool = False,
    bass_audio: Optional[np.ndarray] = None,
    synth_audio: Optional[np.ndarray] = None,
    vocal_audio: Optional[np.ndarray] = None,
    bass_profile_name: str = "auto",
    bass_sub_weight: Optional[float] = None,
    bass_body_weight: Optional[float] = None,
    bass_bite_weight: Optional[float] = None,
    audio_path: Optional[str] = None,
    analysis_audio_path: Optional[str] = None,
    bass_audio_path: Optional[str] = None,
    synth_audio_path: Optional[str] = None,
    vocal_audio_path: Optional[str] = None,
    min_bpm: float = 55.0,
    max_bpm: float = 215.0,
    ffmpeg_bin: Optional[str] = None,
    on_progress: ProgressCallback = None,
    detection_config: Optional[AnalysisConfig] = None,
    instruments: Optional[FrozenSet[str]] = None,
) -> Tuple[List[Event], Optional[float], float, Optional[BeatGrid], str, Optional[str], DetectionMetrics, List[float]]:
    def _progress(value: float, message: str) -> None:
        if on_progress is not None:
            on_progress(value, message)

    hop_time = hop_size / float(sample_rate)

    # --- Parallel madmom dispatch ---
    # madmom neural network passes are CPU-bound (numpy, no GIL release).
    # Submit all independent passes to a ProcessPoolExecutor so they run
    # concurrently across cores instead of sequentially.
    onset_audio_path = analysis_audio_path or audio_path

    # Instrument-group guards — None means all instruments requested (backward-compatible).
    want_drums = instruments is None or "drums" in instruments
    want_bass = instruments is None or "bass" in instruments
    want_synth = instruments is None or "synth" in instruments or "pad" in instruments
    want_vocal = instruments is None or "vocal" in instruments

    need_vocal_onsets = want_vocal and vocal_audio is not None and vocal_audio_path is not None

    # --- ML model detection (ADTOF + Basic Pitch) ---
    # These run in the main process before the madmom pool because they load
    # PyTorch/TF models that are expensive to fork into subprocesses.
    adtof_drum_events: Optional[List[Event]] = None
    bp_bass_notes = None
    bp_synth_notes = None

    # Build ADTOF thresholds from config.
    adtof_thresholds: Optional[Dict[str, float]] = None
    if detection_config is not None:
        _at: Dict[str, float] = {}
        if detection_config.adtof_kick_threshold is not None:
            _at["kick"] = detection_config.adtof_kick_threshold
        if detection_config.adtof_snare_threshold is not None:
            _at["snare"] = detection_config.adtof_snare_threshold
        if detection_config.adtof_hihat_threshold is not None:
            _at["hihat"] = detection_config.adtof_hihat_threshold
        if detection_config.adtof_tom_threshold is not None:
            _at["tom"] = detection_config.adtof_tom_threshold
        if detection_config.adtof_cymbal_threshold is not None:
            _at["cymbal"] = detection_config.adtof_cymbal_threshold
        if _at:
            adtof_thresholds = _at

    # Build per-stem Basic Pitch kwargs from config.
    bp_bass_kwargs: Dict[str, object] = {}
    bp_synth_kwargs: Dict[str, object] = {}
    if detection_config is not None:
        # Bass stem — lower thresholds, tight frequency band.
        if detection_config.bp_bass_onset_threshold is not None:
            bp_bass_kwargs["onset_threshold"] = detection_config.bp_bass_onset_threshold
        if detection_config.bp_bass_frame_threshold is not None:
            bp_bass_kwargs["frame_threshold"] = detection_config.bp_bass_frame_threshold
        if detection_config.bp_bass_min_note_length is not None:
            bp_bass_kwargs["min_note_length"] = detection_config.bp_bass_min_note_length
        if detection_config.bp_bass_min_frequency is not None and detection_config.bp_bass_min_frequency > 0:
            bp_bass_kwargs["min_frequency"] = detection_config.bp_bass_min_frequency
        if detection_config.bp_bass_max_frequency is not None and detection_config.bp_bass_max_frequency > 0:
            bp_bass_kwargs["max_frequency"] = detection_config.bp_bass_max_frequency
        if detection_config.bp_bass_min_energy_db is not None and detection_config.bp_bass_min_energy_db < 0:
            bp_bass_kwargs["min_energy_db"] = detection_config.bp_bass_min_energy_db
        # Synth/pad stem — wider frequency range.
        if detection_config.bp_synth_onset_threshold is not None:
            bp_synth_kwargs["onset_threshold"] = detection_config.bp_synth_onset_threshold
        if detection_config.bp_synth_frame_threshold is not None:
            bp_synth_kwargs["frame_threshold"] = detection_config.bp_synth_frame_threshold
        if detection_config.bp_synth_min_note_length is not None:
            bp_synth_kwargs["min_note_length"] = detection_config.bp_synth_min_note_length
        if detection_config.bp_synth_min_frequency is not None and detection_config.bp_synth_min_frequency > 0:
            bp_synth_kwargs["min_frequency"] = detection_config.bp_synth_min_frequency
        if detection_config.bp_synth_max_frequency is not None and detection_config.bp_synth_max_frequency > 0:
            bp_synth_kwargs["max_frequency"] = detection_config.bp_synth_max_frequency
        if detection_config.bp_synth_min_energy_db is not None and detection_config.bp_synth_min_energy_db < 0:
            bp_synth_kwargs["min_energy_db"] = detection_config.bp_synth_min_energy_db

    # ADTOF drum transcription (runs in main process).
    if want_drums and onset_audio_path is not None:
        _progress(0.74, "drum transcription (ADTOF)")
        adtof_drum_events = detect_drums_adtof(onset_audio_path, thresholds=adtof_thresholds)
        if adtof_drum_events is not None:
            _progress(0.76, f"ADTOF detected {len(adtof_drum_events)} drum events")

    # Basic Pitch for bass stem.
    if want_bass and emit_bass and bass_audio_path is not None:
        _progress(0.77, f"pitch detection bass (Basic Pitch) on: {bass_audio_path}")
        bp_bass_notes = detect_notes_basic_pitch(bass_audio_path, **bp_bass_kwargs)
        if bp_bass_notes is not None:
            _progress(0.78, f"Basic Pitch detected {len(bp_bass_notes)} bass notes")

    # Basic Pitch for synth/pad (other stem).
    if want_synth and synth_audio_path is not None:
        _progress(0.79, "pitch detection synth/pad (Basic Pitch)")
        bp_synth_notes = detect_notes_basic_pitch(synth_audio_path, **bp_synth_kwargs)
        if bp_synth_notes is not None:
            _progress(0.80, f"Basic Pitch detected {len(bp_synth_notes)} synth/pad notes")

    # --- Madmom parallel dispatch (beats + vocal) ---
    # ADTOF/Basic Pitch handle drums/bass/synth/pad; madmom is only
    # needed for beat tracking and vocal onsets.
    madmom_pass_count = 0
    if audio_path is not None:
        madmom_pass_count += 1  # beat tracking
    if need_vocal_onsets:
        madmom_pass_count += 1

    rough_bpm: Optional[float] = None
    tracker_beat_times: List[float] = []
    madmom_tempo_hypotheses: List[Tuple[float, float]] = []
    beat_source = "none"
    madmom_downbeat_phase: Optional[int] = None
    vocal_madmom_result = None

    # Build a descriptive label for the parallel phase.
    stem_labels: List[str] = []
    if audio_path is not None:
        stem_labels.append("beats")
    if need_vocal_onsets:
        stem_labels.append("vocals")

    # Incremental progress per future completion within the 0.80-0.85 range.
    total_futures = max(1, madmom_pass_count)
    completed_futures = 0

    def _future_done(label: str) -> None:
        nonlocal completed_futures
        completed_futures += 1
        frac = completed_futures / total_futures
        _progress(0.80 + frac * 0.05, f"madmom analysis ({label} done, {completed_futures}/{total_futures})")

    # Build madmom keyword overrides from config (only include keys with non-None values).
    madmom_kwargs: Dict[str, object] = {}
    if detection_config is not None:
        if detection_config.madmom_threshold is not None:
            madmom_kwargs["threshold"] = detection_config.madmom_threshold
        if detection_config.madmom_combine is not None:
            madmom_kwargs["combine"] = detection_config.madmom_combine
        if detection_config.madmom_pre_max is not None:
            madmom_kwargs["pre_max"] = detection_config.madmom_pre_max
        if detection_config.madmom_post_max is not None:
            madmom_kwargs["post_max"] = detection_config.madmom_post_max

    if madmom_pass_count > 1:
        # Multiple independent madmom passes — run in parallel.
        _progress(0.80, f"analysing {' + '.join(stem_labels)} in parallel ({madmom_pass_count} passes)")
        workers = min(6, madmom_pass_count + 1)  # +1 for dependent downbeat pass
        with ProcessPoolExecutor(max_workers=workers) as pool:
            beat_future: Optional[Future] = None
            vocal_onset_future: Optional[Future] = None

            # Submit all independent passes.
            if audio_path is not None:
                beat_future = pool.submit(
                    _estimate_madmom_beats,
                    audio_path=audio_path,
                    min_bpm=min_bpm,
                    max_bpm=max_bpm,
                    ffmpeg_bin=ffmpeg_bin,
                )
            if need_vocal_onsets:
                vocal_onset_future = pool.submit(
                    detect_madmom_onsets,
                    audio_path=vocal_audio_path,
                    method="cnn",
                    ffmpeg_bin=ffmpeg_bin,
                    **madmom_kwargs,
                )

            # Collect beat tracking first — downbeat detection depends on it.
            downbeat_future: Optional[Future] = None
            if beat_future is not None:
                try:
                    madmom_bpm, madmom_beats, madmom_tempo_hypotheses = beat_future.result()
                    _future_done("beats")
                    if madmom_bpm is not None and len(madmom_beats) >= 2:
                        rough_bpm = madmom_bpm
                        tracker_beat_times = madmom_beats
                        beat_source = "madmom"
                        # Submit dependent downbeat pass (other futures still running).
                        downbeat_future = pool.submit(
                            _detect_madmom_downbeat_phase,
                            audio_path=audio_path,
                            beat_times=madmom_beats,
                            beats_per_bar=4,
                            ffmpeg_bin=ffmpeg_bin,
                        )
                except Exception as exc:
                    print(f"[parallel] beat tracking failed: {exc}", file=sys.stderr)

            # Collect remaining futures.
            if vocal_onset_future is not None:
                try:
                    vocal_madmom_result = vocal_onset_future.result()
                    _future_done("vocals")
                except Exception as exc:
                    print(f"[parallel] vocal onset detection failed: {exc}", file=sys.stderr)
            if downbeat_future is not None:
                try:
                    madmom_downbeat_phase = downbeat_future.result()
                except Exception as exc:
                    print(f"[parallel] downbeat detection failed: {exc}", file=sys.stderr)
    else:
        # Single pass or no madmom — run sequentially (no pool overhead).
        if audio_path is not None:
            _progress(0.80, "tracking beats (RNN)")
            madmom_bpm, madmom_beats, madmom_tempo_hypotheses = _estimate_madmom_beats(
                audio_path=audio_path,
                min_bpm=min_bpm,
                max_bpm=max_bpm,
                ffmpeg_bin=ffmpeg_bin,
            )
            if madmom_bpm is not None and len(madmom_beats) >= 2:
                rough_bpm = madmom_bpm
                tracker_beat_times = madmom_beats
                beat_source = "madmom"
                _progress(0.82, "detecting downbeats")
                madmom_downbeat_phase = _detect_madmom_downbeat_phase(
                    audio_path=audio_path,
                    beat_times=madmom_beats,
                    beats_per_bar=4,
                    ffmpeg_bin=ffmpeg_bin,
                )

    _progress(0.85, "processing drum events")

    # --- Two-tier BPM cascade: autocorrelation fallback ---
    if beat_source == "none":
        rough_bands = {
            "kick": (30.0, 180.0),
            "snare": (180.0, 2800.0),
            "hat": (4200.0, 15000.0),
            "perc": (900.0, 8500.0),
        }
        _norm_w = detection_config.local_norm_window if detection_config and detection_config.local_norm_window is not None else 3.0
        rough_onsets, _, _ = compute_band_onsets(audio, sample_rate, frame_size, hop_size, rough_bands, norm_window_sec=_norm_w)
        rough_global = rough_onsets["kick"] + rough_onsets["snare"] + (0.5 * rough_onsets["hat"]) + (
            0.5 * rough_onsets["perc"]
        )
        rough_bpm = estimate_bpm(rough_global, hop_time, min_bpm=min_bpm, max_bpm=max_bpm)
        beat_source = "autocorrelation"

    # Profile and onsets are always computed — global_onset is required for
    # BPM octave scoring and beat grid regardless of which instruments are requested.
    profile = _pick_profile(profile_name, detection_config)
    _norm_w = detection_config.local_norm_window if detection_config and detection_config.local_norm_window is not None else 3.0
    onsets, raw_rms, _ = compute_band_onsets(audio, sample_rate, frame_size, hop_size, profile.bands_hz, norm_window_sec=_norm_w)

    # Compute weighted global onset envelope (used for octave scoring and beat grid).
    global_onset = (
        onsets["kick"]
        + onsets["snare"]
        + (profile.hat_weight * onsets["hat"])
        + (profile.perc_weight * onsets["perc"])
    )

    # --- ADTOF drum events ---
    drum_events: List[Event] = []
    kick_scored_events: List[ScoredEvent] = []
    snare_scored_events: List[ScoredEvent] = []

    if want_drums and adtof_drum_events is not None:
        drum_events = list(adtof_drum_events)
        drum_events.sort(key=lambda e: (e.time, e.type))

        kick_scored_events = [
            ScoredEvent(type="kick", time=e.time, confidence=e.confidence)
            for e in drum_events if e.type == "kick"
        ]
        snare_scored_events = [
            ScoredEvent(type="snare", time=e.time, confidence=e.confidence)
            for e in drum_events if e.type == "snare"
        ]

    # --- Multi-hypothesis octave scoring ---
    if rough_bpm is not None and rough_bpm > 0:
        rough_bpm, tracker_beat_times = _score_octave_hypotheses(
            base_bpm=rough_bpm,
            beat_times=tracker_beat_times,
            kick_events=kick_scored_events,
            snare_events=snare_scored_events,
            global_onset=global_onset,
            hop_time=hop_time,
            duration_sec=float(len(audio)) / float(sample_rate),
            min_bpm=min_bpm,
            max_bpm=max_bpm,
            tempo_hypotheses=madmom_tempo_hypotheses if beat_source == "madmom" else None,
            detection_config=detection_config,
        )

    # --- Autocorrelation refinement + integer snap ---
    # Narrow autocorrelation search around the octave-resolved BPM corrects
    # model bias (madmom), then integer snap exploits the fact that
    # electronic music is produced at whole-number tempos.
    if rough_bpm is not None and rough_bpm > 0:
        cfg = detection_config
        rough_bpm = _refine_bpm_via_autocorrelation(
            candidate_bpm=rough_bpm,
            global_onset=global_onset,
            hop_time=hop_time,
            search_half_range=cfg.autocorr_search_half_range if cfg and cfg.autocorr_search_half_range is not None else 4,
            margin_threshold=cfg.autocorr_margin_threshold if cfg and cfg.autocorr_margin_threshold is not None else 0.01,
        )

    bpm = rough_bpm

    # --- Post-BPM conflict resolution ---
    drum_scored: List[ScoredEvent] = [
        ScoredEvent(type=e.type, time=e.time, confidence=e.confidence)
        for e in drum_events
    ]
    drum_scored.sort(key=lambda e: (e.time, e.type))
    pre_filter_counts = _count_event_types(drum_scored)

    # ADTOF handles multi-class assignment internally — no priority masks needed.
    post_filter_counts = dict(pre_filter_counts)

    _progress(0.87, "resolving event conflicts")

    events: List[Event] = list(drum_events)
    bass_profile_used = None

    if want_bass and emit_bass and bp_bass_notes is not None:
        _bass_dur_thresh = (detection_config.bass_duration_threshold_sec
                            if detection_config and detection_config.bass_duration_threshold_sec is not None
                            else 1.7144)
        bass_short, bass_sustained = split_bass_by_duration(
            bp_bass_notes, threshold_sec=_bass_dur_thresh,
        )
        bass_profile_used = "basic_pitch"
        events.extend(bass_short)
        events.extend(bass_sustained)

    if want_synth and bp_synth_notes is not None:
        bp_synth_events, bp_pad_events = classify_synth_notes(bp_synth_notes)
        if instruments is None or "synth" in instruments:
            events.extend(bp_synth_events)
        if instruments is None or "pad" in instruments:
            events.extend(bp_pad_events)

    if want_vocal and vocal_audio is not None:
        vocal_events = analyze_vocal_gestures(
            audio=vocal_audio,
            sample_rate=sample_rate,
            frame_size=frame_size,
            hop_size=hop_size,
            min_confidence=0.0,
            audio_path=vocal_audio_path,
            ffmpeg_bin=ffmpeg_bin,
            precomputed_madmom_onsets=vocal_madmom_result,
            detection_config=detection_config,
        )
        events.extend(vocal_events)

    metrics = DetectionMetrics(
        candidate_count=len(drum_events),
        classified_count=len(drum_events),
        ambiguous_count=0,
        mean_margin=0.0,
        kick_snare_overlap_rate=round(_overlap_rate(drum_scored, "kick", "snare", 0.06), 4),
        snare_perc_overlap_rate=round(
            _overlap_rate(drum_scored, "snare", "perc", profile.snare_perc_window_sec), 4
        ),
        pre_filter_counts=pre_filter_counts,
        post_filter_counts=post_filter_counts,
    )

    _progress(0.89, "building beat grid")

    phase_offset_sec = 0.0
    beat_grid = _build_beat_grid(
        duration_sec=float(len(audio)) / float(sample_rate),
        bpm=bpm,
        global_onset=global_onset,
        hop_time=hop_time,
        kick_events=kick_scored_events,
        phase_offset_sec=phase_offset_sec,
        tracker_beat_times=tracker_beat_times,
        mode_override=beat_source if beat_source not in ("autocorrelation", "none") else None,
        min_bpm=min_bpm,
        max_bpm=max_bpm,
        madmom_downbeat_phase=madmom_downbeat_phase,
        detection_config=detection_config,
    )

    bpm_confidence = 0.0
    if beat_grid is not None:
        bpm_confidence = float(_clamp(beat_grid.confidence, 0.0, 1.0))
        if beat_grid.bpm_derived is not None and beat_grid.bpm_derived > 0:
            bpm = beat_grid.bpm_derived
    elif bpm is not None:
        bpm_confidence = 0.35

    # Half-frame latency correction: the STFT onset function at frame i
    # reflects spectral content centered at (i*hop + frame_size/2) but the
    # peak detector reports time as i*hop (the window start).  Shift all
    # event times forward by half a frame to align with the true transient.
    half_frame_sec = frame_size / (2.0 * sample_rate)
    events = [
        Event(type=e.type, time=round(e.time + half_frame_sec, 4), confidence=e.confidence, duration_sec=e.duration_sec)
        for e in events
    ]

    events.sort(key=lambda e: (e.time, e.type))

    energy_envelope: List[float] = []
    if beat_grid is not None and beat_grid.beat_times and raw_rms is not None and raw_rms.size > 0:
        energy_envelope = _compute_energy_envelope(raw_rms, beat_grid.beat_times, hop_size, sample_rate)

    return events, bpm, bpm_confidence, beat_grid, profile.name, bass_profile_used, metrics, energy_envelope


def _compute_energy_envelope(
    rms: np.ndarray,
    beat_times: List[float],
    hop_size: int,
    sample_rate: int,
) -> List[float]:
    """Downsample per-frame RMS to one value per beat using windowed averaging.

    For each beat, averages RMS frames in a window from the previous beat to the
    next beat. Peak-normalizes the result to [0, 1].
    """
    if len(beat_times) < 2 or rms.size == 0:
        return []

    hop_time = hop_size / float(sample_rate)
    n_beats = len(beat_times)
    beat_energy: List[float] = []

    for i in range(n_beats):
        t_start = beat_times[i - 1] if i > 0 else 0.0
        t_end = beat_times[i + 1] if i < n_beats - 1 else beat_times[-1] + (beat_times[-1] - beat_times[-2])
        f_start = max(0, int(t_start / hop_time))
        f_end = min(rms.size - 1, int(t_end / hop_time))
        if f_end >= f_start:
            beat_energy.append(float(np.mean(rms[f_start:f_end + 1])))
        else:
            beat_energy.append(0.0)

    max_val = max(beat_energy) if beat_energy else 0.0
    if max_val > 1e-8:
        beat_energy = [v / max_val for v in beat_energy]

    return beat_energy


def _event_to_dict(e: Event) -> Dict[str, object]:
    d: Dict[str, object] = {
        "type": e.type,
        "time": round(e.time, 4),
        "confidence": round(e.confidence, 4),
    }
    if e.duration_sec is not None:
        d["durationSeconds"] = round(e.duration_sec, 4)
    return d


def build_output(
    track_id: str,
    bpm: Optional[float],
    bpm_confidence: float,
    beat_grid: Optional[BeatGrid],
    events: Sequence[Event],
    energy_envelope: Optional[List[float]] = None,
) -> Dict[str, object]:
    payload: Dict[str, object] = {
        "trackId": track_id,
        "bpm": round(float(bpm), 3) if bpm is not None else 0.0,
        "bpmConfidence": round(float(_clamp(bpm_confidence, 0.0, 1.0)), 4),
        "events": [
            _event_to_dict(e)
            for e in events
        ],
    }

    if beat_grid is not None and beat_grid.beat_times:
        payload["beatGrid"] = {
            "mode": beat_grid.mode,
            "bpmDerived": round(float(beat_grid.bpm_derived), 3) if beat_grid.bpm_derived is not None else 0.0,
            "confidence": round(float(_clamp(beat_grid.confidence, 0.0, 1.0)), 4),
            "beatTimes": [round(float(t), 4) for t in beat_grid.beat_times],
            "downbeatIndices": [int(i) for i in beat_grid.downbeat_indices if i >= 0],
            "onsetToPeakSeconds": round(float(beat_grid.onset_to_peak_sec), 4),
        }

    if energy_envelope and len(energy_envelope) > 0:
        payload["energyEnvelope"] = {
            "resolution": "beat",
            "values": [round(float(v), 4) for v in energy_envelope],
        }

    return payload
