"""Value types and data containers for the percussion analysis pipeline."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple

import numpy as np



@dataclass(frozen=True)
class Peak:
    time_sec: float
    strength: float


@dataclass(frozen=True)
class Event:
    type: str
    time: float
    confidence: float
    duration_sec: Optional[float] = None


@dataclass(frozen=True)
class BeatGrid:
    mode: str
    beat_times: List[float]
    downbeat_indices: List[int]
    bpm_derived: Optional[float]
    confidence: float
    onset_to_peak_sec: float = 0.0


@dataclass(frozen=True)
class DetectionProfile:
    name: str
    bands_hz: Dict[str, Tuple[float, float]]
    hat_weight: float
    perc_weight: float
    kick_hat_exclusion_window_sec: float = 0.020
    snare_hat_exclusion_window_sec: float = 0.010
    snare_perc_window_sec: float = 0.05
    snare_perc_snare_dominance_ratio: float = 1.15
    snare_perc_perc_dominance_ratio: float = 1.05


@dataclass(frozen=True)
class FrameAnalysis:
    band_energy: Dict[str, np.ndarray]
    band_onsets: Dict[str, np.ndarray]
    centroid_norm: np.ndarray
    rolloff85_norm: np.ndarray
    flatness: np.ndarray
    rms: np.ndarray


@dataclass(frozen=True)
class ScoredEvent:
    type: str
    time: float
    confidence: float


@dataclass(frozen=True)
class DetectionMetrics:
    candidate_count: int
    classified_count: int
    ambiguous_count: int
    mean_margin: float
    kick_snare_overlap_rate: float
    snare_perc_overlap_rate: float
    pre_filter_counts: Dict[str, int]
    post_filter_counts: Dict[str, int]


@dataclass(frozen=True)
class AnalysisConfig:
    """External config loaded from JSON. All Optional — None means use hardcoded default."""
    # drum
    kick_band_hz: Optional[Tuple[float, float]] = None
    snare_band_hz: Optional[Tuple[float, float]] = None
    snare_hat_suppression: Optional[float] = None
    snare_perc_window: Optional[float] = None
    snare_perc_snare_dominance: Optional[float] = None
    snare_perc_perc_dominance: Optional[float] = None
    hat_band_hz: Optional[Tuple[float, float]] = None
    hat_weight: Optional[float] = None
    perc_band_hz: Optional[Tuple[float, float]] = None
    perc_weight: Optional[float] = None
    kick_hat_exclusion: Optional[float] = None
    # vocal
    vocal_chest_band_hz: Optional[Tuple[float, float]] = None
    vocal_formant_band_hz: Optional[Tuple[float, float]] = None
    vocal_presence_band_hz: Optional[Tuple[float, float]] = None
    vocal_chest_weight: Optional[float] = None
    vocal_formant_weight: Optional[float] = None
    vocal_presence_weight: Optional[float] = None
    # algorithm — madmom onset
    madmom_threshold: Optional[float] = None
    madmom_combine: Optional[float] = None
    madmom_pre_max: Optional[float] = None
    madmom_post_max: Optional[float] = None
    # algorithm — peak detection
    adaptive_window_sec: Optional[float] = None
    # algorithm — local loudness normalization
    local_norm_window: Optional[float] = None
    # algorithm — BPM refinement
    autocorr_search_half_range: Optional[int] = None
    autocorr_margin_threshold: Optional[float] = None
    # algorithm — octave scoring
    octave_kick_weight: Optional[float] = None
    octave_snare_weight: Optional[float] = None
    octave_onset_weight: Optional[float] = None
    octave_prior_weight: Optional[float] = None
    octave_kick_weight_no_prior: Optional[float] = None
    octave_snare_weight_no_prior: Optional[float] = None
    octave_onset_weight_no_prior: Optional[float] = None
    octave_tolerance: Optional[float] = None
    octave_tie_break_margin: Optional[float] = None
    # algorithm — grid confidence
    grid_stability_weight: Optional[float] = None
    grid_onset_align_weight: Optional[float] = None
    grid_event_density_weight: Optional[float] = None
    grid_kick_bonus_weight: Optional[float] = None
    grid_edge_coverage_weight: Optional[float] = None
    grid_rel_var_scale: Optional[float] = None
    synthetic_grid_penalty: Optional[float] = None
    # algorithm — downbeat
    downbeat_tolerance: Optional[float] = None
    downbeat_min_agreement: Optional[float] = None
    non_downbeat_weight: Optional[float] = None
    # ADTOF drum transcription thresholds
    adtof_kick_threshold: Optional[float] = None
    adtof_snare_threshold: Optional[float] = None
    adtof_hihat_threshold: Optional[float] = None
    adtof_tom_threshold: Optional[float] = None
    adtof_cymbal_threshold: Optional[float] = None
    # Basic Pitch — Bass
    bp_bass_onset_threshold: Optional[float] = None
    bp_bass_frame_threshold: Optional[float] = None
    bp_bass_min_note_length: Optional[float] = None
    bp_bass_min_frequency: Optional[float] = None
    bp_bass_max_frequency: Optional[float] = None
    bp_bass_min_energy_db: Optional[float] = None
    # Basic Pitch — Synth/Pad
    bp_synth_onset_threshold: Optional[float] = None
    bp_synth_frame_threshold: Optional[float] = None
    bp_synth_min_note_length: Optional[float] = None
    bp_synth_min_frequency: Optional[float] = None
    bp_synth_max_frequency: Optional[float] = None
    bp_synth_min_energy_db: Optional[float] = None
    # Bass duration split (stab vs sustained)
    bass_duration_threshold_sec: Optional[float] = None

    @staticmethod
    def from_json(data: dict) -> "AnalysisConfig":
        """Build from the JSON dict written by C#. Missing keys produce None."""
        def _band(d: dict, key: str) -> Optional[Tuple[float, float]]:
            v = d.get(key)
            if isinstance(v, (list, tuple)) and len(v) == 2:
                return (float(v[0]), float(v[1]))
            return None

        def _f(d: dict, key: str) -> Optional[float]:
            v = d.get(key)
            return float(v) if v is not None else None

        def _i(d: dict, key: str) -> Optional[int]:
            v = d.get(key)
            return int(v) if v is not None else None

        drum = data.get("drum", {})
        kick = drum.get("kick", {})
        snare = drum.get("snare", {})
        hat = drum.get("hat", {})
        perc = drum.get("perc", {})
        snare_pc = snare.get("percConflict", {})

        bass = data.get("bass", {})

        vocal = data.get("vocal", {})
        vocal_bands = vocal.get("bands", {})
        vocal_weights = vocal.get("weights", {})

        alg = data.get("algorithm", {})

        return AnalysisConfig(
            kick_band_hz=_band(kick, "bandHz"),
            snare_band_hz=_band(snare, "bandHz"),
            snare_hat_suppression=_f(snare, "hatSuppression"),
            snare_perc_window=_f(snare_pc, "window"),
            snare_perc_snare_dominance=_f(snare_pc, "snareDominance"),
            snare_perc_perc_dominance=_f(snare_pc, "percDominance"),
            hat_band_hz=_band(hat, "bandHz"),
            hat_weight=_f(hat, "weight"),
            perc_band_hz=_band(perc, "bandHz"),
            perc_weight=_f(perc, "weight"),
            kick_hat_exclusion=_f(drum, "kickHatExclusion"),
            vocal_chest_band_hz=_band(vocal_bands, "chest"),
            vocal_formant_band_hz=_band(vocal_bands, "formant"),
            vocal_presence_band_hz=_band(vocal_bands, "presence"),
            vocal_chest_weight=_f(vocal_weights, "chest"),
            vocal_formant_weight=_f(vocal_weights, "formant"),
            vocal_presence_weight=_f(vocal_weights, "presence"),
            madmom_threshold=_f(alg, "madmomThreshold"),
            madmom_combine=_f(alg, "madmomCombine"),
            madmom_pre_max=_f(alg, "madmomPreMax"),
            madmom_post_max=_f(alg, "madmomPostMax"),
            adaptive_window_sec=_f(alg, "adaptiveWindowSec"),
            local_norm_window=_f(alg, "localNormWindow"),
            autocorr_search_half_range=_i(alg, "autocorrSearchHalfRange"),
            autocorr_margin_threshold=_f(alg, "autocorrMarginThreshold"),
            octave_kick_weight=_f(alg, "octaveKickWeight"),
            octave_snare_weight=_f(alg, "octaveSnareWeight"),
            octave_onset_weight=_f(alg, "octaveOnsetWeight"),
            octave_prior_weight=_f(alg, "octavePriorWeight"),
            octave_kick_weight_no_prior=_f(alg, "octaveKickWeightNoPrior"),
            octave_snare_weight_no_prior=_f(alg, "octaveSnareWeightNoPrior"),
            octave_onset_weight_no_prior=_f(alg, "octaveOnsetWeightNoPrior"),
            octave_tolerance=_f(alg, "octaveTolerance"),
            octave_tie_break_margin=_f(alg, "octaveTieBreakMargin"),
            grid_stability_weight=_f(alg, "gridStabilityWeight"),
            grid_onset_align_weight=_f(alg, "gridOnsetAlignWeight"),
            grid_event_density_weight=_f(alg, "gridEventDensityWeight"),
            grid_kick_bonus_weight=_f(alg, "gridKickBonusWeight"),
            grid_edge_coverage_weight=_f(alg, "gridEdgeCoverageWeight"),
            grid_rel_var_scale=_f(alg, "gridRelVarScale"),
            synthetic_grid_penalty=_f(alg, "syntheticGridPenalty"),
            downbeat_tolerance=_f(alg, "downbeatTolerance"),
            downbeat_min_agreement=_f(alg, "downbeatMinAgreement"),
            non_downbeat_weight=_f(alg, "nonDownbeatWeight"),
            adtof_kick_threshold=_f(alg, "adtofKickThreshold"),
            adtof_snare_threshold=_f(alg, "adtofSnareThreshold"),
            adtof_hihat_threshold=_f(alg, "adtofHihatThreshold"),
            adtof_tom_threshold=_f(alg, "adtofTomThreshold"),
            adtof_cymbal_threshold=_f(alg, "adtofCymbalThreshold"),
            bp_bass_onset_threshold=_f(alg, "bpBassOnsetThreshold"),
            bp_bass_frame_threshold=_f(alg, "bpBassFrameThreshold"),
            bp_bass_min_note_length=_f(alg, "bpBassMinNoteLength"),
            bp_bass_min_frequency=_f(alg, "bpBassMinFrequency"),
            bp_bass_max_frequency=_f(alg, "bpBassMaxFrequency"),
            bp_bass_min_energy_db=_f(alg, "bpBassMinEnergyDb"),
            bp_synth_onset_threshold=_f(alg, "bpSynthOnsetThreshold"),
            bp_synth_frame_threshold=_f(alg, "bpSynthFrameThreshold"),
            bp_synth_min_note_length=_f(alg, "bpSynthMinNoteLength"),
            bp_synth_min_frequency=_f(alg, "bpSynthMinFrequency"),
            bp_synth_max_frequency=_f(alg, "bpSynthMaxFrequency"),
            bp_synth_min_energy_db=_f(alg, "bpSynthMinEnergyDb"),
            bass_duration_threshold_sec=_f(bass, "durationThresholdSec"),
        )
