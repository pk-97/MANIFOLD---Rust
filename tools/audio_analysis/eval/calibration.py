"""BUG-235 fix — per-fixture kick onset-convention calibration (P4 §0,
docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md P4 entry: "Read-back: §4.2, D6, D9,
D11"). This is a PREREQUISITE measurement/calibration step, NOT tuning: it
extends D14's mechanism ("measure a per-stage/per-detector systematic bias,
apply the correction once at a defined seam") from decode/format skew to a
per-fixture ONSET-CONVENTION mismatch between raw ADTOF kick output and the
5 manifold_own hand-labeled kick fixtures.

WHY a per-fixture (not global) constant, and why it exists at all
------------------------------------------------------------------
BUG-235 (docs/BUG_BACKLOG.md) found that ADTOF's raw kick onsets are
systematically EARLY relative to these 5 fixtures' hand-labeled truth, by an
amount that is tight and roughly track-dependent (-20ms .. -125ms), NOT
scatter/noise. Root cause (confirmed, not a scorer bug — see BUG-235's own
diagnostic): the two onset conventions disagree by construction.
`tests/fixtures/audio_labels/README.md` defines truth as "onset = walk-back
to 25% of the sub-envelope peak" — a late, energy-anchored convention.
ADTOF's own model+peak-picker convention sits earlier (closer to the
sub-transient's actual physical attack). Babyslakh's aligned-MIDI truth (the
exact synthesis note-onset) apparently sits closer to ADTOF's convention
than this pack's hand-crafted walk-back rule does, which is the leading
(unconfirmed) explanation for why babyslakh scores so much higher on the
same detector (design doc P3 landing report). The fix is a per-fixture
constant-offset correction, applied ONLY at the scoring seam (this module),
NEVER inside the detector (`manifold_audio.adtof_detection` is untouched by
this file) — the same seam discipline D14 already established for
decode/format skew.

Measurement method
-------------------
For each of the 5 manifold_own kick_onset_csv fixtures: run raw ADTOF kick
detection (manifold_audio.adtof_detection.detect_drums_adtof, DEFAULT
thresholds — the exact function/config the P3 baseline used) against
mix.wav; nearest-match each predicted kick to the nearest truth kick (the
`mix_time_s` column — already the scoring column the P3 baseline and
eval/run.py use) within a WIDE window (MATCH_WINDOW_SEC, wider than the
scoring tolerance so the true bias, whatever its magnitude, is captured
rather than clipped); the median signed difference (pred - truth) over
matched pairs is that fixture's calibration offset.

Applying it
------------
`apply_calibration(times, offset_sec)` returns `[t - offset_sec for t in
times]` — subtracting a negative offset (early bias) shifts predictions
LATER, toward truth. This is called by scoring code only (this module's own
`score_kick_onset_fixture_calibrated`, and P4's sweep driver for the 5
manifold_own fixtures) — never by the detector.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval import metrics  # noqa: E402
from eval.run import AUDIO_ANALYSIS_ROOT, _load_kick_truth_csv, _resolve_path, load_fixtures  # noqa: E402

MATCH_WINDOW_SEC = 0.3  # wide vs. D10's 50ms scoring tolerance — must not clip the true bias (measured up to 125ms)
DEFAULT_CALIBRATION_PATH = AUDIO_ANALYSIS_ROOT / "eval" / "calibration" / "kick_onset_calibration.json"

MANIFOLD_OWN_KICK_FIXTURE_IDS = [
    "apricots_128bpm",
    "bad_guy_128bpm",
    "feel_the_vibration_174bpm",
    "inhale_exhale_145bpm",
    "tears_140bpm",
]


def _nearest_signed_diffs(pred: List[float], truth: List[float], window_sec: float) -> List[float]:
    """For each pred time, the signed diff to its nearest truth time if
    within window_sec (else dropped — an unmatched prediction tells us
    nothing about the systematic bias, it's a separate P/R concern)."""
    diffs: List[float] = []
    if not truth:
        return diffs
    truth_arr = np.asarray(sorted(truth), dtype=np.float64)
    for p in pred:
        idx = int(np.argmin(np.abs(truth_arr - p)))
        d = p - float(truth_arr[idx])
        if abs(d) <= window_sec:
            diffs.append(d)
    return diffs


def measure_kick_calibration_for_fixture(fixture: Dict[str, Any], window_sec: float = MATCH_WINDOW_SEC) -> Dict[str, Any]:
    """Raw ADTOF kick detection (default thresholds, same as the P3 baseline)
    vs. the fixture's mix_time_s truth column. Returns the measured median
    signed offset (seconds) plus the raw counts backing it."""
    from manifold_audio.adtof_detection import detect_drums_adtof

    base_dir = _resolve_path(fixture["path"])
    mix_path = base_dir / "mix.wav"
    truth = _load_kick_truth_csv(_resolve_path(fixture["labels_path"]))["mix"]

    events = detect_drums_adtof(str(mix_path))
    pred_kicks = [e.time for e in events if e.type == "kick"]

    diffs = _nearest_signed_diffs(pred_kicks, truth, window_sec)
    median_offset = float(np.median(diffs)) if diffs else 0.0
    return {
        "id": fixture["id"],
        "n_pred": len(pred_kicks),
        "n_truth": len(truth),
        "n_matched_for_calibration": len(diffs),
        "median_offset_sec": median_offset,
        "min_offset_sec": float(np.min(diffs)) if diffs else None,
        "max_offset_sec": float(np.max(diffs)) if diffs else None,
        "match_window_sec": window_sec,
    }


def measure_all(fixtures_path: Path = AUDIO_ANALYSIS_ROOT / "eval" / "fixtures.toml") -> List[Dict[str, Any]]:
    fixtures = {f["id"]: f for f in load_fixtures(fixtures_path)}
    rows = []
    for fid in MANIFOLD_OWN_KICK_FIXTURE_IDS:
        fixture = fixtures[fid]
        print(f"[calibration] measuring {fid} ...", file=sys.stderr)
        rows.append(measure_kick_calibration_for_fixture(fixture))
    return rows


def write_calibration_file(rows: List[Dict[str, Any]], out_path: Path = DEFAULT_CALIBRATION_PATH) -> Path:
    payload = {
        "_comment": (
            "BUG-235 fix. Per-fixture median signed offset (ADTOF raw kick time "
            "minus nearest manifold_own truth time, mix_time_s column) between "
            "raw ADTOF's onset-picking convention and this fixture pack's "
            "hand-labeled convention (tests/fixtures/audio_labels/README.md: "
            "'onset = walk-back to 25% of the sub-envelope peak' -- a later, "
            "energy-anchored convention than ADTOF's own, earlier, "
            "transient-anchored one). Applied ONLY at the scoring seam "
            "(eval/calibration.py:apply_calibration) -- never inside the "
            "detector (manifold_audio.adtof_detection is untouched). See "
            "docs/BUG_BACKLOG.md BUG-235 and docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md D14."
        ),
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "method": (
            "median(pred_time - nearest_truth_time) over raw ADTOF kicks "
            "(default thresholds) vs mix_time_s truth, matched within "
            f"±{MATCH_WINDOW_SEC}s (wider than D10's 50ms scoring tolerance "
            "so the true bias isn't clipped by the match window itself)."
        ),
        "match_window_sec": MATCH_WINDOW_SEC,
        "offsets_sec": {r["id"]: r["median_offset_sec"] for r in rows},
        "measurement_detail": rows,
    }
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(payload, indent=2))
    return out_path


def load_calibration(path: Path = DEFAULT_CALIBRATION_PATH) -> Dict[str, float]:
    data = json.loads(path.read_text())
    return {k: float(v) for k, v in data["offsets_sec"].items()}


def apply_calibration(times: List[float], offset_sec: float) -> List[float]:
    """Shifts predicted times by the measured bias so they land on truth's
    convention: corrected = pred - offset (offset is pred - truth, so this
    is pred - (pred - truth) ~= truth when the bias is a clean constant)."""
    return [t - offset_sec for t in times]


def score_kick_onset_fixture_calibrated(fixture: Dict[str, Any], calibration: Dict[str, float]) -> Dict[str, Any]:
    """Raw ADTOF kick P/R/F1 against mix_time_s truth, WITH the fixture's
    calibration offset applied to predictions before scoring. Companion to
    measure_kick_calibration_for_fixture (same detector call, same truth
    column) — the paired uncalibrated/calibrated comparison this module's
    CLI writes to p4_calibrated_baseline.json."""
    from manifold_audio.adtof_detection import detect_drums_adtof

    base_dir = _resolve_path(fixture["path"])
    mix_path = base_dir / "mix.wav"
    truth = _load_kick_truth_csv(_resolve_path(fixture["labels_path"]))["mix"]
    tol = metrics.EVENT_TOLERANCE_SEC

    events = detect_drums_adtof(str(mix_path))
    pred_kicks = [e.time for e in events if e.type == "kick"]

    uncalibrated_prf = metrics.event_prf(pred_kicks, truth, tolerance_sec=tol)
    offset = calibration.get(fixture["id"], 0.0)
    corrected = apply_calibration(pred_kicks, offset)
    calibrated_prf = metrics.event_prf(corrected, truth, tolerance_sec=tol)

    return {
        "id": fixture["id"],
        "n_pred": len(pred_kicks),
        "n_truth": len(truth),
        "calibration_offset_sec": offset,
        "uncalibrated": uncalibrated_prf.to_dict(),
        "calibrated": calibrated_prf.to_dict(),
    }


def build_calibrated_baseline_report(
    fixtures_path: Path = AUDIO_ANALYSIS_ROOT / "eval" / "fixtures.toml",
    calibration_path: Path = DEFAULT_CALIBRATION_PATH,
) -> Dict[str, Any]:
    fixtures = {f["id"]: f for f in load_fixtures(fixtures_path)}
    calibration = load_calibration(calibration_path)
    rows = []
    for fid in MANIFOLD_OWN_KICK_FIXTURE_IDS:
        print(f"[calibration] scoring {fid} (calibrated vs uncalibrated) ...", file=sys.stderr)
        rows.append(score_kick_onset_fixture_calibrated(fixtures[fid], calibration))

    uncal_f1 = [r["uncalibrated"]["f1"] for r in rows]
    cal_f1 = [r["calibrated"]["f1"] for r in rows]
    return {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "phase": "P4 §0 — BUG-235 calibration (prerequisite, not tuning)",
        "note": (
            "Scope: this recalibrates the RAW ADTOF baseline path "
            "(eval/full_pack_baseline.py::run_baseline_adtof_drums), which is "
            "what BUG-235 diagnosed. It does NOT touch the separate "
            "onset_compensation_seconds knob in the full analyze_percussion "
            "pipeline (percussion_settings.rs / D14's Rust-side mechanism) -- "
            "that is a different seam for a different (decode/format) offset."
        ),
        "rows": rows,
        "mean_uncalibrated_f1": float(np.mean(uncal_f1)) if uncal_f1 else None,
        "mean_calibrated_f1": float(np.mean(cal_f1)) if cal_f1 else None,
    }


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_measure = sub.add_parser("measure", help="measure per-fixture offsets and write the calibration file")
    p_measure.add_argument("--fixtures", type=Path, default=AUDIO_ANALYSIS_ROOT / "eval" / "fixtures.toml")
    p_measure.add_argument("--out", type=Path, default=DEFAULT_CALIBRATION_PATH)

    p_baseline = sub.add_parser("baseline", help="calibrated-vs-uncalibrated five-fixture ADTOF baseline")
    p_baseline.add_argument("--fixtures", type=Path, default=AUDIO_ANALYSIS_ROOT / "eval" / "fixtures.toml")
    p_baseline.add_argument("--calibration", type=Path, default=DEFAULT_CALIBRATION_PATH)
    p_baseline.add_argument("--report", type=Path, default=AUDIO_ANALYSIS_ROOT / "eval" / "scoreboard" / "p4_calibrated_baseline.json")

    args = parser.parse_args(argv)

    if args.cmd == "measure":
        rows = measure_all(args.fixtures)
        out_path = write_calibration_file(rows, args.out)
        print(f"[calibration] wrote {out_path}")
        for r in rows:
            print(f"  {r['id']}: median_offset={r['median_offset_sec']*1000:.1f}ms (n_matched={r['n_matched_for_calibration']}/{r['n_pred']})")
        return 0

    if args.cmd == "baseline":
        report = build_calibrated_baseline_report(args.fixtures, args.calibration)
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2))
        print(f"[calibration] wrote {args.report}")
        print(f"  mean uncalibrated F1: {report['mean_uncalibrated_f1']:.4f}")
        print(f"  mean calibrated F1:   {report['mean_calibrated_f1']:.4f}")
        return 0

    return 1


if __name__ == "__main__":
    raise SystemExit(main())
