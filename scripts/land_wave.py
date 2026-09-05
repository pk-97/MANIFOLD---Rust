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

Usage: python3 scripts/land_wave.py <wave-branch> ["merge message"]

Exits non-zero (before pushing) when: the gate fails, the tree is dirty,
origin/main advanced concurrently, or the wave is not ahead of origin/main.
"""

import subprocess
import sys


def run(cmd, check=True, capture=True):
    r = subprocess.run(cmd, text=True, capture_output=capture)
    if check and r.returncode != 0:
        sys.exit(f"FAIL {' '.join(cmd)}\n{r.stdout}\n{r.stderr}")
    return r.stdout.strip() if capture else ""


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
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
    run(["python3", "scripts/landing_gate.py"], capture=False)

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
