#!/usr/bin/env python3
"""One status screen: lanes, GPU holder, worktree slots, running landing gate.

Composes existing tools, never reimplements them: fleet_health.py (lanes),
gpu_queue.py status, agent-worktree.py list, watch_land's [RUN] parser, and
landing_metrics.timings_path for the median leg seconds behind the ETA.
Each source runs in parallel under its own timeout; a source that fails or
hangs prints one line saying so and the screen still finishes.
"""
import concurrent.futures
import json
import os
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))

SOURCE_TIMEOUT = 2.5
MAX_DETAIL = 4
TIMING_ROWS = 300  # recent gate runs feed the median


def run(cmd, timeout):
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    return out.stdout + out.stderr


def lanes_block(runner=run):
    lines = runner([sys.executable, str(HERE / "fleet_health.py"), "--no-mark"], SOURCE_TIMEOUT).splitlines()
    lines = [l for l in lines if l.strip()]
    if not lines:
        return ["lanes: no output"]
    detail = [l for l in lines[:-1]][:MAX_DETAIL - 1]
    return [f"lanes: {lines[-1]}"] + [f"  {l}" for l in detail]


def gpu_block(runner=run):
    out = runner([str(HERE / "gpu_queue.py"), "status"], SOURCE_TIMEOUT).strip().splitlines()
    return [f"gpu: {out[0]}" if out else "gpu: no output"]


def slots_block(runner=run):
    counts = {}
    for line in runner([str(HERE / "agent-worktree.py"), "list"], SOURCE_TIMEOUT).splitlines():
        m = re.match(r"^([A-Z][A-Z-]+)\s+slot-\d+", line)
        if m:
            counts[m.group(1)] = counts.get(m.group(1), 0) + 1
    if not counts:
        return ["slots: none listed"]
    parts = ", ".join(f"{k} {v}" for k, v in sorted(counts.items()))
    return [f"slots: {sum(counts.values())} total, {parts}"]


SLOTS_CACHE = Path.home() / ".cache/manifold/status-slots.txt"


def slots_source(runner=run, cache=None):
    """agent-worktree list scans git and disk (~2.5 s); a timeout shows the last good line."""
    cache = cache or SLOTS_CACHE
    try:
        lines = slots_block(runner)
    except Exception as exc:
        try:
            age = round((time.time() - cache.stat().st_mtime) / 60)
            return [f"slots: list too slow ({exc}); last known {age} min ago: "
                    + cache.read_text().strip().removeprefix("slots: ")]
        except OSError:
            raise exc
    try:
        cache.parent.mkdir(parents=True, exist_ok=True)
        cache.write_text(lines[0] + "\n")
    except OSError:
        pass
    return lines


def read_timings(path):
    rows = []
    for line in Path(path).read_text().splitlines():
        try:
            rows.append(json.loads(line))
        except ValueError:
            continue
    return rows[-TIMING_ROWS:]


def median_leg_seconds(rows):
    legs = {}
    for row in rows:
        for check in row.get("checks", []):
            if check.get("duration_s"):
                legs.setdefault(check["label"], []).append(check["duration_s"])
    return {label: statistics.median(v) for label, v in legs.items()}


def eta_seconds(medians, label, elapsed):
    if label not in medians:
        return None
    return max(0, round(medians[label] - elapsed))


def gate_processes():
    """Running landing_gate.py / land_branch.py processes and the repo each works on."""
    out = run(["ps", "-axo", "pid=,command="], SOURCE_TIMEOUT)
    procs = []
    for line in out.splitlines():
        m = re.match(r"\s*(\d+)\s+(.*)", line)
        if not m or not re.search(r"scripts/(landing_gate|land_branch)\.py", m.group(2)):
            continue
        if "ps -axo" in m.group(2):
            continue
        cmd = m.group(2)
        repo = re.search(r"--repo[= ]((?:[^\s\"']+)|\"[^\"]+\")", cmd)
        procs.append({"pid": int(m.group(1)),
                      "cmd": "land_branch" if "land_branch" in cmd else "landing_gate",
                      "repo": Path(repo.group(1).strip('"')) if repo else ROOT})
    return procs


def _live_leg(repo):
    """Newest transcript under the repo's landing-logs: (label, path)."""
    logs = Path(repo) / "target/landing-logs"
    files = sorted(logs.glob("*.log"), key=lambda p: p.stat().st_mtime) if logs.is_dir() else []
    if not files:
        return None
    path = files[-1]
    label = re.sub(r"-\d{8}T.*$", "", path.stem)
    try:  # a [RUN] line in the transcript names the leg exactly; reuse watch_land's parser
        import watch_land
        for line in path.read_text(errors="replace").splitlines()[-200:]:
            m = watch_land._RUN_RE.match(line.strip())
            if m:
                label = m.group("label").strip()
    except Exception:
        pass
    return label, path


def gate_block(procs, now=None, timings=None, started=None):
    if not procs:
        return ["gate: none running"]
    now = now or time.time()
    medians = median_leg_seconds(timings or [])
    lines = []
    for proc in procs:
        leg = _live_leg(proc["repo"])
        if leg is None:
            lines.append(f"gate: {proc['cmd']} pid {proc['pid']} running, no leg transcript yet")
            continue
        label, path = leg
        begin = (started or {}).get(path) or getattr(path.stat(), "st_birthtime", path.stat().st_mtime)
        elapsed = max(0, round(now - begin))
        eta = eta_seconds(medians, label.split("/")[0], elapsed)
        tail = f", eta {eta}s" if eta is not None else ", no eta (no history)"
        lines.append(f"gate: {proc['cmd']} pid {proc['pid']} on {label}, {elapsed}s in{tail}")
    return lines


def gate_source():
    timings = []
    try:
        import landing_metrics
        timings = read_timings(landing_metrics.timings_path(ROOT))
    except Exception:
        pass
    return gate_block(gate_processes(), timings=timings)


def guarded(name, fn, timeout=SOURCE_TIMEOUT + 0.1):
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=1)
    future = pool.submit(fn)
    try:
        return future.result(timeout=timeout)
    except concurrent.futures.TimeoutError:
        return [f"{name}: timed out after {timeout}s"]
    except Exception as exc:  # a failing source must not take the screen down
        return [f"{name}: unavailable ({exc})"]
    finally:
        pool.shutdown(wait=False, cancel_futures=True)


SOURCES = [("lanes", lanes_block), ("gpu", gpu_block), ("slots", slots_source), ("gate", gate_source)]


def main(argv=None):
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(SOURCES)) as pool:
        futures = [pool.submit(guarded, name, fn) for name, fn in SOURCES]
        for future in futures:
            for line in future.result():
                print(line)
    return 0


if __name__ == "__main__":
    code = main()
    sys.stdout.flush()
    os._exit(code)  # abandoned source threads must not hold the screen open
