#!/usr/bin/env python3
"""Run the UI-flow suite against its manifest-declared scenes (S8).

Reads scripts/ui-flows/manifest.json and runs the selected flows through one
`<manifold binary> ui-snap batch <scene> <script> ...` process (the binary
`cargo xtask` would run, built once up front). Each flow reports exactly what
`ui-snap <scene> --script scripts/ui-flows/<flow>.json` would, and anything
the batch can't vouch for reruns that way, solo; scripts/ui_flows_batch_proof.py
is the oracle that the two modes agree. The manifest
is the single source of the flow->scene mapping, so a flow can never be run under
the wrong scene by lore (the P-P landing's false FAIL) and no flow file can be
silently skipped (the BUG-252 count-match gate, made mechanical here).

Manifest sections:
  flows          — flow -> scene. Every one MUST pass; a FAIL is a regression.
  expected_fail  — flow -> {scene, bug, reason}. Known-red flows, mapped to their
                   correct authoring scene but failing for a tracked/pending
                   reason. Reported as XFAIL; an unexpected PASS is flagged so the
                   flow gets promoted back into `flows`.
  unresolved     — flow -> reason. No confident scene; listed, never guessed.

Harness exit codes (crates/manifold-app/src/ui_snapshot/script.rs):
  0 = all assertions passed · 1 = an assertion failed · 2 = setup error
      (unknown scene / unreadable script).

Runner exit: 0 iff every `flows` entry PASSed, no `expected_fail` entry
unexpectedly PASSed, and every flow file on disk is accounted for
(flows | expected_fail | unresolved); 2 if the binary does not build. Run under
the build lock:
  .claude/scripts/with-build-lock.sh scripts/run_ui_flows.py

GPU: the build runs with no lock held; the flow loop then holds the
machine-wide GPU lock (scripts/gpu_queue.py) once, and every flow process
(batch or solo) inherits it. Without the outer hold each flow re-queued on its own and other
lanes' GPU jobs slotted in between flows (BUG-i3hc (flow gate re-queues the
GPU lock per flow)). Every line is flushed so a caller's timeout keeps the
transcript so far.

Filter to a subset with flow-name substrings:
  scripts/run_ui_flows.py scene-setup audio
Landing flow gate (BUG-313 postmortem — the drag flow that caught the bug was
red on main and nothing ran it): derive the filters from a git range via the
manifest's `path_triggers` (path prefix -> filter list; a touched
scripts/ui-flows/<flow>.json always runs that flow):
  scripts/run_ui_flows.py --touched origin/main...HEAD
No trigger matches the diff -> exits 0 without building anything.
"""
import collections
import functools
import json
import os
import subprocess
import sys
import threading
import time

import gpu_queue

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FLOW_DIR = os.path.join(ROOT, "scripts", "ui-flows")
MANIFEST = os.path.join(FLOW_DIR, "manifest.json")
# Same package, features and binary as the `cargo xtask` alias (.cargo/config.toml).
BUILD_CMD = ["cargo", "build", "--quiet", "-p", "manifold-app",
             "--features", "ui-snapshot,perf-soak",
             "--message-format=json-render-diagnostics"]
BIN_NAME = "manifold"

say = functools.partial(print, flush=True)


def build_binary():
    """Build the flow binary once; return its path, or None after printing why.

    Cargo honours CARGO_TARGET_DIR (the landing gate pins it), and the JSON
    artifact message names the executable wherever that is."""
    say("flow gate: building " + " ".join(BUILD_CMD[:-1]))
    start = time.monotonic()
    r = subprocess.run(BUILD_CMD, cwd=ROOT, stdout=subprocess.PIPE, text=True)
    binary = None
    for line in r.stdout.splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if (msg.get("reason") == "compiler-artifact"
                and msg.get("target", {}).get("name") == BIN_NAME
                and msg.get("executable")):
            binary = msg["executable"]
    if r.returncode != 0 or binary is None:
        say(f"flow gate: build FAILED (exit {r.returncode}"
            + ("" if binary else f", no `{BIN_NAME}` executable reported") + ")")
        return None
    say(f"flow gate: built {binary} ({time.monotonic() - start:.0f}s)")
    return binary


def flow_script(name):
    return os.path.join("scripts", "ui-flows", f"{name}.json")


def run_flow(binary, name, scene):
    """One flow in its own process. Returns (exit code, last stderr line)."""
    r = subprocess.run(
        [binary, "ui-snap", scene, "--script", flow_script(name)],
        cwd=ROOT, capture_output=True, text=True,
    )
    tail = (r.stderr.strip().splitlines() or ["(no stderr)"])[-1]
    return r.returncode, tail


BATCH_RECORD = "@@ui-snap-batch@@"


def run_batch(binary, jobs, report, fell_back=None):
    """Run `jobs` ([(name, scene)]) in as few `ui-snap batch` processes as
    possible; call report(name, scene, code, tail, seconds) once per job as
    each finishes, and fell_back(name, why) for each job that ran solo.

    The batch process reports each flow with the code and tail a solo run
    would give (crates/manifold-app/src/ui_snapshot/script.rs `run_batch`).
    Anything it can't vouch for runs solo here instead: a flow it marks
    `rerun`, and the flow it was inside when it died. After a death the rest
    go to a fresh batch. Every pass either reports a flow or falls back to
    solo for all that remain, so this always terminates."""
    pending = list(jobs)
    while pending:
        argv = [binary, "ui-snap", "batch"]
        for name, scene in pending:
            argv += [scene, flow_script(name)]
        proc = subprocess.Popen(argv, cwd=ROOT, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True)
        err_tail = collections.deque(maxlen=20)
        drain = threading.Thread(target=lambda: err_tail.extend(proc.stderr))
        drain.start()
        finished, solo, current = set(), {}, None
        for line in proc.stdout:
            if not line.startswith(BATCH_RECORD):
                continue
            rec = json.loads(line[len(BATCH_RECORD):])
            if "begin" in rec:
                current = rec["begin"]
                continue
            index, current = rec["index"], None
            finished.add(index)
            if "rerun" in rec:
                solo[index] = f"handed back: {rec['rerun']}"
                say(f"  (batch handed back {pending[index][0]}: {rec['rerun']}; running it solo)")
            else:
                name, scene = pending[index]
                report(name, scene, rec["code"], rec["tail"], rec["seconds"])
        proc.wait()
        drain.join()
        proc.stdout.close()
        proc.stderr.close()
        if current is not None:
            finished.add(current)
            last_err = (list(err_tail) or ["(no stderr)"])[-1].strip()
            solo[current] = f"batch died, exit {proc.returncode}: {last_err}"
            say(f"  (batch died in {pending[current][0]}, exit {proc.returncode}: "
                f"{last_err}; running it solo)")
        if not finished:
            say(f"  (batch exited {proc.returncode} without running a flow; "
                "running the rest solo)")
            solo = {i: f"batch ran nothing, exit {proc.returncode}" for i in range(len(pending))}
        for index, why in solo.items():
            name, scene = pending[index]
            if fell_back is not None:
                fell_back(name, why)
            start = time.monotonic()
            code, tail = run_flow(binary, name, scene)
            report(name, scene, code, tail, time.monotonic() - start)
        done = finished | solo.keys()
        pending = [job for i, job in enumerate(pending) if i not in done]


def write_gate_marker(range_spec, filters, ok):
    """Record a --touched run for the pre-land flow-gate guard.

    preToolUseBash.py (flow_gate_guard) denies a merge into main when the
    merged branch touches flow-mapped paths and this marker is missing, red,
    or written at a different HEAD than the branch tip — so "the gate ran,
    green, on exactly what lands" is machine-checked, not remembered. The
    marker lives in the MAIN checkout (resolved via --git-common-dir) so a
    run inside a branch worktree is visible to the guard. Best-effort: a
    marker failure never fails the suite.
    """
    try:
        common = subprocess.run(
            ["git", "rev-parse", "--git-common-dir"],
            cwd=ROOT, capture_output=True, text=True, timeout=15)
        head = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT, capture_output=True, text=True, timeout=15)
        if common.returncode != 0 or head.returncode != 0:
            return
        main_root = os.path.dirname(
            os.path.normpath(os.path.join(ROOT, common.stdout.strip())))
        marker_dir = os.path.join(main_root, ".claude", "orchestration")
        os.makedirs(marker_dir, exist_ok=True)
        import datetime
        with open(os.path.join(marker_dir, "flow-gate-marker.json"), "w") as f:
            json.dump({
                "head": head.stdout.strip(),
                "range": range_spec,
                "filters": filters,
                "pass": ok,
                "ts": datetime.datetime.now(datetime.timezone.utc)
                      .isoformat(timespec="seconds"),
            }, f, indent=1)
        say(f"flow gate: marker written for HEAD {head.stdout.strip()[:12]} "
            f"(pass={ok})")
    except Exception as e:
        print(f"flow gate: marker write failed (non-fatal): {e}",
              file=sys.stderr)


def filters_for_touched(range_spec, manifest):
    """Map a git diff range to flow-name filters via manifest `path_triggers`.
    Returns (filters, hits) — hits is {touched_path: [matched prefixes/flows]}
    for the gate's own output. A touched flow file runs itself (exact-name
    filter). Raises SystemExit(2) if the diff itself fails."""
    import diff_scope
    try:
        if "..." in range_spec:
            base, head = range_spec.split("...", 1)
            base = diff_scope.git(ROOT, "merge-base", base, head).strip()
        elif ".." in range_spec:
            base, head = range_spec.split("..", 1)
        else:
            base, head = range_spec, None
        paths, _ = diff_scope.effective_paths(ROOT, base, head)
    except RuntimeError as error:
        print(f"flow gate: {error}", file=sys.stderr)
        raise SystemExit(2)
    return filters_for_paths(paths, manifest)

def filters_for_paths(paths, manifest):
    triggers = manifest.get("path_triggers", {})
    filters, hits = set(), {}
    for path in paths:
        if path.startswith("scripts/ui-flows/") and path.endswith(".json") and os.path.basename(path) != "manifest.json":
            name = os.path.splitext(os.path.basename(path))[0]
            filters.add(name)
            hits.setdefault(path, []).append(name)
        for prefix, flist in triggers.items():
            if path.startswith(prefix):
                filters.update(flist)
                hits.setdefault(path, []).append(prefix)
    return sorted(filters), hits


def main():
    args = sys.argv[1:]
    touched_range = None
    if "--touched" in args:
        i = args.index("--touched")
        if i + 1 >= len(args):
            print("usage: run_ui_flows.py [--touched <git-range>] [filter ...]",
                  file=sys.stderr)
            return 2
        touched_range = args[i + 1]
        del args[i:i + 2]
    filters = args
    with open(MANIFEST) as f:
        manifest = json.load(f)
    if touched_range is not None:
        gate_filters, hits = filters_for_touched(touched_range, manifest)
        if not gate_filters:
            say(f"flow gate: no flow-mapped paths touched in {touched_range} "
                "— nothing to run")
            write_gate_marker(touched_range, [], True)
            return 0
        say(f"flow gate: {touched_range} → {len(hits)} flow-mapped file(s) "
            f"→ filters {gate_filters}")
        filters = filters + gate_filters if filters else gate_filters
    flows = manifest["flows"]
    xfail = manifest.get("expected_fail", {})
    unresolved = manifest.get("unresolved", {})

    on_disk = {
        os.path.splitext(n)[0]
        for n in os.listdir(FLOW_DIR)
        if n.endswith(".json") and n != "manifest.json"
    }
    accounted = set(flows) | set(xfail) | set(unresolved)
    missing = sorted(on_disk - accounted)   # flow files nobody maps
    stale = sorted(accounted - on_disk)     # manifest entries with no file

    def keep(n):
        return not filters or any(s in n for s in filters)

    required = [n for n in sorted(flows) if keep(n)]
    known_red = [n for n in sorted(xfail) if keep(n)]
    green_fail, xfail_ok, xfail_surprise = [], [], []

    if required or known_red:
        binary = build_binary()
        if binary is None:
            if touched_range is not None:
                write_gate_marker(touched_range, filters, False)
            return 2
        gate_start = time.monotonic()
        with gpu_queue.hold(f"run_ui_flows: {len(required) + len(known_red)} flows",
                            out=sys.stdout):
            say("— required flows —")

            def report_required(name, scene, code, tail, seconds):
                if code == 0:
                    say(f"  PASS   {name}  [{scene}]  {seconds:.1f}s")
                else:
                    green_fail.append(name)
                    say(f"  FAIL   {name}  [{scene}]  {seconds:.1f}s  exit={code}  {tail}")

            run_batch(binary, [(n, flows[n]) for n in required], report_required)

            if known_red:
                say("— known-red flows (expected fail) —")

            def report_known_red(name, scene, code, tail, seconds):
                bug = xfail[name].get("bug", "?")
                if code != 0:
                    xfail_ok.append(name)
                    say(f"  XFAIL  {name}  [{scene}]  {seconds:.1f}s  ({bug}) exit={code}")
                else:
                    xfail_surprise.append(name)
                    say(f"  XPASS  {name}  [{scene}]  {seconds:.1f}s  now GREEN — promote into flows ({bug})")

            run_batch(binary, [(n, xfail[n]["scene"]) for n in known_red], report_known_red)
        say(f"flow gate: {len(required) + len(known_red)} flows in "
            f"{time.monotonic() - gate_start:.0f}s under one GPU hold")

    ran_green = len(required)
    say(f"\n{ran_green - len(green_fail)}/{ran_green} required flows passed"
        + (f", {len(green_fail)} REGRESSED: {green_fail}" if green_fail else ""))
    say(f"{len(xfail_ok)} known-red (xfail) still red"
        + (f"; {len(xfail_surprise)} now GREEN (promote): {xfail_surprise}" if xfail_surprise else ""))
    if unresolved:
        say(f"unresolved (no confident scene): {sorted(unresolved)}")
    say(f"{len(accounted)}/{len(on_disk)} flow files accounted for in the manifest")
    if missing:
        say(f"UNMAPPED flow files (add to manifest): {missing}")
    if stale:
        say(f"STALE manifest entries (no such flow file): {stale}")

    ok = not green_fail and not xfail_surprise and not missing and not stale
    if touched_range is not None:
        write_gate_marker(touched_range, filters, ok)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
