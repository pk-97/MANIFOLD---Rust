"""P4 ONE-SHOT HELDOUT ACCEPTANCE READ (orchestrator-directed, round 2
picks). This module is DELIBERATELY SEPARATE from eval/sweep_p4.py, which
structurally cannot read heldout fixtures (D9) -- heldout scoring is its
own invocation, same convention as eval/run.py's --set heldout flag.

EXACTLY four configs are run, no others, no iteration:
  1. baseline    -- PrecisionConfig() all-defaults
  2. kick pick   -- threshold_factor 1.15 on kick only (0.12 -> 0.138)
  3. snare pick  -- threshold_factor 1.3  on snare only (0.14 -> 0.182)
  4. hat pick    -- threshold_factor 0.5  on hat only   (0.18 -> 0.09)
(shape_gate/cofire/beat_phase were explicitly NOT picked -- parked as
trigger-selection-layer candidates, not detector knobs, per the
orchestrator's round-2 verdict; they are not run here.)

Fixtures: liveshow_stagnate + liveshow_basalt ONLY -- the two heldout
liveshow songs, both sparse_visual truth type. Scored per the round-2
correction: recall + active-passage precision + one-shot recall (if
isolated truth exists in this pair) -- never raw precision/F1 against
sparse-visual truth.

This is the one-shot heldout read. Results are committed and reported
verbatim; this script is not re-run with different configs based on what
it finds.
"""

from __future__ import annotations

import datetime as dt
import json
import sys
from pathlib import Path
from typing import Any, Dict, List

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval import metrics  # noqa: E402
from eval.beat_scoring import LIVESHOW_SONG_FIXTURES, load_tempo_points  # noqa: E402
from eval.run import AUDIO_ANALYSIS_ROOT  # noqa: E402
from eval.sweep_p4 import (  # noqa: E402
    ACTIVE_PASSAGE_WINDOW_BEATS,
    SPARSE_VISUAL,
    TrackData,
    _isolated_truth,
    _liveshow_song_bpm,
    _liveshow_song_truth,
    active_passage_filter,
)
from manifold_audio.adtof_detection import detect_drums_adtof_activations  # noqa: E402
from manifold_audio.audio_io import load_audio_mono  # noqa: E402
from manifold_audio.basic_pitch_detection import detect_notes_basic_pitch  # noqa: E402
from manifold_audio.precision_postprocessing import (  # noqa: E402
    BeatGridRef,
    PrecisionConfig,
    extract_adtof_gate_candidates,
    run_precision_pipeline,
)

# The ONLY two fixtures this module ever reads -- both split == "heldout".
HELDOUT_LIVESHOW_FIXTURES = [fx for fx in LIVESHOW_SONG_FIXTURES if fx["split"] == "heldout"]
assert {fx["id"] for fx in HELDOUT_LIVESHOW_FIXTURES} == {"liveshow_stagnate", "liveshow_basalt"}

# EXACTLY these three picks, per the orchestrator's round-2 verdict -- no
# other knob, no other value.
ROUND2_PICKS = {
    "kick": 1.15,
    "snare": 1.3,
    "hat": 0.5,
}


def build_heldout_corpus() -> List[TrackData]:
    slices_dir = AUDIO_ANALYSIS_ROOT / "eval" / "data" / "liveshow_song_slices"
    tempo_points = load_tempo_points()
    out: List[TrackData] = []
    for fx in HELDOUT_LIVESHOW_FIXTURES:
        wav_path = slices_dir / f"{fx['id']}.wav"
        if not wav_path.exists():
            print(f"[heldout_acceptance_p4] MISSING slice: {wav_path}", file=sys.stderr)
            continue
        truth, _seg_start, _seg_end = _liveshow_song_truth(fx, tempo_points)
        print(f"[heldout_acceptance_p4] {fx['id']}: ADTOF inference ...", file=sys.stderr)
        activations, fps = detect_drums_adtof_activations(str(wav_path))
        print(f"[heldout_acceptance_p4] {fx['id']}: basic_pitch gate pass ...", file=sys.stderr)
        notes = detect_notes_basic_pitch(str(wav_path), onset_threshold=0.05, min_note_length=30.0)
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


def score_class_on_heldout(corpus: List[TrackData], class_name: str, config: PrecisionConfig) -> Dict[str, Any]:
    """Sparse-visual-only scoring (this corpus has no dense tracks by
    construction): recall + active-passage precision + one-shot recall."""
    rows: List[Dict[str, Any]] = []
    one_shot_matched = 0
    one_shot_total = 0

    for track in corpus:
        truth = track.truth_by_class.get(class_name)
        if not truth:
            continue
        if class_name == "synth":
            continue  # not in scope this round
        if track.activations is None:
            continue
        gate = extract_adtof_gate_candidates(track.activations, track.fps, class_name, config)
        pred = run_precision_pipeline(
            {class_name: gate}, config, audio=track.audio, sr=track.sr, grid=track.grid,
        )[class_name]

        recall_prf = metrics.event_prf(pred, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
        active_precision = None
        n_pred_in_window = None
        if track.grid is not None and track.grid.bpm > 0:
            window_sec = ACTIVE_PASSAGE_WINDOW_BEATS * 60.0 / track.grid.bpm
            restricted_pred = active_passage_filter(pred, truth, window_sec)
            n_pred_in_window = len(restricted_pred)
            active_prf = metrics.event_prf(restricted_pred, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
            active_precision = active_prf.precision

        row = {
            "id": track.id, "domain": track.domain,
            "n_pred": len(pred), "n_truth": len(truth),
            "recall": recall_prf.recall,
            "active_passage_precision": active_precision,
            "n_pred_in_active_window": n_pred_in_window,
        }
        one_shot_truth = track.one_shot_truth_by_class.get(class_name, [])
        if one_shot_truth:
            oneshot_prf = metrics.event_prf(pred, one_shot_truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
            row["one_shot_recall"] = oneshot_prf.recall
            row["one_shot_n_truth"] = len(one_shot_truth)
            one_shot_matched += oneshot_prf.true_positives
            one_shot_total += len(one_shot_truth)
        rows.append(row)

    valid = [r for r in rows if r["n_truth"] > 0]
    mean_recall = sum(r["recall"] for r in valid) / len(valid) if valid else None
    active_precisions = [r["active_passage_precision"] for r in valid if r["active_passage_precision"] is not None]
    mean_active_precision = sum(active_precisions) / len(active_precisions) if active_precisions else None
    one_shot_recall_overall = (one_shot_matched / one_shot_total) if one_shot_total > 0 else None

    return {
        "per_track": rows,
        "mean_recall": mean_recall,
        "mean_active_passage_precision": mean_active_precision,
        "one_shot_recall_overall": one_shot_recall_overall,
        "one_shot_n_truth_total": one_shot_total,
        "n_tracks_scored": len(valid),
    }


def main() -> int:
    print("[heldout_acceptance_p4] building heldout corpus (liveshow_stagnate + liveshow_basalt only) ...", file=sys.stderr)
    corpus = build_heldout_corpus()
    print(f"[heldout_acceptance_p4] corpus: {[t.id for t in corpus]}", file=sys.stderr)

    baseline_config = PrecisionConfig()

    report: Dict[str, Any] = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "phase": "P4 one-shot heldout acceptance read (round-2 orchestrator picks)",
        "fixtures": [t.id for t in corpus],
        "truth_type": "sparse_visual (all heldout fixtures)",
        "picks_evaluated": ROUND2_PICKS,
        "per_class": {},
    }

    for class_name, factor in ROUND2_PICKS.items():
        pick_config = PrecisionConfig()
        pick_config.thresholds[class_name] = baseline_config.thresholds[class_name] * factor

        baseline_result = score_class_on_heldout(corpus, class_name, baseline_config)
        pick_result = score_class_on_heldout(corpus, class_name, pick_config)

        report["per_class"][class_name] = {
            "pick_params": {"knob": "threshold_factor", "factor": factor, "threshold": pick_config.thresholds[class_name]},
            "baseline": baseline_result,
            "pick": pick_result,
        }
        print(
            f"[heldout_acceptance_p4] {class_name}: baseline recall={baseline_result['mean_recall']} "
            f"pick recall={pick_result['mean_recall']}", file=sys.stderr,
        )

    out_path = AUDIO_ANALYSIS_ROOT / "eval" / "scoreboard" / "p4_heldout_acceptance.json"
    out_path.write_text(json.dumps(report, indent=2))
    print(f"[heldout_acceptance_p4] wrote {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
