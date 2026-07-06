#!/usr/bin/env python3
"""
Standalone test runner for lsp-nudge.py's decide() — synthetic commands only,
never a real hook subprocess against a live session (per DESIGN.md).

Run: python3 .claude/hooks/test_lsp_nudge.py
"""
import importlib.util
from pathlib import Path

HOOK_PATH = Path(__file__).resolve().parent / "lsp-nudge.py"

spec = importlib.util.spec_from_file_location("lsp_nudge", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

PASS = []
FAIL = []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    if not cond:
        print(f"FAIL: {name} {detail}")


def denies(cmd):
    return hook.decide(cmd) is not None


# --- Should fire: workspace/directory symbol sweeps -------------------------
check("def sweep, no path", denies('rg "fn sync_clips_to_time"'))
check("def sweep, crates dir", denies("rg 'struct PlaybackEngine' crates/"))
check("impl sweep", denies("rg 'impl Command for' crates/manifold-editing/src/"))
check("glob does not exempt", denies("rg 'struct Layer' -g '*.rs' crates/"))
check("enum sweep", denies('grep -rn "enum ContentCommand" crates/'))

# --- Should pass: explicit single-file target = reading intent ---------------
check(
    "quoted path with spaces (the screenshot case)",
    not denies('grep -n "struct UIStyle" -A 40 "/Users/x/MANIFOLD - Rust/crates/manifold-ui/src/node.rs"'),
)
check("relative .rs file", not denies("rg -n 'pub struct Layer' crates/manifold-core/src/layer.rs"))
check("two explicit files", not denies("rg 'fn tick' a.rs b.rs"))
check("markdown file", not denies("rg 'struct Foo' docs/NODE_CATALOG.md"))
check("unbalanced quotes fall back safely", not denies('rg "struct Foo crates/manifold-core/src/layer.rs'))

# --- Should pass: not symbol-shaped at all -----------------------------------
check("bare keyword", not denies('rg "trait" crates/'))
check("plain identifier", not denies("rg sync_clips_to_time crates/"))
check("string search", not denies('rg "purpose: \\"" crates/manifold-renderer/'))
check("bypass tag", not denies('rg "struct Layer" crates/ #grep-ok'))
check("no searcher", not denies("cargo test -p manifold-core --lib"))
check("empty", not denies(""))

# --- #grep-ok comment-swallowing footgun -------------------------------------
check(
    "marker at true end of line — allowed",
    not denies('rg "struct Layer" crates/ #grep-ok'),
)
check(
    "marker with only trailing whitespace — allowed",
    not denies('rg "struct Layer" crates/ #grep-ok   '),
)
check(
    "marker followed by ; cmd on same line — blocked (would be silently dropped)",
    denies("rg 'struct Layer' crates/ #grep-ok; printf 'x' >> live_grades.session.jsonl"),
)
check(
    "marker followed by && cmd on same line — blocked",
    denies("rg 'struct Layer' crates/ #grep-ok && echo done"),
)
check(
    "marker immediately followed by non-whitespace — blocked",
    denies("rg 'struct Layer' crates/ #grep-ok-ish"),
)
check(
    "marker inside a single-quoted string — not a real marker, symbol shape still fires",
    denies("rg 'struct Layer #grep-ok' crates/"),
)
check(
    "marker inside a double-quoted string — not a real marker, symbol shape still fires",
    denies('rg "struct Layer #grep-ok" crates/'),
)
check(
    "marker on an earlier line, real command on next line — allowed (new statement)",
    not denies('rg "struct Layer" crates/ #grep-ok\necho done'),
)

print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
raise SystemExit(1 if FAIL else 0)
