"""Run the unchanged Rust mod_harness on the five corrected full-mix fixtures.

Requires a freshly built --harness. No model, beat grid, calibration, deduplication,
or per-song tuning. Report audio-sample availability, not wall-clock/app latency.
"""
from __future__ import annotations

import argparse
import csv
from datetime import datetime
import hashlib
import json
import math
from pathlib import Path
import re
import statistics
import subprocess
import wave
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parents[3]


def match_events(pred, truth, early, late):
    """Ordered one-to-one matching: maximum count, then minimum absolute error."""
    n, m = len(pred), len(truth)
    table = [[(0, 0.0, ()) for _ in range(m + 1)] for _ in range(n + 1)]
    for i in range(n - 1, -1, -1):
        for j in range(m - 1, -1, -1):
            choices = [table[i + 1][j], table[i][j + 1]]
            error = pred[i] - truth[j]
            if -early - 1e-9 <= error <= late + 1e-9:
                count, cost, pairs = table[i + 1][j + 1]
                choices.append((count + 1, cost + abs(error), ((j, i),) + pairs))
            table[i][j] = min(choices, key=lambda x: (-x[0], x[1]))
    return table[0][0][2]


def exclusion_regions(review, duration):
    regions = []
    for row in review:
        if row["review_status"] == "clip_boundary":
            regions.append({"reason": "clip_boundary", "start_s": 0.0,
                            "end_s": min(0.250, duration)})
        elif row["review_status"] == "needs_listening":
            regions.append({"reason": "unresolved_ending", "start_s":
                            float(row["estimated_attack_s"]) - 0.100,
                            "end_s": duration})
    return regions


def delay_stats(delays):
    return {"median_ms": round(statistics.median(delays), 2) if delays else None,
            "p90_ms": round(sorted(delays)[math.ceil(.9 * len(delays)) - 1], 2)
            if delays else None}


def score_events(pred, truth, regions):
    ignored = [dict(r, trigger_times_s=[p for p in pred
                                       if r["start_s"] <= p <= r["end_s"]])
               for r in regions]
    pred = sorted(p for p in pred if not any(r["start_s"] <= p <= r["end_s"]
                                            for r in regions))
    truth = sorted(truth)
    if any(r["start_s"] <= t <= r["end_s"] for t in truth for r in regions):
        raise ValueError("accepted label overlaps excluded region")
    scores = {}
    for ms in (35, 50, 70):
        pairs = match_events(pred, truth, ms / 1000, ms / 1000)
        nt, np = {t for t, _ in pairs}, {p for _, p in pairs}
        tp = len(pairs)
        scores[str(ms)] = {"matched": tp, "missed": len(truth) - tp,
                           "extra": len(pred) - tp,
                           "missed_times_s": [t for i, t in enumerate(truth) if i not in nt],
                           "extra_times_s": [p for i, p in enumerate(pred) if i not in np]}
    # A separate diagnostic, not relaxed headline accuracy. Time coincidence
    # cannot prove the associated trigger was caused by the kick rather than bass.
    pairs = match_events(pred, truth, .035, .200)
    matched_p = {p for _, p in pairs}
    associations = [{"attack_s": truth[t], "available_s": pred[p],
                     "delay_ms": round(1000 * (pred[p] - truth[t]), 3)}
                    for t, p in pairs]
    extras = [p for i, p in enumerate(pred) if i not in matched_p]
    possible_duplicates = [p for p in extras if any(-.035 <= p - truth[t] <= .200
                                                   for t, _ in pairs)]
    return {"labels": len(truth), "scored_triggers": len(pred), "excluded": ignored,
            "accuracy_by_tolerance_ms": scores,
            "association_early_35_late_200_ms": {
                "matched": len(pairs), "unmatched_labels": len(truth) - len(pairs),
                "unmatched_triggers": len(extras), "pairs": associations,
                "possible_duplicate_times_s": possible_duplicates,
                **delay_stats([p["delay_ms"] for p in associations])}}


def parse_harness(output):
    grid = re.search(r"@ (\d+) Hz, (\d+) hops of (\d+) samples", output)
    fires = re.search(r"kick_hops=(\[[^\n]*\])", output)
    if grid is None or fires is None:
        raise ValueError("missing sample grid or kick_hops in mod_harness output")
    sr, count, hop = map(int, grid.groups())
    indices = json.loads(fires[1])
    if not (sr > 0 and hop > 0 and indices == sorted(set(indices))
            and all(isinstance(i, int) and 0 <= i < count for i in indices)):
        raise ValueError("invalid harness grid or kick indices")
    # mod_harness records one output per completed push of exactly one hop.
    # Record zero becomes available after hop samples, not at sample zero.
    return sr, hop, count, indices, [(i + 1) * hop / sr for i in indices]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--harness", required=True, type=Path)
    ap.add_argument("--audio-root", required=True, type=Path)
    ap.add_argument("--out-dir", required=True, type=Path)
    ap.add_argument("--report", required=True, type=Path)
    args = ap.parse_args()
    args.out_dir.mkdir(parents=True, exist_ok=True)
    label_root = ROOT / "tests/fixtures/audio_labels"
    review_path = label_root / "attack_review.csv"
    review = list(csv.DictReader(review_path.open()))
    tracks = sorted({r["track"] for r in review})
    results = []
    for track in tracks:
        audio = args.audio_root / track / "mix.wav"
        labels = label_root / f"{track}.csv"
        truth = [float(r["mix_time_s"]) for r in csv.DictReader(labels.open())]
        tr = [r for r in review if r["track"] == track]
        expected = [float(r["estimated_attack_s"]) for r in tr
                    if r["review_status"] == "visual_estimate"]
        if truth != expected:
            raise ValueError(f"{track}: labels differ from accepted visual estimates")
        with wave.open(str(audio)) as wav:
            sr, frames = wav.getframerate(), wav.getnframes()
        cmd = [str(args.harness.resolve()), str(audio.resolve()), "--out",
               str((args.out_dir / f"{track}.png").resolve())]
        print(f"Running {track} full mix", flush=True)
        run = subprocess.run(cmd, capture_output=True, text=True, timeout=120, check=False)
        (args.out_dir / f"{track}.log").write_text(run.stdout + run.stderr)
        run.check_returncode()
        rate, hop, count, indices, pred = parse_harness(run.stdout)
        if sr != rate or count != frames // hop:
            raise ValueError(f"{track}: harness grid differs from WAV samples")
        result = dict(track=track, sample_rate=sr, hop_samples=hop, duration_s=frames / sr,
                      audio_sha256=sha(audio), labels_sha256=sha(labels),
                      kick_hops=indices, raw_available_times_s=pred,
                      **score_events(pred, truth, exclusion_regions(tr, frames / sr)))
        results.append(result)
        tight = result["accuracy_by_tolerance_ms"]["50"]
        print(f"{track}: 50ms matched={tight['matched']}/{len(truth)} "
              f"extra={tight['extra']}", flush=True)
    totals = {str(ms): {k: sum(r["accuracy_by_tolerance_ms"][str(ms)][k] for r in results)
                       for k in ("matched", "missed", "extra")} for ms in (35, 50, 70)}
    all_pairs = [p for r in results for p in r["association_early_35_late_200_ms"]["pairs"]]
    report = {
        "date": datetime.now(ZoneInfo("Australia/Sydney")).date().isoformat(),
        "detector_revision": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "harness_sha256": sha(args.harness), "review_sha256": sha(review_path),
        "method": {
            "detector": "Unchanged StreamingSendAnalyzer via mod_harness; Low Kick > 0.999",
            "input": "Native-rate stereo mean; defaults; continuous full file; no tail padding",
            "time": "(zero-based kick_hop + 1) * hop_samples / sample_rate; no calibration",
            "primary_tolerance_ms": 50, "matching": "one-to-one maximum count then minimum error",
            "exclusions": "clip start [0,0.250]; unresolved ending [estimate-0.100,EOF]",
            "limit": "Visual labels only. Audio-clock detector availability, not end-to-end latency. "
                     "Late associations and possible duplicates are temporal diagnoses, not causal proof.",
        },
        "labels": sum(r["labels"] for r in results), "totals": totals,
        "association_delays": delay_stats([p["delay_ms"] for p in all_pairs]),
        "tracks": results,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"totals": totals, "association_delays": report["association_delays"]}, indent=2))


if __name__ == "__main__":
    main()
