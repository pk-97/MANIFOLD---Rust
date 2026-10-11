"""Causal pitch measurements at note candidates: what note is playing just before the candidate and just after it,
from audio up to the candidate's emission only.

Windows: before = [onset - 70 ms, onset - 10 ms]; after = the 40 ms ending at emission.
- Low pitch (bass): YIN on the signal low-passed under 500 Hz and taken to 4 kHz, 40-400 Hz: f0 before and after
  (log2 Hz, 0 when unvoiced), the change in semitones, the YIN dip (lower = clearer pitch) before and after, and the
  low band level before, after and its change.
- Pitch classes (leads): 12-bin chroma from a 2048-point spectrum of 100-4000 Hz ending at each window: cosine
  distance before to after, the largest rise of one class, the after chroma's entropy, the level change, and the
  share of strong spectral peaks after (500-6000 Hz) that were not there before.
"""
import numpy as np
from scipy.signal import butter, resample_poly, sosfilt

NAMES = ('f0_before', 'f0_after', 'f0_change_st', 'yin_before', 'yin_after', 'low_before_db', 'low_after_db', 'low_change_db',
         'chroma_distance', 'chroma_rise', 'chroma_entropy', 'mid_change_db', 'new_peaks')
LOW_SR = 4000


def yin(seg, sr, fmin=40.0, fmax=400.0, thresh=.15):
    """(f0 Hz or 0, dip) of one segment: the first lag whose cumulative-mean-normalised difference dips under thresh,
    else the deepest dip."""
    lo, hi = int(sr / fmax), int(sr / fmin)
    w = len(seg) - hi
    if w < lo:
        return 0.0, 1.0
    d = np.array([np.sum((seg[:w] - seg[k:k + w]) ** 2) for k in range(hi + 1)])
    cmnd = np.ones_like(d)
    cmnd[1:] = d[1:] * np.arange(1, hi + 1) / np.maximum(np.cumsum(d[1:]), 1e-12)
    band = cmnd[lo:hi + 1]
    under = np.flatnonzero(band < thresh)
    k = under[0] if len(under) else int(np.argmin(band))
    while k + 1 < len(band) and band[k + 1] < band[k]:
        k += 1
    dip = float(band[k])
    return (sr / (lo + k), dip) if dip < .5 else (0.0, dip)


def chroma(mag, freqs):
    sel = (freqs >= 100) & (freqs <= 4000)
    pc = np.round(12 * np.log2(freqs[sel] / 440.0)).astype(int) % 12
    c = np.bincount(pc, weights=mag[sel], minlength=12)
    return c / max(1e-12, c.sum())


def pitch_features(x, sr, onset, emit):
    x = np.asarray(x, float)
    low = resample_poly(sosfilt(butter(6, 500, btype='low', fs=sr, output='sos'), x), LOW_SR, sr)
    n = 2048
    win = np.hanning(n)
    freqs = np.fft.rfftfreq(n, 1 / sr)
    mid = (freqs >= 500) & (freqs <= 6000)
    out = np.zeros((len(onset), len(NAMES)))

    def low_seg(a, b):
        return low[max(0, int(a * LOW_SR)):max(0, int(b * LOW_SR))]

    def spectrum_ending(t):
        e = int(t * sr)
        seg = x[max(0, e - n):e]
        seg = np.concatenate([np.zeros(n - len(seg)), seg]) * win
        return np.abs(np.fft.rfft(seg))

    for i, (on, em) in enumerate(zip(onset, emit)):
        b, a = low_seg(on - .070, on - .010), low_seg(em - .040, em)
        f0b, db = yin(b, LOW_SR) if len(b) else (0.0, 1.0)
        f0a, da = yin(a, LOW_SR) if len(a) else (0.0, 1.0)
        lb = 10 * np.log10(np.mean(b ** 2) + 1e-12) if len(b) else -120.0
        la = 10 * np.log10(np.mean(a ** 2) + 1e-12) if len(a) else -120.0
        st = abs(12 * np.log2(f0a / f0b)) if f0a and f0b else 0.0
        sb, sa = spectrum_ending(on - .010), spectrum_ending(em)
        cb, ca = chroma(sb, freqs), chroma(sa, freqs)
        cos = 1 - float(np.dot(cb, ca) / max(1e-12, np.linalg.norm(cb) * np.linalg.norm(ca)))
        ent = float(-np.sum(ca * np.log(ca + 1e-12)))
        mb, ma = np.sum(sb[mid] ** 2), np.sum(sa[mid] ** 2)
        lb_, la_ = 20 * np.log10(sb[mid] + 1e-9), 20 * np.log10(sa[mid] + 1e-9)
        strong = la_ >= la_.max() - 30
        peaks = strong & (la_ >= np.roll(la_, 1)) & (la_ >= np.roll(la_, -1))
        new = float(np.mean(la_[peaks] - lb_[peaks] >= 6)) if peaks.any() else 0.0
        out[i] = (np.log2(f0b) if f0b else 0, np.log2(f0a) if f0a else 0, st, db, da, lb, la, la - lb,
                  cos, float(np.max(ca - cb)), ent, 10 * np.log10((ma + 1e-12) / (mb + 1e-12)), new)
    return out
