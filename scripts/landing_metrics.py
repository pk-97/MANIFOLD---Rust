#!/usr/bin/env python3
"""Landing-loop metrics for the last N days: the harness measured like product.

Reads .claude/orchestration/landing-gate-timings.jsonl (one row per gate run)
and main's first-parent merges (one per landing). Prints one block: gate runs
per landing, red share, GPU proof reuse, queue wait, and the slowest legs/tests.
Exits 1 when runs per landing pass RUNS_PER_LANDING_RED so trunk_health files
a bead; everything else is reported, not gated. Fails open on tooling errors.

Why: between 2026-09-20 and 2026-10-06 the log held 469 runs for 233 branches
and nobody read it; serial red discovery cost 3-10 runs per water landing.

Obsolete when: landings are queued one at a time and gated once each.
"""
import argparse
import collections
import json
import statistics
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# The week before the rerun rule landed measured 2.23 (2026-09-30..10-07);
# one gate run to find reds plus one to land is the shape the rule buys.
RUNS_PER_LANDING_RED = 2.0


def timings_path(repo):
    """The gate appends to the main checkout's log whichever worktree ran it."""
    out = subprocess.run(["git", "-C", str(repo), "rev-parse", "--git-common-dir"],
                         capture_output=True, text=True, timeout=30)
    if out.returncode:
        raise RuntimeError(out.stderr.strip())
    main = (Path(repo) / out.stdout.strip()).resolve().parent
    return main / ".claude/orchestration/landing-gate-timings.jsonl"


def read_rows(path, since):
    rows = []
    for line in Path(path).read_text().splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        if datetime.fromisoformat(row["ts"]) >= since:
            rows.append(row)
    return rows


def landings_since(repo, since):
    """First-parent merges on main in the window: one per landing."""
    out = subprocess.run(["git", "-C", str(repo), "log", "--merges", "--first-parent",
                          f"--since={since.isoformat()}", "--format=%H", "origin/main"],
                         capture_output=True, text=True, timeout=60)
    if out.returncode:
        raise RuntimeError(out.stderr.strip())
    return len(out.stdout.split())


def summarize(rows, landings):
    """{metric: value} over gate-run rows; pure so the arithmetic is testable."""
    runs = len(rows)
    red = sum(1 for r in rows if r.get("failed"))
    legs = collections.defaultdict(list)
    status = collections.Counter()
    slow_max = {}
    slow_landings = collections.Counter()
    for row in rows:
        seen = set()
        for check in row["checks"]:
            label = check["label"].split("/")[0]
            status[(label, check["status"])] += 1
            if check.get("duration_s"):
                legs[label].append(check["duration_s"])
            for test in check.get("slow_tests", []):
                name, seconds = test["name"], test["s"]
                slow_max[name] = max(slow_max.get(name, 0), seconds)
                seen.add(name)
        # A timing row is one logged landing attempt, even across several checks.
        slow_landings.update(seen)
    proofs = {s: status[("gpu-proofs", s)] for s in ("PASS", "REUSED", "FAIL", "SKIP")}
    proof_runs = proofs["PASS"] + proofs["REUSED"] + proofs["FAIL"]
    waits = [r["gpu_wait_s"] for r in rows if r.get("gpu_wait_s") is not None]
    return {
        "landings": landings,
        "gate_runs": runs,
        "runs_per_landing": round(runs / landings, 2) if landings else None,
        "red_share": round(red / runs, 2) if runs else None,
        "gpu_proofs_runs": proof_runs,
        "gpu_proofs_reuse_share": round(proofs["REUSED"] / proof_runs, 2) if proof_runs else None,
        "gpu_proofs_median_s": round(statistics.median(legs["gpu-proofs"])) if legs["gpu-proofs"] else None,
        "gpu_wait_total_s": round(sum(waits)),
        "gpu_wait_runs_logged": len(waits),
        "flow_gate_over_10min": sum(1 for s in legs["flow-gate"] if s > 600),
        "leg_hours": {label: round(sum(v) / 3600, 1) for label, v in
                      sorted(legs.items(), key=lambda kv: -sum(kv[1]))[:5]},
        "slow_tests": [{"name": name, "max_s": seconds, "landings": slow_landings[name]}
                       for name, seconds in
                       sorted(slow_max.items(), key=lambda item: (-item[1], item[0]))[:10]],
    }


def render(metrics, days):
    m = metrics
    lines = [f"landing metrics, last {days} days:",
             f"  landings {m['landings']}, gate runs {m['gate_runs']}, "
             f"runs per landing {m['runs_per_landing']}, red share {m['red_share']}",
             f"  gpu-proofs: {m['gpu_proofs_runs']} runs, reuse share {m['gpu_proofs_reuse_share']}, "
             f"median {m['gpu_proofs_median_s']}s",
             f"  gpu queue wait: {m['gpu_wait_total_s']}s over {m['gpu_wait_runs_logged']} logged runs; "
             f"flow-gate over 10 min: {m['flow_gate_over_10min']}",
             "  hours by leg: " + ", ".join(f"{k} {v}" for k, v in m["leg_hours"].items())]
    lines.append("  slowest tests (max seconds; logged landing attempts):")
    lines.extend(f"    {test['name']}: {test['max_s']:.3f}s; {test['landings']} landings"
                 for test in m["slow_tests"])
    if not m["slow_tests"]:
        lines.append("    none recorded")
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--days", type=int, default=7)
    parser.add_argument("--timings", type=Path, default=None,
                        help="gate timing log (default: the main checkout's)")
    parser.add_argument("--repo", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    since = datetime.now(timezone.utc) - timedelta(days=args.days)
    try:
        timings = args.timings or timings_path(args.repo)
        metrics = summarize(read_rows(timings, since), landings_since(args.repo, since))
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"landing metrics skipped: {error}")
        return 0
    print(render(metrics, args.days))
    ratio = metrics["runs_per_landing"]
    if ratio is not None and ratio > RUNS_PER_LANDING_RED:
        print(f"LANDING METRICS: RED (runs per landing {ratio} > {RUNS_PER_LANDING_RED}; "
              "reds are being found by rerunning the gate — see the rerun: rule in "
              ".claude/GIT_TREE_DISCIPLINE.md section 2 (Landing protocol))")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
