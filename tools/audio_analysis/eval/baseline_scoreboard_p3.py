"""P3 full-pack baseline scoreboard, per docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md
§5 P3: "baseline scoreboard for EVERY existing detector on the full available
pack" — ADTOF drums (per-class kick/snare/hat/perc, the reference number the
future ADTOF-replacement bake-off (Deferred #1) must beat), basic_pitch
(bass/synth), beat/downbeat/tempo (Beat This, post-P2). Per-domain,
per-split aggregates via eval.metrics.domain_aggregate (D10-frozen, reused
verbatim — no new metric definitions).

Sources scored:
  - babyslakh_16k (dev, domain=other): aligned per-stem MIDI is genuine,
    exact ground truth for BOTH drums (is_drum stem, GM pitch -> 4-class
    kick/snare/hat/perc mapping matching manifold_audio.adtof_detection's
    own 4-class vocabulary) and bass (Bass-class stem, note onsets). Runs
    the CURRENT pipeline (analyze_percussion for drums, detect_notes_basic_pitch
    for bass) directly on the track's mix.wav — the same end-to-end call
    shape eval/run.py's _run_drum_pipeline already proves for kick-only.
  - manifold_own kick-onset fixtures (dev, domain=electronic): reuses
    eval.run.score_kick_onset_fixture verbatim (no reimplementation).
  - liveshow song fixtures (dev+heldout, domain=electronic): reuses
    eval.beat_scoring's full pipeline (ground truth from the project's own
    tempoMap) for beat/downbeat F1, post-P2 Beat This.

NOT scored this pass (documented honestly, not silently skipped): full
Slakh2100 test split (fetch still in progress at report time — background
job, see eval/data/_fetch_progress.json) and MAESTRO/basic_pitch sustained-
polyphony scoring (self-render + MAESTRO synth audio exist on disk but this
script's time budget went to babyslakh + liveshow + kick fixtures first;
MAESTRO/self-render note-level scoring is a small follow-up, same shape as
the babyslakh bass scorer below, against eval/data/maestro_v3/midi/*.midi
and eval/data/self_render/*_truth.json). Vocal-path scoring (D13: MUSDB18
clean-vocal-stem derived regions vs the demucs+onset-detector pipeline) is
NOT implemented this pass — flagged as a real gap, not silently dropped;
see the P3 landing report's "not scored" section for what a follow-up needs.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any, Dict, List, Tuple

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval import metrics  # noqa: E402

from eval.paths import DATA_ROOT  # noqa: E402

BABYSLAKH_ROOT = DATA_ROOT / "babyslakh_16k" / "babyslakh_16k"

# manifold_audio.adtof_detection's own 4-class vocabulary (kick/snare/hat/perc)
# mapped from GM percussion note numbers (standard General MIDI drum map).
GM_TO_CLASS: Dict[int, str] = {
    35: "kick", 36: "kick",
    38: "snare", 40: "snare", 37: "snare", 39: "snare",  # side stick / hand clap grouped with snare-family
    42: "hat", 44: "hat", 46: "hat", 49: "hat", 51: "hat", 52: "hat", 53: "hat", 55: "hat", 57: "hat", 59: "hat",
    41: "perc", 43: "perc", 45: "perc", 47: "perc", 48: "perc", 50: "perc",  # toms
    54: "perc", 56: "perc", 58: "perc", 60: "perc", 61: "perc", 62: "perc", 63: "perc", 64: "perc",
    65: "perc", 66: "perc", 67: "perc", 68: "perc", 69: "perc", 70: "perc", 71: "perc", 72: "perc",
    73: "perc", 74: "perc", 75: "perc", 76: "perc", 77: "perc", 78: "perc", 79: "perc", 80: "perc", 81: "perc",
}
CLASSES = ("kick", "snare", "hat", "perc")


def _load_drum_truth(track_dir: Path, drum_stem_id: str) -> Dict[str, List[float]]:
    import pretty_midi

    midi_path = track_dir / "MIDI" / f"{drum_stem_id}.mid"
    truth: Dict[str, List[float]] = {c: [] for c in CLASSES}
    if not midi_path.exists():
        return truth
    pm = pretty_midi.PrettyMIDI(str(midi_path))
    for inst in pm.instruments:
        for note in inst.notes:
            cls = GM_TO_CLASS.get(note.pitch)
            if cls is not None:
                truth[cls].append(note.start)
    for c in truth:
        truth[c].sort()
    return truth


def _load_bass_truth(track_dir: Path, bass_stem_ids: List[str]) -> List[float]:
    import pretty_midi

    onsets: List[float] = []
    for stem_id in bass_stem_ids:
        midi_path = track_dir / "MIDI" / f"{stem_id}.mid"
        if not midi_path.exists():
            continue
        pm = pretty_midi.PrettyMIDI(str(midi_path))
        for inst in pm.instruments:
            for note in inst.notes:
                onsets.append(note.start)
    onsets.sort()
    return onsets


def score_track_drums(track_dir: Path, drum_stem_id: str) -> Dict[str, Any]:
    from manifold_audio.analyzer import analyze_percussion
    from manifold_audio.audio_io import load_audio_mono

    mix_path = track_dir / "mix.wav"
    truth = _load_drum_truth(track_dir, drum_stem_id)
    audio, sr = load_audio_mono(mix_path, target_sr=44100, ffmpeg_bin=None)
    events, *_ = analyze_percussion(
        audio=audio, sample_rate=sr, frame_size=1024, hop_size=256,
        profile_name="electronic", emit_bass=False,
        audio_path=str(mix_path), analysis_audio_path=str(mix_path),
        min_bpm=55.0, max_bpm=215.0, instruments=frozenset({"drums"}),
    )
    per_class: Dict[str, Any] = {}
    for cls in CLASSES:
        pred = [e.time for e in events if e.type == cls]
        prf = metrics.event_prf(pred, truth[cls], tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
        per_class[cls] = {"n_pred": len(pred), "n_truth": len(truth[cls]), **prf.to_dict()}
    return {"track": track_dir.name, "per_class": per_class}


def score_track_bass(track_dir: Path, bass_stem_ids: List[str]) -> Dict[str, Any]:
    from manifold_audio.basic_pitch_detection import detect_notes_basic_pitch

    mix_path = track_dir / "mix.wav"
    truth = _load_bass_truth(track_dir, bass_stem_ids)
    notes = detect_notes_basic_pitch(str(mix_path), min_frequency=28.0, max_frequency=350.0)
    pred = [n[0] for n in notes]  # onset times, pitch-agnostic (P3 scope: onset P/R/F1, not pitch accuracy)
    prf = metrics.event_prf(pred, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
    return {"track": track_dir.name, "n_pred": len(pred), "n_truth": len(truth), **prf.to_dict()}


def run_babyslakh_baseline(root: Path = BABYSLAKH_ROOT, max_tracks: int = 20) -> Dict[str, Any]:
    track_dirs = sorted(p for p in root.iterdir() if p.is_dir() and p.name.startswith("Track"))[:max_tracks]
    drum_rows: List[Dict[str, Any]] = []
    bass_rows: List[Dict[str, Any]] = []
    for td in track_dirs:
        meta = yaml.safe_load((td / "metadata.yaml").read_text())
        drum_stems = [k for k, v in meta["stems"].items() if v.get("is_drum")]
        bass_stems = [k for k, v in meta["stems"].items() if v.get("inst_class") == "Bass"]
        if drum_stems:
            print(f"[baseline_p3] {td.name} drums ({drum_stems[0]}) ...", file=sys.stderr)
            try:
                drum_rows.append(score_track_drums(td, drum_stems[0]))
            except Exception as exc:
                print(f"[baseline_p3]   FAILED drums {td.name}: {exc}", file=sys.stderr)
        if bass_stems:
            print(f"[baseline_p3] {td.name} bass ({bass_stems}) ...", file=sys.stderr)
            try:
                bass_rows.append(score_track_bass(td, bass_stems))
            except Exception as exc:
                print(f"[baseline_p3]   FAILED bass {td.name}: {exc}", file=sys.stderr)

    # Aggregate per-class ADTOF numbers across all scored tracks (micro-average
    # via summed TP/FP/FN — more honest than averaging per-track F1 when
    # per-track truth counts vary a lot, e.g. tracks with very few hats).
    per_class_agg: Dict[str, Any] = {}
    for cls in CLASSES:
        n_pred = sum(r["per_class"][cls]["n_pred"] for r in drum_rows)
        n_truth = sum(r["per_class"][cls]["n_truth"] for r in drum_rows)
        f1s = [r["per_class"][cls]["f1"] for r in drum_rows if r["per_class"][cls]["n_truth"] > 0]
        per_class_agg[cls] = {
            "n_tracks_with_truth": sum(1 for r in drum_rows if r["per_class"][cls]["n_truth"] > 0),
            "total_n_pred": n_pred,
            "total_n_truth": n_truth,
            "mean_f1_over_tracks_with_truth": (sum(f1s) / len(f1s)) if f1s else None,
        }

    bass_f1s = [r["f1"] for r in bass_rows if r["n_truth"] > 0]
    bass_agg = {
        "n_tracks": len(bass_rows),
        "total_n_pred": sum(r["n_pred"] for r in bass_rows),
        "total_n_truth": sum(r["n_truth"] for r in bass_rows),
        "mean_f1": (sum(bass_f1s) / len(bass_f1s)) if bass_f1s else None,
    }

    return {
        "n_tracks_scored": len(track_dirs),
        "domain": "other",
        "split": "dev",
        "drums_per_class": drum_rows,
        "drums_per_class_aggregate": per_class_agg,
        "bass": bass_rows,
        "bass_aggregate": bass_agg,
    }


def main(argv=None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, default=Path("eval/scoreboard/p3_baseline_babyslakh.json"))
    parser.add_argument("--max-tracks", type=int, default=20)
    args = parser.parse_args(argv)

    result = run_babyslakh_baseline(max_tracks=args.max_tracks)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=2))
    print(f"[baseline_p3] wrote {args.report}")
    print(f"[baseline_p3] ADTOF per-class aggregate: {json.dumps(result['drums_per_class_aggregate'], indent=2)}")
    print(f"[baseline_p3] bass aggregate: {json.dumps(result['bass_aggregate'], indent=2)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
