"""P4 §2 — round-1 (+ round-2 correction) knob sweep, DEV FIXTURES ONLY
(docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md P4 brief: "sweep round 1 (dev
fixtures ONLY: babyslakh + self_render + liveshow dev songs)"). HELDOUT IS
FORBIDDEN in every sweep run this module performs -- there is no parameter
anywhere in this file that accepts a heldout fixture id or the liveshow
heldout songs (liveshow_stagnate, liveshow_basalt); only
DEV_LIVESHOW_FIXTURES below is ever read, same structural guard convention
as eval/sweep.py's own module docstring.

This is a MEASUREMENT/SWEEP-ONLY module: it commits scoreboards flagging
the top 3 configs per class per knob family -- it does NOT choose a winner
("recommended"/"best" language is deliberately absent from every output
row; the orchestrator is the tuning judge, per this phase's brief).

ROUND-2 CORRECTION (orchestrator verdict on round 1): round 1's single
pooled mean-F1 aggregate silently mixed two incompatible truth qualities --
babyslakh/self_render/manifold_own MIDI/hand-labeled truth is DENSE (every
real event is labeled, so a false positive is a real precision error), while
liveshow's onset_truth.json layers are Peter's placed CLIP EDGES, a
deliberate SUBSET of real events (design doc addendum 2026-07-17: "onset
truth within a tolerance window" from manually placed clips, not an
exhaustive transcription) -- a "false positive" against that truth is very
often just a REAL, unlabeled event, so raw precision computed against it is
fiction. Folding a fictional liveshow precision into the same mean as
babyslakh's real precision made every round-1 aggregate untrustworthy (the
orchestrator's diagnosis: liveshow_pattern's snare F1 0.0 against n_truth=4
sparse labels, pooled with babyslakh_Track00003's genuine 1395-pred-vs-328-
truth over-detection, as if both numbers meant the same thing).

The fix: every fixture now carries `truth_type` ("dense" or
"sparse_visual"), and the two contribute differently to scoring (see
TruthType below and score_config_for_class's split). The headline metric
per class is the PAIR (dense mean F1, sparse mean recall) -- reported
together, never collapsed into one number. Ranking (top-3 per family) sorts
by dense F1 (the only number here immune to the label-sparsity problem);
sparse recall is reported alongside every row, never used as the sort key,
since it has no matching precision signal to combine into a single
objective -- stated explicitly rather than silently picked.

Corpus & one-time inference
----------------------------
Per D7 ("tuning iterates over cached arrays -- seconds per track, no model
re-runs"), each track's ADTOF activations / basic_pitch gate-pass notes are
computed EXACTLY ONCE (build_dev_corpus), then every sweep config is scored
by re-running only the pure-numpy precision_postprocessing pipeline over
that cached data -- no repeated model inference.

One-shot protection
---------------------
For every liveshow-sourced truth onset whose nearest same-class truth
neighbor is >= ONE_SHOT_ISOLATION_SEC away, recall is reported SEPARATELY
(one_shot_recall) alongside the aggregate -- design doc: "configs that kill
lone hits must be visibly marked, not hidden in aggregates." babyslakh/
self_render/manifold_own fixtures are dense, on-grid labeled material and
are not a meaningful source of "isolated impact moment" truth, so this
check is liveshow-only by construction (documented, not silently narrowed).
"""

from __future__ import annotations

import argparse
import datetime as dt
import itertools
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import numpy as np
import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval.paths import DATA_ROOT
from eval import metrics  # noqa: E402
from eval.baseline_scoreboard_p3 import BABYSLAKH_ROOT, CLASSES as ADTOF_CLASSES, _load_drum_truth  # noqa: E402
from eval.beat_scoring import LIVESHOW_SONG_FIXTURES, load_tempo_points  # noqa: E402
from eval.calibration import MANIFOLD_OWN_KICK_FIXTURE_IDS, apply_calibration, load_calibration  # noqa: E402
from eval.liveshow_extract import beats_to_seconds  # noqa: E402
from eval.run import AUDIO_ANALYSIS_ROOT, _load_kick_truth_csv, _resolve_path, load_fixtures  # noqa: E402
from manifold_audio.adtof_detection import detect_drums_adtof_activations  # noqa: E402
from manifold_audio.basic_pitch_detection import detect_notes_basic_pitch  # noqa: E402
from manifold_audio.audio_io import load_audio_mono  # noqa: E402
from manifold_audio.precision_postprocessing import (  # noqa: E402
    BeatGridRef,
    Candidate,
    MedianAdaptiveConfig,
    PrecisionConfig,
    ShapeGateConfig,
    extract_adtof_gate_candidates,
    extract_basic_pitch_gate_candidates,
    run_precision_pipeline,
)

# Dense: every real event is labeled (MIDI-aligned or hand-labeled kick
# onsets) -- a predicted event with no matching truth is a real precision
# error. Sparse-visual: truth is a deliberate SUBSET of real events (Peter's
# placed clip edges), so raw precision against it is not meaningful --
# see module docstring's round-2 correction.
DENSE = "dense"
SPARSE_VISUAL = "sparse_visual"

# How far (in beats) around any sparse-visual truth event a prediction must
# fall to count toward "active-passage precision" -- a restricted precision
# number that only penalizes over-firing NEAR a labeled passage, not
# anywhere Peter simply didn't happen to place a clip.
ACTIVE_PASSAGE_WINDOW_BEATS = 2.0

ONE_SHOT_ISOLATION_SEC = 2.0
BP_GATE_ONSET_THRESHOLD = 0.05  # permissive gate pass for basic_pitch (synth) -- knob (a) sweeps the real accept threshold on top of this
# basic_pitch's own default min_note_length (127.7ms) silently drops every
# note in a 128 BPM 16th-note arp (spacing 117.2ms < 127.7ms) -- found while
# building this corpus (self_render_arp_16th_128bpm scored 0 predictions at
# the default). D3's own ruling ("a 16th-note arp becomes sixteen clips a
# bar -- this is correct behaviour") makes this a real methodological
# requirement for the gate pass, not a knob-set parameter: 30ms is short
# enough for any 16th note down to ~496 BPM.
BP_GATE_MIN_NOTE_LENGTH_MS = 30.0

# Only DEV liveshow songs -- the heldout pair (liveshow_stagnate,
# liveshow_basalt) is never referenced anywhere in this file.
DEV_LIVESHOW_FIXTURES = [fx for fx in LIVESHOW_SONG_FIXTURES if fx["split"] == "dev"]

LIVESHOW_TRUTH_CLASSES = {"kick", "snare", "hat", "perc", "synth"}


@dataclass
class TrackData:
    id: str
    domain: str
    source: str  # "babyslakh" | "self_render" | "liveshow" | "manifold_own"
    audio: np.ndarray
    sr: int
    audio_path: str
    activations: Optional[np.ndarray]
    fps: int
    basic_pitch_notes: Optional[List[Tuple[float, float, int, float]]]
    truth_type: str = DENSE
    truth_by_class: Dict[str, List[float]] = field(default_factory=dict)
    one_shot_truth_by_class: Dict[str, List[float]] = field(default_factory=dict)
    grid: Optional[BeatGridRef] = None
    # BUG-235 scoring-seam correction (per fixture, kick only) -- applied to
    # PREDICTIONS before matching against truth, never inside the detector.
    calibration_offset_sec: Dict[str, float] = field(default_factory=dict)


def _isolated_truth(times: List[float], isolation_sec: float = ONE_SHOT_ISOLATION_SEC) -> List[float]:
    """Truth onsets whose nearest same-class neighbor (either side) is
    >= isolation_sec away -- "one-shot/impact moments" per the design doc."""
    if len(times) < 1:
        return []
    ts = sorted(times)
    out = []
    for i, t in enumerate(ts):
        left_gap = t - ts[i - 1] if i > 0 else float("inf")
        right_gap = ts[i + 1] - t if i < len(ts) - 1 else float("inf")
        if min(left_gap, right_gap) >= isolation_sec:
            out.append(t)
    return out


def active_passage_filter(pred_times: List[float], truth_times: List[float], window_sec: float) -> List[float]:
    """Predictions within window_sec of ANY truth time (either side) --
    the "active-passage" subset a sparse-visual fixture's precision is
    restricted to (round-2 correction: elsewhere, an unmatched prediction
    is very plausibly a real, unlabeled event, not a false positive)."""
    if not truth_times or not pred_times:
        return []
    truth_arr = np.asarray(sorted(truth_times), dtype=np.float64)
    out = []
    for p in pred_times:
        idx = int(np.searchsorted(truth_arr, p))
        near = False
        if idx > 0 and abs(p - truth_arr[idx - 1]) <= window_sec:
            near = True
        if not near and idx < truth_arr.size and abs(truth_arr[idx] - p) <= window_sec:
            near = True
        if near:
            out.append(p)
    return out


# ---------------------------------------------------------------------------
# Corpus builders
# ---------------------------------------------------------------------------


def _build_babyslakh_tracks(max_tracks: int) -> List[TrackData]:
    root = BABYSLAKH_ROOT
    if not root.is_dir():
        print(f"[sweep_p4] babyslakh root not found: {root}", file=sys.stderr)
        return []
    track_dirs = sorted(p for p in root.iterdir() if p.is_dir() and p.name.startswith("Track"))
    out: List[TrackData] = []
    for td in track_dirs:
        if len(out) >= max_tracks:
            break
        meta_path = td / "metadata.yaml"
        if not meta_path.exists():
            continue
        meta = yaml.safe_load(meta_path.read_text())
        drum_stems = [k for k, v in (meta.get("stems") or {}).items() if v.get("is_drum")]
        if not drum_stems:
            continue
        mix_path = td / "mix.wav"
        if not mix_path.exists():
            continue
        truth = _load_drum_truth(td, drum_stems[0])
        if not any(truth.values()):
            continue
        print(f"[sweep_p4] babyslakh {td.name}: ADTOF inference ...", file=sys.stderr)
        activations, fps = detect_drums_adtof_activations(str(mix_path))
        audio, sr = load_audio_mono(mix_path, target_sr=44100, ffmpeg_bin=None)
        out.append(TrackData(
            id=f"babyslakh_{td.name}", domain="other", source="babyslakh",
            audio=audio, sr=sr, audio_path=str(mix_path),
            activations=activations, fps=fps, basic_pitch_notes=None,
            truth_type=DENSE,
            truth_by_class={c: truth.get(c, []) for c in ADTOF_CLASSES},
        ))
    return out


def _build_manifold_own_kick_tracks() -> List[TrackData]:
    """Round-2 addition (orchestrator instruction): the 5 calibrated
    manifold_own kick fixtures join the KICK dev corpus, dense (hand-labeled
    per BUG-235's own 25%-envelope-walkback convention -- every kick in
    these 5 tracks is labeled, so this is real precision, not sparse-visual).
    Calibration is applied at THIS scoring seam (calibration_offset_sec),
    never inside the detector."""
    fixtures_path = AUDIO_ANALYSIS_ROOT / "eval" / "fixtures.toml"
    fixtures = {f["id"]: f for f in load_fixtures(fixtures_path)}
    calibration = load_calibration()
    out: List[TrackData] = []
    for fid in MANIFOLD_OWN_KICK_FIXTURE_IDS:
        fixture = fixtures.get(fid)
        if fixture is None:
            continue
        base_dir = _resolve_path(fixture["path"])
        mix_path = base_dir / "mix.wav"
        if not mix_path.exists():
            print(f"[sweep_p4] manifold_own {fid} audio missing, skipping: {mix_path}", file=sys.stderr)
            continue
        truth_mix = _load_kick_truth_csv(_resolve_path(fixture["labels_path"]))["mix"]
        print(f"[sweep_p4] manifold_own {fid}: ADTOF inference ...", file=sys.stderr)
        activations, fps = detect_drums_adtof_activations(str(mix_path))
        audio, sr = load_audio_mono(mix_path, target_sr=44100, ffmpeg_bin=None)
        out.append(TrackData(
            id=fid, domain=fixture.get("domain", "electronic"), source="manifold_own",
            audio=audio, sr=sr, audio_path=str(mix_path),
            activations=activations, fps=fps, basic_pitch_notes=None,
            truth_type=DENSE,
            truth_by_class={"kick": truth_mix},
            grid=BeatGridRef(bpm=float(fixture.get("bpm", 128.0)), anchor_sec=0.0),
            calibration_offset_sec={"kick": calibration.get(fid, 0.0)},
        ))
    return out


def _build_self_render_tracks() -> List[TrackData]:
    base = DATA_ROOT / "self_render"
    out: List[TrackData] = []

    kick_hat_wav = base / "kick_hat_128bpm.wav"
    kick_hat_truth = json.loads((base / "kick_hat_128bpm_truth.json").read_text())
    if kick_hat_wav.exists():
        print("[sweep_p4] self_render kick_hat_128bpm: ADTOF inference ...", file=sys.stderr)
        activations, fps = detect_drums_adtof_activations(str(kick_hat_wav))
        audio, sr = load_audio_mono(kick_hat_wav, target_sr=44100, ffmpeg_bin=None)
        truth = {"kick": [], "snare": [], "hat": [], "perc": []}
        for note in kick_hat_truth:
            if note["pitch"] == 36:
                truth["kick"].append(note["start_sec"])
            elif note["pitch"] == 42:
                truth["hat"].append(note["start_sec"])
        out.append(TrackData(
            id="self_render_kick_hat_128bpm", domain="electronic", source="self_render",
            audio=audio, sr=sr, audio_path=str(kick_hat_wav),
            activations=activations, fps=fps, basic_pitch_notes=None,
            truth_type=DENSE,
            truth_by_class=truth,
            grid=BeatGridRef(bpm=128.0, anchor_sec=0.0),
        ))

    arp_wav = base / "arp_16th_128bpm.wav"
    arp_truth = json.loads((base / "arp_16th_128bpm_truth.json").read_text())
    if arp_wav.exists():
        print("[sweep_p4] self_render arp_16th_128bpm: basic_pitch gate pass ...", file=sys.stderr)
        notes = detect_notes_basic_pitch(str(arp_wav), onset_threshold=BP_GATE_ONSET_THRESHOLD, min_note_length=BP_GATE_MIN_NOTE_LENGTH_MS)
        audio, sr = load_audio_mono(arp_wav, target_sr=44100, ffmpeg_bin=None)
        synth_truth = sorted(n["start_sec"] for n in arp_truth)
        out.append(TrackData(
            id="self_render_arp_16th_128bpm", domain="electronic", source="self_render",
            audio=audio, sr=sr, audio_path=str(arp_wav),
            activations=None, fps=100, basic_pitch_notes=notes,
            truth_type=DENSE,
            truth_by_class={"synth": synth_truth},
            grid=BeatGridRef(bpm=128.0, anchor_sec=0.0),
        ))
    return out


def _liveshow_song_truth(fixture: Dict[str, Any], tempo_points, pad_sec: float = 0.5) -> Tuple[Dict[str, List[float]], float, float]:
    """Per-class truth times relative to the CACHED slice wav's own t=0
    (which is seg_start_sec - pad_sec, confirmed 2026-07-17 against the
    on-disk slices' durations). Returns (truth_by_class, seg_start_sec, seg_end_sec)."""
    onset_truth = json.loads((Path(__file__).resolve().parent / "liveshow_labels" / "onset_truth.json").read_text())
    start_beat, end_beat = tuple(fixture["beat_range"])
    seg_start_sec = beats_to_seconds(start_beat, tempo_points)
    seg_end_sec = beats_to_seconds(end_beat, tempo_points)

    truth: Dict[str, List[float]] = {c: [] for c in LIVESHOW_TRUTH_CLASSES}
    for layer in onset_truth:
        cls = layer["instrument"]
        if cls not in LIVESHOW_TRUTH_CLASSES:
            continue
        for edge_abs in layer["edges_secs_in_audio"]:
            if seg_start_sec <= edge_abs < seg_end_sec:
                truth[cls].append(edge_abs - seg_start_sec + pad_sec)
    for c in truth:
        truth[c].sort()
    return truth, seg_start_sec, seg_end_sec


def _liveshow_song_bpm(fixture: Dict[str, Any], tempo_points) -> float:
    start_beat, end_beat = tuple(fixture["beat_range"])
    t0 = beats_to_seconds(start_beat, tempo_points)
    t1 = beats_to_seconds(start_beat + 4.0, tempo_points)  # one bar
    if t0 is None or t1 is None or t1 <= t0:
        return 128.0
    return 4.0 * 60.0 / (t1 - t0)


def _build_liveshow_dev_tracks() -> List[TrackData]:
    slices_dir = DATA_ROOT / "liveshow_song_slices"
    tempo_points = load_tempo_points()
    out: List[TrackData] = []
    for fx in DEV_LIVESHOW_FIXTURES:
        wav_path = slices_dir / f"{fx['id']}.wav"
        if not wav_path.exists():
            print(f"[sweep_p4] liveshow slice missing, skipping: {wav_path}", file=sys.stderr)
            continue
        truth, seg_start, _seg_end = _liveshow_song_truth(fx, tempo_points)
        if not any(truth.values()):
            continue
        print(f"[sweep_p4] {fx['id']}: ADTOF inference ...", file=sys.stderr)
        activations, fps = detect_drums_adtof_activations(str(wav_path))
        print(f"[sweep_p4] {fx['id']}: basic_pitch gate pass ...", file=sys.stderr)
        notes = detect_notes_basic_pitch(str(wav_path), onset_threshold=BP_GATE_ONSET_THRESHOLD, min_note_length=BP_GATE_MIN_NOTE_LENGTH_MS)
        audio, sr = load_audio_mono(wav_path, target_sr=44100, ffmpeg_bin=None)
        one_shot = {c: _isolated_truth(t) for c, t in truth.items()}
        bpm = _liveshow_song_bpm(fx, tempo_points)
        out.append(TrackData(
            id=fx["id"], domain=fx.get("domain", "electronic"), source="liveshow",
            audio=audio, sr=sr, audio_path=str(wav_path),
            activations=activations, fps=fps, basic_pitch_notes=notes,
            truth_type=SPARSE_VISUAL,
            truth_by_class=truth, one_shot_truth_by_class=one_shot,
            grid=BeatGridRef(bpm=bpm, anchor_sec=0.0),
        ))
    return out


def build_dev_corpus(max_babyslakh_tracks: int = 8, include_manifold_own_kick: bool = True) -> List[TrackData]:
    corpus: List[TrackData] = []
    corpus.extend(_build_babyslakh_tracks(max_babyslakh_tracks))
    corpus.extend(_build_self_render_tracks())
    corpus.extend(_build_liveshow_dev_tracks())
    if include_manifold_own_kick:
        corpus.extend(_build_manifold_own_kick_tracks())
    return corpus


# ---------------------------------------------------------------------------
# Scoring one config, one class
# ---------------------------------------------------------------------------


def _gate_candidates_for_track(track: TrackData, class_name: str, config: PrecisionConfig) -> Optional[List[Candidate]]:
    if class_name == "synth":
        if track.basic_pitch_notes is None:
            return None
        return extract_basic_pitch_gate_candidates(track.basic_pitch_notes, config)
    if track.activations is None:
        return None
    return extract_adtof_gate_candidates(track.activations, track.fps, class_name, config)


def score_config_for_class(corpus: List[TrackData], class_name: str, config: PrecisionConfig) -> Dict[str, Any]:
    """Round-2 corrected scoring: DENSE fixtures contribute full P/R/F1;
    SPARSE_VISUAL fixtures contribute RECALL ONLY to the headline aggregate,
    plus a separately-reported "active-passage precision" (precision
    restricted to predictions within ACTIVE_PASSAGE_WINDOW_BEATS of any
    truth event) -- never raw precision/F1 (see module docstring). The
    headline per class is (dense_mean_f1, sparse_mean_recall) as a pair."""
    dense_rows: List[Dict[str, Any]] = []
    sparse_rows: List[Dict[str, Any]] = []
    one_shot_matched = 0
    one_shot_total = 0

    for track in corpus:
        truth = track.truth_by_class.get(class_name)
        if not truth:
            continue
        gate = _gate_candidates_for_track(track, class_name, config)
        if gate is None:
            continue
        pred = run_precision_pipeline(
            {class_name: gate}, config, audio=track.audio, sr=track.sr, grid=track.grid,
        )[class_name]
        offset = track.calibration_offset_sec.get(class_name, 0.0)
        if offset:
            pred = apply_calibration(pred, offset)

        if track.truth_type == DENSE:
            prf = metrics.event_prf(pred, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
            dense_rows.append({
                "id": track.id, "domain": track.domain, "source": track.source,
                "n_pred": len(pred), "n_truth": len(truth), f"{class_name}_f1": prf.f1,
                "precision": prf.precision, "recall": prf.recall,
            })
            continue

        # SPARSE_VISUAL: recall against the full truth set (legitimate --
        # recall only asks "did we find the labeled ones", unaffected by
        # unlabeled real events elsewhere); active-passage precision
        # restricted to predictions near a labeled event.
        recall_prf = metrics.event_prf(pred, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
        active_precision = None
        n_pred_in_window = None
        if track.grid is not None and track.grid.bpm > 0:
            window_sec = ACTIVE_PASSAGE_WINDOW_BEATS * 60.0 / track.grid.bpm
            restricted_pred = active_passage_filter(pred, truth, window_sec)
            n_pred_in_window = len(restricted_pred)
            active_prf = metrics.event_prf(restricted_pred, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
            active_precision = active_prf.precision
        sparse_row = {
            "id": track.id, "domain": track.domain, "source": track.source,
            "n_pred": len(pred), "n_truth": len(truth),
            "recall": recall_prf.recall,
            "active_passage_precision": active_precision,
            "n_pred_in_active_window": n_pred_in_window,
        }
        one_shot_truth = track.one_shot_truth_by_class.get(class_name, [])
        if one_shot_truth:
            oneshot_prf = metrics.event_prf(pred, one_shot_truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
            # Recall specifically on the isolated-truth subset: how many of
            # THOSE truth onsets got a matched prediction (precision here
            # would be dominated by unrelated predictions matching non-
            # one-shot truth, so only recall is reported, per the brief).
            sparse_row["one_shot_recall"] = oneshot_prf.recall
            sparse_row["one_shot_n_truth"] = len(one_shot_truth)
            one_shot_matched += oneshot_prf.true_positives
            one_shot_total += len(one_shot_truth)
        sparse_rows.append(sparse_row)

    dense_valid = [r for r in dense_rows if r["n_truth"] > 0]
    sparse_valid = [r for r in sparse_rows if r["n_truth"] > 0]

    dense_mean_f1 = float(np.mean([r[f"{class_name}_f1"] for r in dense_valid])) if dense_valid else None
    dense_by_domain = metrics.domain_aggregate(dense_valid, f"{class_name}_f1") if dense_valid else {}

    sparse_mean_recall = float(np.mean([r["recall"] for r in sparse_valid])) if sparse_valid else None
    sparse_active_precisions = [r["active_passage_precision"] for r in sparse_valid if r["active_passage_precision"] is not None]
    sparse_mean_active_precision = float(np.mean(sparse_active_precisions)) if sparse_active_precisions else None

    one_shot_recall_overall = (one_shot_matched / one_shot_total) if one_shot_total > 0 else None

    return {
        "params": None,  # filled in by the caller
        "dense": {
            "per_track": dense_rows,
            "mean_f1": dense_mean_f1,
            "by_domain": dense_by_domain,
            "n_tracks_scored": len(dense_valid),
        },
        "sparse_visual": {
            "per_track": sparse_rows,
            "mean_recall": sparse_mean_recall,
            "mean_active_passage_precision": sparse_mean_active_precision,
            "n_tracks_scored": len(sparse_valid),
        },
        "headline": {"dense_f1": dense_mean_f1, "sparse_recall": sparse_mean_recall},
        "one_shot_recall_overall": one_shot_recall_overall,
        "one_shot_n_truth_total": one_shot_total,
        # Back-compat convenience field some callers key ranking off of --
        # ALWAYS dense F1 (see module docstring: never sparse recall alone).
        "mean_f1": dense_mean_f1,
    }


# ---------------------------------------------------------------------------
# Coarse per-family sweeps
# ---------------------------------------------------------------------------


def _clone_config(config: PrecisionConfig) -> PrecisionConfig:
    import copy
    return copy.deepcopy(config)


def sweep_threshold(corpus: List[TrackData], class_name: str, base: PrecisionConfig, factors: List[float]) -> List[Dict[str, Any]]:
    default = base.thresholds[class_name]
    results = []
    for factor in factors:
        cfg = _clone_config(base)
        cfg.thresholds[class_name] = default * factor
        row = score_config_for_class(corpus, class_name, cfg)
        row["params"] = {"knob": "threshold_factor", "factor": factor, "threshold": cfg.thresholds[class_name]}
        results.append(row)
    return results


def sweep_refractory(corpus: List[TrackData], class_name: str, base: PrecisionConfig, values_ms: List[float]) -> List[Dict[str, Any]]:
    results = []
    for ms in values_ms:
        cfg = _clone_config(base)
        cfg.refractory_ms[class_name] = ms
        row = score_config_for_class(corpus, class_name, cfg)
        row["params"] = {"knob": "refractory_ms", "value": ms}
        results.append(row)
    return results


def sweep_median_adaptive(corpus: List[TrackData], class_name: str, base: PrecisionConfig, configs: List[Optional[Tuple[float, float]]]) -> List[Dict[str, Any]]:
    """configs: list of (thresh_factor, thresh_delta) or None (= disabled, the default)."""
    results = []
    for entry in configs:
        cfg = _clone_config(base)
        if entry is None:
            cfg.median_adaptive[class_name] = MedianAdaptiveConfig(enabled=False)
            label = "disabled"
        else:
            factor, delta = entry
            cfg.median_adaptive[class_name] = MedianAdaptiveConfig(enabled=True, thresh_factor=factor, thresh_delta=delta)
            label = f"factor={factor},delta={delta}"
        row = score_config_for_class(corpus, class_name, cfg)
        row["params"] = {"knob": "median_adaptive", "config": label}
        results.append(row)
    return results


def sweep_cofire(corpus: List[TrackData], class_name: str, base: PrecisionConfig, boosts: List[float]) -> List[Dict[str, Any]]:
    results = []
    for boost in boosts:
        cfg = _clone_config(base)
        if class_name == "snare":
            cfg.cofire_boost_snare = boost
        elif class_name in ("kick", "hat"):
            cfg.cofire_solo_boost[class_name] = boost
        else:
            continue
        row = score_config_for_class(corpus, class_name, cfg)
        row["params"] = {"knob": "cofire_boost", "value": boost}
        results.append(row)
    return results


def sweep_shape_gate(corpus: List[TrackData], class_name: str, base: PrecisionConfig, values: List[Optional[float]]) -> List[Dict[str, Any]]:
    """values: floor (kick/snare) or ceiling (hat) thresholds; None = gate disabled (default)."""
    results = []
    for v in values:
        cfg = _clone_config(base)
        if v is None:
            cfg.shape_gates[class_name] = ShapeGateConfig(enabled=False)
            label = "disabled"
        elif class_name == "hat":
            cfg.shape_gates[class_name] = ShapeGateConfig(enabled=True, ceiling=v)
            label = f"ceiling={v}"
        else:
            cfg.shape_gates[class_name] = ShapeGateConfig(enabled=True, floor=v)
            label = f"floor={v}"
        row = score_config_for_class(corpus, class_name, cfg)
        row["params"] = {"knob": "shape_gate", "config": label}
        results.append(row)
    return results


def sweep_beat_phase(corpus: List[TrackData], class_name: str, base: PrecisionConfig, strengths: List[float]) -> List[Dict[str, Any]]:
    results = []
    for s in strengths:
        cfg = _clone_config(base)
        cfg.beat_phase_strength[class_name] = s
        row = score_config_for_class(corpus, class_name, cfg)
        row["params"] = {"knob": "beat_phase_strength", "value": s}
        results.append(row)
    return results


FAMILIES_BY_CLASS = {
    "kick": ("threshold", "refractory", "median_adaptive", "cofire", "shape_gate", "beat_phase"),
    "snare": ("threshold", "refractory", "median_adaptive", "cofire", "shape_gate", "beat_phase"),
    "hat": ("threshold", "refractory", "median_adaptive", "cofire", "shape_gate"),
    "perc": ("threshold", "refractory", "median_adaptive", "beat_phase"),
    "synth": ("threshold", "refractory"),
}

COARSE_GRIDS: Dict[str, Any] = {
    "threshold": [0.7, 0.85, 1.0, 1.15, 1.3],
    "refractory": [0.0, 15.0, 30.0, 50.0, 80.0],
    "median_adaptive": [None, (1.0, 0.02), (1.3, 0.03), (0.8, 0.01)],
    "cofire": [1.0, 1.15, 1.3, 1.5],
    "shape_gate_floor": [None, 0.3, 0.5, 0.7],
    "shape_gate_ceiling": [None, 5.0, 3.0, 2.0],
    "beat_phase": [0.0, 0.1, 0.2, 0.3],
}

# Round-2 "hat extras" (orchestrator instruction #3): round 1's coarse
# threshold grid bottomed out at factor 0.7 and it was still the single
# biggest dense-F1 gain found for hat -- extend lower and finer-grained.
# Values are FACTORS of the default threshold (0.18), matching
# sweep_threshold's own convention, chosen so factor*0.18 spans ~0.09-0.153
# (the brief's literal "0.5-0.85" read as raw activation values would be
# far ABOVE today's 0.18 default, i.e. stricter/opposite of "extend lower" --
# this is the coherent reading).
HAT_EXTRA_THRESHOLD_FACTORS = [0.5, 0.55, 0.6, 0.65, 0.7, 0.75, 0.8, 0.85]
HAT_EXTRA_COFIRE_BOOSTS = [1.3, 1.4, 1.5, 1.6, 1.7]


def run_family_sweep(corpus: List[TrackData], class_name: str, family: str, base: PrecisionConfig) -> List[Dict[str, Any]]:
    if family == "threshold":
        grid = HAT_EXTRA_THRESHOLD_FACTORS if class_name == "hat" else COARSE_GRIDS["threshold"]
        return sweep_threshold(corpus, class_name, base, grid)
    if family == "refractory":
        return sweep_refractory(corpus, class_name, base, COARSE_GRIDS["refractory"])
    if family == "median_adaptive":
        return sweep_median_adaptive(corpus, class_name, base, COARSE_GRIDS["median_adaptive"])
    if family == "cofire":
        grid = HAT_EXTRA_COFIRE_BOOSTS if class_name == "hat" else COARSE_GRIDS["cofire"]
        return sweep_cofire(corpus, class_name, base, grid)
    if family == "shape_gate":
        grid = COARSE_GRIDS["shape_gate_ceiling"] if class_name == "hat" else COARSE_GRIDS["shape_gate_floor"]
        return sweep_shape_gate(corpus, class_name, base, grid)
    if family == "beat_phase":
        return sweep_beat_phase(corpus, class_name, base, COARSE_GRIDS["beat_phase"])
    raise ValueError(f"unknown family {family}")


def _is_dead_knob(rows: List[Dict[str, Any]]) -> bool:
    """Dead iff BOTH the dense-F1 series AND the sparse-recall series show
    no variation across the whole sweep (a knob could move one truth-type
    and not the other -- e.g. a class with sparse-only or dense-only
    coverage for some tracks)."""
    dense_f1s = [r["dense"]["mean_f1"] for r in rows if r["dense"]["mean_f1"] is not None]
    sparse_recalls = [r["sparse_visual"]["mean_recall"] for r in rows if r["sparse_visual"]["mean_recall"] is not None]
    dense_dead = len(dense_f1s) < 2 or (max(dense_f1s) - min(dense_f1s)) < 1e-6
    sparse_dead = len(sparse_recalls) < 2 or (max(sparse_recalls) - min(sparse_recalls)) < 1e-6
    return dense_dead and sparse_dead


def build_class_scoreboard(corpus: List[TrackData], class_name: str) -> Dict[str, Any]:
    base = PrecisionConfig()
    baseline_row = score_config_for_class(corpus, class_name, base)
    baseline_row["params"] = {"knob": "baseline", "config": "all defaults"}

    families = FAMILIES_BY_CLASS[class_name]
    per_family: Dict[str, Any] = {}
    family_best_delta: Dict[str, float] = {}
    for family in families:
        rows = run_family_sweep(corpus, class_name, family, base)
        dead = _is_dead_knob(rows)
        deltas = [((r["mean_f1"] or 0.0) - (baseline_row["mean_f1"] or 0.0)) for r in rows]
        best_delta = max(deltas) if deltas else 0.0
        family_best_delta[family] = best_delta
        top3 = sorted(rows, key=lambda r: (r["mean_f1"] if r["mean_f1"] is not None else -1.0), reverse=True)[:3]
        per_family[family] = {
            "all_configs": rows,
            "top_3_by_dev_f1": top3,
            "dead_knob": dead,
            "best_delta_vs_baseline": best_delta,
        }

    # Joint sweep of the two families with the largest observed positive
    # delta (factual selection criterion, not a tuning recommendation --
    # the joint grid itself is still just more measurement rows).
    ranked_families = sorted(family_best_delta.items(), key=lambda kv: kv[1], reverse=True)
    top2_families = [f for f, _ in ranked_families[:2]]
    joint_rows = []
    if len(top2_families) == 2:
        f1_name, f2_name = top2_families
        f1_top = per_family[f1_name]["top_3_by_dev_f1"][:2]
        f2_top = per_family[f2_name]["top_3_by_dev_f1"][:2]
        for r1, r2 in itertools.product(f1_top, f2_top):
            cfg = _clone_config(base)
            _apply_family_params(cfg, class_name, f1_name, r1["params"])
            _apply_family_params(cfg, class_name, f2_name, r2["params"])
            row = score_config_for_class(corpus, class_name, cfg)
            row["params"] = {"knob": "joint", "families": [f1_name, f2_name], "from": [r1["params"], r2["params"]]}
            joint_rows.append(row)

    return {
        "class": class_name,
        "baseline": baseline_row,
        "per_family": per_family,
        "top2_families_by_delta": top2_families,
        "joint_top2_sweep": sorted(joint_rows, key=lambda r: (r["mean_f1"] if r["mean_f1"] is not None else -1.0), reverse=True),
    }


def _apply_family_params(cfg: PrecisionConfig, class_name: str, family: str, params: Dict[str, Any]) -> None:
    if family == "threshold":
        cfg.thresholds[class_name] = params["threshold"]
    elif family == "refractory":
        cfg.refractory_ms[class_name] = params["value"]
    elif family == "median_adaptive":
        label = params["config"]
        if label == "disabled":
            cfg.median_adaptive[class_name] = MedianAdaptiveConfig(enabled=False)
        else:
            factor_s, delta_s = label.split(",")
            factor = float(factor_s.split("=")[1])
            delta = float(delta_s.split("=")[1])
            cfg.median_adaptive[class_name] = MedianAdaptiveConfig(enabled=True, thresh_factor=factor, thresh_delta=delta)
    elif family == "cofire":
        if class_name == "snare":
            cfg.cofire_boost_snare = params["value"]
        else:
            cfg.cofire_solo_boost[class_name] = params["value"]
    elif family == "shape_gate":
        label = params["config"]
        if label == "disabled":
            cfg.shape_gates[class_name] = ShapeGateConfig(enabled=False)
        elif label.startswith("ceiling"):
            cfg.shape_gates[class_name] = ShapeGateConfig(enabled=True, ceiling=float(label.split("=")[1]))
        else:
            cfg.shape_gates[class_name] = ShapeGateConfig(enabled=True, floor=float(label.split("=")[1]))
    elif family == "beat_phase":
        cfg.beat_phase_strength[class_name] = params["value"]


def hat_oneshot_activation_diagnostic(corpus: List[TrackData]) -> List[Dict[str, Any]]:
    """Round-2 instruction #3: for every liveshow one-shot hat truth time,
    dump the raw ADTOF hat activation curve's value at that timestamp (and
    a small +/-30ms local max, since a label's exact placement can sit a
    frame or two off the model's own picked peak) -- tells us whether the
    0.0 one-shot recall found in every round-1 config is because the
    activation is essentially undetectable there, or just under-threshold
    (fixable by knob (a))."""
    out = []
    default_threshold = PrecisionConfig().thresholds["hat"]
    for track in corpus:
        if track.activations is None:
            continue
        one_shot_hats = track.one_shot_truth_by_class.get("hat", [])
        if not one_shot_hats:
            continue
        hat_curve = np.maximum(track.activations[:, 3], track.activations[:, 4])
        for t in one_shot_hats:
            frame_idx = int(round(t * track.fps))
            frame_idx = max(0, min(frame_idx, len(hat_curve) - 1))
            lo = max(0, frame_idx - 3)
            hi = min(len(hat_curve), frame_idx + 4)
            local_max = float(np.max(hat_curve[lo:hi]))
            out.append({
                "track": track.id,
                "truth_time_sec": t,
                "activation_at_truth_frame": float(hat_curve[frame_idx]),
                "activation_local_max_pm30ms": local_max,
                "default_threshold": default_threshold,
                "clears_default_threshold": local_max >= default_threshold,
            })
    return out


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--max-babyslakh-tracks", type=int, default=8)
    # synth EXCLUDED this round (orchestrator instruction #4): round 1 had
    # only n=1 dev track with synth truth (self_render_arp_16th_128bpm) --
    # a real coverage gap, not swept until more dev synth truth exists.
    parser.add_argument("--classes", nargs="+", default=[c for c in FAMILIES_BY_CLASS if c != "synth"])
    parser.add_argument("--out-dir", type=Path, default=AUDIO_ANALYSIS_ROOT / "eval" / "scoreboard")
    args = parser.parse_args(argv)

    if "synth" in args.classes:
        print("[sweep_p4] synth EXCLUDED this round (n=1 dev coverage gap) -- remove --classes override to honor this", file=sys.stderr)
        args.classes = [c for c in args.classes if c != "synth"]

    print("[sweep_p4] building dev corpus (one-time inference per track) ...", file=sys.stderr)
    corpus = build_dev_corpus(max_babyslakh_tracks=args.max_babyslakh_tracks)
    print(f"[sweep_p4] corpus: {len(corpus)} tracks", file=sys.stderr)

    args.out_dir.mkdir(parents=True, exist_ok=True)
    corpus_summary = [{"id": t.id, "domain": t.domain, "source": t.source, "truth_type": t.truth_type} for t in corpus]

    for class_name in args.classes:
        print(f"[sweep_p4] class={class_name} ...", file=sys.stderr)
        scoreboard = build_class_scoreboard(corpus, class_name)
        scoreboard["generated_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
        scoreboard["corpus_summary"] = corpus_summary
        out_path = args.out_dir / f"p4_sweep_r1_{class_name}.json"
        out_path.write_text(json.dumps(scoreboard, indent=2))
        headline = scoreboard["baseline"]["headline"]
        print(f"[sweep_p4] wrote {out_path} (baseline headline dense_f1={headline['dense_f1']}, sparse_recall={headline['sparse_recall']})", file=sys.stderr)

    if "hat" in args.classes:
        print("[sweep_p4] hat one-shot activation diagnostic ...", file=sys.stderr)
        dump = hat_oneshot_activation_diagnostic(corpus)
        dump_path = args.out_dir / "p4_hat_oneshot_activation_dump.json"
        dump_path.write_text(json.dumps({"generated_at": dt.datetime.now(dt.timezone.utc).isoformat(), "rows": dump}, indent=2))
        print(f"[sweep_p4] wrote {dump_path}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
