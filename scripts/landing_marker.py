"""The landing-gate marker: the machine fact that a merge to main may happen.

landing_gate.py writes .claude/orchestration/landing-gate-marker.json in the
main checkout at the end of every run: the tree hash it gated, whether it
passed, the failing tests, and the pre-existing tests it classified. A merge to
main is allowed only when a passing marker names exactly the tree being merged.
Readers: the merge guard in .claude/hooks/preToolUseBash.py (loads this module),
land_branch.py, land_wave.py. Nothing but landing_gate.py writes the marker
(.claude/hooks/worktree-guard.py denies edits to it).

Obsolete when: landing moves to a server-side gate that runs the same checks.
"""

import json
import os
import subprocess
from pathlib import Path

SCHEMA = 1
MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
MARKER_PATH = MAIN_CHECKOUT / ".claude" / "orchestration" / "landing-gate-marker.json"


def tree_of(repo, ref):
    """Tree hash of `ref` in `repo`, or None when git cannot resolve it."""
    r = subprocess.run(["git", "-C", str(repo), "rev-parse", f"{ref}^{{tree}}"],
                       capture_output=True, text=True, timeout=30)
    return r.stdout.strip() if r.returncode == 0 and r.stdout.strip() else None


def write_marker(record, path=None):
    """Atomic write (temp file + rename) so a reader never sees half a marker."""
    path = Path(path or MARKER_PATH)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
    os.replace(tmp, path)


def marker_problem(tree, path=None):
    """Why the marker does not clear a merge of `tree`, or None when it does."""
    path = Path(path or MARKER_PATH)
    if not path.exists():
        return "no landing-gate marker exists"
    try:
        marker = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        return f"marker is unreadable ({error})"
    if not isinstance(marker, dict) or marker.get("schema") != SCHEMA:
        return "marker has an unknown schema"
    if marker.get("tree") != tree:
        return (f"marker is for tree {str(marker.get('tree'))[:12]}, "
                f"the branch tip's tree is {str(tree)[:12]}")
    if marker.get("pass") is not True:
        return "marker records a RED gate run"
    if marker.get("failing_tests"):
        return "marker lists failing tests"
    for entry in marker.get("pre_existing_tests") or []:
        if not isinstance(entry, dict) or not entry.get("test") or not entry.get("bead"):
            return "marker lists a pre-existing failure with no bead"
    return None
