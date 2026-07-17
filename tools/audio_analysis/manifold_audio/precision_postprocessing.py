"""Shared precision post-processing module (P4 §1,
docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §4.2/D6/D9/D11) — applied to the
ADTOF drum path (kick/snare/hat/perc) and the basic_pitch synth path,
per-class. This is a PARAMETER LAYER, not a new model: every knob OTHER
than (a) has a neutral default (refractory=0, cofire weights=1.0,
beat-phase strength=0.0, shape gates disabled, median-adaptive disabled)
that is a structural no-op regardless of the threshold value in force (see
the "neutral path" tests in eval/tests/test_precision_postprocessing.py).

Threshold defaults (knob a) were updated 2026-07-18 to the P4 precision-
pass ACCEPTED values (orchestrator heldout acceptance read,
eval/scoreboard/p4_heldout_acceptance.json) — kick/snare/hat threshold_factor
1.15/1.3/0.5 against the pre-P4 baseline; perc/synth unchanged at 1.0x. So
`PrecisionConfig()` today reproduces the CURRENT shipped pipeline
(manifold_audio.adtof_detection._DEFAULT_THRESHOLDS, same five numbers),
not the historical pre-P4 pipeline — the pre-P4 equivalence is still
provable, just by passing the old threshold values explicitly (see
test_gate_extraction_reproduces_note_peak_picking_at_any_threshold), since
the neutral-path property (gate+accept collapsing to plain threshold
picking when every other knob is off) never depended on which specific
number the threshold held.

Pipeline shape (per class)
---------------------------
1. GATE pass — extract a SUPERSET of candidates at `threshold * BORDERLINE_MARGIN`
   (a fixed, documented architectural constant, not a swept knob — see
   BORDERLINE_MARGIN below) using the SAME extraction machinery the shipped
   pipeline already uses (adtof_pytorch's NotePeakPickingProcessor for
   drums, basic_pitch's own onset/amplitude for synth), OR the ported
   median-adaptive algorithm when knob (c) is enabled for that class. Each
   candidate carries its raw confidence.
2. REFINE — per-class knobs (d) co-fire weight and (f) beat-phase prior
   ADJUST confidence (never remove a candidate by themselves); (e) signal-
   shape gates REMOVE a candidate outright (hard gate, as named in the
   design doc — "validation gate", unlike (f)'s explicitly soft "nudge,
   never veto").
3. ACCEPT — a candidate survives iff its (possibly adjusted) confidence
   clears knob (a)'s per-class threshold. Candidates already at or above
   threshold before any adjustment ("core") always clear this; only
   "borderline" gate-only candidates depend on (d)/(f) to cross the line.
4. REFRACTORY — knob (b): same-class dedup, keeping the higher-confidence
   candidate within the window. Default 0ms (no additional suppression
   beyond each extraction pass's own built-in merge).

Neutral path, why it holds at ANY threshold
---------------------------------------------
At every OTHER knob's default (co-fire weights = 1.0, beat-phase strength
= 0.0, signal-shape gates disabled, refractory = 0ms, median-adaptive
disabled): the REFINE step is a no-op (multiply-by-1, add-zero, gates that
never fire), so no borderline candidate is ever promoted over its threshold
and no core candidate is ever removed — the ACCEPT step reduces to exactly
"confidence >= threshold", whatever that threshold's numeric value is. This
holds independent of knob (a)'s value, which is WHY updating the shipped
threshold defaults (2026-07-18, see above) doesn't touch this property —
verified structurally (the code path), and empirically against synthetic
activation curves at both the current defaults and the historical pre-P4
values (see test module).

Class scope, per the design doc's own §1 brief (not this module's choice)
----------------------------------------------------------------------------
  (a) per-class threshold         — kick, snare, hat, perc, synth
  (b) per-class refractory        — kick, snare, hat, perc, synth
  (c) median-adaptive ODF baseline — kick, snare, hat, perc (ADTOF path only;
                                      basic_pitch/synth is not an ODF detector)
  (d) co-fire weights             — kick (solo boost), hat (solo boost),
                                      snare (co-fire boost) — perc/synth
                                      untouched by this knob per the brief's
                                      own wording ("boost co-fired snares;
                                      boost SOLO kicks and solo hats")
  (e) signal-shape validation gate — kick (low-band floor), snare (mid-band
                                      floor), hat (busyness ceiling) — perc/
                                      synth have no gate defined in the brief
  (f) soft beat-phase prior       — kick, snare, perc only (brief's own scope)
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Sequence, Tuple

import numpy as np

# Gate-pass margin below the accept threshold, expressed as a fraction of
# the threshold itself (gate_threshold = threshold * BORDERLINE_MARGIN).
# Fixed and documented here (not a 7th tunable knob) — it exists purely so
# (d)/(f) have sub-threshold candidates available to promote; at the
# default knob values nothing gets promoted (see module docstring), so this
# constant's exact value does not affect neutral-default equivalence. 0.6 is
# wide enough that a real co-fire/beat-phase boost (sweep range, P4 §2) can
# plausibly cross the gap without admitting so many candidates the gate
# pass becomes a second, uncontrolled detector.
BORDERLINE_MARGIN = 0.6

ADTOF_TUNABLE_CLASSES = ("kick", "snare", "hat", "perc")
COFIRE_CLASSES = ("kick", "snare", "hat")
SHAPE_GATE_CLASSES = ("kick", "snare", "hat")
BEAT_PHASE_CLASSES = ("kick", "snare", "perc")


@dataclass(frozen=True)
class Candidate:
    """One candidate onset before/through the refine pipeline."""

    time_sec: float
    raw_confidence: float
    adjusted_confidence: float
    class_name: str
    tier: str  # "core" (>= threshold pre-adjustment) or "borderline" (gate-only)
    shape_gate_passed: bool = True


@dataclass
class MedianAdaptiveConfig:
    """Knob (c) — port of crates/manifold-audio/src/analysis.rs:1779-1865's
    median-adaptive ODF baseline + peak-pick (rolling median history ->
    threshold = median*factor+delta -> local-max turnover test ->
    refractory). Disabled by default (False) — the shipped ADTOF path
    (NotePeakPickingProcessor via adtof_pytorch) is used instead, which IS
    today's behavior."""

    enabled: bool = False
    median_window_hops: int = 16  # ODF_MEDIAN_HOPS in the Rust source
    thresh_factor: float = 1.0
    thresh_delta: float = 0.02
    peak_lookback_hops: int = 8
    refractory_hops: int = 5


@dataclass
class ShapeGateConfig:
    """Knob (e) — signal-shape validation gate. `enabled=False` (default)
    never rejects a candidate regardless of `floor`/`ceiling` (both are only
    consulted when enabled=True, so their default numeric values are inert
    until a sweep turns the gate on)."""

    enabled: bool = False
    floor: float = 0.5     # kick/snare: normalized band presence must be >= floor
    ceiling: float = 3.0   # hat: normalized local busyness must be <= ceiling


@dataclass
class PrecisionConfig:
    """The full P4 §1 knob set for one track's multi-class candidate
    refinement. Every knob other than (a) defaults to a structural no-op
    (see module docstring's "neutral path" section); (a)'s defaults are the
    CURRENT SHIPPED thresholds (post P4 acceptance, 2026-07-18)."""

    # (a) per-class activation thresholds. ADTOF classes default to the
    # existing manifold_audio.adtof_detection._DEFAULT_THRESHOLDS (kick/
    # snare/hat/perc order there is [kick, snare, tom(perc), hihat(hat), cymbal(hat)]
    # -- this module works in MANIFOLD class-name space, not ADTOF label
    # order, so kick/snare/perc map 1:1 and hat is the max of ADTOF's two
    # hihat+cymbal thresholds' effective floor). These are the P4 precision-
    # pass ACCEPTED values (orchestrator heldout acceptance read,
    # eval/scoreboard/p4_heldout_acceptance.json), not the historical pre-P4
    # numbers -- kick 0.12->0.138 (factor 1.15), snare 0.14->0.182 (factor
    # 1.3), hat 0.18->0.09 (factor 0.5); perc/synth unchanged.
    thresholds: Dict[str, float] = field(default_factory=lambda: {
        "kick": 0.138, "snare": 0.182, "hat": 0.09, "perc": 0.14, "synth": 0.5,
    })
    # (b) per-class refractory windows, milliseconds. 0 = no additional
    # suppression (each extraction pass already merges near-duplicates
    # internally at its own fixed "combine" window).
    refractory_ms: Dict[str, float] = field(default_factory=lambda: {
        "kick": 0.0, "snare": 0.0, "hat": 0.0, "perc": 0.0, "synth": 0.0,
    })
    # (c) median-adaptive ODF baseline, per ADTOF-tunable class.
    median_adaptive: Dict[str, MedianAdaptiveConfig] = field(default_factory=lambda: {
        c: MedianAdaptiveConfig() for c in ADTOF_TUNABLE_CLASSES
    })
    # (d) co-fire weights. window_ms is shared (design doc: "window ±20ms").
    # solo_boost multiplies kick/hat confidence when NO other-class
    # candidate fires within the window; cofire_boost multiplies snare
    # confidence when one DOES. Both default 1.0 (no-op).
    cofire_window_ms: float = 20.0
    cofire_solo_boost: Dict[str, float] = field(default_factory=lambda: {"kick": 1.0, "hat": 1.0})
    cofire_boost_snare: float = 1.0
    # (e) signal-shape validation gates, per class.
    shape_gates: Dict[str, ShapeGateConfig] = field(default_factory=lambda: {
        c: ShapeGateConfig() for c in SHAPE_GATE_CLASSES
    })
    # (f) soft beat-phase prior. strength=0.0 -> zero nudge regardless of
    # phase alignment (never a veto, per the design doc's own wording).
    beat_phase_strength: Dict[str, float] = field(default_factory=lambda: {
        c: 0.0 for c in BEAT_PHASE_CLASSES
    })
    beat_phase_subdivision: int = 4  # 16th notes at this subdivision of a beat


# ---------------------------------------------------------------------------
# (c) Median-adaptive ODF baseline — port of analysis.rs:1779-1865
# ---------------------------------------------------------------------------


def median_adaptive_peak_pick(
    activation: np.ndarray,
    cfg: MedianAdaptiveConfig,
) -> List[int]:
    """Direct port of the live kick/transient detector's rolling-median
    adaptive baseline + local-maximum peak-pick + refractory
    (crates/manifold-audio/src/analysis.rs, SendState.odf_hist /
    the `bf.transients` block, lines ~1779-1865). `activation` is one
    class's per-frame curve (e.g. ADTOF's raw sigmoid output at 100fps).

    Faithful to the Rust structure: history ring of `median_window_hops`
    values; once full, the CANDIDATE is the second-to-last value (one hop
    of lag, matching the Rust `hist[ODF_MEDIAN_HOPS - 1]` read BEFORE the
    current hop is pushed); it fires when it is a local maximum over the
    lookback window, has turned over (current <= candidate), clears
    `median*thresh_factor + thresh_delta`, and the refractory has elapsed.
    Returns frame indices (not seconds) of every fire."""

    n = len(activation)
    w = cfg.median_window_hops
    if n == 0 or w < 2:
        return []

    hist = np.zeros(w, dtype=np.float64)
    refractory = 0
    fired_frames: List[int] = []

    for i in range(n):
        current = float(activation[i])
        if i >= w:  # history ring only meaningful once it has filled once
            candidate = hist[w - 1]
            median = float(np.median(hist))
            threshold = median * cfg.thresh_factor + cfg.thresh_delta

            lookback_lo = max(0, w - 1 - cfg.peak_lookback_hops)
            past_max = float(np.max(hist[lookback_lo:w - 1])) if lookback_lo < w - 1 else 0.0
            is_peak = candidate >= past_max and current <= candidate

            if is_peak and refractory == 0 and candidate > threshold:
                fired_frames.append(i - 1)  # candidate was hist[-1], i.e. one hop back
                refractory = cfg.refractory_hops
            else:
                refractory = max(0, refractory - 1)

        # Push current into the ring (shift left, newest last) — mirrors
        # `h.copy_within(1.., 0); h[ODF_MEDIAN_HOPS - 1] = odf;`.
        hist[:-1] = hist[1:]
        hist[-1] = current

    return fired_frames


# ---------------------------------------------------------------------------
# Stage 1 — candidate extraction (ADTOF classes)
# ---------------------------------------------------------------------------


_ADTOF_CLASS_TO_ACTIVATION_INDEX = {
    # LABELS_5 = [35(kick), 38(snare), 47(perc/tom), 42(hat/hihat), 49(hat/cymbal)]
    "kick": 0,
    "snare": 1,
    "perc": 2,
    # "hat" is fed by TWO activation columns (hihat=3, cymbal=4) in ADTOF's
    # own label set; this module treats them as one "hat" activation via
    # elementwise max, matching adtof_detection._MIDI_TO_TYPE's own
    # many-to-one mapping (both 42 and 49 -> "hat").
}


def _class_activation_curve(activations: np.ndarray, class_name: str) -> np.ndarray:
    if class_name == "hat":
        return np.maximum(activations[:, 3], activations[:, 4])
    idx = _ADTOF_CLASS_TO_ACTIVATION_INDEX[class_name]
    return activations[:, idx]


def extract_adtof_gate_candidates(
    activations: np.ndarray,
    fps: int,
    class_name: str,
    config: PrecisionConfig,
) -> List[Candidate]:
    """Stage 1 for one ADTOF-tunable class: gate-pass candidate extraction.
    Uses the SAME NotePeakPickingProcessor adtof_pytorch already ships
    (identical pre_avg/post_avg/pre_max/post_max/combine constants — only
    the threshold varies) unless knob (c) is enabled for this class, in
    which case the ported median-adaptive algorithm replaces it entirely
    for that class."""
    from adtof_pytorch.post_processing import NotePeakPickingProcessor

    curve = _class_activation_curve(activations, class_name)
    threshold = config.thresholds[class_name]
    gate_threshold = threshold * BORDERLINE_MARGIN

    median_cfg = config.median_adaptive.get(class_name)
    if median_cfg is not None and median_cfg.enabled:
        frames = median_adaptive_peak_pick(curve, median_cfg)
        times_and_conf = [(f / float(fps), float(curve[f])) for f in frames]
        # The ported algorithm has its own internal threshold test
        # (median*factor+delta); it does not separately honor `threshold`/
        # `gate_threshold` — its OWN thresh_factor/thresh_delta are knob (c)'s
        # tunables. Every fire it returns is therefore "core" by construction
        # (it already decided to fire), consistent with (c) being a full
        # replacement of stage 1, not a layer under (a)'s gate/accept split.
        return [
            Candidate(time_sec=t, raw_confidence=c, adjusted_confidence=c, class_name=class_name, tier="core")
            for t, c in times_and_conf
        ]

    proc = NotePeakPickingProcessor(threshold=gate_threshold, pre_avg=0.1, post_avg=0.01, pre_max=0.02, post_max=0.01, combine=0.02, fps=fps)
    peaks = proc.process(curve)
    out: List[Candidate] = []
    for t, _pitch in peaks:
        frame_idx = max(0, min(int(round(t * fps)), len(curve) - 1))
        conf = float(curve[frame_idx])
        tier = "core" if conf >= threshold else "borderline"
        out.append(Candidate(time_sec=t, raw_confidence=conf, adjusted_confidence=conf, class_name=class_name, tier=tier))
    return out


def extract_basic_pitch_gate_candidates(
    notes: Sequence[Tuple[float, float, int, float]],
    config: PrecisionConfig,
) -> List[Candidate]:
    """Stage 1 for synth: basic_pitch has already produced (start, end,
    pitch, amplitude) notes at ITS OWN onset_threshold (a permissive value
    the caller should set below config.thresholds['synth'] so a real gate
    pass exists — see eval/sweep_p4.py's fixed low onset_threshold for the
    gate pass). Each note's amplitude is its confidence proxy (same
    convention manifold_audio.basic_pitch_detection.classify_synth_notes
    already uses)."""
    threshold = config.thresholds["synth"]
    out: List[Candidate] = []
    for start, _end, _pitch, amp in notes:
        conf = max(0.0, min(1.0, float(amp)))
        tier = "core" if conf >= threshold else "borderline"
        out.append(Candidate(time_sec=float(start), raw_confidence=conf, adjusted_confidence=conf, class_name="synth", tier=tier))
    return out


# ---------------------------------------------------------------------------
# (d) Co-fire weights
# ---------------------------------------------------------------------------


def _has_neighbor_within(t: float, others_sorted: np.ndarray, window_sec: float) -> bool:
    if others_sorted.size == 0:
        return False
    idx = np.searchsorted(others_sorted, t)
    lo = others_sorted[idx - 1] if idx > 0 else None
    hi = others_sorted[idx] if idx < others_sorted.size else None
    if lo is not None and abs(t - lo) <= window_sec:
        return True
    if hi is not None and abs(hi - t) <= window_sec:
        return True
    return False


def apply_cofire_weights(candidates_by_class: Dict[str, List[Candidate]], config: PrecisionConfig) -> Dict[str, List[Candidate]]:
    """Boosts co-fired snares and solo kicks/hats (design doc §1(d) exact
    wording). "Other classes" for a given class = every OTHER key in
    candidates_by_class (kick/snare/hat/perc — synth is never a cofire
    partner, out of scope per the brief)."""
    window_sec = config.cofire_window_ms / 1000.0
    all_times_by_class = {
        cls: np.asarray(sorted(c.time_sec for c in cands), dtype=np.float64)
        for cls, cands in candidates_by_class.items()
    }

    out: Dict[str, List[Candidate]] = {}
    for cls, cands in candidates_by_class.items():
        if cls not in COFIRE_CLASSES:
            out[cls] = cands
            continue
        other_times = np.concatenate([
            arr for other_cls, arr in all_times_by_class.items() if other_cls != cls and other_cls != "synth"
        ]) if len(all_times_by_class) > 1 else np.asarray([], dtype=np.float64)
        other_times.sort()

        new_cands = []
        for cand in cands:
            has_neighbor = _has_neighbor_within(cand.time_sec, other_times, window_sec)
            if cls == "snare":
                weight = config.cofire_boost_snare if has_neighbor else 1.0
            else:  # kick, hat: solo boost
                weight = config.cofire_solo_boost.get(cls, 1.0) if not has_neighbor else 1.0
            adjusted = cand.adjusted_confidence * weight
            tier = "core" if adjusted >= config.thresholds[cls] else cand.tier
            new_cands.append(Candidate(
                time_sec=cand.time_sec, raw_confidence=cand.raw_confidence,
                adjusted_confidence=adjusted, class_name=cls, tier=tier,
                shape_gate_passed=cand.shape_gate_passed,
            ))
        out[cls] = new_cands
    return out


# ---------------------------------------------------------------------------
# (e) Signal-shape validation gates
# ---------------------------------------------------------------------------


def compute_band_envelope(audio: np.ndarray, sr: int, lo_hz: float, hi_hz: float, hop_sec: float = 0.01, win_sec: float = 0.03) -> Tuple[np.ndarray, np.ndarray]:
    """Whole-track band-limited RMS envelope (D6: "whole-track per-stem
    normalization"). Butterworth bandpass (scipy), then a sliding-window
    RMS at hop_sec resolution. Returns (times_sec, envelope) — envelope is
    RAW RMS (not yet normalized); callers normalize by the track's own p99
    (kick/snare floor) or local median (hat busyness), matching each gate's
    own convention."""
    from scipy.signal import butter, sosfiltfilt

    nyq = sr / 2.0
    lo = max(1.0, lo_hz) / nyq
    hi = min(hi_hz, nyq * 0.99) / nyq
    if lo >= hi:
        # Degenerate band (e.g. sr too low) -- return silence rather than raise;
        # callers treat an all-zero envelope as "never passes the floor",
        # which is the conservative (safe) failure direction for a gate.
        n_hops = max(1, int(len(audio) / sr / hop_sec))
        return np.arange(n_hops) * hop_sec, np.zeros(n_hops, dtype=np.float64)

    sos = butter(4, [lo, hi], btype="band", output="sos")
    filtered = sosfiltfilt(sos, audio)

    hop = max(1, int(round(hop_sec * sr)))
    win = max(hop, int(round(win_sec * sr)))
    n_hops = max(1, (len(filtered) - win) // hop + 1)
    times = np.arange(n_hops) * hop_sec
    env = np.empty(n_hops, dtype=np.float64)
    for i in range(n_hops):
        start = i * hop
        frame = filtered[start:start + win]
        env[i] = float(np.sqrt(np.mean(frame.astype(np.float64) ** 2))) if frame.size else 0.0
    return times, env


def _envelope_value_at(times: np.ndarray, env: np.ndarray, t: float) -> float:
    if env.size == 0:
        return 0.0
    idx = int(np.clip(np.searchsorted(times, t), 0, len(env) - 1))
    return float(env[idx])


KICK_BAND_HZ = (30.0, 90.0)    # matches scripts/kick_label_extract.py's own convention (docs precedent)
SNARE_BAND_HZ = (150.0, 3000.0)
HAT_BAND_HZ = (5000.0, 12000.0)


def apply_shape_gates(
    candidates_by_class: Dict[str, List[Candidate]],
    audio: np.ndarray,
    sr: int,
    config: PrecisionConfig,
) -> Dict[str, List[Candidate]]:
    """Kick: low-band presence floor. Snare: mid-band amplitude floor. Hat:
    local busyness ceiling (ratio of the hat-band envelope's local value to
    its own surrounding-window median -- a dense/bursty passage reads far
    above its local baseline, the case ADTOF's hat detector is most prone
    to double-firing on). All normalized per-track (kick/snare: value / p99
    of that track's own band envelope; hat: local value / local median) so
    one floor/ceiling generalizes across tracks of different loudness."""
    out = dict(candidates_by_class)

    for cls, band, use_local_median in (
        ("kick", KICK_BAND_HZ, False),
        ("snare", SNARE_BAND_HZ, False),
        ("hat", HAT_BAND_HZ, True),
    ):
        gate_cfg = config.shape_gates.get(cls)
        if gate_cfg is None or not gate_cfg.enabled or cls not in out or not out[cls]:
            continue
        times, env = compute_band_envelope(audio, sr, band[0], band[1])
        p99 = float(np.percentile(env, 99)) if env.size else 0.0

        new_cands = []
        for cand in out[cls]:
            raw_val = _envelope_value_at(times, env, cand.time_sec)
            if use_local_median:
                # ±0.5s local window for the busyness baseline.
                lo_idx = np.searchsorted(times, cand.time_sec - 0.5)
                hi_idx = np.searchsorted(times, cand.time_sec + 0.5)
                local = env[lo_idx:hi_idx]
                local_median = float(np.median(local)) if local.size else 0.0
                ratio = raw_val / (local_median + 1e-9)
                passed = ratio <= gate_cfg.ceiling
            else:
                normalized = raw_val / (p99 + 1e-9)
                passed = normalized >= gate_cfg.floor
            new_cands.append(Candidate(
                time_sec=cand.time_sec, raw_confidence=cand.raw_confidence,
                adjusted_confidence=cand.adjusted_confidence, class_name=cls,
                tier=cand.tier, shape_gate_passed=passed,
            ))
        out[cls] = new_cands
    return out


# ---------------------------------------------------------------------------
# (f) Soft beat-phase prior
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class BeatGridRef:
    """Minimal grid reference for the beat-phase prior: a constant BPM +
    one anchor (downbeat-aligned) timestamp. Sufficient for dev-fixture
    scoring, where the true BPM/anchor is already known (self_render,
    babyslakh's declared tempo, liveshow's own tempo-map truth) -- a
    production seam would derive this from Beat This (§4.1) instead;
    documented honestly as a round-1 scope choice, not a design claim."""

    bpm: float
    anchor_sec: float = 0.0


def phase_alignment_score(t: float, grid: BeatGridRef, subdivision: int) -> float:
    """1.0 at exact grid alignment, 0.0 at the midpoint between grid
    positions (linear falloff), for a grid spaced at
    (60/bpm)/subdivision seconds relative to grid.anchor_sec."""
    if grid.bpm <= 0:
        return 0.0
    spacing = (60.0 / grid.bpm) / max(1, subdivision)
    rel = (t - grid.anchor_sec) % spacing
    dist = min(rel, spacing - rel)
    return max(0.0, 1.0 - 2.0 * dist / spacing)


def apply_beat_phase_prior(
    candidates_by_class: Dict[str, List[Candidate]],
    grid: Optional[BeatGridRef],
    config: PrecisionConfig,
) -> Dict[str, List[Candidate]]:
    """Nudges (never vetoes) kick/snare/perc confidence toward the beat
    grid. strength=0.0 (default) is an exact no-op regardless of `grid`."""
    if grid is None:
        return candidates_by_class
    out = dict(candidates_by_class)
    for cls in BEAT_PHASE_CLASSES:
        strength = config.beat_phase_strength.get(cls, 0.0)
        if cls not in out or strength == 0.0:
            continue
        new_cands = []
        for cand in out[cls]:
            score = phase_alignment_score(cand.time_sec, grid, config.beat_phase_subdivision)
            adjusted = cand.adjusted_confidence + strength * score
            tier = "core" if adjusted >= config.thresholds[cls] else cand.tier
            new_cands.append(Candidate(
                time_sec=cand.time_sec, raw_confidence=cand.raw_confidence,
                adjusted_confidence=adjusted, class_name=cls, tier=tier,
                shape_gate_passed=cand.shape_gate_passed,
            ))
        out[cls] = new_cands
    return out


# ---------------------------------------------------------------------------
# Accept + (b) refractory
# ---------------------------------------------------------------------------


def accept_and_apply_refractory(cands: List[Candidate], class_name: str, config: PrecisionConfig) -> List[float]:
    """ACCEPT: adjusted_confidence >= threshold AND shape_gate_passed.
    Then (b) REFRACTORY: same-class dedup within refractory_ms, keeping the
    higher-confidence candidate. Returns final event times only (P/R/F1 per
    D10 scores times, not confidence)."""
    threshold = config.thresholds[class_name]
    accepted = [c for c in cands if c.adjusted_confidence >= threshold and c.shape_gate_passed]
    accepted.sort(key=lambda c: c.time_sec)

    refractory_sec = config.refractory_ms.get(class_name, 0.0) / 1000.0
    if refractory_sec <= 0.0 or len(accepted) < 2:
        return [c.time_sec for c in accepted]

    kept: List[Candidate] = []
    for cand in accepted:
        if kept and cand.time_sec - kept[-1].time_sec < refractory_sec:
            if cand.adjusted_confidence > kept[-1].adjusted_confidence:
                kept[-1] = cand
            continue
        kept.append(cand)
    return [c.time_sec for c in kept]


def run_precision_pipeline(
    candidates_by_class: Dict[str, List[Candidate]],
    config: PrecisionConfig,
    audio: Optional[np.ndarray] = None,
    sr: Optional[int] = None,
    grid: Optional[BeatGridRef] = None,
) -> Dict[str, List[float]]:
    """Runs stages 2-4 (refine -> accept -> refractory) over an already-
    gate-extracted candidate set (stage 1's job — see
    extract_adtof_gate_candidates / extract_basic_pitch_gate_candidates).
    Returns final per-class event times."""
    refined = apply_cofire_weights(candidates_by_class, config)
    if audio is not None and sr is not None:
        refined = apply_shape_gates(refined, audio, sr, config)
    refined = apply_beat_phase_prior(refined, grid, config)

    return {cls: accept_and_apply_refractory(cands, cls, config) for cls, cands in refined.items()}
