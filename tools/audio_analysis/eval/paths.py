"""Shared eval-data store: ONE gitignored data root in the MAIN checkout.

Fetched dataset audio (eval/data/*, ~1GB and growing) was package-relative
before 2026-07-18, i.e. it lived inside whichever worktree slot ran the
fetch -- and the slot ring wipes and reuses slots, so every re-acquire
re-downloaded everything (the bake-off re-run had to re-pull E-GMD AND
babyslakh). Peter's call: store the data once so tuning runs are rapid.

All eval data resolution goes through DATA_ROOT below: the MAIN checkout's
tools/audio_analysis/eval/data, regardless of which worktree the code runs
from. Worktrees live at <main>/.claude/worktrees/<slot>/..., so the main
root is recovered by stripping that infix; running from the main checkout
(or any non-slot clone) resolves to the checkout itself. Same pattern as
the BundledRuntime symlink precedent (eval/../.gitignore) but with no
per-slot setup step -- code, not convention.
"""
from pathlib import Path

AUDIO_ANALYSIS_ROOT = Path(__file__).resolve().parents[1]
_REPO_ROOT = Path(__file__).resolve().parents[3]


def _main_repo_root() -> Path:
    parts = _REPO_ROOT.parts
    if ".claude" in parts:
        i = parts.index(".claude")
        # Only strip a genuine <main>/.claude/worktrees/<slot> infix, not an
        # unrelated ".claude" path component.
        if len(parts) > i + 1 and parts[i + 1] == "worktrees":
            return Path(*parts[:i])
    return _REPO_ROOT


MAIN_REPO_ROOT = _main_repo_root()
MAIN_AUDIO_ANALYSIS_ROOT = MAIN_REPO_ROOT / "tools" / "audio_analysis"
DATA_ROOT = MAIN_AUDIO_ANALYSIS_ROOT / "eval" / "data"
