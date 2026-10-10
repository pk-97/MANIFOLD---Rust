"""Export hook for the 54 extra tree features (crates/manifold-audio/src/kick/extra.rs).

Every filter, window, bin mask, template and constant the Rust port reads, taken from
the reference modules themselves: kick_goal_tail_features, kick_goal_templates,
kick_goal_profile, kick_goal_lowbank. Window offsets (the -50..-5 ms fit, the 60 ms
guards, the c-5..c-2 base frames) are recipe structure and live in the Rust code.
"""
from __future__ import annotations

import sys
from pathlib import Path

import numpy as np
from scipy.signal import butter

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

from tools.audio_analysis.eval import kick_goal_lowbank as lowbank  # noqa: E402
from tools.audio_analysis.eval import kick_goal_tail_features as tail  # noqa: E402
from tools.audio_analysis.eval import kick_goal_templates as tmpl  # noqa: E402

SAMPLE_RATE, HOP = 48000, 256


def _i64(v):
    return np.asarray(v, dtype=np.int64)


def _f64(v):
    return np.asarray(v, dtype=np.float64)


def model_entries(final: dict) -> dict[str, np.ndarray]:
    sr = SAMPLE_RATE
    templates = final['templates']
    if not isinstance(templates, np.ndarray) or templates.ndim != 3 or templates.shape[2] != tmpl.BANDS:
        raise ValueError(f"final['templates'] must be one (K, span, {tmpl.BANDS}) array, got {type(templates)}")
    templates = templates.astype(np.float64)
    span = templates.shape[1]
    low = tmpl.EDGES[1:] <= 300
    high = tmpl.EDGES[:-1] >= 1000
    k = len(templates)

    freqs = np.fft.rfftfreq(tmpl.N_FFT, 1 / sr)
    which = np.digitize(freqs, tmpl.EDGES) - 1
    which[(which < 0) | (which >= tmpl.BANDS)] = -1

    low_sos = np.stack([
        butter(2, [fc - fc / lowbank.Q / 2, fc + fc / lowbank.Q / 2], btype='bandpass', fs=sr, output='sos')
        for fc in lowbank.CENTRES
    ])

    return {
        'extra.sample_rate': _i64(sr),
        'extra.hop': _i64(HOP),
        'extra.ms': _f64(tail.MS),
        # tail (kick_goal_tail_features.low_env_db / tail_features)
        'extra.tail_sos': _f64(butter(4, (30, 140), btype='band', fs=sr, output='sos')),
        'extra.tail_ma_len': _i64(int(.012 * sr)),
        'extra.tail_past_ms': _i64(4000),
        'extra.tail_floor': _f64(1e-12),
        # band spectrum (kick_goal_templates.band_spec)
        'extra.spec_window': _f64(np.hanning(tmpl.N_FFT)),
        'extra.spec_band_of_bin': _i64(which),
        'extra.spec_floor': _f64(1e-10),
        # templates (kick_goal_templates.patches / template_features), pre-normalised with the reference unit()
        'extra.patch_span': _i64(span),
        'extra.band_low': _i64(low),
        'extra.band_high': _i64(high),
        'extra.unit_eps': _f64(1e-9),
        'extra.templates_all': _f64(tmpl.unit(templates)),
        'extra.templates_low': _f64(tmpl.unit(templates[:, :, low])).reshape(k, -1),
        'extra.templates_high': _f64(tmpl.unit(templates[:, :, high])).reshape(k, -1),
        # low bank (kick_goal_lowbank.band_envelopes / lowbank_features)
        'extra.low_sos': _f64(low_sos),
        'extra.low_ma_len': _i64(max(1, int(round(.005 * sr)))),
        'extra.low_floor': _f64(1e-9),
    }
