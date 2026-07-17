"""ADTOF bake-off phase B1 -- first scoreboard (docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md
2026-07-18 addendum): Stage-1 DSP-only cluster-and-label detector
(manifold_audio.stage1_dsp_detection) vs post-P4 ADTOF, side by side, per
class, on IDENTICAL dev fixtures, under the identical scoring mechanism.

DEV ONLY -- HELDOUT FORBIDDEN. This module never references a heldout
fixture/split; the orchestrator triggers a heldout read separately at
verdict time (per the addendum's B1/B2/B3 phasing and D9).

ADTOF arm reuses eval.sweep_p4.score_config_for_class with a bare
PrecisionConfig() (== the accepted P4 production defaults) -- the EXACT
mechanism that produced the addendum's cited bar (dense F1 kick 0.858 /
snare 0.641 / hat 0.303 / perc 0.521), so this scoreboard's ADTOF numbers are
directly comparable to that bar, not a re-derivation under different rules.

Stage-1 arm runs manifold_audio.stage1_dsp_detection.detect_drums_stage1 on
the SAME corpus's raw audio, folds its 6-class vocabulary down to ADTOF's
4-class one (clap -> snare, tom -> perc -- matching
eval.baseline_scoreboard_p3.GM_TO_CLASS's own folding, so Slakh/E-GMD/
babyslakh truth already speaks this vocabulary), then scores identically via
eval.metrics.event_prf (dense truth only -- every fixture in this corpus is
dense: MIDI-aligned or exact self-render truth, no sparse-visual liveshow
fixtures in scope for B1).

Corpus (dev only): babyslakh (dense, domain=other), self_render kick_hat +
edm_kit (dense, domain=electronic), manifold_own 5 calibrated kick fixtures
(dense, domain=electronic, kick-only), E-GMD dev subset (dense,
domain=other -- eval.egmd_drum_truth), Slakh2100-test dev drum-stem tracks
currently fetched (dense, domain=other -- eval.slakh_drum_truth).

Usage:
    python -m eval.bakeoff_b1 --out eval/scoreboard/bakeoff_b1_stage1.json
"""

from __future__ import annotations

import datetime as dt
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import yaml

from eval import egmd_drum_truth, metrics, slakh_drum_truth  # noqa: E402
from eval.baseline_scoreboard_p3 import BABYSLAKH_ROOT  # noqa: E402
from manifold_audio.precision_postprocessing import PrecisionConfig  # noqa: E402
from eval.sweep_p4 import (  # noqa: E402
    TrackData,
    _build_babyslakh_tracks,
    _build_manifold_own_kick_tracks,
    _build_self_render_tracks,
    score_config_for_class,
)
from manifold_audio.adtof_detection import detect_drums_adtof_activations  # noqa: E402
from manifold_audio.audio_io import load_audio_mono  # noqa: E402
from manifold_audio.stage1_dsp_detection import detect_drums_stage1  # noqa: E402
from eval.run import AUDIO_ANALYSIS_ROOT  # noqa: E402

CLASSES = ("kick", "snare", "hat", "perc")

# Stage-1's 6-class vocabulary folded to ADTOF's 4-class one -- matches
# eval.baseline_scoreboard_p3.GM_TO_CLASS's own folding (clap -> snare via
# GM pitch 39, tom -> perc), so all truth sources in this corpus and both
# detectors' predictions speak the SAME 4-class vocabulary.
STAGE1_TO_ADTOF_CLASS = {
    "kick": "kick", "snare": "snare", "clap": "snare",
    "hat": "hat", "tom": "perc", "perc": "perc",
}

EGMD_MAX_TRACKS: Optional[int] = None  # None = all fetched dev rows
SLAKH_MAX_TRACKS: Optional[int] = None


@dataclass
class Stage1Result:
    track_id: str
    domain: str
    events_by_class: Dict[str, List[float]]
    degenerate: bool
    degenerate_reason: Optional[str]
    k: int
    silhouette: Optional[float]


def _build_egmd_tracks(max_tracks: Optional[int] = EGMD_MAX_TRACKS) -> List[TrackData]:
    rows = egmd_drum_truth.available_rows(split="dev")
    if max_tracks is not None:
        rows = rows[:max_tracks]
    out: List[TrackData] = []
    for i, row in enumerate(rows):
        audio_path = Path(row["audio_path"])
        if not audio_path.exists():
            continue
        print(f"[bakeoff_b1] egmd {row['id']} ({i + 1}/{len(rows)}): ADTOF inference ...", file=sys.stderr)
        activations, fps = detect_drums_adtof_activations(str(audio_path))
        audio, sr = load_audio_mono(audio_path, target_sr=44100, ffmpeg_bin=None)
        truth = egmd_drum_truth.load_drum_truth(Path(row["midi_path"]))
        out.append(TrackData(
            id=f"egmd_{row['id'].replace('/', '_')}", domain="other", source="egmd",
            audio=audio, sr=sr, audio_path=str(audio_path),
            activations=activations, fps=fps, basic_pitch_notes=None,
            truth_type="dense", truth_by_class=truth,
        ))
    return out


def _build_slakh_drum_tracks(max_tracks: Optional[int] = SLAKH_MAX_TRACKS) -> List[TrackData]:
    rows = [r for r in slakh_drum_truth.available_drum_tracks() if r["split"] == "dev"]
    if max_tracks is not None:
        rows = rows[:max_tracks]
    out: List[TrackData] = []
    for i, row in enumerate(rows):
        audio_path = Path(row["audio_path"])
        print(f"[bakeoff_b1] slakh_drums {row['id']} ({i + 1}/{len(rows)}): ADTOF inference ...", file=sys.stderr)
        activations, fps = detect_drums_adtof_activations(str(audio_path))
        audio, sr = load_audio_mono(audio_path, target_sr=44100, ffmpeg_bin=None)
        truth = slakh_drum_truth.load_drum_truth(Path(row["midi_path"]))
        out.append(TrackData(
            id=f"slakh_{row['id']}", domain="other", source="slakh2100_test",
            audio=audio, sr=sr, audio_path=str(audio_path),
            activations=activations, fps=fps, basic_pitch_notes=None,
            truth_type="dense", truth_by_class=truth,
        ))
    return out


def _build_edm_kit_track() -> Optional[TrackData]:
    from eval.paths import DATA_ROOT
    base = DATA_ROOT / "self_render"
    wav = base / "edm_kit_128bpm.wav"
    if not wav.exists():
        return None
    truth_notes = json.loads((base / "edm_kit_128bpm_truth.json").read_text())
    print("[bakeoff_b1] self_render edm_kit_128bpm: ADTOF inference ...", file=sys.stderr)
    activations, fps = detect_drums_adtof_activations(str(wav))
    audio, sr = load_audio_mono(wav, target_sr=44100, ffmpeg_bin=None)
    pitch_to_class = {36: "kick", 38: "snare", 39: "snare", 42: "hat", 45: "perc"}
    truth: Dict[str, List[float]] = {c: [] for c in CLASSES}
    for n in truth_notes:
        cls = pitch_to_class.get(n["pitch"])
        if cls:
            truth[cls].append(n["start_sec"])
    for c in truth:
        truth[c].sort()
    return TrackData(
        id="self_render_edm_kit_128bpm", domain="electronic", source="self_render",
        audio=audio, sr=sr, audio_path=str(wav),
        activations=activations, fps=fps, basic_pitch_notes=None,
        truth_type="dense", truth_by_class=truth,
    )


def build_b1_corpus(max_babyslakh_tracks: int = 8) -> List[TrackData]:
    corpus: List[TrackData] = []
    corpus.extend(_build_babyslakh_tracks(max_babyslakh_tracks))
    corpus.extend(_build_self_render_tracks())  # existing: kick_hat_128bpm (+ arp, synth-only)
    edm_kit = _build_edm_kit_track()
    if edm_kit is not None:
        corpus.append(edm_kit)
    corpus.extend(_build_manifold_own_kick_tracks())
    corpus.extend(_build_egmd_tracks())
    corpus.extend(_build_slakh_drum_tracks())
    return corpus


def _babyslakh_drum_stem_audio_override(corpus: List[TrackData]) -> Dict[str, Any]:
    """Stage-1 is a drum-STEM detector (its whole premise: onset detection +
    clustering on isolated drum audio, per §7.1 -- production feeds it a
    demucs-separated stem, never the full mix). eval.sweep_p4's babyslakh
    builder loads `mix.wav` for every track (correct for ADTOF, which is a
    mix-level model) -- reusing that SAME audio for Stage-1 would test it on
    the wrong signal entirely (measured 2026-07-18: 0 hat predictions and
    ~3-8x over-prediction on snare/perc across the babyslakh dev tracks
    before this override existed, from Stage-1 clustering every OTHER
    instrument's onsets in the mix too). This loads each babyslakh track's
    OWN is_drum stem audio instead, keyed by the same track id
    eval.sweep_p4._build_babyslakh_tracks already assigned ("babyslakh_<dir>")."""
    override: Dict[str, Any] = {}
    for track in corpus:
        if track.source != "babyslakh":
            continue
        track_dir_name = track.id[len("babyslakh_"):]
        track_dir = BABYSLAKH_ROOT / track_dir_name
        meta_path = track_dir / "metadata.yaml"
        if not meta_path.exists():
            continue
        meta = yaml.safe_load(meta_path.read_text())
        drum_stems = [k for k, v in (meta.get("stems") or {}).items() if v.get("is_drum")]
        if not drum_stems:
            continue
        stem_path = track_dir / "stems" / f"{drum_stems[0]}.wav"
        if not stem_path.exists():
            continue
        audio, sr = load_audio_mono(stem_path, target_sr=44100, ffmpeg_bin=None)
        override[track.id] = (audio, sr)
    return override


def _manifold_own_drum_stem_audio_override(corpus: List[TrackData]) -> Dict[str, Any]:
    """SAME bug class as the babyslakh override above, found later (B2 round
    1, 2026-07-18): eval.sweep_p4._build_manifold_own_kick_tracks ALSO loads
    `mix.wav` (correct for ADTOF), and this bakeoff script's Stage-1 arm was
    silently reusing it too -- these 5 fixtures (tests/fixtures/audio/*)
    each have their OWN isolated `drums.wav` Ableton stem sitting right next
    to mix.wav on disk (never used by this script until now). Measured
    impact: apricots_128bpm's kick F1 went from 0.0 (fed the full
    commercial-sounding mix) to 0.645 (fed drums.wav) in an isolated check;
    this override applies the same fix inside the full scoreboard run."""
    override: Dict[str, Any] = {}
    for track in corpus:
        if track.source != "manifold_own":
            continue
        drums_path = Path(track.audio_path).with_name("drums.wav")
        if not drums_path.exists():
            continue
        audio, sr = load_audio_mono(drums_path, target_sr=44100, ffmpeg_bin=None)
        override[track.id] = (audio, sr)
    return override


def run_stage1_on_corpus(corpus: List[TrackData]) -> List[Stage1Result]:
    audio_override = _babyslakh_drum_stem_audio_override(corpus)
    audio_override.update(_manifold_own_drum_stem_audio_override(corpus))
    out: List[Stage1Result] = []
    for track in corpus:
        audio, sr = audio_override.get(track.id, (track.audio, track.sr))
        events, cluster_result = detect_drums_stage1(audio, sr)
        by_class: Dict[str, List[float]] = {c: [] for c in CLASSES}
        for e in events:
            folded = STAGE1_TO_ADTOF_CLASS.get(e.type)
            if folded:
                by_class[folded].append(e.time)
        out.append(Stage1Result(
            track_id=track.id, domain=track.domain, events_by_class=by_class,
            degenerate=cluster_result.degenerate, degenerate_reason=cluster_result.degenerate_reason,
            k=cluster_result.k, silhouette=cluster_result.silhouette,
        ))
    return out


def score_stage1_for_class(corpus: List[TrackData], stage1_results: List[Stage1Result], class_name: str) -> Dict[str, Any]:
    by_id = {r.track_id: r for r in stage1_results}
    rows = []
    for track in corpus:
        truth = track.truth_by_class.get(class_name)
        if not truth:
            continue
        s1 = by_id.get(track.id)
        if s1 is None:
            continue
        pred = s1.events_by_class.get(class_name, [])
        prf = metrics.event_prf(pred, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
        rows.append({
            "id": track.id, "domain": track.domain, "source": track.source,
            "n_pred": len(pred), "n_truth": len(truth), f"{class_name}_f1": prf.f1,
            "precision": prf.precision, "recall": prf.recall,
        })
    valid = [r for r in rows if r["n_truth"] > 0]
    mean_f1 = float(np.mean([r[f"{class_name}_f1"] for r in valid])) if valid else None
    by_domain = metrics.domain_aggregate(valid, f"{class_name}_f1") if valid else {}
    return {"per_track": rows, "mean_f1": mean_f1, "by_domain": by_domain, "n_tracks_scored": len(valid)}


def main(argv: Optional[List[str]] = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--max-babyslakh-tracks", type=int, default=8)
    parser.add_argument("--out", type=Path, default=AUDIO_ANALYSIS_ROOT / "eval" / "scoreboard" / "bakeoff_b1_stage1.json")
    args = parser.parse_args(argv)

    print("[bakeoff_b1] building DEV corpus (babyslakh + self_render + manifold_own kick + E-GMD dev + Slakh-drums dev) ...", file=sys.stderr)
    corpus = build_b1_corpus(max_babyslakh_tracks=args.max_babyslakh_tracks)
    print(f"[bakeoff_b1] corpus: {len(corpus)} tracks", file=sys.stderr)

    print("[bakeoff_b1] running Stage-1 DSP detector on every track ...", file=sys.stderr)
    stage1_results = run_stage1_on_corpus(corpus)
    degenerate = [r for r in stage1_results if r.degenerate]

    base_config = PrecisionConfig()
    per_class: Dict[str, Any] = {}
    for class_name in CLASSES:
        print(f"[bakeoff_b1] scoring class={class_name} (stage1 + adtof) ...", file=sys.stderr)
        adtof_score = score_config_for_class(corpus, class_name, base_config)
        stage1_score = score_stage1_for_class(corpus, stage1_results, class_name)
        per_class[class_name] = {
            "stage1": stage1_score,
            "adtof_post_p4": {
                "mean_f1": adtof_score["dense"]["mean_f1"],
                "by_domain": adtof_score["dense"]["by_domain"],
                "n_tracks_scored": adtof_score["dense"]["n_tracks_scored"],
                "per_track": adtof_score["dense"]["per_track"],
            },
        }
        print(f"    stage1 mean_f1={stage1_score['mean_f1']} (n={stage1_score['n_tracks_scored']}) "
              f"vs adtof_post_p4 mean_f1={adtof_score['dense']['mean_f1']} (n={adtof_score['dense']['n_tracks_scored']})",
              file=sys.stderr)

    payload = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "phase": "B1 -- first scoreboard, DEV ONLY, HELDOUT FORBIDDEN",
        "corpus_summary": [{"id": t.id, "domain": t.domain, "source": t.source} for t in corpus],
        "n_tracks": len(corpus),
        "per_class": per_class,
        "stage1_degenerate_tracks": [
            {"id": r.track_id, "domain": r.domain, "k": r.k, "silhouette": r.silhouette, "reason": r.degenerate_reason}
            for r in degenerate
        ],
        "n_stage1_degenerate": len(degenerate),
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(payload, indent=2))
    print(f"[bakeoff_b1] wrote {args.out}", file=sys.stderr)
    print(f"[bakeoff_b1] {len(degenerate)}/{len(corpus)} tracks flagged degenerate by Stage-1 clustering", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
