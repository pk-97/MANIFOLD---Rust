"""Fit Stage-1 class-signature profiles from DEV truth ONLY (B2 lever 2,
docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md 2026-07-18 addendum's B2 round-1
orchestrator note: "Replace hand-tuned signature thresholds with dev-fitted
ones ... supervised calibration of thresholds is fair game in the tuning
loop, it is NOT model training").

This is a NEAREST-CENTROID CLASSIFIER FIT, not a trained model: pools
(feature_vector, class_label) pairs from DEV-only truth --
  - self_render edm_kit_128bpm: all 5 classes (kick/snare/clap/hat/tom),
    clean truth, no two classes share an onset instant (see its own
    docstring in eval/fetch/self_render.py) -- the only source with
    clap/tom truth at all.
  - babyslakh dev tracks (isolated is_drum STEM audio, not mix.wav --
    Stage-1 is a drum-stem detector; matches the same fix already applied
    in eval/bakeoff_b1.py for the ADTOF-comparison corpus): kick/snare/hat/
    perc via eval.baseline_scoreboard_p3.GM_TO_CLASS.
  - E-GMD dev (eval.egmd_drum_truth): kick/snare/hat/perc via GM_TO_CLASS,
    audio already isolated by construction (E-GMD IS a drum-only recording).
  - manifold_own's 5 kick fixtures' isolated `drums.wav` Ableton stem
    (tests/fixtures/audio/<id>/drums.wav): kick only.

kick_hat_128bpm is DELIBERATELY EXCLUDED: its own pattern collides kick and
hat at the exact same onset instant on every downbeat (4-on-the-floor kick +
straight-8th hats sharing the beat grid), so a "kick" truth onset there does
not carry a clean single-instrument signature -- it's the merged kick+hat
attack. edm_kit_128bpm already supplies clean, non-colliding kick+hat truth;
adding kick_hat_128bpm's contaminated examples would only blur the fit.

Fits ONE global StandardScaler over the pooled features (mean/scale per
dimension), then each class's profile centroid = the MEAN of that class's
STANDARDIZED feature vectors. Written to
eval/calibration/stage1_class_profiles.json -- manifold_audio.
stage1_dsp_detection._label_clusters loads this file and assigns each
PER-TRACK cluster (clustering itself stays per-track/relative, unchanged --
"per-track adaptation staying primary" per the design) to its nearest
profile centroid, replacing the hand-written if/elif threshold cascade.

Usage:
    python -m eval.fit_stage1_profiles --out eval/calibration/stage1_class_profiles.json
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import json
import sys
from pathlib import Path
from typing import Any, Dict, List, Tuple

import numpy as np
import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval import egmd_drum_truth  # noqa: E402
from eval.baseline_scoreboard_p3 import BABYSLAKH_ROOT, CLASSES as GM4_CLASSES, GM_TO_CLASS, _load_drum_truth  # noqa: E402
from eval.run import AUDIO_ANALYSIS_ROOT, _load_kick_truth_csv, _resolve_path, load_fixtures  # noqa: E402
from eval.calibration import MANIFOLD_OWN_KICK_FIXTURE_IDS  # noqa: E402
from manifold_audio.audio_io import load_audio_mono  # noqa: E402
from manifold_audio.stage1_dsp_detection import FEATURE_NAMES, extract_onset_features  # noqa: E402

DEFAULT_OUT = AUDIO_ANALYSIS_ROOT / "eval" / "calibration" / "stage1_class_profiles.json"


def _features_for_truth(audio: np.ndarray, sr: int, truth_by_class: Dict[str, List[float]]) -> List[Tuple[np.ndarray, str]]:
    """Pool ALL truth onsets (any class) for this track, sorted, extract
    features in one call (so each onset's window is capped by its true
    NEXT onset regardless of class -- matching how extract_onset_features
    behaves at inference time on Stage-1's own pooled onset list), then
    re-associate each feature row with the class it came from via its
    (rounded) truth time."""
    time_to_class: Dict[float, str] = {}
    all_times: List[float] = []
    for cls, times in truth_by_class.items():
        for t in times:
            key = round(t, 4)
            time_to_class[key] = cls
            all_times.append(t)
    if not all_times:
        return []
    sorted_times = np.asarray(sorted(set(round(t, 4) for t in all_times)), dtype=np.float64)
    feats = extract_onset_features(audio, sr, sorted_times)
    out: List[Tuple[np.ndarray, str]] = []
    for f in feats:
        cls = time_to_class.get(round(f.time_sec, 4))
        if cls is None:
            continue
        out.append((np.array([
            f.centroid_hz, f.flatness, f.low_ratio, f.mid_ratio, f.high_ratio, f.decay_rate_db_per_sec,
        ]), cls))
    return out


def _edm_kit_examples() -> List[Tuple[np.ndarray, str]]:
    base = AUDIO_ANALYSIS_ROOT / "eval" / "data" / "self_render"
    wav = base / "edm_kit_128bpm.wav"
    if not wav.exists():
        print("[fit_stage1_profiles] edm_kit_128bpm.wav missing -- run `python -m eval.fetch.self_render` first", file=sys.stderr)
        return []
    truth_notes = json.loads((base / "edm_kit_128bpm_truth.json").read_text())
    pitch_to_class = {36: "kick", 38: "snare", 39: "clap", 42: "hat", 45: "tom"}
    truth_by_class: Dict[str, List[float]] = {}
    for n in truth_notes:
        cls = pitch_to_class.get(n["pitch"])
        if cls:
            truth_by_class.setdefault(cls, []).append(n["start_sec"])
    audio, sr = load_audio_mono(wav, target_sr=44100, ffmpeg_bin=None)
    examples = _features_for_truth(audio, sr, truth_by_class)
    print(f"[fit_stage1_profiles] self_render edm_kit_128bpm: {len(examples)} examples", file=sys.stderr)
    return examples


def _babyslakh_dev_examples(max_tracks: int = 8) -> List[Tuple[np.ndarray, str]]:
    root = BABYSLAKH_ROOT
    if not root.is_dir():
        return []
    out: List[Tuple[np.ndarray, str]] = []
    track_dirs = sorted(p for p in root.iterdir() if p.is_dir() and p.name.startswith("Track"))
    used = 0
    for td in track_dirs:
        if used >= max_tracks:
            break
        meta_path = td / "metadata.yaml"
        if not meta_path.exists():
            continue
        meta = yaml.safe_load(meta_path.read_text())
        drum_stems = [k for k, v in (meta.get("stems") or {}).items() if v.get("is_drum")]
        if not drum_stems:
            continue
        stem_path = td / "stems" / f"{drum_stems[0]}.wav"
        if not stem_path.exists():
            continue
        truth = _load_drum_truth(td, drum_stems[0])
        if not any(truth.values()):
            continue
        audio, sr = load_audio_mono(stem_path, target_sr=44100, ffmpeg_bin=None)
        examples = _features_for_truth(audio, sr, truth)
        out.extend(examples)
        used += 1
    print(f"[fit_stage1_profiles] babyslakh dev ({used} tracks, isolated drum stems): {len(out)} examples", file=sys.stderr)
    return out


def _egmd_dev_examples() -> List[Tuple[np.ndarray, str]]:
    rows = egmd_drum_truth.available_rows(split="dev")
    out: List[Tuple[np.ndarray, str]] = []
    for i, row in enumerate(rows):
        audio_path = Path(row["audio_path"])
        if not audio_path.exists():
            continue
        truth = egmd_drum_truth.load_drum_truth(Path(row["midi_path"]))
        if not any(truth.values()):
            continue
        audio, sr = load_audio_mono(audio_path, target_sr=44100, ffmpeg_bin=None)
        out.extend(_features_for_truth(audio, sr, truth))
    print(f"[fit_stage1_profiles] E-GMD dev ({len(rows)} tracks): {len(out)} examples", file=sys.stderr)
    return out


def _manifold_own_kick_examples() -> List[Tuple[np.ndarray, str]]:
    fixtures_path = AUDIO_ANALYSIS_ROOT / "eval" / "fixtures.toml"
    fixtures = {f["id"]: f for f in load_fixtures(fixtures_path)}
    out: List[Tuple[np.ndarray, str]] = []
    for fid in MANIFOLD_OWN_KICK_FIXTURE_IDS:
        fixture = fixtures.get(fid)
        if fixture is None:
            continue
        base_dir = _resolve_path(fixture["path"])
        drums_path = base_dir / "drums.wav"
        if not drums_path.exists():
            continue
        truth_mix = _load_kick_truth_csv(_resolve_path(fixture["labels_path"]))["mix"]
        audio, sr = load_audio_mono(drums_path, target_sr=44100, ffmpeg_bin=None)
        out.extend(_features_for_truth(audio, sr, {"kick": truth_mix}))
    print(f"[fit_stage1_profiles] manifold_own kick fixtures (isolated drums.wav): {len(out)} examples", file=sys.stderr)
    return out


def fit_profiles(max_babyslakh_tracks: int = 8) -> Dict[str, Any]:
    all_examples: List[Tuple[np.ndarray, str]] = []
    all_examples.extend(_edm_kit_examples())
    all_examples.extend(_babyslakh_dev_examples(max_babyslakh_tracks))
    all_examples.extend(_egmd_dev_examples())
    all_examples.extend(_manifold_own_kick_examples())

    if not all_examples:
        raise RuntimeError("no dev examples collected -- fetch fixtures first")

    X = np.stack([e[0] for e in all_examples])
    labels = [e[1] for e in all_examples]

    scaler_mean = X.mean(axis=0)
    scaler_scale = X.std(axis=0)
    scaler_scale[scaler_scale < 1e-9] = 1.0
    Xs = (X - scaler_mean) / scaler_scale

    counts: Dict[str, int] = {}
    centroids: Dict[str, List[float]] = {}
    for cls in sorted(set(labels)):
        mask = np.array([lbl == cls for lbl in labels])
        counts[cls] = int(mask.sum())
        centroids[cls] = Xs[mask].mean(axis=0).tolist()

    return {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "method": "nearest-centroid classifier fit from DEV truth only (supervised threshold calibration, not model training)",
        "feature_names": list(FEATURE_NAMES),
        "scaler_mean": scaler_mean.tolist(),
        "scaler_scale": scaler_scale.tolist(),
        "class_profile_centroids": centroids,
        "n_examples_per_class": counts,
        "n_examples_total": len(all_examples),
        "sources": {
            "self_render_edm_kit_128bpm": "kick/snare/clap/hat/tom -- clean, non-colliding truth",
            "babyslakh_dev": f"kick/snare/hat/perc via GM_TO_CLASS, isolated is_drum stem audio, up to {max_babyslakh_tracks} tracks",
            "egmd_dev": "kick/snare/hat/perc via GM_TO_CLASS, isolated by construction",
            "manifold_own_kick_fixtures": "kick only, isolated drums.wav Ableton stem",
        },
        "excluded": {
            "self_render_kick_hat_128bpm": "kick+hat collide at the exact same onset instant on every downbeat -- contaminated, not a clean signature",
        },
    }


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--max-babyslakh-tracks", type=int, default=8)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = parser.parse_args(argv)

    payload = fit_profiles(max_babyslakh_tracks=args.max_babyslakh_tracks)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(payload, indent=2))
    print(f"[fit_stage1_profiles] wrote {args.out}", file=sys.stderr)
    print(f"[fit_stage1_profiles] n_examples_per_class: {payload['n_examples_per_class']}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
