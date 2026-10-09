"""Reproduce the rejected kick false-positive ratio-growth experiment.

The recorded v5 report remains the authority for the original candidates.  This
module only evaluates the fixed post-candidate ratio gate and records local
stem-ablation evidence for the two diagnosed tracks.
"""

from __future__ import annotations

import argparse
import csv
from datetime import date
import hashlib
import json
import math
from pathlib import Path
import subprocess
import sys
from typing import Iterable

import numpy as np
from scipy.io import wavfile
from scipy.signal import butter, lfilter, resample_poly, sosfilt

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from eval.live_kick_baseline import (
    exclusion_regions,
    match_events,
    parse_harness,
    score_events,
)


ROOT = Path(__file__).resolve().parents[3]
BASE_REPORT = ROOT / "tools/audio_analysis/eval/scoreboard/kick_attack_trial_2026-10-09.json"
SOURCE = ROOT / "crates/manifold-audio/examples/kick_attack_probe.rs"
STEMS = ("bass", "drums", "vocals", "others")
BANDS = ((45.0, 140.0), (140.0, 400.0), (400.0, 2_000.0))
FAST_TAU_S = 0.003
SLOW_TAU_S = 0.080
GROWTH = 4.0
TIMEOUT_S = 0.035


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_audio(path: Path, target_sr: int | None = None) -> tuple[int, np.ndarray]:
    sample_rate, samples = wavfile.read(path)
    if np.issubdtype(samples.dtype, np.integer):
        samples = samples.astype(np.float64) / float(2 ** (samples.dtype.itemsize * 8 - 1))
    else:
        samples = samples.astype(np.float64)
    if samples.ndim == 2:
        samples = samples.mean(axis=1)
    if target_sr is not None and sample_rate != target_sr:
        divisor = math.gcd(sample_rate, target_sr)
        samples = resample_poly(
            samples, target_sr // divisor, sample_rate // divisor
        ).astype(np.float64, copy=False)
        sample_rate = target_sr
    return int(sample_rate), samples


def causal_features(samples: np.ndarray, sample_rate: int) -> tuple[np.ndarray, int]:
    """Return native-rate causal (hop, band, fast/slow) power envelopes."""
    if sample_rate <= 4_000:
        raise ValueError(f"sample rate {sample_rate} Hz is too low for the 2 kHz band")
    hop = max(1, round(sample_rate * 256 / 48_000))
    count = len(samples) // hop
    bands = []
    for low, high in BANDS:
        filtered = sosfilt(
            butter(2, [low, high], btype="bandpass", fs=sample_rate, output="sos"),
            samples,
        )
        power = filtered * filtered
        envelopes = []
        for tau in (FAST_TAU_S, SLOW_TAU_S):
            alpha = math.exp(-1.0 / (tau * sample_rate))
            smoothed = lfilter([1.0 - alpha], [1.0, -alpha], power)
            envelopes.append(smoothed[hop - 1 : count * hop : hop])
        bands.append(np.stack(envelopes, axis=1))
    return np.stack(bands, axis=1), hop


def ratio_growth_gate(
    envelopes: np.ndarray,
    candidates: Iterable[int],
    hop: int,
    sample_rate: int,
) -> tuple[list[float], list[dict[str, object]]]:
    """Apply the one fixed 4x low.fast/body.fast growth gate.

    A decision is made only at a completed future hop within 35 ms.  The
    returned time is the confirmation hop end; no candidate is backdated.
    """
    dt = hop / sample_rate
    accepted: list[float] = []
    decisions: list[dict[str, object]] = []
    for candidate in candidates:
        if candidate < 0 or candidate >= len(envelopes):
            raise ValueError(f"candidate hop {candidate} is outside the envelope")
        low_fast = float(envelopes[candidate, 0, 0])
        body_fast = float(envelopes[candidate, 1, 0])
        initial_ratio = low_fast / (body_fast + 1e-12)
        decision: dict[str, object] = {
            "candidate_hop": int(candidate),
            "candidate_s": (candidate + 1) * dt,
            "initial_ratio_low_fast_body_fast": initial_ratio,
            "confirmed_s": None,
            "status": ("rejected_timeout" if (len(envelopes) - 1 - candidate) * dt >= TIMEOUT_S
                       else "unconfirmed_eof"),
        }
        if not (math.isfinite(initial_ratio) and low_fast > 1e-12 and body_fast > 1e-12):
            decision["status"] = "rejected_no_ratio"
            decisions.append(decision)
            continue
        threshold = GROWTH * initial_ratio
        for future in range(candidate + 1, len(envelopes)):
            delay = (future - candidate) * dt
            if delay > TIMEOUT_S + 1e-12:
                break
            ratio = float(envelopes[future, 0, 0]) / (
                float(envelopes[future, 1, 0]) + 1e-12
            )
            if math.isfinite(ratio) and ratio >= threshold:
                confirmed = (future + 1) * dt
                decision.update(
                    status="accepted", confirmed_s=confirmed,
                    confirmation_hop=int(future), confirmation_ratio=ratio,
                )
                accepted.append(confirmed)
                break
        decisions.append(decision)
    return accepted, decisions


def _read_labels(path: Path) -> list[float]:
    return [float(row["mix_time_s"]) for row in csv.DictReader(path.open())]


def _review_rows(path: Path) -> list[dict[str, str]]:
    return list(csv.DictReader(path.open()))


def _validate_provenance(
    base: dict[str, object], audio_root: Path, harness: Path
) -> tuple[dict[str, str], list[dict[str, str]]]:
    expected_binary = base.get("harness_sha256")
    actual_binary = sha(harness)
    source_hashes = base.get("working_tree_sources_sha256")
    if not isinstance(source_hashes, dict) or str(SOURCE.relative_to(ROOT)) not in source_hashes:
        raise ValueError("recorded v5 report has no kick_attack_probe source hash")
    source_key = str(SOURCE.relative_to(ROOT))
    if source_hashes[source_key] != sha(SOURCE):
        raise ValueError("kick_attack_probe source SHA-256 differs from the recorded v5 report")

    label_root = ROOT / "tests/fixtures/audio_labels"
    review_path = label_root / "attack_review.csv"
    expected_review = base.get("review_sha256")
    if expected_review != sha(review_path):
        raise ValueError("attack_review.csv SHA-256 differs from the recorded v5 report")
    tracks = base.get("tracks")
    if not isinstance(tracks, list):
        raise ValueError("recorded v5 report has no tracks")
    for recorded in tracks:
        track = recorded["track"]
        audio = audio_root / track / "mix.wav"
        labels = label_root / f"{track}.csv"
        if recorded["audio_sha256"] != sha(audio):
            raise ValueError(f"{track}: mix SHA-256 differs from the recorded v5 report")
        if recorded["labels_sha256"] != sha(labels):
            raise ValueError(f"{track}: labels SHA-256 differs from the recorded v5 report")
        # A rebuild can change binary bytes without changing this experiment.
        # Require exact native event replay, as well as the source/input hashes.
        replay = subprocess.run([str(harness.resolve()), str(audio.resolve())],
                                capture_output=True, text=True, timeout=120, check=True)
        rate, hop, count, indices, _ = parse_harness(replay.stdout)
        expected_count = round(recorded["duration_s"] * rate) // hop
        if ((rate, hop) != (recorded["sample_rate"], recorded["hop_samples"])
                or count != expected_count or indices != recorded["kick_hops"]):
            raise ValueError(f"{track}: Rust replay differs from recorded v5 events")
    return {"harness": actual_binary, "recorded_harness": str(expected_binary),
            "candidate_replay": "All five Rust event-hop lists match the recorded v5 exactly",
            "source": str(source_hashes[source_key]),
            "review": str(expected_review)}, tracks


def _classification(times: list[float], truth: list[float], review: list[dict[str, str]], duration: float) -> list[str]:
    regions = exclusion_regions(review, duration)
    usable = [i for i, t in enumerate(times)
              if not any(r["start_s"] <= t <= r["end_s"] for r in regions)]
    pairs = match_events([times[i] for i in usable], truth, 0.035, 0.200)
    matched = {usable[p] for _, p in pairs}
    result = []
    for index, time in enumerate(times):
        reasons = [r["reason"] for r in regions if r["start_s"] <= time <= r["end_s"]]
        if reasons:
            result.append("excluded_" + reasons[0])
        elif index in matched:
            result.append("matched_association")
        else:
            result.append("scored_extra")
    return result


def _ablation(
    track: str,
    audio_root: Path,
    out_dir: Path,
    harness: Path,
    mix_sr: int,
    mix: np.ndarray,
    candidates: list[float],
    classifications: list[str],
    hop: int,
) -> dict[str, object]:
    stem_features: dict[str, np.ndarray] = {}
    stem_provenance: dict[str, object] = {}
    fires: dict[str, list[float]] = {}
    for stem in STEMS:
        stem_path = audio_root / track / f"{stem}.wav"
        stem_sr, stem_samples = read_audio(stem_path, mix_sr)
        if len(stem_samples) < len(mix):
            raise ValueError(f"{track}/{stem}: stem is shorter than mix after resampling")
        stem_samples = stem_samples[: len(mix)]
        stem_features[stem], stem_hop = causal_features(stem_samples, mix_sr)
        if stem_hop != hop:
            raise ValueError(f"{track}/{stem}: envelope hop differs from mix")
        ablated = mix - stem_samples
        wav_path = out_dir / f"{track}__without_{stem}.wav"
        wavfile.write(wav_path, mix_sr, ablated.astype(np.float32))
        run = subprocess.run(
            [str(harness.resolve()), str(wav_path.resolve())],
            capture_output=True, text=True, timeout=120, check=False,
        )
        log_path = out_dir / f"{track}__without_{stem}.log"
        log_path.write_text(run.stdout + run.stderr)
        run.check_returncode()
        rate, parsed_hop, count, _, event_times = parse_harness(run.stdout)
        if rate != mix_sr or parsed_hop != hop or count != len(mix) // hop:
            raise ValueError(f"{track}/{stem}: harness grid differs from mix samples")
        fires[stem] = event_times
        stem_provenance[stem] = {
            "input_sha256": sha(stem_path), "ablation_path": str(wav_path),
            "ablation_sha256": sha(wav_path), "sample_rate": stem_sr,
            "event_times_s": event_times,
        }

    evidence = []
    for candidate, classification in zip(candidates, classifications):
        hop_index = round(candidate / (hop / mix_sr)) - 1
        proportions = {
            stem: float(stem_features[stem][hop_index, :2, 0].sum())
            for stem in STEMS
        }
        total = sum(proportions.values())
        proportions = {stem: (value / total if total else 0.0)
                       for stem, value in proportions.items()}
        removed_by = [stem for stem, event_times in fires.items()
                      if not any(abs(event - candidate) < 0.030 for event in event_times)]
        evidence.append({
            "candidate_s": candidate, "classification": classification,
            "stem_power_proportions_low_plus_body_fast": proportions,
            "ablations_removing_event_within_30ms": removed_by,
        })
    return {
        "qualification": "Local counterfactual evidence; it does not prove instrument causality.",
        "stems": stem_provenance,
        "event_evidence": evidence,
        "scored_extra_summary": [row for row in evidence if row["classification"] == "scored_extra"],
    }


def run(args: argparse.Namespace) -> dict[str, object]:
    base = json.loads(BASE_REPORT.read_text())
    hashes, recorded_tracks = _validate_provenance(
        base, args.audio_root.resolve(), args.harness.resolve()
    )
    review_path = ROOT / "tests/fixtures/audio_labels/attack_review.csv"
    review = _review_rows(review_path)
    args.out_dir.mkdir(parents=True, exist_ok=True)
    results = []
    for recorded in recorded_tracks:
        track = str(recorded["track"])
        audio_path = args.audio_root / track / "mix.wav"
        label_path = ROOT / "tests/fixtures/audio_labels" / f"{track}.csv"
        sr, mix = read_audio(audio_path)
        candidates = list(map(float, recorded["raw_available_times_s"]))
        candidate_hops = list(map(int, recorded["kick_hops"]))
        envelopes, hop = causal_features(mix, sr)
        if len(candidate_hops) != len(candidates):
            raise ValueError(f"{track}: recorded candidate times and hops differ")
        original = score_events(
            candidates, _read_labels(label_path),
            exclusion_regions([r for r in review if r["track"] == track], len(mix) / sr),
        )
        accepted, decisions = ratio_growth_gate(envelopes, candidate_hops, hop, sr)
        updated = score_events(
            accepted, _read_labels(label_path),
            exclusion_regions([r for r in review if r["track"] == track], len(mix) / sr),
        )
        classifications = _classification(
            candidates, _read_labels(label_path),
            [r for r in review if r["track"] == track], len(mix) / sr,
        )
        result: dict[str, object] = {
            "track": track, "sample_rate": sr, "hop_samples": hop,
            "duration_s": len(mix) / sr,
            "input_sha256": {"mix": sha(audio_path), "labels": sha(label_path)},
            "original_score": original,
            "ratio_growth_score": updated,
            "candidate_decisions": [dict(d, classification=classifications[i])
                                     for i, d in enumerate(decisions)],
        }
        if track in {"inhale_exhale_145bpm", "bad_guy_128bpm"}:
            result["ablation"] = _ablation(
                track, args.audio_root, args.out_dir, args.harness, sr, mix,
                candidates, classifications, hop,
            )
        results.append(result)
        strict = updated["accuracy_by_tolerance_ms"]["50"]
        wide = updated["association_early_35_late_200_ms"]
        print(f"{track}: 50ms matched={strict['matched']} extra={strict['extra']} "
              f"wide={wide['matched']}/{wide['unmatched_triggers']}", flush=True)

    report = {
        "date": date.today().isoformat(), "decision": "rejected",
        "base_v5_report": str(BASE_REPORT), "label_limit":
        "The 66 labels are provisional visual estimates, not listened ground truth.",
        "provenance": {
            "base_report_sha256": sha(BASE_REPORT),
            "detector_revision": base.get("detector_revision"), **hashes,
            "method": "Recorded v5 candidates; native-rate causal SciPy Butterworth envelopes; fixed 4x low.fast/body.fast growth within 35ms.",
        },
        "tracks": results,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audio-root", required=True, type=Path)
    parser.add_argument("--harness", required=True, type=Path)
    parser.add_argument("--out-dir", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    run(parser.parse_args())


if __name__ == "__main__":
    main()
