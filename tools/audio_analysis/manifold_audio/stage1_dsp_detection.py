"""Stage 1 -- DSP-only drum object detector, no training, no ADTOF anywhere
in the path. Per docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §7.1's approach text
(Deferred #1, activated by the 2026-07-18 ADTOF bake-off addendum, phase B1):

    "DSP object detectors on the demucs drum stem (no training): the live
    kick detector's logic offline with non-causal luxuries -- larger/
    centered windows, whole-track normalization, backward sample-accurate
    attack refinement ...; clap/snare/hat/tom via per-onset features
    (centroid, flatness, band ratios, decay shape). Key mechanism: cluster
    the track's onsets first (3-8 drum objects), label clusters by centroid
    signature -- per-track calibration, never one-onset-at-a-time global
    templates."

Pipeline (per track)
----------------------
1. `detect_onsets` -- non-causal onset detection: whole-TRACK normalization
   of the onset envelope (an offline luxury a live/causal detector can't
   afford -- it only knows the past), per-band peak picking, frame-CENTER
   time conversion. NO backtracking (removed 2026-07-18, BUG-241): walking
   peaks back to the preceding broadband-RMS minimum landed on the previous
   hit's tail instead of this hit's attack on dense material, killing kick
   recall track-dependently -- see the comment in detect_onsets.
2. `extract_onset_features` -- per onset, over a short post-onset window
   (capped at the next onset so windows never bleed into the next hit):
   spectral centroid, spectral flatness, low/mid/high band-energy ratios,
   and a decay-rate (dB/sec envelope slope -- a fast decay reads as a short,
   percussive transient; a slow one reads as a sustained/tonal hit).
3. `cluster_onsets` -- PER-TRACK, unsupervised KMeans over the standardized
   feature vectors, sweeping k in [3, 8] and picking the silhouette-best k
   -- "cluster first", the design doc's named key mechanism. Flags
   `degenerate` when the best silhouette is at/below floor, or one cluster
   swallows almost every onset (both read as "this track didn't actually
   separate into distinct drum objects").
4. `_label_clusters` -- labels each cluster by NEAREST DEV-FITTED class
   profile centroid (B2 lever 2, 2026-07-18 addendum: a nearest-centroid
   classifier fit from DEV truth only by eval/fit_stage1_profiles.py,
   written to eval/calibration/stage1_class_profiles.json -- supervised
   threshold calibration, not model training). Clustering stays per-track/
   relative (per-track StandardScaler, unchanged, "per-track adaptation
   staying primary" per the design); only the label assignment consults the
   fitted global profiles. Falls back to a hand-tuned heuristic threshold
   cascade (_label_clusters_heuristic) if the fitted file is absent.
5. `detect_drums_stage1` -- emits `List[Event]` in the SAME contract as
   `manifold_audio.adtof_detection.detect_drums_adtof` (type/time/confidence),
   confidence derived from each onset's standardized distance to its
   assigned cluster centroid (a per-track RELATIVE confidence signal -- there
   is no trained classifier margin here, by construction).

Vocabulary: kick, snare, clap, hat, tom, perc (the design doc's 5-8 class
range collapses in practice to these 6 names; `perc` is the catch-all for
anything that doesn't clearly match another signature, matching ADTOF's own
'perc' catch-all for non-kick/snare/hat classes so downstream scoring can
compare like-for-like).
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np
import scipy.signal
from sklearn.cluster import KMeans
from sklearn.metrics import silhouette_score
from sklearn.preprocessing import StandardScaler

from manifold_audio.models import Event
from manifold_audio.spectral import compute_band_onsets

MIN_ONSETS_FOR_CLUSTERING = 3
MIN_K = 3
MAX_K = 8
FEATURE_WINDOW_SEC = 0.08
DECAY_WINDOW_SEC = 0.15

# Onset-detection bands + frame sizing, deliberately mirroring "the live kick
# detector's logic offline" (docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §7.1) --
# manifold_audio.spectral.compute_band_onsets IS that logic (per-band
# spectral flux + local-loudness normalization), the same mechanism
# manifold_audio.analyzer.analyze_percussion already runs for the live kick
# path. A first version of this module used librosa's generic single-channel
# mel-spectrogram onset envelope instead and found (via the self_render
# EDM-kit fixture's known truth): 0% kick/clap/tom recall -- a pure
# low-frequency kick thump barely registers as "spectral flux" against a
# broadband mel envelope tuned for melodic/harmonic content, so a dedicated
# low-band channel is not optional, it is the whole point of "the live kick
# detector's logic" being named explicitly in the brief.
ONSET_DETECTION_BANDS_HZ: Dict[str, Tuple[float, float]] = {
    "low": (20.0, 150.0),      # kick
    "low_mid": (150.0, 1000.0),  # tom / snare body
    "mid": (1000.0, 4000.0),   # snare snap / clap
    "high": (4000.0, 16000.0),  # hat / clap sizzle
}
ONSET_FRAME_SIZE = 2048
ONSET_HOP_SIZE = 256
ONSET_MIN_SEPARATION_SEC = 0.03  # within one band's own peak-picker
ONSET_MERGE_WINDOW_SEC = 0.015  # collapse near-duplicate picks ACROSS bands
# Peak must clear this fraction of the band's OWN whole-track max. Was 0.15;
# lowered to 0.075 (2026-07-18, BUG-241 follow-up tuning) after stage-wise
# accounting showed the threshold as the second kick-killer behind the
# removed backtrack: at 0.075 fixture kick recall (any onset, +-50ms) went
# apricots 15/16 -> 16/16, inhale_exhale 11/14 -> 14/14, tears 6/10 -> 10/10
# at ~30% more raw events (precision is the downstream refractory/threshold
# knobs' job). 0.05 bought +1 kick on one track for another +30% events;
# the live-detector median-adaptive picker was measured strictly worse
# offline (causal lag). Sweepable in bake-off rounds.
ONSET_HEIGHT_FRACTION = 0.075
LOW_BAND_HZ = (20.0, 150.0)
MID_BAND_HZ = (150.0, 2000.0)
# Upper bound intentionally open (effectively Nyquist, not a fixed 12kHz cap):
# a true high-PASS fraction, not a band-pass. A fixed upper cap here once
# made a genuinely brighter, high-passed transient (the clap's crude
# diff-filter timbre, see eval/fetch/self_render.py's _clap_burst) measure a
# LOWER high_ratio than plain broadband noise (the hat) -- its energy was
# concentrated ABOVE the cap, i.e. excluded from the ratio's numerator AND
# denominator alike, inverting the intended clap > hat ordering. Caught via
# the self_render EDM-kit fixture (known truth) before it could silently
# mislabel every clap as a hat.
HIGH_BAND_HZ = (2000.0, 24000.0)
DEGENERATE_SILHOUETTE_FLOOR = 0.05
DEGENERATE_DOMINANT_CLUSTER_FRACTION = 0.95
CLASS_NAMES = ("kick", "snare", "clap", "hat", "tom", "perc")

# Column order for _feature_matrix / the fitted class-profile JSON. Order
# matters: eval/fit_stage1_profiles.py writes centroids in this exact order.
FEATURE_NAMES = ("centroid_hz", "flatness", "low_ratio", "mid_ratio", "high_ratio", "decay_rate_db_per_sec")

# B2 lever 2 (docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md 2026-07-18 addendum):
# dev-fitted class-signature profiles replace the hand-tuned threshold
# cascade (see _label_clusters_heuristic below, kept as the fallback when
# this file doesn't exist). Fit by eval/fit_stage1_profiles.py from DEV
# truth only (self_render edm_kit_128bpm + babyslakh dev drum stems + E-GMD
# dev + manifold_own kick fixtures) -- a nearest-centroid classifier fit,
# not model training.
CLASS_PROFILES_PATH = Path(__file__).resolve().parents[1] / "eval" / "calibration" / "stage1_class_profiles.json"
_class_profiles_cache: Optional[Dict[str, object]] = None


def _load_class_profiles() -> Optional[Dict[str, object]]:
    global _class_profiles_cache
    if _class_profiles_cache is not None:
        return _class_profiles_cache
    if not CLASS_PROFILES_PATH.exists():
        return None
    with open(CLASS_PROFILES_PATH) as f:
        _class_profiles_cache = json.load(f)
    return _class_profiles_cache


@dataclass(frozen=True)
class OnsetFeatures:
    time_sec: float
    centroid_hz: float
    flatness: float
    low_ratio: float
    mid_ratio: float
    high_ratio: float
    decay_rate_db_per_sec: float  # positive = decaying (energy falling over time)


@dataclass
class ClusterResult:
    onset_times: List[float] = field(default_factory=list)
    labels: np.ndarray = field(default_factory=lambda: np.array([], dtype=int))
    k: int = 0
    silhouette: Optional[float] = None
    cluster_class: Dict[int, str] = field(default_factory=dict)
    confidences: List[float] = field(default_factory=list)
    degenerate: bool = False
    degenerate_reason: Optional[str] = None


def _pick_band_peaks(curve: np.ndarray, min_distance_frames: int, height_fraction: float) -> np.ndarray:
    """Local-max peak picker with a threshold that is a fraction of the
    curve's OWN whole-track max -- a non-causal/offline luxury (a live
    causal detector can only estimate this from the past; here the whole
    track's peak is known upfront). Returns frame indices."""
    if curve.size == 0:
        return np.array([], dtype=int)
    cmax = float(np.max(curve))
    if cmax <= 0:
        return np.array([], dtype=int)
    peaks, _ = scipy.signal.find_peaks(curve, height=cmax * height_fraction, distance=max(1, min_distance_frames))
    return peaks


def detect_onsets(
    audio: np.ndarray,
    sr: int,
    frame_size: int = ONSET_FRAME_SIZE,
    hop_size: int = ONSET_HOP_SIZE,
) -> np.ndarray:
    """Non-causal, multi-band onset detection -- "the live kick detector's
    logic offline" (§7.1): manifold_audio.spectral.compute_band_onsets'
    per-band spectral-flux + local-loudness-normalized envelopes (the SAME
    mechanism the live kick path already runs), with `norm_window_sec` set
    to the WHOLE TRACK duration (whole-track normalization, an offline
    luxury a live/causal detector can't afford). Each band is peak-picked
    independently (a kick's dedicated low-band channel is what actually
    catches it -- a single broadband envelope drowns a pure low-frequency
    thump under busier mid/high content). Onsets from different bands within
    ONSET_MERGE_WINDOW_SEC of each other collapse to one (the same physical
    transient usually lights up more than one band)."""
    total_sec = len(audio) / float(sr) if sr > 0 else 0.0
    band_onsets, _raw_rms, hop = compute_band_onsets(
        audio, sr, frame_size, hop_size, ONSET_DETECTION_BANDS_HZ,
        norm_window_sec=total_sec,  # whole-track normalization (offline luxury)
    )
    min_distance_frames = max(1, int(round(ONSET_MIN_SEPARATION_SEC * sr / hop_size)))

    all_times: List[float] = []
    for curve in band_onsets.values():
        peaks = _pick_band_peaks(curve, min_distance_frames, ONSET_HEIGHT_FRACTION)
        if peaks.size == 0:
            continue
        # NO backtracking (removed 2026-07-18, BUG-241). History: a first
        # version backtracked against the flux curve (~50ms early bias, fixed
        # in B1 by switching to broadband RMS); the RMS version then turned
        # out to be the BUG-241 root cause. On dense material the broadband
        # RMS envelope's local minima belong to the PREVIOUS hit's tail, not
        # this hit's attack, so backtracking dragged peaks 60-140ms early --
        # track-dependently (sparse mixes have a clean pre-attack dip, dense
        # ones don't). Minimal-pair evidence (kick recall vs fixture truth,
        # +-50ms): feel_the_vibration 2/16 with RMS backtrack vs 8/16
        # without; inhale_exhale 2/14 vs 11/14; tears 1/10 vs 6/10; apricots
        # (sparse, the working case) 15/16 either way. Backtracking against
        # each band's OWN energy curve was also measured and is strictly
        # worse than no backtracking (4/16, 6/14, 2/10). The flux peak's
        # frame-CENTER time (below) already carries the B1 timing correction
        # and is the onset.
        refined = peaks
        # Frame-index -> time uses the frame's CENTER, not its start.
        # manifold_audio.audio_io.frame_signal builds UNCENTERED frames
        # (frame F covers samples [F*hop, F*hop+frame_size)), and frame_size
        # (2048 samples ~ 46ms) is far larger than hop_size (256 ~ 5.8ms) --
        # a Hanning-windowed frame's energy is concentrated near its middle,
        # so a transient shows up as a magnitude/flux rise in frames whose
        # WINDOW already overlaps it, well before any frame whose START time
        # equals the transient's true instant. Measured via the self_render
        # fixtures' known onset truth: an uncorrected frame-start convention
        # produced a systematic ~30ms EARLY bias on every detected onset.
        all_times.extend(((refined.astype(np.float64) * hop_size + frame_size / 2.0) / sr).tolist())

    if not all_times:
        return np.array([], dtype=np.float64)

    merged: List[float] = []
    for t in sorted(all_times):
        if merged and (t - merged[-1]) < ONSET_MERGE_WINDOW_SEC:
            continue
        merged.append(t)
    return np.asarray(merged, dtype=np.float64)


def _band_energy_ratios(mag: np.ndarray, freqs: np.ndarray) -> Tuple[float, float, float]:
    total = float(np.sum(mag)) + 1e-12

    def _ratio(lo: float, hi: float) -> float:
        mask = (freqs >= lo) & (freqs < hi)
        return float(np.sum(mag[mask])) / total

    return _ratio(*LOW_BAND_HZ), _ratio(*MID_BAND_HZ), _ratio(*HIGH_BAND_HZ)


def _decay_rate_db_per_sec(rms_db: np.ndarray, hop_sec: float) -> float:
    """Least-squares slope of the dB envelope vs time; sign flipped so
    POSITIVE means decaying (energy falling). 0.0 if too few frames to fit."""
    n = len(rms_db)
    if n < 3 or hop_sec <= 0:
        return 0.0
    t = np.arange(n) * hop_sec
    a = np.vstack([t, np.ones(n)]).T
    slope, _intercept = np.linalg.lstsq(a, rms_db, rcond=None)[0]
    return float(-slope)


def extract_onset_features(
    audio: np.ndarray,
    sr: int,
    onset_times: np.ndarray,
    feature_window_sec: float = FEATURE_WINDOW_SEC,
    decay_window_sec: float = DECAY_WINDOW_SEC,
) -> List[OnsetFeatures]:
    """Per-onset spectral + decay features, each window capped at the next
    onset so features never bleed across hits."""
    n = len(audio)
    sorted_times = sorted(onset_times.tolist())
    out: List[OnsetFeatures] = []
    for i, t in enumerate(sorted_times):
        next_t = sorted_times[i + 1] if i + 1 < len(sorted_times) else t + feature_window_sec * 4
        window_end_sec = min(t + feature_window_sec, next_t)
        start = int(round(t * sr))
        end = min(n, max(int(round(window_end_sec * sr)), start + 32))
        start = min(start, max(0, n - 32))
        seg = audio[start:end]
        if seg.size < 32:
            seg = np.pad(seg, (0, 32 - seg.size))

        windowed = seg * np.hanning(len(seg))
        mag = np.abs(np.fft.rfft(windowed))
        freqs = np.fft.rfftfreq(len(seg), d=1.0 / sr)
        centroid = float(np.sum(freqs * mag) / (np.sum(mag) + 1e-12))
        geo_mean = float(np.exp(np.mean(np.log(mag + 1e-12))))
        arith_mean = float(np.mean(mag) + 1e-12)
        flatness = geo_mean / arith_mean
        low_ratio, mid_ratio, high_ratio = _band_energy_ratios(mag, freqs)

        decay_end_sec = min(t + decay_window_sec, next_t)
        d_end = min(n, max(int(round(decay_end_sec * sr)), start + 32))
        d_seg = audio[start:d_end]
        hop = max(1, int(sr * 0.005))  # 5ms hops
        n_hops = max(1, (len(d_seg) - 1) // hop)
        rms_db_frames: List[float] = []
        for h in range(n_hops):
            frame = d_seg[h * hop: h * hop + hop]
            if frame.size == 0:
                continue
            rms = float(np.sqrt(np.mean(frame.astype(np.float64) ** 2)) + 1e-9)
            rms_db_frames.append(20.0 * np.log10(rms))
        decay_rate = _decay_rate_db_per_sec(np.asarray(rms_db_frames), hop / sr) if rms_db_frames else 0.0

        out.append(OnsetFeatures(
            time_sec=t, centroid_hz=centroid, flatness=flatness,
            low_ratio=low_ratio, mid_ratio=mid_ratio, high_ratio=high_ratio,
            decay_rate_db_per_sec=decay_rate,
        ))
    return out


def _feature_matrix(features: List[OnsetFeatures]) -> np.ndarray:
    return np.array([
        [f.centroid_hz, f.flatness, f.low_ratio, f.mid_ratio, f.high_ratio, f.decay_rate_db_per_sec]
        for f in features
    ], dtype=np.float64)


def _label_clusters_nearest_profile(raw_features: np.ndarray, labels: np.ndarray, k: int, profiles: Dict[str, object]) -> Dict[int, str]:
    """B2 lever 2: label each cluster by NEAREST DEV-FITTED class profile
    centroid (Euclidean distance in the GLOBAL standardized feature space
    eval/fit_stage1_profiles.py fit from DEV truth), not a hand-written
    threshold cascade. Clustering itself stays per-track/relative (the
    caller's own per-track StandardScaler, unchanged) -- only the LABEL
    assignment uses the fitted global scale."""
    scaler_mean = np.asarray(profiles["scaler_mean"], dtype=np.float64)
    scaler_scale = np.asarray(profiles["scaler_scale"], dtype=np.float64)
    centroids: Dict[str, np.ndarray] = {
        cls: np.asarray(vec, dtype=np.float64) for cls, vec in profiles["class_profile_centroids"].items()
    }
    cluster_class: Dict[int, str] = {}
    for c in range(k):
        mask = labels == c
        if not np.any(mask):
            continue
        cluster_mean_raw = raw_features[mask].mean(axis=0)
        cluster_std = (cluster_mean_raw - scaler_mean) / scaler_scale
        best_cls, best_dist = None, float("inf")
        for cls, profile_vec in centroids.items():
            dist = float(np.linalg.norm(cluster_std - profile_vec))
            if dist < best_dist:
                best_dist, best_cls = dist, cls
        cluster_class[c] = best_cls if best_cls is not None else "perc"
    return cluster_class


def _label_clusters_heuristic(raw_features: np.ndarray, labels: np.ndarray, k: int) -> Dict[int, str]:
    """Label each cluster by its RAW-feature centroid signature -- fixed
    heuristic thresholds on physically-meaningful ratios/Hz (per-track
    calibration: relative to THIS track's own onsets, never an absolute
    global template). Column order matches _feature_matrix: [centroid_hz,
    flatness, low_ratio, mid_ratio, high_ratio, decay_rate_db_per_sec].

    SUPERSEDED as the primary path by _label_clusters_nearest_profile (B2
    lever 2, docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md 2026-07-18 addendum) --
    kept as the fallback _label_clusters uses when the fitted profile file
    (eval/calibration/stage1_class_profiles.json) doesn't exist, so the
    module still functions (degraded) in an environment that hasn't run
    eval/fit_stage1_profiles.py yet."""
    cluster_class: Dict[int, str] = {}
    for c in range(k):
        mask = labels == c
        if not np.any(mask):
            continue
        centroid_hz, flatness, low_r, mid_r, high_r, _decay = raw_features[mask].mean(axis=0)

        # Order matters: kick/tom are checked first (low-centroid, tonal --
        # unambiguous), then snare's mid-band signature (a bandpassed
        # noise+tone snare reads as LOWER centroid and flatness AND higher
        # mid_ratio than either a hat or a clap -- verified against the
        # self_render EDM-kit fixture's own per-class feature means), then
        # clap vs hat is a centroid split (clap's highpass timbre measures
        # further into the "high" band than a hat's plain broadband noise).
        #
        # Kick: BAND-DOMINANCE, not an absolute centroid cutoff (fixed
        # 2026-07-18, B2 lever 1). The original rule was `centroid_hz < 200
        # and low_r > 0.45`, calibrated against a pure 60Hz synthetic sine
        # burst (centroid ~83Hz). A REAL kick (E-GMD, manifold_own's
        # Ableton-stem `drums.wav`) has a beater-click transient + harmonic
        # content that pushes its measured spectral centroid into the
        # 1000-2000Hz range over an 80ms window even though low_ratio is
        # still clearly the dominant band relative to that SAME cluster's
        # own mid/high (e.g. one diagnosed real cluster: centroid=1464Hz,
        # low=0.735, mid=0.096, high=0.156 -- unambiguously kick-shaped by
        # dominance, but the 200Hz absolute cutoff silently excluded it,
        # so no cluster was EVER labeled kick on real material -- confirmed
        # via manifold_own's apricots_128bpm drums.wav, not just
        # E-GMD/babyslakh). Dominance is centroid-Hz-independent and
        # generalizes to both the synthetic burst and real kicks.
        if low_r > mid_r and low_r > high_r and low_r > 0.45:
            cluster_class[c] = "kick"
        elif centroid_hz < 700.0 and flatness < 0.30:
            cluster_class[c] = "tom"
        elif mid_r > 0.15 and high_r > 0.30:
            cluster_class[c] = "snare"
        elif high_r > 0.40 and flatness > 0.45 and centroid_hz > 12000.0:
            cluster_class[c] = "clap"
        elif high_r > 0.30 and flatness > 0.35:
            cluster_class[c] = "hat"
        else:
            cluster_class[c] = "perc"
    return cluster_class


def _label_clusters(raw_features: np.ndarray, labels: np.ndarray, k: int) -> Dict[int, str]:
    """Entry point cluster_onsets calls. Uses the DEV-fitted nearest-centroid
    classifier when eval/calibration/stage1_class_profiles.json exists
    (the shipped, expected case -- see eval/fit_stage1_profiles.py), else
    falls back to the hand-tuned heuristic cascade."""
    profiles = _load_class_profiles()
    if profiles is not None:
        return _label_clusters_nearest_profile(raw_features, labels, k, profiles)
    return _label_clusters_heuristic(raw_features, labels, k)


def cluster_onsets(
    features: List[OnsetFeatures],
    min_k: int = MIN_K,
    max_k: int = MAX_K,
    random_state: int = 20260718,
) -> ClusterResult:
    onset_times = [f.time_sec for f in features]
    n = len(features)
    if n < MIN_ONSETS_FOR_CLUSTERING:
        return ClusterResult(
            onset_times=onset_times, labels=np.zeros(n, dtype=int), k=max(1, n), silhouette=None,
            cluster_class={i: "perc" for i in range(max(1, n))}, confidences=[0.0] * n,
            degenerate=True, degenerate_reason=f"too few onsets ({n} < {MIN_ONSETS_FOR_CLUSTERING})",
        )

    raw = _feature_matrix(features)
    scaler = StandardScaler()
    scaled = scaler.fit_transform(raw)

    upper_k = min(max_k, n - 1)
    lower_k = min(min_k, upper_k)

    best_k: Optional[int] = None
    best_labels: Optional[np.ndarray] = None
    best_score = -2.0
    for k in range(lower_k, upper_k + 1):
        km = KMeans(n_clusters=k, n_init=10, random_state=random_state)
        labels = km.fit_predict(scaled)
        if len(set(labels.tolist())) < 2:
            continue
        try:
            score = silhouette_score(scaled, labels)
        except ValueError:
            continue
        if score > best_score:
            best_score, best_k, best_labels = score, k, labels

    if best_labels is None:
        km = KMeans(n_clusters=lower_k, n_init=10, random_state=random_state)
        best_labels = km.fit_predict(scaled)
        best_k = lower_k
        best_score_final: Optional[float] = None
    else:
        best_score_final = best_score

    cluster_class = _label_clusters(raw, best_labels, best_k)

    # Per-onset confidence: closeness to its assigned cluster's centroid in
    # STANDARDIZED feature space (a per-track relative signal, not a trained
    # classifier margin -- there is no training in Stage 1 by construction).
    centroids_scaled = {c: scaled[best_labels == c].mean(axis=0) for c in set(best_labels.tolist())}
    dists = [float(np.linalg.norm(scaled[i] - centroids_scaled[int(lbl)])) for i, lbl in enumerate(best_labels)]
    max_d = max(dists) if dists else 0.0
    confidences = [max(0.05, min(1.0, 1.0 - (d / max_d if max_d > 0 else 0.0))) for d in dists]

    degenerate, reason = False, None
    if best_score_final is not None and best_score_final < DEGENERATE_SILHOUETTE_FLOOR:
        degenerate, reason = True, f"silhouette {best_score_final:.4f} < floor {DEGENERATE_SILHOUETTE_FLOOR}"
    else:
        counts = np.bincount(best_labels, minlength=best_k)
        dominant_frac = float(np.max(counts)) / n
        if dominant_frac >= DEGENERATE_DOMINANT_CLUSTER_FRACTION and best_k > 1:
            degenerate, reason = True, f"one cluster holds {dominant_frac:.1%} of all onsets (k={best_k})"

    return ClusterResult(
        onset_times=onset_times, labels=best_labels, k=best_k, silhouette=best_score_final,
        cluster_class=cluster_class, confidences=confidences, degenerate=degenerate, degenerate_reason=reason,
    )


def detect_drums_stage1(audio: np.ndarray, sr: int) -> Tuple[List[Event], ClusterResult]:
    """Full Stage-1 pipeline: onset detect -> feature extract -> per-track
    cluster -> centroid-signature label -> Event JSON contract (same shape
    as manifold_audio.adtof_detection.detect_drums_adtof's output). Returns
    (events, cluster_result) -- the cluster_result carries the diagnostics
    (k, silhouette, degenerate flag/reason) callers need to report per-track
    clustering health, per the bake-off brief ("where clustering failed --
    count them, don't hide")."""
    onset_times = detect_onsets(audio, sr)
    features = extract_onset_features(audio, sr, onset_times)
    result = cluster_onsets(features)

    events: List[Event] = []
    for i, feat in enumerate(features):
        label = int(result.labels[i]) if i < len(result.labels) else 0
        cls = result.cluster_class.get(label, "perc")
        conf = result.confidences[i] if i < len(result.confidences) else 0.0
        events.append(Event(type=cls, time=round(feat.time_sec, 4), confidence=round(conf, 4)))
    events.sort(key=lambda e: (e.time, e.type))
    return events, result
