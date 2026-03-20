"""Detection profile definitions and selection.

Drum band profiles are used for spectral analysis in BPM octave scoring.
Bass/synth/pad profiles removed — replaced by Basic Pitch neural detection.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Dict, Optional

from manifold_audio.models import DetectionProfile

if TYPE_CHECKING:
    from manifold_audio.models import AnalysisConfig


def _build_detection_profiles() -> Dict[str, DetectionProfile]:
    return {
        "electronic": DetectionProfile(
            name="electronic",
            bands_hz={
                "kick": (28.0, 180.0),
                "snare": (180.0, 2800.0),
                "hat": (5000.0, 16000.0),
                "perc": (2800.0, 9000.0),
            },
            hat_weight=0.45,
            perc_weight=0.50,
        ),
    }


def _pick_profile(profile_name: str, config: Optional["AnalysisConfig"] = None) -> DetectionProfile:
    profiles = _build_detection_profiles()
    key = str(profile_name or "").strip().lower()
    profile = profiles.get(key, profiles["electronic"])
    if config is None:
        return profile
    return _apply_config_to_profile(profile, config)


def _apply_config_to_profile(profile: DetectionProfile, config: "AnalysisConfig") -> DetectionProfile:
    """Override DetectionProfile fields from AnalysisConfig. Missing config fields keep profile defaults."""
    bands_hz = dict(profile.bands_hz)
    if config.kick_band_hz is not None:
        bands_hz["kick"] = config.kick_band_hz
    if config.snare_band_hz is not None:
        bands_hz["snare"] = config.snare_band_hz
    if config.hat_band_hz is not None:
        bands_hz["hat"] = config.hat_band_hz
    if config.perc_band_hz is not None:
        bands_hz["perc"] = config.perc_band_hz

    return DetectionProfile(
        name=profile.name,
        bands_hz=bands_hz,
        hat_weight=config.hat_weight if config.hat_weight is not None else profile.hat_weight,
        perc_weight=config.perc_weight if config.perc_weight is not None else profile.perc_weight,
        kick_hat_exclusion_window_sec=config.kick_hat_exclusion if config.kick_hat_exclusion is not None else profile.kick_hat_exclusion_window_sec,
        snare_hat_exclusion_window_sec=config.snare_hat_suppression if config.snare_hat_suppression is not None else profile.snare_hat_exclusion_window_sec,
        snare_perc_window_sec=config.snare_perc_window if config.snare_perc_window is not None else profile.snare_perc_window_sec,
        snare_perc_snare_dominance_ratio=config.snare_perc_snare_dominance if config.snare_perc_snare_dominance is not None else profile.snare_perc_snare_dominance_ratio,
        snare_perc_perc_dominance_ratio=config.snare_perc_perc_dominance if config.snare_perc_perc_dominance is not None else profile.snare_perc_perc_dominance_ratio,
    )
