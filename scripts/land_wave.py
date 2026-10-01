#!/usr/bin/env python3
"""Land a wave branch to main WITHOUT the main checkout.

Why this exists (2026-09-05): a harness worktree-pinning bug locked the lead
session out of the main checkout for an entire session, forcing a manual
Terminal landing — which then hit a dirty-tree conflict and the vim merge
message trap. A landing must never depend on the main checkout being
reachable, and it must never open an editor.

Protocol preserved: fetch → integrate origin/main into the wave → landing
gate → no-ff merge commit (parents: origin/main, wave tip) → ff push to
main → verify. The merge commit is built with `git commit-tree`, which is
exactly what `git merge --no-ff` produces, minus the checkout requirement.

Usage: scripts/land_wave.py <wave-branch> ["merge message"]
       scripts/land_wave.py --batch <branch>[@<tip>] <branch>[@<tip>] ... \\
           [--gate "<cmd>"] [--message "<text>"]

Exits non-zero (before pushing) when: the gate fails, the tree is dirty,
origin/main advanced concurrently, or the wave is not ahead of origin/main.

Batch mode is the standard path when more than one branch is ready; a single
ready branch still lands alone. The gate costs 30-60 minutes, so N branches
are gated once, not N times:
  1. Fetch, build a landing branch `land/batch-<pid>` at origin/main, and
     `git merge --no-ff` each branch (or its pinned `@tip`) in the order
     given, which is queue order. Messages are scripted, no editor. A branch
     that conflicts is aborted and dropped, with the reason in the report.
  2. Run the gate once on the result (default `scripts/landing_gate.py`;
     --gate overrides; it runs in this checkout on the landing branch).
  3. Red gate: for each branch in turn, rebuild the batch without it and
     re-gate. The first rebuild that goes green names the culprit; it is
     dropped with the gate's failure tail in the report and the rest land.
     If no single removal turns it green, nothing lands (several culprits
     or a base failure) and the script exits non-zero. A gate that dies on a
     Python traceback (initial or in a rebuild) stops the search and says
     "gate crashed" / "culprit search crashed"; it never blames the branches.
  4. Land with the same commit-tree merge as single mode: tree of the
     landing tip, parents origin/main and the landing tip, message listing
     every landed branch and tip (and every dropped one with its reason),
     then ff-only push and verify.
Self-tests: scripts/test_land_wave.py (throwaway repos, stub gate).
"""

import os
import shlex
import subprocess
import sys


def run(cmd, check=True, capture=True):
    r = subprocess.run(cmd, text=True, capture_output=capture)
    if check and r.returncode != 0:
        sys.exit(f"FAIL {' '.join(cmd)}\n{r.stdout}\n{r.stderr}")
    return r.stdout.strip() if capture else ""


def parse_spec(spec):
    name, _, tip = spec.partition("@")
    return name, (tip or None)


def build_landing(origin_main, specs, landing):
    """Create `landing` at origin_main and merge each spec in order.
    Returns (included [(name, tip)], dropped [(name, tip, reason)])."""
    run(["git", "checkout", "-q", "-B", landing, origin_main])
    included, dropped = [], []
    for name, pin in specs:
        tip = run(["git", "rev-parse", pin or name])
        r = subprocess.run(
            ["git", "merge", "--no-ff", "--no-edit", "-m",
             f"Merge {name} ({tip[:9]})", tip], text=True, capture_output=True)
        if r.returncode != 0:
            subprocess.run(["git", "merge", "--abort"], capture_output=True)
            dropped.append((name, tip, "merge conflict with the batch so far"))
        else:
            included.append((name, tip))
    return included, dropped


def run_gate(gate):
    """(ok, output tail, crashed). `crashed` means the gate died on an uncaught
    Python exception instead of reporting a failed check; a rebuild that crashes
    says nothing about which branch is at fault."""
    r = subprocess.run(gate, text=True, capture_output=True)
    out = (r.stdout + r.stderr).strip()
    crashed = r.returncode != 0 and "Traceback (most recent call last)" in out
    return r.returncode == 0, out[-600:], crashed


def batch_message(included, dropped, message):
    lines = [message or f"Land batch of {len(included)} branches", ""]
    lines += [f"- {n} {t}" for n, t in included]
    if dropped:
        lines += ["", "Dropped:"]
        lines += [f"- {n} {t[:9]}: {why}" for n, t, why in dropped]
    return "\n".join(lines)


def land_batch(specs, gate, message=None):
    if run(["git", "status", "--porcelain"]):
        sys.exit("dirty worktree — commit or stash first")
    run(["git", "fetch", "origin"])
    origin_main = run(["git", "rev-parse", "origin/main"])
    landing = f"land/batch-{os.getpid()}"

    included, dropped = build_landing(origin_main, specs, landing)
    if not included:
        sys.exit(f"nothing to land: every branch was dropped\n{dropped}")
    ok, tail, crashed = run_gate(gate)
    if crashed:
        sys.exit(f"gate crashed; nothing landed\n{tail}")
    if not ok:
        culprit = None
        for name, tip in list(included):
            rest = [(n, t) for n, t in included if n != name]
            if not rest:
                continue
            inc2, drop2 = build_landing(origin_main, rest, landing)
            ok2, tail2, crashed2 = run_gate(gate)
            if crashed2:
                sys.exit(f"culprit search crashed (gate died rebuilding without {name}); nothing landed\n{tail2}")
            if ok2 and not drop2:
                culprit, included = (name, tip, "gate red: " + tail), inc2
                break
        if culprit is None:
            sys.exit(f"gate red and no single branch removal fixes it; nothing landed\n{tail}")
        dropped.append(culprit)

    land_tip = run(["git", "rev-parse", "HEAD"])
    tree = run(["git", "rev-parse", f"{land_tip}^{{tree}}"])
    merge_sha = run(["git", "commit-tree", tree, "-p", origin_main, "-p", land_tip,
                     "-m", batch_message(included, dropped, message)])
    run(["git", "push", "origin", f"{merge_sha}:refs/heads/main"])
    run(["git", "fetch", "origin"])
    landed = run(["git", "rev-parse", "origin/main"])
    if landed != merge_sha:
        sys.exit(f"push verification failed: origin/main at {landed}")
    print(f"landed {', '.join(n for n, _ in included)} -> main @ {merge_sha[:9]}")
    for n, t, why in dropped:
        print(f"DROPPED {n} {t[:9]}: {why}")
    return merge_sha, included, dropped


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    if sys.argv[1] == "--batch":
        args, gate, message, specs = sys.argv[2:], ["scripts/landing_gate.py"], None, []
        while args:
            a = args.pop(0)
            if a == "--gate":
                gate = shlex.split(args.pop(0))
            elif a == "--message":
                message = args.pop(0)
            else:
                specs.append(parse_spec(a))
        if not specs:
            sys.exit(__doc__)
        land_batch(specs, gate, message)
        return
    wave = sys.argv[1]
    message = (
        sys.argv[2]
        if len(sys.argv) > 2
        else f"Merge {wave}"
    )

    if run(["git", "status", "--porcelain"]):
        sys.exit("dirty worktree — commit or stash first")

    run(["git", "fetch", "origin"])
    origin_main = run(["git", "rev-parse", "origin/main"])
    wave_tip = run(["git", "rev-parse", wave])

    if run(["git", "merge-base", "--is-ancestor", wave_tip, origin_main], check=False) == "" \
            and run(["git", "rev-parse", wave_tip]) == origin_main:
        print("already landed")
        return

    # 1. integrate origin/main into the wave (scripted message, no editor)
    run(["git", "checkout", wave])
    if not run(["git", "merge-base", "--is-ancestor", origin_main, wave_tip], check=False):
        r = subprocess.run(
            ["git", "merge", "--no-edit", "-m",
             f"Merge origin/main into {wave} (landing integration)", "origin/main"],
            text=True, capture_output=True)
        if r.returncode != 0:
            sys.exit(
                "conflict integrating origin/main — resolve in the wave, "
                f"re-run the gate, then re-run this script\n{r.stdout}\n{r.stderr}")
        wave_tip = run(["git", "rev-parse", wave])

    # 2. landing gate on the merged tree
    run(["scripts/landing_gate.py"], capture=False)

    # 3. canonical no-ff merge commit without the main checkout
    tree = run(["git", "rev-parse", f"{wave_tip}^{{tree}}"])
    merge_sha = run([
        "git", "commit-tree", tree,
        "-p", origin_main, "-p", wave_tip, "-m", message])

    # 4. ff push (git enforces ff; never force)
    run(["git", "push", "origin", f"{merge_sha}:refs/heads/main"])

    # 5. verify
    run(["git", "fetch", "origin"])
    landed = run(["git", "rev-parse", "origin/main"])
    if landed != merge_sha:
        sys.exit(f"push verification failed: origin/main at {landed}")
    print(f"landed {wave} -> main @ {merge_sha[:9]}")


if __name__ == "__main__":
    main()
