#!/usr/bin/env python3
"""Proof that `ui-snap batch` changes nothing a flow reports or writes.

Runs every manifest flow (flows + expected_fail, or the subset matching the
name filters given) twice from one built binary under one GPU hold: first each
in its own `ui-snap <scene> --script` process, then all through
run_ui_flows.run_batch. Each flow's run directory
(target/ui-snapshots/<scene>/run-<flow>/) is emptied before each mode, so only
that mode's artifacts are compared.

Per flow it compares the verdict (exit code and the tail line the gate
prints) and the sha256 of every file written (PNGs, dumps, result.json). A
flow that differs is run solo once more: if the second solo run differs from
the first in the same place, the flow is nondeterministic on its own and is
listed, not blamed on batching; otherwise the difference is batch-induced.

Exit 0 iff no flow has a batch-induced difference and every flow actually
ran inside a batch (a flow the batch handed back or died in ran solo, which
proves nothing about batching). Usage:
  scripts/ui_flows_batch_proof.py [filter ...]
"""
import hashlib
import json
import os
import shutil
import sys
import time

import gpu_queue
import run_ui_flows

say = run_ui_flows.say


def run_dir(name, scene):
    return os.path.join(run_ui_flows.ROOT, "target", "ui-snapshots", scene, f"run-{name}")


def artifacts(name, scene):
    """{relative path: sha256} of everything the flow wrote."""
    root = run_dir(name, scene)
    out = {}
    for base, _, files in os.walk(root):
        for f in files:
            path = os.path.join(base, f)
            with open(path, "rb") as fh:
                out[os.path.relpath(path, root)] = hashlib.sha256(fh.read()).hexdigest()
    return out


def clear(jobs):
    for name, scene in jobs:
        shutil.rmtree(run_dir(name, scene), ignore_errors=True)


def solo_pass(binary, jobs):
    clear(jobs)
    seen = {}
    for name, scene in jobs:
        code, tail = run_ui_flows.run_flow(binary, name, scene)
        seen[name] = {"code": code, "tail": tail, "files": artifacts(name, scene)}
    return seen


def differences(a, b):
    """{what: description} for every way two observations of a flow differ;
    `what` is "exit", "tail" or an artifact path."""
    out = {}
    if a["code"] != b["code"]:
        out["exit"] = f"exit {a['code']} vs {b['code']}"
    if (a["code"] != 0 or b["code"] != 0) and a["tail"] != b["tail"]:
        out["tail"] = f"tail {a['tail']!r} vs {b['tail']!r}"
    for path in sorted(a["files"].keys() | b["files"].keys()):
        if a["files"].get(path) != b["files"].get(path):
            if path not in a["files"] or path not in b["files"]:
                out[path] = f"{path} only in {'solo' if path in a['files'] else 'batch'}"
            else:
                out[path] = f"{path} hash differs"
    return out


def main():
    filters = sys.argv[1:]
    with open(run_ui_flows.MANIFEST) as f:
        manifest = json.load(f)
    scenes = dict(manifest["flows"])
    scenes.update({n: e["scene"] for n, e in manifest.get("expected_fail", {}).items()})
    jobs = [(n, scenes[n]) for n in sorted(scenes)
            if not filters or any(s in n for s in filters)]
    if not jobs:
        say("batch proof: no flow matches the filters")
        return 2
    binary = run_ui_flows.build_binary()
    if binary is None:
        return 2

    with gpu_queue.hold(f"ui_flows_batch_proof: {len(jobs)} flows x2+", out=sys.stdout):
        say(f"batch proof: solo pass over {len(jobs)} flows")
        start = time.monotonic()
        solo = solo_pass(binary, jobs)
        solo_secs = time.monotonic() - start

        say(f"batch proof: batched pass over {len(jobs)} flows")
        clear(jobs)
        verdicts, fallbacks = {}, {}

        def report(name, scene, code, tail, seconds):
            verdicts[name] = {"code": code, "tail": tail}

        start = time.monotonic()
        run_ui_flows.run_batch(binary, jobs, report,
                               fell_back=lambda name, why: fallbacks.__setitem__(name, why))
        batch_secs = time.monotonic() - start
        batched = {n: dict(verdicts[n], files=artifacts(n, s)) for n, s in jobs}

        differing = {n: differences(solo[n], batched[n]) for n, _ in jobs}
        differing = {n: d for n, d in differing.items() if d}
        rerun = solo_pass(binary, [(n, s) for n, s in jobs if n in differing])

    induced, unstable = {}, {}
    for name, diffs in differing.items():
        solo_diffs = differences(solo[name], rerun[name])
        own = [text for what, text in diffs.items() if what not in solo_diffs]
        if own:
            induced[name] = own
        else:
            unstable[name] = list(diffs.values())

    say(f"\nbatch proof: solo {solo_secs:.0f}s, batched {batch_secs:.0f}s "
        f"({len(jobs)} flows)")
    for name, why in sorted(fallbacks.items()):
        say(f"  RAN SOLO     {name}: {why}")
    for name, diffs in sorted(unstable.items()):
        say(f"  UNSTABLE     {name} (differs between two solo runs too): {'; '.join(diffs)}")
    for name, diffs in sorted(induced.items()):
        say(f"  BATCH DIFF   {name}: {'; '.join(diffs)}")
    same = len(jobs) - len(differing)
    say(f"{same}/{len(jobs)} flows identical (verdict + every artifact hash); "
        f"{len(unstable)} unstable solo; {len(induced)} batch-induced; "
        f"{len(fallbacks)} ran solo instead of batched")
    return 0 if not induced and not fallbacks else 1


if __name__ == "__main__":
    sys.exit(main())
