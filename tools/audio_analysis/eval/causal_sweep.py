"""Causal live-trigger parameter sweep — DEV ONLY (2026-07-18).

For every `self_render` fixture with truth, runs the real causal live path
(`causal_events.rs`) across a sensitivity grid and scores each run with the
same metric `eval.causal_compare` uses. Emits a table and a JSON artifact.

No algorithm changes — this is a tuning sweep of the shipped live path.

Usage:
    python -m eval.causal_sweep --out artifact.json
    python -m eval.causal_sweep --out artifact.json --sensitivities 1.0 2.0 4.0
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval import metrics  # noqa: E402
from eval.causal_compare import CLASSES, SELF_RENDER_PITCH_TO_CLASS, cluster_merge, _score_generic  # noqa: E402
from eval.paths import DATA_ROOT  # noqa: E402

DEFAULT_SENSITIVITIES = [1.0, 1.5, 2.0, 3.0, 4.0, 6.0]
EVENT_TOLERANCE_SEC = metrics.EVENT_TOLERANCE_SEC


def _find_self_render_fixtures() -> List[Tuple[str, Path, Path, Dict[str, List[float]]]]:
    base = DATA_ROOT / "self_render"
    out: List[Tuple[str, Path, Path, Dict[str, List[float]]]] = []
    if not base.exists():
        return out
    for wav in sorted(base.glob("*.wav")):
        name = wav.stem
        truth_path = base / f"{name}_truth.json"
        if not truth_path.exists():
            continue
        truth: Dict[str, List[float]] = {c: [] for c in CLASSES}
        for n in json.loads(truth_path.read_text()):
            cls = SELF_RENDER_PITCH_TO_CLASS.get(n["pitch"])
            if cls:
                truth[cls].append(float(n["start_sec"]))
        for c in truth:
            truth[c].sort()
        out.append((name, wav, truth_path, truth))
    return out


def _run_causal_events(
    wav: Path,
    sensitivity: float,
    attack_ms: float,
    release_ms: float,
    events_out: Path,
    example_bin: Path,
) -> None:
    cmd = [
        str(example_bin),
        str(wav),
        "--out", str(events_out),
        "--sensitivity", str(sensitivity),
        "--attack-ms", str(attack_ms),
        "--release-ms", str(release_ms),
    ]
    subprocess.run(cmd, check=True, capture_output=True, text=True)


def _score_events(events_path: Path, truth: Dict[str, List[float]], fixture_id: str) -> Dict[str, Any]:
    raw_events = json.loads(events_path.read_text())
    causal_union = sorted(float(e["time_sec"]) for e in raw_events)
    truth_union = sorted(t for times in truth.values() for t in times)
    causal_hits = cluster_merge(causal_union)
    score = _score_generic(causal_hits, truth_union, {
        "id": fixture_id,
        "truth_type": "dense",
        "truth": truth,
        "bpm": None,
    })
    return score


def _sweep_fixture(
    name: str,
    wav: Path,
    truth: Dict[str, List[float]],
    sensitivities: Sequence[float],
    example_bin: Path,
    attack_ms: float,
    release_ms: float,
) -> List[Dict[str, Any]]:
    rows: List[Dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix=f"causal_sweep_{name}_") as tmp:
        tmp_path = Path(tmp)
        for s in sensitivities:
            events_path = tmp_path / f"{name}_s{s:.3f}.json"
            _run_causal_events(wav, s, attack_ms, release_ms, events_path, example_bin)
            score = _score_events(events_path, truth, f"self_render_{name}")
            rows.append({
                "sensitivity": s,
                "attack_ms": attack_ms,
                "release_ms": release_ms,
                "score": score,
            })
    return rows


def _auto_extend_sensitivities(base: List[float], rows: List[Dict[str, Any]]) -> List[float]:
    """If recall is still climbing at the top of the grid, extend geometrically."""
    if len(base) < 2 or len(rows) < 2:
        return base
    # rows are in the same order as base
    if rows[-1]["score"]["recall"] <= rows[-2]["score"]["recall"]:
        return base
    extended = list(base)
    while True:
        next_s = round(extended[-1] * 1.5, 6)
        if next_s > 24.0:
            break
        extended.append(next_s)
        # We need the score for this new sensitivity to decide whether to keep extending.
        # The caller will re-run the sweep with the extended list, so just stop here.
        break
    return extended


def main(argv: Optional[List[str]] = None) -> int:  # noqa: F821
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, required=True, help="write the JSON artifact here")
    parser.add_argument("--sensitivities", type=float, nargs="+", default=None, help="sensitivity grid")
    parser.add_argument("--attack-ms", type=float, default=5.0, help="trigger attack time constant (ms)")
    parser.add_argument("--release-ms", type=float, default=120.0, help="trigger release time constant (ms)")
    parser.add_argument(
        "--example-bin",
        type=Path,
        default=None,
        help="path to the built causal_events example binary (default: target/debug/examples/causal_events in worktree)",
    )
    args = parser.parse_args(argv)

    sensitivities: List[float] = list(args.sensitivities or DEFAULT_SENSITIVITIES)
    repo_root = Path(__file__).resolve().parents[3]
    example_bin = args.example_bin
    if example_bin is None:
        # Running from worktree -> use that worktree's target binary.
        example_bin = repo_root / "target" / "debug" / "examples" / "causal_events"
    if not example_bin.exists():
        raise SystemExit(f"causal_events binary not found: {example_bin} (build with cargo build -p manifold-audio --example causal_events)")

    fixtures = _find_self_render_fixtures()
    if not fixtures:
        raise SystemExit(f"no self_render fixtures with truth found under {DATA_ROOT / 'self_render'}")

    # First pass with base grid; auto-extend if recall still climbing.
    print(f"sweeping {len(fixtures)} fixture(s) @ attack={args.attack_ms}ms release={args.release_ms}ms")
    print(f"base sensitivities: {sensitivities}")
    print()

    all_results: Dict[str, List[Dict[str, Any]]] = {}
    extended_for: Dict[str, List[float]] = {}
    for name, wav, _truth_path, truth in fixtures:
        rows = _sweep_fixture(name, wav, truth, sensitivities, example_bin, args.attack_ms, args.release_ms)
        extended = _auto_extend_sensitivities(sensitivities, rows)
        if len(extended) > len(sensitivities):
            print(f"[{name}] recall still climbing at s={sensitivities[-1]}; extending to {extended}")
            rows = _sweep_fixture(name, wav, truth, extended, example_bin, args.attack_ms, args.release_ms)
            extended_for[name] = extended
        all_results[name] = rows

    # Print table.
    header = f"{'fixture':<28}{'sens':>6}{'F1':>7}{'P':>7}{'R':>7}{'pred':>6}{'truth':>7}{'|dt|ms':>9}"
    print(header)
    print("-" * len(header))
    for name, rows in sorted(all_results.items()):
        for row in rows:
            s = row["score"]
            t = f"{s['median_abs_timing_ms']:.1f}" if s["median_abs_timing_ms"] is not None else "n/a"
            print(
                f"{name:<28}{row['sensitivity']:>6.2f}{s['f1']:>7.3f}{s['precision']:>7.3f}"
                f"{s['recall']:>7.3f}{s['n_pred']:>6}{s['n_truth']:>7}{t:>9}"
            )
        print()

    # Summary: best sensitivity per fixture (by F1, tie-break recall).
    print("best by F1 per fixture:")
    print(f"{'fixture':<28}{'sens':>6}{'F1':>7}{'P':>7}{'R':>7}{'|dt|ms':>9}")
    for name, rows in sorted(all_results.items()):
        best = max(rows, key=lambda r: (r["score"]["f1"], r["score"]["recall"]))
        s = best["score"]
        t = f"{s['median_abs_timing_ms']:.1f}" if s["median_abs_timing_ms"] is not None else "n/a"
        print(f"{name:<28}{best['sensitivity']:>6.2f}{s['f1']:>7.3f}{s['precision']:>7.3f}{s['recall']:>7.3f}{t:>9}")
    print()

    # Write JSON artifact.
    artifact = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "example_bin": str(example_bin),
        "attack_ms": args.attack_ms,
        "release_ms": args.release_ms,
        "tolerance_sec": EVENT_TOLERANCE_SEC,
        "default_sensitivities": DEFAULT_SENSITIVITIES,
        "extended_for": extended_for,
        "fixtures": {
            name: {
                "wav": str(wav),
                "truth_path": str(tp),
                "rows": rows,
            }
            for (name, wav, tp, _truth), rows in zip(fixtures, all_results.values())
        },
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(artifact, indent=2))
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
