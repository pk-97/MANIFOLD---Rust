"""Audio Event Classifier -- the D8 HELDOUT EXAM (one-shot, orchestrator-run).

Rebuilt 2026-07-18 as a permanent file from the session that ran the first
exam (design doc docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md SS5b is the record
that exam produced; D8 in SS"Decided" is the protocol this script
implements):

  * Slices: liveshow heldout (liveshow_stagnate + liveshow_basalt, scored
    DENSE_IN_WINDOW per eval.sweep_p4) + E-GMD heldout (dense MIDI truth).
  * Arms: the classifier arm (eval-side Stage-1 onset front-end labeled by
    the trained model via detect_drums_stage1(classifier_weights=...),
    folded to the 4-class vocabulary through CLASSIFIER_TO_ADTOF_CLASS) vs
    the ADTOF arm (post-P4 PrecisionConfig() defaults -- the exact mechanism
    eval.bakeoff_b1 reuses).
  * Bar (D8): per-class F1 >= ADTOF same-slice F1 - 0.05 for kick/snare/hat,
    and drums-filed-as-other on liveshow heldout < 10%.

HELDOUT DISCIPLINE (structural): this script REFUSES to run without the
explicit --i-am-consuming-heldout flag. Dev iteration uses
eval/probe_classifier.py, which shares this module's scoring machinery but
structurally cannot name a heldout slice. Heldout spends ONCE per ship
candidate -- whoever passes the flag owns that spend.

Reproducing-the-exam notes (from docs/landings/2026-07-18-audio-event-
classifier-p1-p3.md's disclosures): ADTOF's liveshow-heldout arm ran with
grid=None (beat-phase prior inert -- it is 0.0 by default anyway, so this
is exactness, not a number-changer); liveshow heldout kick truth rests on
n=1 song (only one heldout song carries kick truth); the classifier arm,
like bakeoff_b1's Stage-1 arm it descends from, runs on the demucs DRUM
STEM (<id>_drums.wav next to each slice, built once, eval-only -- see
eval/bakeoff_b1._liveshow_drum_stem_audio_override), never the raw master;
a MISSING stem for any scored liveshow slice fails loudly with the build
command rather than silently falling back to master audio.

Usage:
    python -m eval.exam_classifier --i-am-consuming-heldout \
        --out eval/scoreboard/exam_classifier_<candidate>.json
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval import egmd_drum_truth, metrics  # noqa: E402
from eval.beat_scoring import LIVESHOW_SONG_FIXTURES, load_tempo_points  # noqa: E402
from eval.paths import DATA_ROOT  # noqa: E402
from eval.sweep_p4 import (  # noqa: E402
    DENSE,
    DENSE_IN_WINDOW,
    _liveshow_song_bpm,
    _liveshow_song_truth,
    derive_active_windows,
    filter_to_windows,
)
from manifold_audio.adtof_detection import detect_drums_adtof_activations  # noqa: E402
from manifold_audio.audio_io import load_audio_mono  # noqa: E402
from manifold_audio.precision_postprocessing import (  # noqa: E402
    PrecisionConfig,
    extract_adtof_gate_candidates,
    run_precision_pipeline,
)
from manifold_audio.stage1_dsp_detection import detect_drums_stage1  # noqa: E402

DRUM_CLASSES = ("kick", "snare", "hat", "perc")
# The D8 bar applies to kick/snare/hat; perc (and the other-filed rate) are
# reported alongside, never silently dropped -- "mixed outcomes ship mixed".
BAR_CLASSES = ("kick", "snare", "hat")
SHIP_TOLERANCE = 0.05
DRUMS_FILED_AS_OTHER_BAR = 0.10

# Same fold as eval.bakeoff_b1: the classifier's synth/other/vocal labels
# have no ADTOF-side slot and drop out of per-class F1 (`other` IS the
# drums-filed-as-other metric's subject, computed separately below).
CLASSIFIER_TO_ADTOF_CLASS = {"kick": "kick", "snare": "snare", "hat": "hat", "perc": "perc"}

DEFAULT_WEIGHTS_REL = Path("models") / "audio_event_classifier_v1.pt"

SLICE_LIVESHOW = "liveshow"
SLICE_EGMD = "E-GMD"


@dataclass
class ExamTrack:
    id: str
    slice_group: str  # SLICE_LIVESHOW | SLICE_EGMD
    domain: str
    audio: np.ndarray
    sr: int
    audio_path: str
    activations: Optional[np.ndarray]
    fps: int
    truth_type: str  # DENSE | DENSE_IN_WINDOW
    truth_by_class: Dict[str, List[float]] = field(default_factory=dict)
    bpm: float = 0.0  # windowing clock for DENSE_IN_WINDOW; 0.0 for dense
    # Classifier-arm audio: for liveshow tracks the demucs drum stem
    # (<id>_drums.wav), per the bakeoff_b1 stem override; None when the stem
    # is missing (classifier_events then refuses loudly) or when the track's
    # own audio already IS the classifier-arm signal (E-GMD = isolated kit).
    classifier_audio: Optional[np.ndarray] = None
    classifier_sr: int = 0
    stem_path: Optional[str] = None


# ---------------------------------------------------------------------------
# Corpus builders (data_root-parameterized; split is the caller's choice --
# probe passes dev fixtures, the exam passes heldout and is flag-gated)
# ---------------------------------------------------------------------------


def build_liveshow_tracks(fixtures: List[Dict[str, Any]], data_root: Path) -> List[ExamTrack]:
    slices_dir = data_root / "liveshow_song_slices"
    tempo_points = load_tempo_points()
    out: List[ExamTrack] = []
    for fx in fixtures:
        wav_path = slices_dir / f"{fx['id']}.wav"
        if not wav_path.exists():
            print(f"[classifier-eval] MISSING liveshow slice, skipping: {wav_path}", file=sys.stderr)
            continue
        truth, _seg_start, _seg_end = _liveshow_song_truth(fx, tempo_points)
        if not any(truth.values()):
            continue
        print(f"[classifier-eval] {fx['id']}: ADTOF inference ...", file=sys.stderr)
        activations, fps = detect_drums_adtof_activations(str(wav_path))
        audio, sr = load_audio_mono(wav_path, target_sr=44100, ffmpeg_bin=None)
        # Classifier-arm stem override (bakeoff_b1 precedent): the Stage-1
        # front-end + classifier run on the demucs drum stem, not the show
        # master. Missing stem is recorded, NOT worked around -- see
        # classifier_events for the loud refusal.
        stem_path = slices_dir / f"{fx['id']}_drums.wav"
        stem_audio: Optional[np.ndarray] = None
        stem_sr = 0
        if stem_path.exists():
            stem_audio, stem_sr = load_audio_mono(stem_path, target_sr=44100, ffmpeg_bin=None)
        else:
            print(f"[classifier-eval] {fx['id']}: drum stem MISSING (classifier arm will refuse): {stem_path}", file=sys.stderr)
        out.append(ExamTrack(
            id=fx["id"], slice_group=SLICE_LIVESHOW, domain=fx.get("domain", "electronic"),
            audio=audio, sr=sr, audio_path=str(wav_path),
            activations=activations, fps=fps,
            truth_type=DENSE_IN_WINDOW, truth_by_class=truth,
            bpm=_liveshow_song_bpm(fx, tempo_points),
            classifier_audio=stem_audio, classifier_sr=stem_sr,
            stem_path=str(stem_path),
        ))
    return out


def build_egmd_tracks(split: str, data_root: Path, max_tracks: Optional[int] = None) -> List[ExamTrack]:
    rows = egmd_drum_truth.available_rows(root=data_root / "egmd", split=split)
    if max_tracks is not None:
        rows = rows[:max_tracks]
    out: List[ExamTrack] = []
    for i, row in enumerate(rows):
        audio_path = Path(row["audio_path"])
        if not audio_path.exists():
            print(f"[classifier-eval] MISSING E-GMD audio, skipping: {audio_path}", file=sys.stderr)
            continue
        print(f"[classifier-eval] egmd {row['id']} ({i + 1}/{len(rows)}): ADTOF inference ...", file=sys.stderr)
        activations, fps = detect_drums_adtof_activations(str(audio_path))
        audio, sr = load_audio_mono(audio_path, target_sr=44100, ffmpeg_bin=None)
        truth = egmd_drum_truth.load_drum_truth(Path(row["midi_path"]))
        out.append(ExamTrack(
            id=f"egmd_{row['id'].replace('/', '_')}", slice_group=SLICE_EGMD, domain="other",
            audio=audio, sr=sr, audio_path=str(audio_path),
            activations=activations, fps=fps,
            truth_type=DENSE, truth_by_class=truth, bpm=0.0,
        ))
    return out


# ---------------------------------------------------------------------------
# The two arms
# ---------------------------------------------------------------------------


def _window_predictions(track: ExamTrack, class_name: str, pred: List[float]) -> Tuple[List[float], int]:
    """DENSE_IN_WINDOW contract (eval.sweep_p4): predictions outside the
    class's own active truth windows are discarded before scoring; truth is
    inside by construction. Dense tracks pass through unwindowed."""
    if track.truth_type != DENSE_IN_WINDOW:
        return pred, 0
    windows = derive_active_windows(track.truth_by_class.get(class_name, []), track.bpm)
    return filter_to_windows(pred, windows), len(windows)


def adtof_predictions(track: ExamTrack, class_name: str, config: PrecisionConfig) -> List[float]:
    """The post-P4 ADTOF arm, bare defaults. grid=None always: the exam's
    liveshow ADTOF arm ran without a beat grid (landing-doc disclosure),
    E-GMD rows never carry one in this harness either, and the beat-phase
    prior is 0.0 by default regardless -- so this is the exam's exact
    mechanism, not an approximation."""
    if track.activations is None:
        return []
    gate = extract_adtof_gate_candidates(track.activations, track.fps, class_name, config)
    return run_precision_pipeline(
        {class_name: gate}, config, audio=track.audio, sr=track.sr, grid=None,
    )[class_name]


def classifier_events(track: ExamTrack, weights_path: str) -> List[Any]:
    """The classifier arm on the track's classifier-arm audio: for liveshow
    that is the demucs DRUM STEM (the bakeoff_b1 stem override -- the Stage-1
    front-end is a drum-stem detector, and the exam inherits that contract);
    for E-GMD the track's own audio, which is already an isolated kit
    recording. A missing liveshow stem is a hard failure with the one-time
    build command -- never a silent fall-back to the master, which would
    measure a different signal and call it the same number."""
    if track.slice_group == SLICE_LIVESHOW:
        if track.classifier_audio is None:
            raise RuntimeError(
                f"[classifier-eval] drum stem MISSING for {track.id}: {track.stem_path}\n"
                f"Build it once (eval-only, same as the dev slices): "
                f"`python -m demucs -n htdemucs --two-stems=drums` on the slice wav, "
                f"saved as <id>_drums.wav next to it. Refusing to score the classifier "
                f"arm on the raw master instead."
            )
        audio, sr = track.classifier_audio, track.classifier_sr
    else:
        audio, sr = track.audio, track.sr
    events, _cluster = detect_drums_stage1(audio, sr, classifier_weights=weights_path)
    return events


def score_arm_for_class(
    tracks: List[ExamTrack], class_name: str, arm: str,
    config: PrecisionConfig, weights_path: Optional[str],
    classifier_cache: Dict[str, List[Any]],
) -> Dict[str, Any]:
    rows: List[Dict[str, Any]] = []
    for track in tracks:
        truth = track.truth_by_class.get(class_name)
        if not truth:
            continue
        if arm == "adtof":
            pred = adtof_predictions(track, class_name, config)
        else:
            if track.id not in classifier_cache:
                classifier_cache[track.id] = classifier_events(track, weights_path)
            pred = sorted(
                e.time for e in classifier_cache[track.id]
                if CLASSIFIER_TO_ADTOF_CLASS.get(e.type) == class_name
            )
        pred_w, n_windows = _window_predictions(track, class_name, pred)
        prf = metrics.event_prf(pred_w, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
        row = {
            "id": track.id, "domain": track.domain,
            "n_pred": len(pred_w), "n_pred_unwindowed": len(pred), "n_truth": len(truth),
            "precision": prf.precision, "recall": prf.recall, "f1": prf.f1,
        }
        if track.truth_type == DENSE_IN_WINDOW:
            row["n_windows"] = n_windows
        rows.append(row)
    valid = [r for r in rows if r["n_truth"] > 0]
    mean_f1 = float(np.mean([r["f1"] for r in valid])) if valid else None
    return {"per_track": rows, "mean_f1": mean_f1, "n_tracks_scored": len(valid)}


def drums_filed_as_other(
    tracks: List[ExamTrack], weights_path: str,
    classifier_cache: Dict[str, List[Any]],
) -> Dict[str, Any]:
    """The D8 `other` false-negative read: of classifier-labeled onsets that
    match a DRUM-class truth onset (kick/snare/hat/perc union, event
    tolerance), what fraction did the classifier file as `other`? Drums
    misfiled as `other` are triggers that silently die -- the fold above
    drops them from every per-class row, so they are measured here, pooled
    across the given tracks. Bar: < 10% on liveshow heldout."""
    matched = 0
    filed_other = 0
    per_track: List[Dict[str, Any]] = []
    for track in tracks:
        drum_truth = sorted(
            t for c in DRUM_CLASSES for t in track.truth_by_class.get(c, [])
        )
        if not drum_truth:
            continue
        if track.id not in classifier_cache:
            classifier_cache[track.id] = classifier_events(track, weights_path)
        truth_arr = np.asarray(drum_truth, dtype=np.float64)
        t_matched = 0
        t_other = 0
        for e in classifier_cache[track.id]:
            idx = int(np.searchsorted(truth_arr, e.time))
            near = (
                (idx > 0 and abs(e.time - truth_arr[idx - 1]) <= metrics.EVENT_TOLERANCE_SEC)
                or (idx < truth_arr.size and abs(truth_arr[idx] - e.time) <= metrics.EVENT_TOLERANCE_SEC)
            )
            if near:
                t_matched += 1
                if e.type == "other":
                    t_other += 1
        matched += t_matched
        filed_other += t_other
        per_track.append({"id": track.id, "n_matched": t_matched, "n_filed_other": t_other})
    rate = (filed_other / matched) if matched > 0 else None
    return {
        "per_track": per_track,
        "n_matched_drum_onsets": matched,
        "n_filed_other": filed_other,
        "drums_filed_as_other_rate": rate,
        "bar": DRUMS_FILED_AS_OTHER_BAR,
        "passes_bar": (rate is not None and rate < DRUMS_FILED_AS_OTHER_BAR),
    }


# ---------------------------------------------------------------------------
# Scoreboard assembly (shared by exam and probe)
# ---------------------------------------------------------------------------


def run_scoreboard(
    groups: Dict[str, List[ExamTrack]],
    classes_by_group: Dict[str, Tuple[str, ...]],
    weights_path: Optional[str],
    adtof_only: bool,
    group_titles: Optional[Dict[str, str]] = None,
) -> Dict[str, Any]:
    """Score both arms over every group. groups maps slice-group label ->
    tracks; classes_by_group maps it -> the classes scored there (liveshow
    carries kick/snare/hat truth reliably; E-GMD carries perc too).
    group_titles supplies the table's per-slice display names (e.g.
    "liveshow heldout" for the exam, "liveshow dev" for the probe)."""
    config = PrecisionConfig()
    classifier_cache: Dict[str, List[Any]] = {}
    scoreboard: Dict[str, Any] = {"groups": {}}
    for group_label, tracks in groups.items():
        group_out: Dict[str, Any] = {
            "n_tracks": len(tracks),
            "title": (group_titles or {}).get(group_label, group_label),
            "per_class": {},
        }
        for class_name in classes_by_group[group_label]:
            adtof_score = score_arm_for_class(tracks, class_name, "adtof", config, None, classifier_cache)
            row: Dict[str, Any] = {"adtof": adtof_score}
            if not adtof_only:
                clf_score = score_arm_for_class(tracks, class_name, "classifier", config, weights_path, classifier_cache)
                clf_f1 = clf_score["mean_f1"]
                adtof_f1 = adtof_score["mean_f1"]
                verdict = None
                if clf_f1 is not None and adtof_f1 is not None and class_name in BAR_CLASSES:
                    verdict = "SHIP-grade" if clf_f1 >= adtof_f1 - SHIP_TOLERANCE else "short"
                row["classifier"] = clf_score
                row["verdict_vs_bar"] = verdict
            group_out["per_class"][class_name] = row
        if not adtof_only and group_label == SLICE_LIVESHOW:
            group_out["drums_filed_as_other"] = drums_filed_as_other(tracks, weights_path, classifier_cache)
        scoreboard["groups"][group_label] = group_out
    return scoreboard


def print_table(scoreboard: Dict[str, Any], adtof_only: bool) -> None:
    """The SS5b-style table: one row per (slice, class), classifier F1 vs
    ADTOF F1, verdict vs the D8 bar; then the drums-filed-as-other line."""
    if adtof_only:
        print("\n| slice | class | ADTOF F1 |")
        print("|---|---|---|")
        for group in scoreboard["groups"].values():
            for class_name, row in group["per_class"].items():
                f1 = row["adtof"]["mean_f1"]
                print(f"| {group['title']} | {class_name} | {_fmt(f1)} |")
        return
    print("\n| slice | class | classifier | ADTOF | verdict |")
    print("|---|---|---|---|---|")
    for group_label, group in scoreboard["groups"].items():
        for class_name, row in group["per_class"].items():
            clf = row["classifier"]["mean_f1"]
            adtof = row["adtof"]["mean_f1"]
            verdict = row["verdict_vs_bar"] or "-"
            print(f"| {group['title']} | {class_name} | {_fmt(clf)} | {_fmt(adtof)} | {verdict} |")
    liveshow = scoreboard["groups"].get(SLICE_LIVESHOW, {})
    dfo = liveshow.get("drums_filed_as_other")
    if dfo is not None:
        rate = dfo["drums_filed_as_other_rate"]
        outcome = "PASS" if dfo["passes_bar"] else "FAIL"
        print(f"\ndrums-filed-as-other on {liveshow['title']}: {_fmt(rate)} "
              f"(bar <{DRUMS_FILED_AS_OTHER_BAR:.0%}) -- {outcome} "
              f"({dfo['n_filed_other']}/{dfo['n_matched_drum_onsets']} matched drum onsets)")


def _fmt(v: Optional[float]) -> str:
    return f"{v:.3f}" if v is not None else "n/a"


def write_scoreboard(path: Path, payload: Dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2))
    print(f"[classifier-eval] wrote {path}", file=sys.stderr)


# ---------------------------------------------------------------------------
# The exam itself
# ---------------------------------------------------------------------------

HELDOUT_LIVESHOW_FIXTURES = [fx for fx in LIVESHOW_SONG_FIXTURES if fx["split"] == "heldout"]


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--i-am-consuming-heldout", action="store_true",
        help="REQUIRED. Heldout spends ONCE per ship candidate (D8) -- passing this flag "
             "declares that this run is that spend. Dev sanity checks use eval/probe_classifier.py.",
    )
    parser.add_argument("--data-root", type=Path, default=DATA_ROOT,
                        help="eval data store (default: eval.paths.DATA_ROOT, the main checkout's store)")
    parser.add_argument("--classifier-weights", type=Path, default=None,
                        help=f"classifier checkpoint (default: <data-root>/{DEFAULT_WEIGHTS_REL})")
    parser.add_argument("--out", type=Path, required=True,
                        help="JSON scoreboard artifact path (e.g. eval/scoreboard/exam_classifier_<candidate>.json)")
    args = parser.parse_args(argv)

    if not args.i_am_consuming_heldout:
        print(
            "[exam_classifier] REFUSING to run: this script reads HELDOUT slices "
            "(liveshow_stagnate, liveshow_basalt, E-GMD heldout), which spend ONCE per "
            "ship candidate (D8). Re-run with --i-am-consuming-heldout if this is that "
            "spend; otherwise use eval/probe_classifier.py (dev slices only).",
            file=sys.stderr,
        )
        return 2

    weights = args.classifier_weights or (args.data_root / DEFAULT_WEIGHTS_REL)
    if not weights.exists():
        print(f"[exam_classifier] classifier weights MISSING: {weights} -- an exam without "
              f"the candidate model is meaningless; refusing to spend heldout on it.", file=sys.stderr)
        return 3

    print("[exam_classifier] consuming heldout (liveshow_stagnate + liveshow_basalt + E-GMD heldout) ...", file=sys.stderr)
    groups = {
        SLICE_LIVESHOW: build_liveshow_tracks(HELDOUT_LIVESHOW_FIXTURES, args.data_root),
        SLICE_EGMD: build_egmd_tracks("heldout", args.data_root),
    }
    classes_by_group = {SLICE_LIVESHOW: BAR_CLASSES, SLICE_EGMD: DRUM_CLASSES}

    scoreboard = run_scoreboard(
        groups, classes_by_group, str(weights), adtof_only=False,
        group_titles={SLICE_LIVESHOW: "liveshow heldout", SLICE_EGMD: "E-GMD heldout"},
    )
    payload = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "kind": "audio_event_classifier D8 heldout exam (one-shot)",
        "classifier_weights": str(weights),
        "bar": {
            "per_class": f"classifier F1 >= ADTOF F1 - {SHIP_TOLERANCE} for kick/snare/hat",
            "drums_filed_as_other": f"< {DRUMS_FILED_AS_OTHER_BAR:.0%} on liveshow heldout",
        },
        "corpus_summary": {
            label: [{"id": t.id, "domain": t.domain, "truth_type": t.truth_type} for t in tracks]
            for label, tracks in groups.items()
        },
        **scoreboard,
    }
    print_table(scoreboard, adtof_only=False)
    write_scoreboard(args.out, payload)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
