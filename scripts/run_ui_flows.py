#!/usr/bin/env python3
"""Run the UI-flow suite against its manifest-declared scenes (S8).

Reads scripts/ui-flows/manifest.json and runs each flow via
`cargo xtask ui-snap <scene> --script scripts/ui-flows/<flow>.json`. The manifest
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
(flows | expected_fail | unresolved). Run under the build lock:
  .claude/scripts/with-build-lock.sh python3 scripts/run_ui_flows.py
Filter to a subset with flow-name substrings:
  python3 scripts/run_ui_flows.py scene-setup audio
Landing flow gate (BUG-313 postmortem — the drag flow that caught the bug was
red on main and nothing ran it): derive the filters from a git range via the
manifest's `path_triggers` (path prefix -> filter list; a touched
scripts/ui-flows/<flow>.json always runs that flow):
  python3 scripts/run_ui_flows.py --touched origin/main...HEAD
No trigger matches the diff -> exits 0 without building anything.
"""
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FLOW_DIR = os.path.join(ROOT, "scripts", "ui-flows")
MANIFEST = os.path.join(FLOW_DIR, "manifest.json")


def run_flow(name, scene):
    script = os.path.join("scripts", "ui-flows", f"{name}.json")
    r = subprocess.run(
        ["cargo", "xtask", "ui-snap", scene, "--script", script],
        cwd=ROOT, capture_output=True, text=True,
    )
    tail = (r.stderr.strip().splitlines() or ["(no stderr)"])[-1]
    return r.returncode, tail


def filters_for_touched(range_spec, manifest):
    """Map a git diff range to flow-name filters via manifest `path_triggers`.
    Returns (filters, hits) — hits is {touched_path: [matched prefixes/flows]}
    for the gate's own output. A touched flow file runs itself (exact-name
    filter). Raises SystemExit(2) if the diff itself fails."""
    r = subprocess.run(
        ["git", "diff", "--name-only", range_spec],
        cwd=ROOT, capture_output=True, text=True,
    )
    if r.returncode != 0:
        print(f"flow gate: `git diff --name-only {range_spec}` failed: "
              f"{r.stderr.strip()}", file=sys.stderr)
        raise SystemExit(2)
    triggers = manifest.get("path_triggers", {})
    filters, hits = set(), {}
    for path in r.stdout.splitlines():
        path = path.strip()
        if not path:
            continue
        if path.startswith("scripts/ui-flows/") and path.endswith(".json") \
                and os.path.basename(path) != "manifest.json":
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
            print(f"flow gate: no flow-mapped paths touched in {touched_range} "
                  "— nothing to run")
            return 0
        print(f"flow gate: {touched_range} → {len(hits)} flow-mapped file(s) "
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

    green_fail, xfail_ok, xfail_surprise = [], [], []

    print("— required flows —")
    for name in sorted(flows):
        if not keep(name):
            continue
        scene = flows[name]
        code, tail = run_flow(name, scene)
        if code == 0:
            print(f"  PASS   {name}  [{scene}]")
        else:
            green_fail.append(name)
            print(f"  FAIL   {name}  [{scene}]  exit={code}  {tail}")

    if any(keep(n) for n in xfail):
        print("— known-red flows (expected fail) —")
    for name in sorted(xfail):
        if not keep(name):
            continue
        entry = xfail[name]
        scene = entry["scene"]
        code, tail = run_flow(name, scene)
        if code != 0:
            xfail_ok.append(name)
            print(f"  XFAIL  {name}  [{scene}]  ({entry.get('bug', '?')}) exit={code}")
        else:
            xfail_surprise.append(name)
            print(f"  XPASS  {name}  [{scene}]  now GREEN — promote into flows ({entry.get('bug', '?')})")

    ran_green = sum(1 for n in flows if keep(n))
    print(f"\n{ran_green - len(green_fail)}/{ran_green} required flows passed"
          + (f", {len(green_fail)} REGRESSED: {green_fail}" if green_fail else ""))
    print(f"{len(xfail_ok)} known-red (xfail) still red"
          + (f"; {len(xfail_surprise)} now GREEN (promote): {xfail_surprise}" if xfail_surprise else ""))
    if unresolved:
        print(f"unresolved (no confident scene): {sorted(unresolved)}")
    print(f"{len(accounted)}/{len(on_disk)} flow files accounted for in the manifest")
    if missing:
        print(f"UNMAPPED flow files (add to manifest): {missing}")
    if stale:
        print(f"STALE manifest entries (no such flow file): {stale}")

    ok = not green_fail and not xfail_surprise and not missing and not stale
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
