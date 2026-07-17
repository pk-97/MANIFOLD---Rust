"""CLI entry point: `python -m eval.run --set dev --report scoreboard/<date>.json`

Orchestrates the P1 baseline: loads fixtures.toml, scores whatever fixtures
in the requested split have real audio available, runs the metamorphic
suite, and writes one scoreboard JSON (aggregates + a worst-N list per
metric, so a later agent/session can read the scoreboard without re-running
anything — the D12 convention, used starting now even though D12's
automation itself is P7).

P1 scope (this file does NOT do): Beat This (P2), the precision
post-processing sweep (P4), chords/phrases/sections emitters (P5), SuperFlux
(P6). It scores the CURRENT pipeline (madmom beats/onsets, ADTOF drums,
basic_pitch notes) as the baseline every later phase's gate is a delta
against.
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import json
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict, List, Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import numpy as np

try:
    import tomllib
except ImportError:  # pragma: no cover - runtime is 3.12, tomllib always present
    import tomli as tomllib  # type: ignore[no-redef]

from manifold_audio.analyzer import analyze_percussion
from manifold_audio.audio_io import load_audio_mono, _read_wav_to_mono_float
from manifold_audio.bpm import estimate_bpm
from manifold_audio.onset_detection import detect_madmom_onsets

from eval import metrics
from eval.metamorphic import DetectorOutput, run_metamorphic_suite

REPO_ROOT = Path(__file__).resolve().parents[3]
AUDIO_ANALYSIS_ROOT = Path(__file__).resolve().parents[1]
SAMPLE_RATE = 44100


def _resolve_path(raw: str) -> Path:
    """fixtures.toml paths are relative to the repo root (tests/fixtures/...)
    or to tools/audio_analysis (eval/data/...) — try both, repo-root first
    since that's what most entries use."""
    for base in (REPO_ROOT, AUDIO_ANALYSIS_ROOT):
        candidate = (base / raw).resolve()
        if candidate.exists():
            return candidate
    return (REPO_ROOT / raw).resolve()


def load_fixtures(toml_path: Path) -> List[Dict[str, Any]]:
    with open(toml_path, "rb") as f:
        data = tomllib.load(f)
    return data["fixture"]


def _fast_onset_detect_fn(audio: np.ndarray, sr: int) -> DetectorOutput:
    """Cheap detector for the metamorphic suite: madmom CNN onsets +
    a bpm estimate from the same activation (autocorrelation) — one model
    pass, not the full multi-stage analyze_percussion(). Writes a temp wav
    because madmom's processors take a file path."""
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=True) as tmp:
        pcm16 = np.clip(audio * 32767.0, -32768, 32767).astype(np.int16)
        import wave

        with wave.open(tmp.name, "wb") as wf:
            wf.setnchannels(1)
            wf.setsampwidth(2)
            wf.setframerate(sr)
            wf.writeframes(pcm16.tobytes())

        result = detect_madmom_onsets(tmp.name, method="cnn")
        if result is None:
            return DetectorOutput(event_times_sec=[], bpm=None)
        activation, onset_times = result
        bpm = estimate_bpm(activation, hop_time=0.01, min_bpm=55.0, max_bpm=215.0)
        return DetectorOutput(event_times_sec=[float(t) for t in onset_times], bpm=bpm)


def _load_kick_truth_csv(labels_path: Path) -> Dict[str, List[float]]:
    mix_times: List[float] = []
    drums_times: List[float] = []
    with open(labels_path, newline="") as f:
        for row in csv.DictReader(f):
            mix_times.append(float(row["mix_time_s"]))
            drums_times.append(float(row["drums_time_s"]))
    return {"mix": mix_times, "drums": drums_times}


def _run_drum_pipeline(audio_path: Path) -> List[float]:
    """Runs the current pipeline (drums only, no stems) on one audio file
    and returns kick-classified event times."""
    audio, sr = load_audio_mono(audio_path, target_sr=SAMPLE_RATE, ffmpeg_bin=None)
    events, bpm, bpm_conf, beat_grid, used_profile, bass_profile, det_metrics, envelope = analyze_percussion(
        audio=audio,
        sample_rate=sr,
        frame_size=1024,
        hop_size=256,
        profile_name="electronic",
        emit_bass=False,
        audio_path=str(audio_path),
        analysis_audio_path=str(audio_path),
        min_bpm=55.0,
        max_bpm=215.0,
        instruments=frozenset({"drums"}),
    )
    return [e.time for e in events if e.type == "kick"]


def score_kick_onset_fixture(fixture: Dict[str, Any]) -> Dict[str, Any]:
    base_dir = _resolve_path(fixture["path"])
    labels_path = _resolve_path(fixture["labels_path"])
    truth = _load_kick_truth_csv(labels_path)
    tol = metrics.EVENT_TOLERANCE_SEC

    result: Dict[str, Any] = {"id": fixture["id"], "bpm": fixture.get("bpm"), "domain": fixture.get("domain", "other")}
    for role, filename in (("mix", "mix.wav"), ("drums", "drums.wav")):
        audio_path = base_dir / filename
        if not audio_path.exists():
            result[role] = {"error": f"missing {audio_path}"}
            continue
        pred_kicks = _run_drum_pipeline(audio_path)
        prf = metrics.event_prf(pred_kicks, truth[role], tolerance_sec=tol)
        result[role] = {"n_pred": len(pred_kicks), "n_truth": len(truth[role]), **prf.to_dict(), "tolerance_sec": tol}
    return result


def run_babyslakh_metamorphic(babyslakh_root: Path, max_tracks: int = 3) -> List[Dict[str, Any]]:
    """Runs the metamorphic suite on the first max_tracks babyslakh mixes —
    fast (one onset-detector pass per perturbation) and satisfies the P1 gate
    ('metamorphic suite passes on babyslakh') with real babyslakh audio."""
    nested = babyslakh_root / "babyslakh_16k"
    root = nested if nested.is_dir() else babyslakh_root
    track_dirs = sorted(p for p in root.iterdir() if p.is_dir() and p.name.startswith("Track"))[:max_tracks]

    out = []
    for track_dir in track_dirs:
        mix_path = track_dir / "mix.wav"
        if not mix_path.exists():
            continue
        audio, sr = load_audio_mono(mix_path, target_sr=SAMPLE_RATE, ffmpeg_bin=None)
        # Cap duration for speed — metamorphic checks re-run the detector
        # several times per track; 20s is enough for gain/stretch/noise checks.
        max_len = 20 * sr
        if len(audio) > max_len:
            audio = audio[:max_len]
        results = run_metamorphic_suite(audio, sr, _fast_onset_detect_fn)
        out.append({"track": track_dir.name, "results": [r.__dict__ for r in results]})
    return out


def measure_kick_noise_floor(fixture: Dict[str, Any], n_runs: int = 3) -> Dict[str, Any]:
    """D11: rerun the FULL pipeline (the ADTOF/torch-based arm, the actual
    nondeterminism source on MPS) N times on the same audio, measure kick-F1
    variance."""
    from eval.noise_floor import measure_noise_floor, write_noise_floor_report

    base_dir = _resolve_path(fixture["path"])
    labels_path = _resolve_path(fixture["labels_path"])
    truth = _load_kick_truth_csv(labels_path)["mix"]
    mix_path = base_dir / "mix.wav"

    def one_run() -> Dict[str, float]:
        pred = _run_drum_pipeline(mix_path)
        prf = metrics.event_prf(pred, truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
        return {"kick_f1": prf.f1, "kick_precision": prf.precision, "kick_recall": prf.recall}

    stats = measure_noise_floor(one_run, n_runs=n_runs)
    return {"fixture": fixture["id"], "n_runs": n_runs, "stats": {k: v.__dict__ for k, v in stats.items()}}


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--set", dest="split", default="dev", choices=["dev", "heldout"])
    parser.add_argument("--fixtures", type=Path, default=AUDIO_ANALYSIS_ROOT / "eval" / "fixtures.toml")
    parser.add_argument("--report", type=Path, default=None)
    parser.add_argument("--skip-noise-floor", action="store_true", help="skip the (slower) N=3 rerun")
    args = parser.parse_args(argv)

    fixtures = load_fixtures(args.fixtures)
    selected = [f for f in fixtures if f["split"] == args.split]

    scoreboard: Dict[str, Any] = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "split": args.split,
        "pipeline_version": "p1-2026-07-17",
        "kick_onset_fixtures": [],
        "metamorphic": {"babyslakh": []},
        "noise_floor": None,
        "skipped": [],
    }

    for fixture in selected:
        if fixture.get("truth_kind") == "kick_onset_csv":
            print(f"[run] scoring {fixture['id']} ...", file=sys.stderr)
            scoreboard["kick_onset_fixtures"].append(score_kick_onset_fixture(fixture))
        elif fixture["id"] == "babyslakh_16k":
            babyslakh_root = _resolve_path(fixture["path"])
            print(f"[run] metamorphic suite on babyslakh ({babyslakh_root}) ...", file=sys.stderr)
            scoreboard["metamorphic"]["babyslakh"] = run_babyslakh_metamorphic(babyslakh_root)
        else:
            scoreboard["skipped"].append({"id": fixture["id"], "reason": "no audio-scoring path wired in P1 (registered, not yet scored)"})

    if not args.skip_noise_floor:
        apricots = next((f for f in selected if f["id"] == "apricots_128bpm"), None)
        if apricots is not None:
            print("[run] D11 noise floor (N=3, apricots mix.wav) ...", file=sys.stderr)
            scoreboard["noise_floor"] = measure_kick_noise_floor(apricots, n_runs=3)

    # Worst-N summary (D12 convention) over the kick fixtures by F1.
    kick_rows = [
        {
            "id": r["id"],
            "domain": r.get("domain", "other"),
            "mix_f1": r.get("mix", {}).get("f1"),
            "drums_f1": r.get("drums", {}).get("f1"),
        }
        for r in scoreboard["kick_onset_fixtures"]
    ]
    kick_rows_valid = [r for r in kick_rows if r["mix_f1"] is not None]
    scoreboard["worst_kick_mix_f1"] = sorted(kick_rows_valid, key=lambda r: r["mix_f1"])[:10]

    # Per-domain aggregates (Peter, 2026-07-17): the tuning loop targets the
    # electronic slice; "other" is a sanity check only, never a co-equal
    # veto. This scoreboard exposes the split — applying that policy is the
    # orchestrator's job, not sweep.py's.
    scoreboard["by_domain"] = {
        "kick_mix_f1": metrics.domain_aggregate(kick_rows_valid, "mix_f1"),
        "kick_drums_f1": metrics.domain_aggregate([r for r in kick_rows if r["drums_f1"] is not None], "drums_f1"),
    }

    report_path = args.report or (AUDIO_ANALYSIS_ROOT / "eval" / "scoreboard" / f"{args.split}_{dt.date.today().isoformat()}.json")
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(scoreboard, indent=2))
    print(f"[run] wrote {report_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
