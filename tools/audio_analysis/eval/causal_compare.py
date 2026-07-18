"""Causal-vs-offline detection comparison — DEV ONLY (2026-07-18).

Scores the discrete hit decisions of the LIVE causal path (Rust
`StreamingSendAnalyzer` -> `LiveTriggerState`, dumped by
`crates/manifold-audio/examples/causal_events.rs` as
`[{"time_sec": ..., "kind": ...}]`) against the OFFLINE detector
(post-P4 ADTOF + precision pipeline, bare `PrecisionConfig()` — the exact
arm `eval.bakeoff_b1` scores) and the track's truth, under the SAME
windowed scoring the P4 sweep uses (`eval.metrics.event_prf` at the frozen
EVENT_TOLERANCE_SEC; DENSE_IN_WINDOW active-window filtering for liveshow
dev slices via `eval.sweep_p4.derive_active_windows`/`filter_to_windows`).

The causal path is CLASSLESS (a fire says "a hit", not which drum), so its
headline row is generic-hit: the union of all its band fires vs the union of
all truth classes. The offline arm is additionally scored per-class (its
native granularity) and as the same generic-hit union for an apples-to-
apples causal-vs-offline row. Timing = median |signed offset| of matched
pairs (same greedy one-to-one matching as event_prf, offsets kept).

HELDOUT FORBIDDEN: no heldout fixture id or E-GMD heldout directory is ever
referenced; only `self_render:*` fixtures and `split == "dev"` liveshow
slices are reachable through this module.

Usage:
    python -m eval.causal_compare --events <causal.json> --track self_render:kick_hat_128bpm \
        [--out artifact.json] [--skip-offline]
    python -m eval.causal_compare --events <causal.json> --track liveshow:integer --out artifact.json
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval import metrics  # noqa: E402
from eval.paths import DATA_ROOT  # noqa: E402
from eval.sweep_p4 import (  # noqa: E402
    DEV_LIVESHOW_FIXTURES,
    _liveshow_song_bpm,
    _liveshow_song_truth,
    derive_active_windows,
    filter_to_windows,
)
from eval.beat_scoring import load_tempo_points  # noqa: E402

CLASSES = ("kick", "snare", "hat", "perc")

# The causal path fires one event PER ROUTE per onset (a kick lights
# transients_low AND transients_full AND kick_low within a few ms of each
# other — the live rig launches one clip per route, by design). For a
# generic-HIT measurement those co-fires are one hit, so the union stream is
# cluster-merged at the analyzer's own onset refractory (6 hops x 5.33 ms
# = 32 ms — ONSET_REFRACTORY_HOPS in manifold-audio/src/analysis.rs; the
# original 106 ms here over-merged 3x and collapsed distinct hits on dense
# material, understating recall — corrected 2026-07-18): a cluster starts
# when the gap since the previous event exceeds it, and the hit time is the
# cluster's FIRST event (the earliest = the live latency). Scoring-side only.
CAUSAL_CLUSTER_REFRACTORY_SEC = 0.032

# Same pitch->class folding bakeoff_b1._build_edm_kit_track uses for
# self_render MIDI truth.
SELF_RENDER_PITCH_TO_CLASS = {36: "kick", 38: "snare", 39: "snare", 42: "hat", 45: "perc"}


def _load_self_render(name: str) -> Dict[str, Any]:
    base = DATA_ROOT / "self_render"
    wav = base / f"{name}.wav"
    truth_path = base / f"{name}_truth.json"
    if not wav.exists() or not truth_path.exists():
        raise SystemExit(f"self_render fixture not found: {wav} / {truth_path}")
    truth: Dict[str, List[float]] = {c: [] for c in CLASSES}
    for n in json.loads(truth_path.read_text()):
        cls = SELF_RENDER_PITCH_TO_CLASS.get(n["pitch"])
        if cls:
            truth[cls].append(float(n["start_sec"]))
    for c in truth:
        truth[c].sort()
    return {"id": f"self_render_{name}", "wav": wav, "truth": truth, "truth_type": "dense", "bpm": None}


def _load_liveshow_dev(slice_id: str) -> Dict[str, Any]:
    fixtures = [fx for fx in DEV_LIVESHOW_FIXTURES if fx["id"] == slice_id]
    if not fixtures:
        dev_ids = [fx["id"] for fx in DEV_LIVESHOW_FIXTURES]
        raise SystemExit(f"unknown or non-dev liveshow slice '{slice_id}' (dev slices: {dev_ids})")
    fx = fixtures[0]
    wav = DATA_ROOT / "liveshow_song_slices" / f"{fx['id']}.wav"
    if not wav.exists():
        raise SystemExit(f"liveshow slice wav missing: {wav}")
    tempo_points = load_tempo_points()
    truth, _seg_start, _seg_end = _liveshow_song_truth(fx, tempo_points)
    bpm = _liveshow_song_bpm(fx, tempo_points)
    # The causal path is classless; the drum classes are its union scope.
    # `synth` (a melodic class the drum-only offline arm also ignores) is
    # excluded from both arms' truth here.
    truth = {c: t for c, t in truth.items() if c in CLASSES}
    return {"id": fx["id"], "wav": wav, "truth": truth, "truth_type": "dense_in_window", "bpm": bpm}


def load_track(spec: str) -> Dict[str, Any]:
    source, _, name = spec.partition(":")
    if source == "self_render":
        return _load_self_render(name)
    if source == "liveshow":
        return _load_liveshow_dev(name)
    raise SystemExit(f"--track must be self_render:<name> or liveshow:<dev-slice-id>, got '{spec}'")


def _matched_offsets(
    pred: Sequence[float], truth: Sequence[float], tolerance_sec: float
) -> List[float]:
    """Signed offsets (pred - truth) of event_prf's greedy one-to-one match.

    metrics.py is FROZEN (D10), so the match is re-implemented here with the
    offsets kept — identical algorithm to metrics._greedy_match_count."""
    pred_sorted = sorted(pred)
    truth_sorted = sorted(truth)
    used = [False] * len(truth_sorted)
    offsets: List[float] = []
    for p in pred_sorted:
        best_idx: Optional[int] = None
        best_dist = tolerance_sec
        for i, t in enumerate(truth_sorted):
            if used[i]:
                continue
            dist = abs(p - t)
            if dist <= best_dist:
                best_dist = dist
                best_idx = i
        if best_idx is not None:
            used[best_idx] = True
            offsets.append(p - truth_sorted[best_idx])
    return offsets


def cluster_merge(times: Sequence[float], refractory_sec: float = CAUSAL_CLUSTER_REFRACTORY_SEC) -> List[float]:
    """Union->hits: first time of each refractory-separated cluster."""
    ts = sorted(times)
    if not ts:
        return []
    out = [ts[0]]
    prev = ts[0]
    for t in ts[1:]:
        if t - prev > refractory_sec:
            out.append(t)
        prev = t
    return out


def _score_generic(pred: Sequence[float], truth_union: List[float], track: Dict[str, Any]) -> Dict[str, Any]:
    """Generic-hit score: union predictions vs union truth, with the same
    DENSE_IN_WINDOW prediction filtering the harness applies on liveshow."""
    windows: List[Tuple[float, float]] = []
    pred_used = list(pred)
    if track["truth_type"] == "dense_in_window":
        windows = derive_active_windows(truth_union, track["bpm"] or 0.0)
        pred_used = filter_to_windows(list(pred), windows)
    prf = metrics.event_prf(pred_used, truth_union, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
    offsets = _matched_offsets(pred_used, truth_union, metrics.EVENT_TOLERANCE_SEC)
    return {
        "f1": prf.f1,
        "precision": prf.precision,
        "recall": prf.recall,
        "n_pred": len(pred_used),
        "n_truth": len(truth_union),
        "median_abs_timing_ms": float(np.median(np.abs(offsets)) * 1000.0) if offsets else None,
        "median_signed_timing_ms": float(np.median(offsets) * 1000.0) if offsets else None,
        "n_matched": len(offsets),
        "n_windows": len(windows) if windows else None,
    }


def _offline_events(track: Dict[str, Any]) -> Dict[str, List[float]]:
    """The offline arm's per-class event times: ADTOF activations -> P4 gate
    candidates -> precision pipeline, bare PrecisionConfig (bakeoff_b1's
    ADTOF arm, identical mechanism)."""
    from manifold_audio.adtof_detection import detect_drums_adtof_activations
    from manifold_audio.audio_io import load_audio_mono
    from manifold_audio.precision_postprocessing import (
        PrecisionConfig,
        extract_adtof_gate_candidates,
        run_precision_pipeline,
    )

    config = PrecisionConfig()
    print(f"[causal_compare] {track['id']}: ADTOF inference (offline arm) ...", file=sys.stderr)
    activations, fps = detect_drums_adtof_activations(str(track["wav"]))
    audio, sr = load_audio_mono(track["wav"], target_sr=44100, ffmpeg_bin=None)
    out: Dict[str, List[float]] = {}
    for cls in CLASSES:
        gate = extract_adtof_gate_candidates(activations, fps, cls, config)
        out[cls] = list(run_precision_pipeline({cls: gate}, config, audio=audio, sr=sr)[cls])
    return out


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--events", type=Path, required=True, help="causal_events.rs JSON dump")
    parser.add_argument("--track", required=True, help="self_render:<name> or liveshow:<dev-slice-id>")
    parser.add_argument("--out", type=Path, default=None, help="write the JSON artifact here")
    parser.add_argument("--skip-offline", action="store_true", help="score the causal arm only (no ADTOF inference)")
    args = parser.parse_args(argv)

    track = load_track(args.track)
    raw_events = json.loads(args.events.read_text())
    causal_union = sorted(float(e["time_sec"]) for e in raw_events)
    kind_counts: Dict[str, int] = {}
    for e in raw_events:
        kind_counts[str(e["kind"])] = kind_counts.get(str(e["kind"]), 0) + 1

    truth_union = sorted(t for times in track["truth"].values() for t in times)
    causal_hits = cluster_merge(causal_union)
    causal_score = _score_generic(causal_hits, truth_union, track)
    causal_per_kind: Dict[str, Any] = {}
    for kind in sorted(kind_counts):
        times = [float(e["time_sec"]) for e in raw_events if str(e["kind"]) == kind]
        causal_per_kind[kind] = _score_generic(times, truth_union, track)

    offline_score: Optional[Dict[str, Any]] = None
    offline_per_class: Optional[Dict[str, Any]] = None
    if not args.skip_offline:
        off_events = _offline_events(track)
        off_union = sorted(t for times in off_events.values() for t in times)
        offline_score = _score_generic(off_union, truth_union, track)
        offline_per_class = {}
        for cls in CLASSES:
            truth = track["truth"].get(cls, [])
            if not truth:
                continue
            windows: List[Tuple[float, float]] = []
            pred = off_events.get(cls, [])
            if track["truth_type"] == "dense_in_window":
                windows = derive_active_windows(truth, track["bpm"] or 0.0)
                pred = filter_to_windows(pred, windows)
            prf = metrics.event_prf(pred, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
            offsets = _matched_offsets(pred, truth, metrics.EVENT_TOLERANCE_SEC)
            offline_per_class[cls] = {
                "f1": prf.f1,
                "precision": prf.precision,
                "recall": prf.recall,
                "n_pred": len(pred),
                "n_truth": len(truth),
                "median_abs_timing_ms": float(np.median(np.abs(offsets)) * 1000.0) if offsets else None,
            }

    print(f"\ntrack: {track['id']}  ({track['truth_type']}, wav: {track['wav'].name})")
    print(f"causal events by kind: {kind_counts}")
    print(f"\n{'arm':<28}{'F1':>7}{'P':>7}{'R':>7}{'pred':>6}{'truth':>7}{'|dt|ms':>9}")
    rows = [("causal (generic hit)", causal_score)]
    if offline_score is not None:
        rows.append(("offline union (generic)", offline_score))
    for name, s in rows:
        t = f"{s['median_abs_timing_ms']:.1f}" if s["median_abs_timing_ms"] is not None else "n/a"
        print(f"{name:<28}{s['f1']:>7.3f}{s['precision']:>7.3f}{s['recall']:>7.3f}{s['n_pred']:>6}{s['n_truth']:>7}{t:>9}")
    print("\ncausal per-kind (unmerged):")
    print(f"{'kind':<28}{'F1':>7}{'P':>7}{'R':>7}{'pred':>6}{'truth':>7}{'|dt|ms':>9}")
    for kind, s in causal_per_kind.items():
        t = f"{s['median_abs_timing_ms']:.1f}" if s["median_abs_timing_ms"] is not None else "n/a"
        print(f"{kind:<28}{s['f1']:>7.3f}{s['precision']:>7.3f}{s['recall']:>7.3f}{s['n_pred']:>6}{s['n_truth']:>7}{t:>9}")
    if offline_per_class:
        print("\noffline per-class:")
        print(f"{'class':<28}{'F1':>7}{'P':>7}{'R':>7}{'pred':>6}{'truth':>7}{'|dt|ms':>9}")
        for cls, s in offline_per_class.items():
            t = f"{s['median_abs_timing_ms']:.1f}" if s["median_abs_timing_ms"] is not None else "n/a"
            print(f"{cls:<28}{s['f1']:>7.3f}{s['precision']:>7.3f}{s['recall']:>7.3f}{s['n_pred']:>6}{s['n_truth']:>7}{t:>9}")

    artifact = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "track": {"id": track["id"], "truth_type": track["truth_type"], "wav": str(track["wav"]), "bpm": track["bpm"]},
        "tolerance_sec": metrics.EVENT_TOLERANCE_SEC,
        "causal": {
            "events_file": str(args.events),
            "kind_counts": kind_counts,
            "n_raw_events": len(causal_union),
            "cluster_refractory_sec": CAUSAL_CLUSTER_REFRACTORY_SEC,
            "n_merged_hits": len(causal_hits),
            "generic_hit": causal_score,
            "per_kind": causal_per_kind,
        },
        "offline": (
            {"generic_hit_union": offline_score, "per_class": offline_per_class}
            if offline_score is not None
            else None
        ),
    }
    if args.out is not None:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(artifact, indent=2))
        print(f"\nwrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
