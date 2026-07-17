"""P1 -- dataset pipeline for the Audio Event Classifier
(docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md §5 P1). Builds labeled mel-patch +
side-feature examples for the D3 class vocabulary (kick, snare, hat, perc,
synth, vocal, other) from the allowlisted sources named in
`train/sources.toml` -- see that file for per-source licensing/coverage
notes and eval/tests/test_train_license_allowlist.py for the enforced
license/banned-reference gate.

Patch shape (D4 defaults): 64 mel bands, 20Hz-16kHz, ~100ms span (~10ms
pre-onset + ~90ms post), hop ~6.25ms -> a (64, 16) float32 log-mel patch,
plus a 6-dim side-feature vector reusing
manifold_audio.stage1_dsp_detection.extract_onset_features's own per-onset
scalars (spectral centroid, flatness, low/mid/high band-energy ratios,
decay rate) -- the DSP front-end's own onset-characterization step (design
doc §1 audit row 1), already computed there for clustering, read here as
D4's "front-end scalars, already computed" side-input rather than
reinvented.

Augmentation, P1 scope: +-10ms onset jitter only (D6 names EQ/gain/
limiting/polarity too; those are P3's compose.py). Every raw onset yields
exactly two examples -- the exact-onset patch (jitter_sec=0.0) and one
randomly jittered copy -- built from a single seeded `numpy.random.Generator`
threaded through every source loader in a fixed, sorted iteration order, so
`build_dataset(seed=...)` is bit-for-bit reproducible.

E-GMD is isolated drum-kit audio with no backing material of its own;
`_composite_with_backing` (the P1-scope compose helper the phase brief
names -- full compose.py is P3) mixes each hit's raw segment with a
randomly-gained, randomly-offset slice of self_render's non-drum wavs
before mel extraction, so the classifier sees drum hits inside a mix
rather than a studio-isolated one-shot.

Dev-split discipline: every loader below reads only the dev-side rows each
upstream module already filters to (eval.sweep_p4.DEV_LIVESHOW_FIXTURES,
eval.egmd_drum_truth.available_rows(split="dev")) -- this module never
itself selects or names the songs/rows reserved for the ship-candidate
read.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Sequence, Tuple

import librosa
import numpy as np

try:
    import tomllib
except ImportError:  # pragma: no cover - runtime is 3.12, tomllib always present
    import tomli as tomllib  # type: ignore[no-redef]

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval.beat_scoring import load_tempo_points  # noqa: E402
from eval.calibration import MANIFOLD_OWN_KICK_FIXTURE_IDS  # noqa: E402
from eval.egmd_drum_truth import available_rows as _egmd_available_rows  # noqa: E402
from eval.egmd_drum_truth import load_drum_truth as _egmd_load_drum_truth  # noqa: E402
from eval.liveshow_extract import beats_to_seconds  # noqa: E402
from eval.paths import DATA_ROOT  # noqa: E402
from eval.run import AUDIO_ANALYSIS_ROOT, _load_kick_truth_csv, _resolve_path, load_fixtures  # noqa: E402
# DEV_LIVESHOW_FIXTURES / derive_active_windows / filter_to_windows are the
# DENSE_IN_WINDOW machinery this module reuses rather than re-deriving
# (docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md's read-back instruction). Only the
# already-dev-filtered fixture list is imported -- this file never names or
# reads the two songs that list itself excludes.
from eval.sweep_p4 import DEV_LIVESHOW_FIXTURES, derive_active_windows, filter_to_windows  # noqa: E402
from manifold_audio.audio_io import load_audio_mono  # noqa: E402
from manifold_audio.stage1_dsp_detection import detect_onsets, extract_onset_features  # noqa: E402

# ---------------------------------------------------------------------------
# D3 class vocabulary
# ---------------------------------------------------------------------------

CLASS_NAMES: Tuple[str, ...] = ("kick", "snare", "hat", "perc", "synth", "vocal", "other")

# ---------------------------------------------------------------------------
# D4 patch defaults
# ---------------------------------------------------------------------------

N_MELS = 64
MEL_FMIN_HZ = 20.0
MEL_FMAX_HZ = 16000.0
N_FRAMES = 16
PRE_ONSET_MS = 10.0
POST_ONSET_MS = 90.0
PATCH_SPAN_MS = PRE_ONSET_MS + POST_ONSET_MS  # 100.0ms, D4 default
HOP_MS = PATCH_SPAN_MS / N_FRAMES  # 6.25ms, within D4's "hop ~=6ms"
N_FFT = 1024
SIDE_FEATURE_NAMES: Tuple[str, ...] = (
    "centroid_hz", "flatness", "low_ratio", "mid_ratio", "high_ratio", "decay_rate_db_per_sec",
)

JITTER_MS = 10.0  # D6: "+-10ms onset jitter"
OTHER_MIN_GAP_SEC = 0.05  # P1 brief: "match no truth within 50ms"
DEFAULT_SEED = 20260718

DEFAULT_SOURCES_TOML = Path(__file__).resolve().parent / "sources.toml"
ALLOWED_LICENSES = {"CC-BY", "CC0", "ours"}


@dataclass
class PatchExample:
    mel: np.ndarray  # (N_MELS, N_FRAMES) float32, log-mel dB
    side_features: np.ndarray  # (6,) float32, SIDE_FEATURE_NAMES order
    label: str
    source_id: str
    track_id: str
    onset_time_sec: float  # the base (unjittered) onset time
    jitter_sec: float  # 0.0 for the base copy, else the applied jitter


# ---------------------------------------------------------------------------
# Mel-patch + side-feature extraction
# ---------------------------------------------------------------------------


def _hop_length(sr: int) -> int:
    return max(1, int(round(HOP_MS / 1000.0 * sr)))


def _patch_segment(audio: np.ndarray, sr: int, onset_sec: float, jitter_sec: float = 0.0) -> np.ndarray:
    """Raw-sample segment long enough for exactly N_FRAMES center=False STFT
    frames at (N_FFT, hop_length), starting PRE_ONSET_MS before the
    (possibly jittered) onset. Zero-padded at track edges -- jitter or a
    near-boundary onset can run the window off either end."""
    hop = _hop_length(sr)
    n_needed = N_FFT + (N_FRAMES - 1) * hop
    start_sample = int(round((onset_sec + jitter_sec - PRE_ONSET_MS / 1000.0) * sr))
    end_sample = start_sample + n_needed
    seg = np.zeros(n_needed, dtype=np.float32)
    src_start = max(0, start_sample)
    src_end = min(len(audio), end_sample)
    if src_end > src_start:
        dst_start = src_start - start_sample
        seg[dst_start: dst_start + (src_end - src_start)] = audio[src_start:src_end]
    return seg


def _mel_from_segment(segment: np.ndarray, sr: int) -> np.ndarray:
    hop = _hop_length(sr)
    fmax = min(MEL_FMAX_HZ, sr / 2.0)
    mel_power = librosa.feature.melspectrogram(
        y=segment.astype(np.float32), sr=sr, n_fft=N_FFT, hop_length=hop,
        center=False, n_mels=N_MELS, fmin=MEL_FMIN_HZ, fmax=fmax,
    )
    mel_db = librosa.power_to_db(mel_power, ref=1.0, amin=1e-6, top_db=None)
    if mel_db.shape[1] < N_FRAMES:
        mel_db = np.pad(mel_db, ((0, 0), (0, N_FRAMES - mel_db.shape[1])), mode="edge")
    elif mel_db.shape[1] > N_FRAMES:
        mel_db = mel_db[:, :N_FRAMES]
    return mel_db.astype(np.float32)


def extract_mel_patch(audio: np.ndarray, sr: int, onset_sec: float, jitter_sec: float = 0.0) -> np.ndarray:
    return _mel_from_segment(_patch_segment(audio, sr, onset_sec, jitter_sec), sr)


def _side_features_from_onset_feature(f) -> np.ndarray:
    return np.array(
        [f.centroid_hz, f.flatness, f.low_ratio, f.mid_ratio, f.high_ratio, f.decay_rate_db_per_sec],
        dtype=np.float32,
    )


def _side_features(audio: np.ndarray, sr: int, onset_sec: float) -> np.ndarray:
    """Single-onset fallback -- used only when a batched lookup misses."""
    feats = extract_onset_features(audio, sr, np.array([onset_sec], dtype=np.float64))
    return _side_features_from_onset_feature(feats[0])


def _batched_onset_features(audio: np.ndarray, sr: int, onset_times: Sequence[float]) -> Dict[float, np.ndarray]:
    """extract_onset_features called ONCE over every onset time in this
    file (any class), so each per-onset window is capped by its true next
    onset rather than bleeding into a busy passage -- same convention as
    eval/fit_stage1_profiles.py's _features_for_truth (pool before calling
    the front-end, not one onset at a time)."""
    if not onset_times:
        return {}
    sorted_times = sorted(set(round(t, 6) for t in onset_times))
    feats = extract_onset_features(audio, sr, np.asarray(sorted_times, dtype=np.float64))
    return {round(f.time_sec, 6): _side_features_from_onset_feature(f) for f in feats}


def _make_examples(
    rng: np.random.Generator,
    audio: np.ndarray,
    sr: int,
    onset_sec: float,
    label: str,
    source_id: str,
    track_id: str,
    feat_map: Dict[float, np.ndarray],
) -> List[PatchExample]:
    """The base (jitter=0.0) example + one +-JITTER_MS-jittered copy, per
    D6's P1-scope augmentation. Both copies reuse the SAME side-feature
    vector (the onset's timbral identity doesn't meaningfully change over a
    few ms of jitter; recomputing it per jittered copy would double the
    front-end cost for no signal)."""
    key = round(onset_sec, 6)
    side = feat_map.get(key)
    if side is None:
        side = _side_features(audio, sr, onset_sec)
    base = PatchExample(
        mel=extract_mel_patch(audio, sr, onset_sec, 0.0), side_features=side,
        label=label, source_id=source_id, track_id=track_id,
        onset_time_sec=onset_sec, jitter_sec=0.0,
    )
    jitter = float(rng.uniform(-JITTER_MS, JITTER_MS)) / 1000.0
    jittered = PatchExample(
        mel=extract_mel_patch(audio, sr, onset_sec, jitter_sec=jitter), side_features=side,
        label=label, source_id=source_id, track_id=track_id,
        onset_time_sec=onset_sec, jitter_sec=jitter,
    )
    return [base, jittered]


# ---------------------------------------------------------------------------
# "other"-class mining (DENSE_IN_WINDOW machinery, reused from eval.sweep_p4)
# ---------------------------------------------------------------------------


def _merge_windows(windows: List[Tuple[float, float]]) -> List[Tuple[float, float]]:
    """filter_to_windows' own precondition is sorted, NON-OVERLAPPING
    windows; unioning several classes' active-window sets for the same song
    can overlap (two classes performed over the same passage), so this
    merges before handing off."""
    if not windows:
        return []
    ordered = sorted(windows)
    merged = [ordered[0]]
    for s, e in ordered[1:]:
        last_s, last_e = merged[-1]
        if s <= last_e:
            merged[-1] = (last_s, max(last_e, e))
        else:
            merged.append((s, e))
    return merged


def mine_other_onsets(
    detected_times: Sequence[float],
    truth_by_class: Dict[str, Sequence[float]],
    bpm: float,
    min_gap_sec: float = OTHER_MIN_GAP_SEC,
) -> List[float]:
    """Stage-1 DSP front-end onsets that land inside SOME class's active
    (dense-in-window) passage but no closer than min_gap_sec to ANY truth
    onset of ANY class -- real detections of non-labeled content (P1
    brief), not label noise. Windows via eval.sweep_p4.derive_active_windows
    (one call per class, unioned + merged); membership via
    eval.sweep_p4.filter_to_windows."""
    windows: List[Tuple[float, float]] = []
    all_truth: List[float] = []
    for times in truth_by_class.values():
        windows.extend(derive_active_windows(list(times), bpm))
        all_truth.extend(times)
    merged_windows = _merge_windows(windows)
    candidates = filter_to_windows(list(detected_times), merged_windows)
    if not all_truth:
        return list(candidates)
    truth_arr = np.asarray(sorted(all_truth), dtype=np.float64)
    out: List[float] = []
    for t in candidates:
        idx = int(np.searchsorted(truth_arr, t))
        near = False
        if idx > 0 and abs(t - truth_arr[idx - 1]) < min_gap_sec:
            near = True
        if not near and idx < truth_arr.size and abs(truth_arr[idx] - t) < min_gap_sec:
            near = True
        if not near:
            out.append(t)
    return out


# ---------------------------------------------------------------------------
# Source loader: liveshow_dev
# ---------------------------------------------------------------------------

LIVESHOW_ONSET_TRUTH_PATH = AUDIO_ANALYSIS_ROOT / "eval" / "liveshow_labels" / "onset_truth.json"
LIVESHOW_SLICES_DIR = DATA_ROOT / "liveshow_song_slices"
# The classes this module reads out of onset_truth.json as their own class.
# `bass_sustained` (also present in that file) is deliberately excluded --
# D3 rules sustained bodies out of scope for this classifier (region
# material, the tracker's job); its onsets are left to fall into `other`
# (or `synth`) on their own merits, exactly as D3 anticipates.
LIVESHOW_TRUTH_CLASS_NAMES: Tuple[str, ...] = ("kick", "snare", "hat", "synth", "vocal")


def _liveshow_truth_for_song(
    fixture: Dict[str, Any], tempo_points, onset_truth: List[Dict[str, Any]], pad_sec: float = 0.5,
) -> Tuple[Dict[str, List[float]], float, float]:
    """Per-class truth times relative to the cached slice wav's own t=0
    (seg_start_sec - pad_sec) -- same time-conversion convention as
    eval.sweep_p4._liveshow_song_truth, extended to the full P1 class set
    (that function's own LIVESHOW_TRUTH_CLASSES omits vocal)."""
    start_beat, end_beat = tuple(fixture["beat_range"])
    seg_start_sec = beats_to_seconds(start_beat, tempo_points)
    seg_end_sec = beats_to_seconds(end_beat, tempo_points)
    truth: Dict[str, List[float]] = {c: [] for c in LIVESHOW_TRUTH_CLASS_NAMES}
    for layer in onset_truth:
        cls = layer["instrument"]
        if cls not in LIVESHOW_TRUTH_CLASS_NAMES:
            continue
        for edge_abs in layer["edges_secs_in_audio"]:
            if seg_start_sec <= edge_abs < seg_end_sec:
                truth[cls].append(edge_abs - seg_start_sec + pad_sec)
    for c in truth:
        truth[c].sort()
    return truth, seg_start_sec, seg_end_sec


def _liveshow_song_bpm(fixture: Dict[str, Any], tempo_points) -> float:
    start_beat = fixture["beat_range"][0]
    t0 = beats_to_seconds(start_beat, tempo_points)
    t1 = beats_to_seconds(start_beat + 4.0, tempo_points)
    if t0 is None or t1 is None or t1 <= t0:
        return 128.0
    return 4.0 * 60.0 / (t1 - t0)


def _load_liveshow_dev(rng: np.random.Generator) -> List[PatchExample]:
    if not LIVESHOW_ONSET_TRUTH_PATH.exists():
        print(f"[dataset] liveshow onset truth missing, skipping: {LIVESHOW_ONSET_TRUTH_PATH}", file=sys.stderr)
        return []
    onset_truth = json.loads(LIVESHOW_ONSET_TRUTH_PATH.read_text())
    tempo_points = load_tempo_points()
    out: List[PatchExample] = []
    for fx in DEV_LIVESHOW_FIXTURES:
        wav_path = LIVESHOW_SLICES_DIR / f"{fx['id']}.wav"
        if not wav_path.exists():
            print(f"[dataset] liveshow slice missing, skipping: {wav_path}", file=sys.stderr)
            continue
        truth, _seg_start, _seg_end = _liveshow_truth_for_song(fx, tempo_points, onset_truth)
        if not any(truth.values()):
            continue
        audio, sr = load_audio_mono(wav_path, target_sr=44100, ffmpeg_bin=None)
        bpm = _liveshow_song_bpm(fx, tempo_points)
        detected = detect_onsets(audio, sr).tolist()
        other_times = mine_other_onsets(detected, truth, bpm)

        all_times = sorted(
            {round(t, 6) for times in truth.values() for t in times} | {round(t, 6) for t in other_times}
        )
        feat_map = _batched_onset_features(audio, sr, all_times)

        for cls, times in truth.items():
            for t in sorted(times):
                out.extend(_make_examples(rng, audio, sr, t, cls, "liveshow_dev", fx["id"], feat_map))
        for t in sorted(other_times):
            out.extend(_make_examples(rng, audio, sr, t, "other", "liveshow_dev", fx["id"], feat_map))
    return out


# ---------------------------------------------------------------------------
# Source loader: egmd_dev (composited over a self_render backing bed)
# ---------------------------------------------------------------------------

BACKING_BED_WAV_NAMES: Tuple[str, ...] = ("arp_16th_128bpm.wav", "sustained_pad_100bpm.wav")
BACKING_GAIN_DB_RANGE: Tuple[float, float] = (-24.0, -10.0)  # quieter than the drum hit -- a bed, not a co-lead


def _load_backing_bed(target_sr: int = 44100) -> np.ndarray:
    base = DATA_ROOT / "self_render"
    parts: List[np.ndarray] = []
    for name in BACKING_BED_WAV_NAMES:
        p = base / name
        if not p.exists():
            continue
        audio, _sr = load_audio_mono(p, target_sr=target_sr, ffmpeg_bin=None)
        parts.append(audio)
    if not parts:
        return np.zeros(target_sr, dtype=np.float32)
    return np.concatenate(parts).astype(np.float32)


def _composite_with_backing(hit_segment: np.ndarray, backing: np.ndarray, rng: np.random.Generator) -> np.ndarray:
    """P1-scope compose helper (the phase brief names this; full
    compositing lives in P3's compose.py): mixes a raw per-hit segment with
    a randomly-gained, randomly-offset slice of non-drum backing material."""
    n = len(hit_segment)
    bed_src = backing
    if len(bed_src) < n:
        reps = int(np.ceil(n / max(1, len(bed_src))))
        bed_src = np.tile(bed_src, max(1, reps))
    start = int(rng.integers(0, max(1, len(bed_src) - n + 1)))
    bed = bed_src[start:start + n]
    gain_db = float(rng.uniform(*BACKING_GAIN_DB_RANGE))
    gain = float(10.0 ** (gain_db / 20.0))
    return (hit_segment + bed * gain).astype(np.float32)


def _load_egmd_dev(rng: np.random.Generator) -> List[PatchExample]:
    backing = _load_backing_bed()
    rows = sorted(_egmd_available_rows(split="dev"), key=lambda r: r["id"])
    out: List[PatchExample] = []
    for row in rows:
        audio_path = Path(row["audio_path"])
        midi_path = Path(row["midi_path"])
        if not audio_path.exists() or not midi_path.exists():
            continue
        truth = _egmd_load_drum_truth(midi_path)
        if not any(truth.values()):
            continue
        audio, sr = load_audio_mono(audio_path, target_sr=44100, ffmpeg_bin=None)
        all_times = sorted({round(t, 6) for times in truth.values() for t in times})
        feat_map = _batched_onset_features(audio, sr, all_times)
        for cls, times in truth.items():
            for t in sorted(times):
                key = round(t, 6)
                side = feat_map.get(key)
                if side is None:
                    side = _side_features(audio, sr, t)
                base_seg = _composite_with_backing(_patch_segment(audio, sr, t, 0.0), backing, rng)
                out.append(PatchExample(
                    mel=_mel_from_segment(base_seg, sr), side_features=side,
                    label=cls, source_id="egmd_dev", track_id=row["id"],
                    onset_time_sec=t, jitter_sec=0.0,
                ))
                jitter = float(rng.uniform(-JITTER_MS, JITTER_MS)) / 1000.0
                jit_seg = _composite_with_backing(_patch_segment(audio, sr, t, jitter), backing, rng)
                out.append(PatchExample(
                    mel=_mel_from_segment(jit_seg, sr), side_features=side,
                    label=cls, source_id="egmd_dev", track_id=row["id"],
                    onset_time_sec=t, jitter_sec=jitter,
                ))
    return out


# ---------------------------------------------------------------------------
# Source loader: manifold_own_kick
# ---------------------------------------------------------------------------


def _load_manifold_own_kick(rng: np.random.Generator) -> List[PatchExample]:
    fixtures_path = AUDIO_ANALYSIS_ROOT / "eval" / "fixtures.toml"
    fixtures = {f["id"]: f for f in load_fixtures(fixtures_path)}
    out: List[PatchExample] = []
    for fid in MANIFOLD_OWN_KICK_FIXTURE_IDS:
        fixture = fixtures.get(fid)
        if fixture is None:
            continue
        base_dir = _resolve_path(fixture["path"])
        mix_path = base_dir / "mix.wav"
        labels_path = _resolve_path(fixture["labels_path"])
        if not mix_path.exists() or not labels_path.exists():
            print(f"[dataset] manifold_own_kick {fid}: audio/labels missing, skipping", file=sys.stderr)
            continue
        truth = sorted(_load_kick_truth_csv(labels_path)["mix"])
        if not truth:
            continue
        audio, sr = load_audio_mono(mix_path, target_sr=44100, ffmpeg_bin=None)
        feat_map = _batched_onset_features(audio, sr, [round(t, 6) for t in truth])
        for t in truth:
            out.extend(_make_examples(rng, audio, sr, t, "kick", "manifold_own_kick", fid, feat_map))
    return out


# ---------------------------------------------------------------------------
# Source loader: self_render
# ---------------------------------------------------------------------------

# GM pitch -> D3 class, per self-rendered fixture. Pitch 39 (clap) in
# edm_kit_128bpm has no class of its own in D3's vocabulary and is
# deliberately dropped (folding it into kick/snare/hat/perc would mis-teach
# that class's boundary, not extend its coverage).
SELF_RENDER_PITCH_CLASS_BY_FIXTURE: Dict[str, Dict[int, str]] = {
    "kick_hat_128bpm": {36: "kick", 42: "hat"},
    "edm_kit_128bpm": {36: "kick", 38: "snare", 42: "hat", 45: "perc"},
}


def _load_self_render(rng: np.random.Generator) -> List[PatchExample]:
    base = DATA_ROOT / "self_render"
    out: List[PatchExample] = []

    for name, pitch_map in SELF_RENDER_PITCH_CLASS_BY_FIXTURE.items():
        wav = base / f"{name}.wav"
        truth_path = base / f"{name}_truth.json"
        if not (wav.exists() and truth_path.exists()):
            continue
        notes = json.loads(truth_path.read_text())
        by_class: Dict[str, List[float]] = {}
        for n in notes:
            cls = pitch_map.get(n["pitch"])
            if cls:
                by_class.setdefault(cls, []).append(n["start_sec"])
        if not by_class:
            continue
        audio, sr = load_audio_mono(wav, target_sr=44100, ffmpeg_bin=None)
        all_times = sorted({round(t, 6) for times in by_class.values() for t in times})
        feat_map = _batched_onset_features(audio, sr, all_times)
        for cls, times in sorted(by_class.items()):
            for t in sorted(times):
                out.extend(_make_examples(rng, audio, sr, t, cls, "self_render", name, feat_map))

    arp_wav = base / "arp_16th_128bpm.wav"
    arp_truth_path = base / "arp_16th_128bpm_truth.json"
    if arp_wav.exists() and arp_truth_path.exists():
        notes = json.loads(arp_truth_path.read_text())
        times = sorted(n["start_sec"] for n in notes)
        audio, sr = load_audio_mono(arp_wav, target_sr=44100, ffmpeg_bin=None)
        feat_map = _batched_onset_features(audio, sr, [round(t, 6) for t in times])
        for t in times:
            out.extend(_make_examples(rng, audio, sr, t, "synth", "self_render", "arp_16th_128bpm", feat_map))

    return out


# ---------------------------------------------------------------------------
# Manifest-driven corpus assembly
# ---------------------------------------------------------------------------

SOURCE_LOADERS: Dict[str, Callable[[np.random.Generator], List[PatchExample]]] = {
    "liveshow_dev": _load_liveshow_dev,
    "egmd_dev": _load_egmd_dev,
    "manifold_own_kick": _load_manifold_own_kick,
    "self_render": _load_self_render,
}


def load_sources(path: Path = DEFAULT_SOURCES_TOML) -> List[Dict[str, Any]]:
    with open(path, "rb") as f:
        data = tomllib.load(f)
    return data["source"]


def build_dataset(
    seed: int = DEFAULT_SEED,
    sources_toml: Path = DEFAULT_SOURCES_TOML,
    only_source_ids: Optional[Sequence[str]] = None,
) -> List[PatchExample]:
    """Deterministic given `seed`: sources are processed in sources.toml's
    own order, each loader iterates its files/tracks/onsets in sorted
    order, and every random draw (jitter, egmd_dev's backing gain/offset)
    comes from ONE seeded numpy.random.Generator threaded through in that
    fixed order -- two calls with the same seed produce identical output."""
    sources = load_sources(sources_toml)
    rng = np.random.default_rng(seed)
    out: List[PatchExample] = []
    for entry in sources:
        if only_source_ids is not None and entry["id"] not in only_source_ids:
            continue
        if entry["license"] not in ALLOWED_LICENSES:
            raise ValueError(f"source {entry['id']!r} has disallowed license {entry['license']!r}")
        loader = SOURCE_LOADERS.get(entry["loader"])
        if loader is None:
            raise ValueError(f"unknown loader {entry['loader']!r} for source {entry['id']!r}")
        print(f"[dataset] {entry['id']} ...", file=sys.stderr)
        examples = loader(rng)
        out.extend(examples)
        print(f"[dataset] {entry['id']}: {len(examples)} examples", file=sys.stderr)
    return out


def support_table(examples: Sequence[PatchExample]) -> Dict[str, Dict[str, int]]:
    table: Dict[str, Dict[str, int]] = {c: {"raw": 0, "augmented_total": 0} for c in CLASS_NAMES}
    for ex in examples:
        row = table.setdefault(ex.label, {"raw": 0, "augmented_total": 0})
        row["augmented_total"] += 1
        if ex.jitter_sec == 0.0:
            row["raw"] += 1
    return table


def first_patch_checksum(examples: Sequence[PatchExample]) -> str:
    if not examples:
        return ""
    return hashlib.sha256(examples[0].mel.tobytes()).hexdigest()


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    parser.add_argument("--report", action="store_true", help="print the per-class support table (default on)")
    args = parser.parse_args(argv)

    examples = build_dataset(seed=args.seed)
    table = support_table(examples)

    print(f"[dataset] total examples (base + jittered): {len(examples)}")
    print(f"{'class':<8} {'raw':>8} {'augmented_total':>16}")
    for cls in CLASS_NAMES:
        row = table.get(cls, {"raw": 0, "augmented_total": 0})
        print(f"{cls:<8} {row['raw']:>8} {row['augmented_total']:>16}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
