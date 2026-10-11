"""Model file parts `extra.*` and `fit.*`: the event measures' constants and the fitted measures' params
(docs/DETECTOR_RUNTIME_DESIGN.md section 4, D11).

Every filter, window, bin mask and constant comes from the reference modules themselves: kick_goal_tail_features,
kick_goal_templates, kick_goal_profile, kick_goal_lowbank. Window offsets (the -50..-5 ms fit, the 60 ms guards,
the c-5..c-2 base frames) are recipe structure and live in the Rust code. Templates (tmpl3) are fitted, so they go
under fit.tmpl3, pre-normalised with the reference unit().
"""
from __future__ import annotations

import numpy as np
from scipy.signal import butter

from tools.audio_analysis.eval import kick_goal_lowbank as lowbank
from tools.audio_analysis.eval import kick_goal_tail_features as tail
from tools.audio_analysis.eval import kick_goal_templates as tmpl

PREFIXES = ('extra.', 'fit.')
SAMPLE_RATE, HOP = 48000, 256
# Measures whose constants are extra.* (f15 is the kick finder's own, base.*).
USES_EXTRA = frozenset({'tail3', 'tmpl3', 'prof32', 'fall16', 'low16', 'cdom4', 'note13'})
BASE_NAMES = frozenset('extra.' + n for n in (
    'sample_rate', 'hop', 'ms', 'tail_sos', 'tail_ma_len', 'tail_past_ms', 'tail_floor', 'spec_window',
    'spec_band_of_bin', 'spec_floor', 'patch_span', 'band_low', 'band_high', 'unit_eps', 'low_sos', 'low_ma_len',
    'low_floor'))
CDOM_NAMES = frozenset('extra.' + n for n in ('cdom_window', 'cdom_bin_lo', 'cdom_bin_n', 'cdom_floor', 'cdom_raw_eps'))
NOTE_NAMES = frozenset('extra.' + n for n in (
    'note_low_rate', 'note_lp_sos', 'note_window', 'note_mid_bins', 'note_chroma_bins', 'note_chroma_class'))
NAMES = BASE_NAMES | CDOM_NAMES | NOTE_NAMES


def names_for(used) -> frozenset:
    """The extra.* names a model using these measures carries: the shared constants with any measure that reads
    them, the complex-onset and note-pitch constants only with cdom4 and note13."""
    used = set(used)
    if not used & USES_EXTRA:
        return frozenset()
    return BASE_NAMES | (CDOM_NAMES if 'cdom4' in used else frozenset()) | (NOTE_NAMES if 'note13' in used else frozenset())
FIT_NAMES = {'tmpl3': frozenset(f'fit.tmpl3.{n}' for n in ('all', 'low', 'high', 'span'))}


def _i64(v):
    return np.asarray(v, dtype=np.int64)


def _f64(v):
    return np.asarray(v, dtype=np.float64)


def _bins(freqs, lo_hz, hi_hz):
    """(first bin, count) of the contiguous rfft bins in [lo_hz, hi_hz]."""
    idx = np.flatnonzero((freqs >= lo_hz) & (freqs <= hi_hz))
    assert np.array_equal(idx, np.arange(idx[0], idx[0] + len(idx))), 'band bins must be contiguous'
    return int(idx[0]), len(idx)


def cdom_entries(sr):
    """kick_complex_onset.complex_features and components._cdom4."""
    from tools.audio_analysis.eval import kick_complex_onset as cx
    from tools.audio_analysis.lab.components import CDOM_RAW_EPS
    n = cx._fft_size(sr)
    lo, cnt = _bins(np.fft.rfftfreq(n, 1 / sr), cx.LOW_FREQUENCY_HZ, cx.HIGH_FREQUENCY_HZ)
    return {
        'extra.cdom_window': _f64(np.hanning(n)),
        'extra.cdom_bin_lo': _i64(lo),
        'extra.cdom_bin_n': _i64(cnt),
        'extra.cdom_floor': _f64(cx.NORMALIZED_DENOMINATOR_FLOOR),
        'extra.cdom_raw_eps': _f64(CDOM_RAW_EPS),
    }


def note_entries(sr):
    """note_pitch.pitch_features: the 4 kHz low-pass, the 2048-point window, the mid band and the chroma bins
    with each bin's pitch class (chroma())."""
    from tools.audio_analysis.eval import note_pitch as npi
    n = 2048
    freqs = np.fft.rfftfreq(n, 1 / sr)
    clo, ccnt = _bins(freqs, 100, 4000)
    cls = np.round(12 * np.log2(freqs[clo:clo + ccnt] / 440.0)).astype(int) % 12
    return {
        'extra.note_low_rate': _i64(npi.LOW_SR),
        'extra.note_lp_sos': _f64(butter(6, 500, btype='low', fs=sr, output='sos')),
        'extra.note_window': _f64(np.hanning(n)),
        'extra.note_mid_bins': _i64(_bins(freqs, 500, 6000)),
        'extra.note_chroma_bins': _i64((clo, ccnt)),
        'extra.note_chroma_class': _i64(cls),
    }


def extra_entries(finder, used) -> dict[str, np.ndarray]:
    sr = SAMPLE_RATE
    low = tmpl.EDGES[1:] <= 300
    high = tmpl.EDGES[:-1] >= 1000
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
        # band spectrum (kick_goal_templates.band_spec), read by prof32, fall16 and tmpl3
        'extra.spec_window': _f64(np.hanning(tmpl.N_FFT)),
        'extra.spec_band_of_bin': _i64(which),
        'extra.spec_floor': _f64(1e-10),
        # patches() span candidate to emission: median(avail - cand) + 1, the finder's fixed deadline + 1.
        'extra.patch_span': _i64(deadline(finder) + 1),
        'extra.band_low': _i64(low),
        'extra.band_high': _i64(high),
        'extra.unit_eps': _f64(1e-9),
        # low bank (kick_goal_lowbank.band_envelopes / lowbank_features)
        'extra.low_sos': _f64(low_sos),
        'extra.low_ma_len': _i64(max(1, int(round(.005 * sr)))),
        'extra.low_floor': _f64(1e-9),
        **(cdom_entries(sr) if 'cdom4' in used else {}),
        **(note_entries(sr) if 'note13' in used else {}),
    }


def tmpl3_entries(params) -> dict[str, np.ndarray]:
    templates = params['templates']
    if not isinstance(templates, np.ndarray) or templates.ndim != 3 or templates.shape[2] != tmpl.BANDS:
        raise ValueError(f'tmpl3 templates must be one (K, span, {tmpl.BANDS}) array, got {type(templates)}')
    templates = templates.astype(np.float64)
    low = tmpl.EDGES[1:] <= 300
    high = tmpl.EDGES[:-1] >= 1000
    k = len(templates)
    return {
        'fit.tmpl3.all': _f64(tmpl.unit(templates)),
        'fit.tmpl3.low': _f64(tmpl.unit(templates[:, :, low])).reshape(k, -1),
        'fit.tmpl3.high': _f64(tmpl.unit(templates[:, :, high])).reshape(k, -1),
        'fit.tmpl3.span': _i64(templates.shape[1]),
    }


FIT_HOOKS = {'tmpl3': tmpl3_entries}


def deadline(finder):
    """Hops from a candidate to its emission, fixed per finder."""
    import math
    if finder == 'kick':
        return math.ceil(0.040 * SAMPLE_RATE / HOP)  # finder_kick base.deadline
    from tools.audio_analysis.eval import snare_cands
    assert finder.startswith('snare:'), finder
    return snare_cands.DEADLINE


def entries(record: dict) -> dict[str, np.ndarray]:
    from tools.audio_analysis.lab.release import measures_of
    used = set(measures_of(record))
    out = extra_entries(record['finder'], used) if used & USES_EXTRA else {}
    for name, params in record['fitted'].items():
        if name not in used:
            raise ValueError(f'fitted measure {name} is not read by the model')
        out.update(FIT_HOOKS[name](params))
    return out
