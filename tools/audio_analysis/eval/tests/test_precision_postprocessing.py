"""manifold_audio/precision_postprocessing.py tests (P4 §1). Pure-logic /
synthetic-signal tests only — no ADTOF/basic_pitch model inference (same
convention as test_full_pack_baseline.py / test_beat_tracker_alignment.py).
The load-bearing property this module promises is NEUTRAL-DEFAULT
EQUIVALENCE: every knob at its default reproduces the pre-P4 pipeline's
candidate set exactly. Each knob gets an isolation test (default = no-op)
plus at least one test proving it DOES something when turned on."""

from __future__ import annotations

import numpy as np
import pytest

from manifold_audio.precision_postprocessing import (
    BORDERLINE_MARGIN,
    BeatGridRef,
    Candidate,
    MedianAdaptiveConfig,
    PrecisionConfig,
    ShapeGateConfig,
    accept_and_apply_refractory,
    apply_beat_phase_prior,
    apply_cofire_weights,
    apply_shape_gates,
    compute_band_envelope,
    extract_adtof_gate_candidates,
    extract_basic_pitch_gate_candidates,
    median_adaptive_peak_pick,
    phase_alignment_score,
    run_precision_pipeline,
)


def _make_activation(n_frames: int, peak_frames, peak_height: float = 0.9, floor: float = 0.02) -> np.ndarray:
    """A 5-class ADTOF-shaped activation array with clean, isolated peaks at
    the given frame indices in column `col` (kick=0 by default here — tests
    pass whichever column they need via the peak_frames dict)."""
    act = np.full((n_frames, 5), floor, dtype=np.float32)
    for col, frames in peak_frames.items():
        for f in frames:
            act[f, col] = peak_height
    return act


# ---------------------------------------------------------------------------
# (c) median-adaptive ODF baseline — port correctness
# ---------------------------------------------------------------------------


def test_median_adaptive_fires_on_isolated_peaks():
    n = 200
    curve = np.full(n, 0.02, dtype=np.float64)
    for f in (50, 100, 150):
        curve[f] = 1.0
    cfg = MedianAdaptiveConfig(median_window_hops=16, thresh_factor=1.0, thresh_delta=0.05, peak_lookback_hops=8, refractory_hops=5)
    fired = median_adaptive_peak_pick(curve, cfg)
    # Each isolated peak should produce exactly one fire near its frame
    # (the algorithm reports the candidate ONE hop before the current index
    # per the Rust port's lag convention).
    assert len(fired) == 3
    for expected, actual in zip((50, 100, 150), fired):
        assert abs(actual - expected) <= 1


def test_median_adaptive_refractory_suppresses_close_repeats():
    n = 200
    curve = np.full(n, 0.02, dtype=np.float64)
    curve[100] = 1.0
    curve[102] = 1.0  # 2 hops later -- inside a 5-hop refractory
    cfg = MedianAdaptiveConfig(refractory_hops=5, thresh_delta=0.05)
    fired = median_adaptive_peak_pick(curve, cfg)
    assert len(fired) == 1


def test_median_adaptive_empty_or_short_input_returns_no_fires():
    assert median_adaptive_peak_pick(np.array([]), MedianAdaptiveConfig()) == []
    assert median_adaptive_peak_pick(np.array([0.5]), MedianAdaptiveConfig(median_window_hops=16)) == []


# ---------------------------------------------------------------------------
# Stage 1 gate extraction + neutral-default equivalence
# ---------------------------------------------------------------------------


def test_gate_extraction_default_config_matches_shipped_adtof_defaults():
    """Neutral-default equivalence, empirically: at PrecisionConfig()'s
    default kick threshold (0.12, == adtof_detection._DEFAULT_THRESHOLDS[0]),
    running the gate pass + full pipeline at all-default knobs must produce
    EXACTLY the peaks NotePeakPickingProcessor(threshold=0.12) would (today's
    shipped behavior), even though the gate pass itself runs at a LOWER
    threshold (0.12 * BORDERLINE_MARGIN) to surface borderline candidates."""
    from adtof_pytorch.post_processing import NotePeakPickingProcessor

    n_frames = 500
    activations = _make_activation(n_frames, {0: [50, 150, 250, 350]}, peak_height=0.9, floor=0.02)
    # Add a genuinely borderline (below-threshold, above-gate) blip that
    # must NOT survive at default config.
    activations[420, 0] = 0.10  # below threshold 0.12, above gate 0.12*0.6=0.072

    config = PrecisionConfig()
    fps = 100

    gate_candidates = extract_adtof_gate_candidates(activations, fps, "kick", config)
    result = run_precision_pipeline({"kick": gate_candidates}, config)

    reference_proc = NotePeakPickingProcessor(threshold=config.thresholds["kick"], pre_avg=0.1, post_avg=0.01, pre_max=0.02, post_max=0.01, combine=0.02, fps=fps)
    reference_peaks = [t for t, _ in reference_proc.process(activations[:, 0])]

    assert result["kick"] == pytest.approx(reference_peaks)
    # The borderline blip must be present in the gate pass (proves the gate
    # margin genuinely admits it) but absent from the final accepted set.
    assert any(abs(c.time_sec - 4.20) < 0.02 for c in gate_candidates)
    assert not any(abs(t - 4.20) < 0.02 for t in result["kick"])


def test_gate_margin_constant_is_below_one():
    # Sanity: BORDERLINE_MARGIN must genuinely gate LOWER than the accept
    # threshold, or there is no borderline tier at all.
    assert 0.0 < BORDERLINE_MARGIN < 1.0


def test_basic_pitch_gate_candidates_tiers_by_threshold():
    notes = [(1.0, 1.2, 60, 0.8), (2.0, 2.1, 62, 0.3)]
    config = PrecisionConfig()  # synth threshold default 0.5
    cands = extract_basic_pitch_gate_candidates(notes, config)
    assert cands[0].tier == "core"
    assert cands[1].tier == "borderline"


# ---------------------------------------------------------------------------
# (d) co-fire weights
# ---------------------------------------------------------------------------


def test_cofire_default_weights_are_a_no_op():
    config = PrecisionConfig()
    kick = [Candidate(1.000, 0.5, 0.5, "kick", "core")]
    snare = [Candidate(1.005, 0.5, 0.5, "snare", "core")]  # 5ms away -> co-fired
    out = apply_cofire_weights({"kick": kick, "snare": snare}, config)
    assert out["kick"][0].adjusted_confidence == 0.5
    assert out["snare"][0].adjusted_confidence == 0.5


def test_cofire_boosts_snare_only_when_another_class_is_nearby():
    config = PrecisionConfig(cofire_boost_snare=2.0)
    snare_cofired = [Candidate(1.000, 0.3, 0.3, "snare", "borderline")]
    snare_solo = [Candidate(5.000, 0.3, 0.3, "snare", "borderline")]
    kick = [Candidate(1.010, 0.5, 0.5, "kick", "core")]  # 10ms from snare_cofired, within 20ms window

    out = apply_cofire_weights({"kick": kick, "snare": snare_cofired + snare_solo}, config)
    boosted = [c for c in out["snare"] if abs(c.time_sec - 1.0) < 1e-6][0]
    unboosted = [c for c in out["snare"] if abs(c.time_sec - 5.0) < 1e-6][0]
    assert boosted.adjusted_confidence == pytest.approx(0.6)
    assert unboosted.adjusted_confidence == pytest.approx(0.3)


def test_cofire_boosts_solo_kick_and_hat_only_when_isolated():
    config = PrecisionConfig(cofire_solo_boost={"kick": 1.5, "hat": 1.5})
    kick_solo = [Candidate(10.0, 0.4, 0.4, "kick", "borderline")]
    kick_cofired = [Candidate(1.0, 0.4, 0.4, "kick", "borderline")]
    snare = [Candidate(1.005, 0.5, 0.5, "snare", "core")]

    out = apply_cofire_weights({"kick": kick_solo + kick_cofired, "snare": snare}, config)
    solo = [c for c in out["kick"] if abs(c.time_sec - 10.0) < 1e-6][0]
    cofired = [c for c in out["kick"] if abs(c.time_sec - 1.0) < 1e-6][0]
    assert solo.adjusted_confidence == pytest.approx(0.6)
    assert cofired.adjusted_confidence == pytest.approx(0.4)


def test_cofire_promotion_can_cross_the_accept_threshold():
    config = PrecisionConfig(cofire_solo_boost={"kick": 1.3})
    config.thresholds["kick"] = 0.5
    kick = [Candidate(10.0, 0.4, 0.4, "kick", "borderline")]  # 0.4*1.3 = 0.52 > 0.5
    out = apply_cofire_weights({"kick": kick}, config)
    assert out["kick"][0].tier == "core"
    assert out["kick"][0].adjusted_confidence > config.thresholds["kick"]


# ---------------------------------------------------------------------------
# (e) signal-shape validation gates
# ---------------------------------------------------------------------------


def _tone_burst(sr: int, freq: float, dur_sec: float, start_sec: float, total_sec: float) -> np.ndarray:
    n_total = int(round(total_sec * sr))
    audio = np.zeros(n_total, dtype=np.float32)
    n_burst = int(round(dur_sec * sr))
    t = np.arange(n_burst) / sr
    burst = np.sin(2 * np.pi * freq * t).astype(np.float32)
    start = int(round(start_sec * sr))
    end = min(n_total, start + n_burst)
    audio[start:end] = burst[: end - start]
    return audio


def test_shape_gate_default_disabled_is_a_no_op():
    config = PrecisionConfig()  # shape_gates default enabled=False
    sr = 44100
    audio = _tone_burst(sr, 60.0, 0.1, 1.0, 3.0)  # low-band tone at t=1.0 (kick-band)
    kick = [Candidate(1.0, 0.5, 0.5, "kick", "core")]
    out = apply_shape_gates({"kick": kick}, audio, sr, config)
    assert out["kick"][0].shape_gate_passed is True


def test_kick_shape_gate_passes_real_low_band_energy_and_rejects_silence():
    config = PrecisionConfig()
    config.shape_gates["kick"] = ShapeGateConfig(enabled=True, floor=0.5)
    sr = 44100
    # Two low-band bursts (so p99 normalization has more than one sample to
    # work with) plus a candidate sitting in silence.
    audio = _tone_burst(sr, 60.0, 0.1, 1.0, 5.0)
    audio = audio + _tone_burst(sr, 60.0, 0.1, 3.0, 5.0)
    candidates = [
        Candidate(1.0, 0.5, 0.5, "kick", "core"),   # on a real low-band burst
        Candidate(4.5, 0.5, 0.5, "kick", "core"),   # in silence
    ]
    out = apply_shape_gates({"kick": candidates}, audio, sr, config)
    on_burst = [c for c in out["kick"] if abs(c.time_sec - 1.0) < 0.05][0]
    in_silence = [c for c in out["kick"] if abs(c.time_sec - 4.5) < 0.05][0]
    assert on_burst.shape_gate_passed is True
    assert in_silence.shape_gate_passed is False


def test_snare_shape_gate_uses_mid_band():
    config = PrecisionConfig()
    config.shape_gates["snare"] = ShapeGateConfig(enabled=True, floor=0.5)
    sr = 44100
    audio = _tone_burst(sr, 800.0, 0.05, 1.0, 3.0) + _tone_burst(sr, 800.0, 0.05, 2.0, 3.0)
    candidates = [Candidate(1.0, 0.5, 0.5, "snare", "core"), Candidate(2.8, 0.5, 0.5, "snare", "core")]
    out = apply_shape_gates({"snare": candidates}, audio, sr, config)
    on_burst = [c for c in out["snare"] if abs(c.time_sec - 1.0) < 0.05][0]
    quiet = [c for c in out["snare"] if abs(c.time_sec - 2.8) < 0.05][0]
    assert on_burst.shape_gate_passed is True
    assert quiet.shape_gate_passed is False


def test_hat_busyness_ceiling_rejects_local_burst_above_baseline():
    config = PrecisionConfig()
    config.shape_gates["hat"] = ShapeGateConfig(enabled=True, ceiling=2.0)
    sr = 44100
    # Steady low-level high-band hiss for the baseline, plus one much louder
    # burst near t=2.0 that should read far above its own local median.
    rng = np.random.default_rng(0)
    audio = (rng.standard_normal(5 * sr) * 0.01).astype(np.float32)
    audio += _tone_burst(sr, 8000.0, 0.05, 2.0, 5.0) * 5.0
    candidates = [Candidate(1.0, 0.5, 0.5, "hat", "core"), Candidate(2.0, 0.5, 0.5, "hat", "core")]
    out = apply_shape_gates({"hat": candidates}, audio, sr, config)
    baseline = [c for c in out["hat"] if abs(c.time_sec - 1.0) < 0.05][0]
    burst = [c for c in out["hat"] if abs(c.time_sec - 2.0) < 0.05][0]
    assert baseline.shape_gate_passed is True
    assert burst.shape_gate_passed is False


def test_compute_band_envelope_degenerate_band_returns_zeros_not_raises():
    sr = 8000  # low sample rate -> hat band (5-12kHz) partially exceeds Nyquist
    audio = np.zeros(sr * 2, dtype=np.float32)
    times, env = compute_band_envelope(audio, sr, 5000.0, 12000.0)
    assert times.size == env.size
    # Should not raise; degenerate band returns whatever numeric envelope
    # scipy produces for a clamped band, or zeros -- either way finite.
    assert np.all(np.isfinite(env))


# ---------------------------------------------------------------------------
# (f) soft beat-phase prior
# ---------------------------------------------------------------------------


def test_beat_phase_score_peaks_on_grid_and_troughs_at_midpoint():
    grid = BeatGridRef(bpm=120.0, anchor_sec=0.0)  # beat spacing 0.5s, 16th = 0.125s at subdivision 4
    assert phase_alignment_score(0.0, grid, subdivision=4) == pytest.approx(1.0)
    assert phase_alignment_score(0.125, grid, subdivision=4) == pytest.approx(1.0)
    assert phase_alignment_score(0.0625, grid, subdivision=4) == pytest.approx(0.0, abs=1e-6)


def test_beat_phase_default_strength_is_a_no_op():
    config = PrecisionConfig()  # beat_phase_strength defaults all 0.0
    grid = BeatGridRef(bpm=120.0)
    kick = [Candidate(0.0, 0.3, 0.3, "kick", "borderline")]
    out = apply_beat_phase_prior({"kick": kick}, grid, config)
    assert out["kick"][0].adjusted_confidence == 0.3


def test_beat_phase_nudge_can_promote_aligned_borderline_candidate():
    config = PrecisionConfig()
    config.thresholds["kick"] = 0.4
    config.beat_phase_strength["kick"] = 0.2
    grid = BeatGridRef(bpm=120.0, anchor_sec=0.0)
    on_grid = [Candidate(0.0, 0.35, 0.35, "kick", "borderline")]      # exactly on grid -> +0.2
    off_grid = [Candidate(0.0625, 0.35, 0.35, "kick", "borderline")]  # exact midpoint -> +0.0

    out_on = apply_beat_phase_prior({"kick": on_grid}, grid, config)
    out_off = apply_beat_phase_prior({"kick": off_grid}, grid, config)

    assert out_on["kick"][0].adjusted_confidence == pytest.approx(0.55)
    assert out_on["kick"][0].tier == "core"
    assert out_off["kick"][0].adjusted_confidence == pytest.approx(0.35)
    assert out_off["kick"][0].tier == "borderline"


def test_beat_phase_none_grid_is_a_no_op():
    config = PrecisionConfig()
    config.beat_phase_strength["kick"] = 0.5
    kick = [Candidate(0.0, 0.3, 0.3, "kick", "borderline")]
    out = apply_beat_phase_prior({"kick": kick}, None, config)
    assert out["kick"][0].adjusted_confidence == 0.3


# ---------------------------------------------------------------------------
# (b) refractory + accept
# ---------------------------------------------------------------------------


def test_refractory_default_zero_keeps_all_accepted_candidates():
    config = PrecisionConfig()
    cands = [
        Candidate(1.000, 0.5, 0.5, "kick", "core"),
        Candidate(1.005, 0.6, 0.6, "kick", "core"),  # 5ms apart -- both survive at refractory=0
    ]
    out = accept_and_apply_refractory(cands, "kick", config)
    assert out == [1.000, 1.005]


def test_refractory_merges_close_candidates_keeping_higher_confidence():
    config = PrecisionConfig()
    config.refractory_ms["kick"] = 30.0
    cands = [
        Candidate(1.000, 0.5, 0.5, "kick", "core"),
        Candidate(1.010, 0.8, 0.8, "kick", "core"),  # 10ms apart, inside 30ms refractory
        Candidate(2.000, 0.4, 0.4, "kick", "core"),  # far away, unaffected
    ]
    out = accept_and_apply_refractory(cands, "kick", config)
    assert out == [1.010, 2.000]


def test_accept_drops_below_threshold_and_failed_shape_gate():
    config = PrecisionConfig()
    config.thresholds["kick"] = 0.5
    cands = [
        Candidate(1.0, 0.6, 0.6, "kick", "core", shape_gate_passed=True),
        Candidate(2.0, 0.6, 0.6, "kick", "core", shape_gate_passed=False),  # shape gate killed it
        Candidate(3.0, 0.3, 0.3, "kick", "borderline", shape_gate_passed=True),  # below threshold
    ]
    out = accept_and_apply_refractory(cands, "kick", config)
    assert out == [1.0]


# ---------------------------------------------------------------------------
# Full pipeline neutral-default equivalence (all knobs default at once)
# ---------------------------------------------------------------------------


def test_full_pipeline_all_defaults_reproduces_gate_pass_core_tier_only():
    config = PrecisionConfig()
    n_frames = 300
    activations = _make_activation(n_frames, {0: [50, 150, 250]}, peak_height=0.9, floor=0.02)
    gate = extract_adtof_gate_candidates(activations, 100, "kick", config)
    result = run_precision_pipeline({"kick": gate}, config, audio=None, sr=None, grid=None)

    core_times = sorted(c.time_sec for c in gate if c.tier == "core")
    assert result["kick"] == pytest.approx(core_times)
