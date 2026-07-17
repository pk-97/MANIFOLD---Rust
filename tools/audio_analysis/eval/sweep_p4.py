"""P4 §2 — round-1 knob sweep, DEV FIXTURES ONLY (docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md
P4 brief: "sweep round 1 (dev fixtures ONLY: babyslakh + self_render +
liveshow dev songs)"). HELDOUT IS FORBIDDEN in every sweep run this module
performs -- there is no parameter anywhere in this file that accepts a
heldout fixture id or the liveshow heldout songs (liveshow_stagnate,
liveshow_basalt); only DEV_LIVESHOW_FIXTURES below is ever read, same
structural guard convention as eval/sweep.py's own module docstring.

This is a MEASUREMENT/SWEEP-ONLY module: it commits scoreboards flagging
the top 3 configs per class per knob family -- it does NOT choose a winner
("recommended"/"best" language is deliberately absent from every output
row; the orchestrator is the tuning judge, per this phase's brief).

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
(one_shot_recall) alongside the aggregate P/R/F1 -- design doc: "configs
that kill lone hits must be visibly marked, not hidden in aggregates."
babyslakh/self_render fixtures are dense, on-grid MIDI-locked material and
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

from eval import metrics  # noqa: E402
from eval.baseline_scoreboard_p3 import BABYSLAKH_ROOT, CLASSES as ADTOF_CLASSES, _load_drum_truth  # noqa: E402
from eval.beat_scoring import LIVESHOW_SONG_FIXTURES, load_tempo_points  # noqa: E402
from eval.liveshow_extract import beats_to_seconds  # noqa: E402
from eval.run import AUDIO_ANALYSIS_ROOT  # noqa: E402
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
    source: str  # "babyslakh" | "self_render" | "liveshow"
    audio: np.ndarray
    sr: int
    audio_path: str
    activations: Optional[np.ndarray]
    fps: int
    basic_pitch_notes: Optional[List[Tuple[float, float, int, float]]]
    truth_by_class: Dict[str, List[float]] = field(default_factory=dict)
    one_shot_truth_by_class: Dict[str, List[float]] = field(default_factory=dict)
    grid: Optional[BeatGridRef] = None


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
            truth_by_class={c: truth.get(c, []) for c in ADTOF_CLASSES},
        ))
    return out


def _build_self_render_tracks() -> List[TrackData]:
    base = AUDIO_ANALYSIS_ROOT / "eval" / "data" / "self_render"
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
    slices_dir = AUDIO_ANALYSIS_ROOT / "eval" / "data" / "liveshow_song_slices"
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
            truth_by_class=truth, one_shot_truth_by_class=one_shot,
            grid=BeatGridRef(bpm=bpm, anchor_sec=0.0),
        ))
    return out


def build_dev_corpus(max_babyslakh_tracks: int = 8) -> List[TrackData]:
    corpus: List[TrackData] = []
    corpus.extend(_build_babyslakh_tracks(max_babyslakh_tracks))
    corpus.extend(_build_self_render_tracks())
    corpus.extend(_build_liveshow_dev_tracks())
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
    rows: List[Dict[str, Any]] = []
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
        prf = metrics.event_prf(pred, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
        row = {
            "id": track.id, "domain": track.domain, "source": track.source,
            "n_pred": len(pred), "n_truth": len(truth), f"{class_name}_f1": prf.f1,
            "precision": prf.precision, "recall": prf.recall,
        }
        one_shot_truth = track.one_shot_truth_by_class.get(class_name, [])
        if one_shot_truth:
            oneshot_prf = metrics.event_prf(pred, one_shot_truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
            # Recall specifically on the isolated-truth subset: how many of
            # THOSE truth onsets got a matched prediction (precision here
            # would be dominated by unrelated predictions matching non-
            # one-shot truth, so only recall is reported, per the brief).
            row["one_shot_recall"] = oneshot_prf.recall
            row["one_shot_n_truth"] = len(one_shot_truth)
            one_shot_matched += oneshot_prf.true_positives
            one_shot_total += len(one_shot_truth)
        rows.append(row)

    valid = [r for r in rows if r["n_truth"] > 0]
    mean_f1 = float(np.mean([r[f"{class_name}_f1"] for r in valid])) if valid else None
    by_domain = metrics.domain_aggregate(valid, f"{class_name}_f1") if valid else {}
    one_shot_recall_overall = (one_shot_matched / one_shot_total) if one_shot_total > 0 else None

    return {
        "params": None,  # filled in by the caller
        "per_track": rows,
        "mean_f1": mean_f1,
        "by_domain": by_domain,
        "one_shot_recall_overall": one_shot_recall_overall,
        "one_shot_n_truth_total": one_shot_total,
        "n_tracks_scored": len(valid),
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


def run_family_sweep(corpus: List[TrackData], class_name: str, family: str, base: PrecisionConfig) -> List[Dict[str, Any]]:
    if family == "threshold":
        return sweep_threshold(corpus, class_name, base, COARSE_GRIDS["threshold"])
    if family == "refractory":
        return sweep_refractory(corpus, class_name, base, COARSE_GRIDS["refractory"])
    if family == "median_adaptive":
        return sweep_median_adaptive(corpus, class_name, base, COARSE_GRIDS["median_adaptive"])
    if family == "cofire":
        return sweep_cofire(corpus, class_name, base, COARSE_GRIDS["cofire"])
    if family == "shape_gate":
        grid = COARSE_GRIDS["shape_gate_ceiling"] if class_name == "hat" else COARSE_GRIDS["shape_gate_floor"]
        return sweep_shape_gate(corpus, class_name, base, grid)
    if family == "beat_phase":
        return sweep_beat_phase(corpus, class_name, base, COARSE_GRIDS["beat_phase"])
    raise ValueError(f"unknown family {family}")


def _is_dead_knob(rows: List[Dict[str, Any]]) -> bool:
    f1s = [r["mean_f1"] for r in rows if r["mean_f1"] is not None]
    if len(f1s) < 2:
        return True
    return (max(f1s) - min(f1s)) < 1e-6


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


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--max-babyslakh-tracks", type=int, default=8)
    parser.add_argument("--classes", nargs="+", default=list(FAMILIES_BY_CLASS.keys()))
    parser.add_argument("--out-dir", type=Path, default=AUDIO_ANALYSIS_ROOT / "eval" / "scoreboard")
    args = parser.parse_args(argv)

    print("[sweep_p4] building dev corpus (one-time inference per track) ...", file=sys.stderr)
    corpus = build_dev_corpus(max_babyslakh_tracks=args.max_babyslakh_tracks)
    print(f"[sweep_p4] corpus: {len(corpus)} tracks", file=sys.stderr)

    args.out_dir.mkdir(parents=True, exist_ok=True)
    for class_name in args.classes:
        print(f"[sweep_p4] class={class_name} ...", file=sys.stderr)
        scoreboard = build_class_scoreboard(corpus, class_name)
        scoreboard["generated_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
        scoreboard["corpus_summary"] = [{"id": t.id, "domain": t.domain, "source": t.source} for t in corpus]
        out_path = args.out_dir / f"p4_sweep_r1_{class_name}.json"
        out_path.write_text(json.dumps(scoreboard, indent=2))
        print(f"[sweep_p4] wrote {out_path} (baseline mean_f1={scoreboard['baseline']['mean_f1']})", file=sys.stderr)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
