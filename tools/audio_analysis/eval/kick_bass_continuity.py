"""Measure a fixed causal AR veto for the frozen sustained-bass fires.

This is a CPU-only development measurement.  The predictor is fit on the 80 ms
immediately before a 20 ms evaluation window, using only past samples.  A fire
is vetoed when residual energy is at most 10 percent of observed energy.
"""
from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
import time

import numpy as np
from scipy.signal import butter, sosfilt

from .kick_attack_rejection import causal_features, read_audio
from .kick_sustain_audit import validate_confirmed_fires
from .kick_dsp_experiments import detect_v5
from .live_kick_baseline import sha
from .run_kick_dsp_experiments import aggregate, development_sources, evaluate, ROOT

ORDER = 8
TRAIN_S = 0.080
EVAL_S = 0.020
LOW_HZ = 45.0
HIGH_HZ = 400.0
VETO_SCORE = 0.10
REFERENCE_VARIANT = "confirmed_persistence"


def bandpass_subsample(samples: np.ndarray, sample_rate: int) -> tuple[np.ndarray, float, int]:
    """Causally bandpass at the native rate, then retain every fixed-rate sample."""
    if sample_rate <= 2 * HIGH_HZ:
        raise ValueError(f"sample rate {sample_rate} Hz is too low for the bandpass")
    step = max(1, round(sample_rate / 2000.0))
    filtered = sosfilt(
        butter(4, [LOW_HZ, HIGH_HZ], btype="bandpass", fs=sample_rate, output="sos"),
        np.asarray(samples, dtype=np.float64),
    )
    return filtered[::step], sample_rate / step, step


def _ridge_ar(train: np.ndarray, order: int) -> np.ndarray:
    """Fit a zero-intercept AR model, with the fixed relative ridge penalty."""
    if len(train) <= order:
        raise ValueError("insufficient training history")
    x = np.stack([train[i - order:i][::-1] for i in range(order, len(train))])
    y = train[order:]
    xtx = x.T @ x
    ridge = 1e-3 * float(np.trace(xtx) / order)
    return np.linalg.solve(xtx + ridge * np.eye(order), x.T @ y)


def score_window(
    signal: np.ndarray,
    end_index: int,
    actual_sample_rate: float,
    order: int = ORDER,
) -> dict[str, object]:
    """Score one event, where ``end_index`` is the exclusive eval endpoint."""
    train_count = round(TRAIN_S * actual_sample_rate)
    eval_count = round(EVAL_S * actual_sample_rate)
    eval_start = end_index - eval_count
    train_start = eval_start - train_count
    result: dict[str, object] = {
        "status": "unassessed_insufficient_history",
        "decision": "keep",
        "score": None,
        "train_samples": train_count,
        "evaluation_samples": eval_count,
    }
    if train_start < 0 or eval_start < order or end_index > len(signal) or eval_count <= 0:
        return result
    train = np.asarray(signal[train_start:eval_start], dtype=np.float64)
    observed = np.asarray(signal[eval_start:end_index], dtype=np.float64)
    observed_energy = float(observed @ observed)
    if not math.isfinite(observed_energy):
        raise ValueError('nonfinite evaluation energy')
    if observed_energy <= 1e-20:
        result["status"] = "unassessed_silence"
        return result
    train_energy = float(train @ train)
    if not math.isfinite(train_energy):
        raise ValueError('nonfinite training energy')
    if train_energy <= 1e-20:
        result["status"] = "unassessed_silence"
        return result
    coefficients = _ridge_ar(train, order)
    predictions = np.array(
        [signal[i - order:i][::-1] @ coefficients for i in range(eval_start, end_index)],
        dtype=np.float64,
    )
    residual_energy = float(np.sum((observed - predictions) ** 2))
    score = residual_energy / observed_energy
    result.update(
        status="veto" if score <= VETO_SCORE else "keep",
        decision="veto" if score <= VETO_SCORE else "keep",
        score=float(score),
        observed_energy=observed_energy,
        residual_energy=residual_energy,
    )
    return result


def score_event(
    signal: np.ndarray, actual_sample_rate: float, fire_time_s: float
) -> dict[str, object]:
    """Map a completed fire time to the subsampled, exclusive eval endpoint."""
    end_index = int(math.ceil(fire_time_s * actual_sample_rate - 1e-12))
    result = score_window(signal, end_index, actual_sample_rate)
    result.update(fire_time_s=float(fire_time_s), evaluation_end_index=end_index)
    return result


def _validate_reference(reference: dict[str, object], path: Path) -> list[dict[str, object]]:
    variants = reference.get("variants")
    if not isinstance(variants, dict) or REFERENCE_VARIANT not in variants:
        raise ValueError(f"{path}: missing frozen {REFERENCE_VARIANT} variant")
    tracks = variants[REFERENCE_VARIANT].get("tracks")
    if not isinstance(tracks, list) or not tracks:
        raise ValueError(f"{path}: frozen reference has no tracks")
    for name, expected in reference.get("sources_sha256", {}).items():
        source = ROOT / "tools/audio_analysis/eval" / name
        if not source.exists() or sha(source) != expected:
            raise ValueError(f"reference source hash differs for {name}")
    labels = ROOT / "tests/fixtures/audio_labels"
    for name, expected in reference.get("labels_sha256", {}).items():
        label = labels / name
        if not label.exists() or sha(label) != expected:
            raise ValueError(f"reference label hash differs for {name}")
    return tracks


def run(audio_root: Path, reference_path: Path, out: Path) -> dict[str, object]:
    reference = json.loads(reference_path.read_text())
    reference_tracks = _validate_reference(reference, reference_path)
    frozen = {str(row["track"]): row for row in reference_tracks}
    tracks: list[dict[str, object]] = []
    cpu_start = time.process_time()
    for source in development_sources(audio_root):
        track_cpu_start = time.process_time()
        track_name = source["track"]
        if track_name not in frozen:
            raise ValueError(f"{track_name}: absent from frozen improved reference")
        path = Path(source["audio_path"])
        if sha(path) != source["audio_sha256"]:
            raise ValueError(f"{track_name}: audio hash differs from development source")
        frozen_row = frozen[track_name]
        if frozen_row.get("audio_sha256") != source["audio_sha256"]:
            raise ValueError(f"{track_name}: audio hash differs from frozen reference")
        sr, samples = read_audio(path)
        env, hop = causal_features(samples, sr)
        original = detect_v5(env, sr, hop)
        if original != source["native_hops"]:
            raise ValueError(f"{track_name}: native v5 replay mismatch")
        improved, decisions = validate_confirmed_fires(env, original, sr, hop)
        if improved != frozen_row.get("kick_hops"):
            raise ValueError(f"{track_name}: improved reference replay mismatch")
        filtered, actual_sr, step = bandpass_subsample(samples, sr)
        event_scores = [
            score_event(filtered, actual_sr, (hop_index + 1) * hop / sr)
            for hop_index in improved
        ]
        kept = [i for i, row in zip(improved, event_scores) if row['decision'] == 'keep']
        times = [(i + 1) * hop / sr for i in kept]
        row: dict[str, object] = dict(
            track=track_name,
            group=source["group"],
            audio_path=str(path),
            audio_sha256=source["audio_sha256"],
            sample_rate=sr,
            duration_s=len(samples) / sr,
            hop=hop,
            native_baseline_replay_exact=True,
            improved_reference_replay_exact=True,
            original_hops=original,
            improved_reference_hops=improved,
            kick_hops=kept,
            decisions=decisions,
            filter=dict(low_hz=LOW_HZ, high_hz=HIGH_HZ, order=4, causal=True),
            subsample_step=step,
            resulting_sample_rate_hz=actual_sr,
            cpu_seconds=None,
            events=event_scores,
            scores=evaluate(source, times),
        )
        row["cpu_seconds"] = float(time.process_time() - track_cpu_start)
        tracks.append(row)
        print(track_name, len(event_scores), "events", flush=True)
    cpu_seconds = time.process_time() - cpu_start
    report: dict[str, object] = dict(
        method=__doc__.strip(),
        settings=dict(
            predictor="zero-intercept order-8 linear autoregression",
            train_window_s=TRAIN_S,
            evaluation_window_s=EVAL_S,
            ridge_relative_to_mean_xtx_diagonal=1e-3,
            veto_score_max=VETO_SCORE,
            insufficient_history="unassessed/keep",
            silence="unassessed/keep",
        ),
        reference=dict(
            path=str(reference_path),
            sha256=sha(reference_path),
            variant=REFERENCE_VARIANT,
            exact_replay=True,
        ),
        source_provenance=[
            dict(track=row["track"], audio_path=row["audio_path"], audio_sha256=row["audio_sha256"])
            for row in tracks
        ],
        cpu_seconds=float(cpu_seconds),
        python_cpu_seconds_per_audio_second=float(
            cpu_seconds / sum(row["duration_s"] for row in tracks)
        ),
        totals=aggregate(tracks),
        tracks=tracks,
        sources_sha256={name: sha(ROOT / 'tools/audio_analysis/eval' / name)
            for name in ('kick_bass_continuity.py', 'kick_attack_rejection.py',
                         'kick_sustain_audit.py', 'kick_dsp_experiments.py',
                         'run_kick_dsp_experiments.py', 'live_kick_baseline.py',
                         'master_kick_comparison.py')},
        labels_sha256=reference['labels_sha256'],
        limitations=[
            "Development recordings only; no held-out evaluation.",
            "Python CPU time is an offline measurement, not a realtime suitability claim.",
            "The filter and predictor operate on the full mix and do not separate stems.",
        ],
    )
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + "\n")
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audio-root", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.reference, args.out)


if __name__ == "__main__":
    main()
