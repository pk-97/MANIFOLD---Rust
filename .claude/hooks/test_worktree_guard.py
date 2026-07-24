#!/usr/bin/env python3
"""
Standalone test runner for worktree-guard.py. Invokes the hook's main() with
synthetic payloads built from the module's own _PROJECT_DIR, so the checks stay
correct wherever the repo lives. Never spawns a subprocess or writes files.

Run: python3 .claude/hooks/test_worktree_guard.py
"""
import importlib.util
import io
import json
import sys
from contextlib import redirect_stdout
from pathlib import Path

HOOK_PATH = Path(__file__).resolve().parent / "worktree-guard.py"

spec = importlib.util.spec_from_file_location("worktree_guard", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

PROJ = hook._PROJECT_DIR
WT = hook._WORKTREES_DIR

PASS = []
FAIL = []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def run_hook(payload):
    """Feed `payload` to hook.main(); return the deny reason string, or None if
    the hook stayed silent (allowed)."""
    stdin = io.StringIO(json.dumps(payload))
    stdout = io.StringIO()
    orig_stdin, orig_stdout = sys.stdin, sys.stdout
    sys.stdin = stdin
    try:
        with redirect_stdout(stdout):
            hook.main()
    finally:
        sys.stdin, sys.stdout = orig_stdin, orig_stdout
    out = stdout.getvalue().strip()
    if not out:
        return None
    return json.loads(out)["hookSpecificOutput"]["permissionDecisionReason"]


def edit(file_path, cwd=None, tool="Edit"):
    ti = {"file_path": file_path, "old_string": "a", "new_string": "b"}
    if tool == "Write":
        ti = {"file_path": file_path, "content": "x"}
    elif tool == "MultiEdit":
        ti = {"file_path": file_path, "edits": [{"old_string": "a", "new_string": "b"}]}
    p = {"tool_name": tool, "tool_input": ti}
    if cwd is not None:
        p["cwd"] = cwd
    return p


def test_main_source_absolute_denies():
    r = run_hook(edit(str(PROJ / "crates/manifold-app/src/ui_root.rs")))
    check("absolute main-checkout source -> deny", r is not None, r)


def test_main_source_relative_denies():
    r = run_hook(edit("crates/manifold-app/src/ui_root.rs", cwd=str(PROJ)))
    check("relative source resolved against main cwd -> deny", r is not None, r)


def test_docs_deny():
    r = run_hook(edit(str(PROJ / "docs/BUG_BACKLOG.md"), tool="Write"))
    check("docs/BUG_BACKLOG.md -> allow (doc fast path, 2026-07-20)", r is None, r)


def test_docs_design_doc_now_allowed():
    r = run_hook(edit(str(PROJ / "docs/VULKAN_BACKEND_DESIGN.md")))
    check("docs/*_DESIGN.md -> allow (fast path widened, 2026-07-24)", r is None, r)


def test_docs_non_md_still_denies():
    r = run_hook(edit(str(PROJ / "docs/diagram.png"), tool="Write"))
    check("docs non-markdown -> deny", r is not None, r)


def test_cargo_toml_denies():
    r = run_hook(edit(str(PROJ / "Cargo.toml")))
    check("main-checkout Cargo.toml -> deny", r is not None, r)


def test_tooling_hook_allowed():
    r = run_hook(edit(str(PROJ / ".claude/hooks/whatever.py")))
    check(".claude/ tooling file -> allow", r is None, r)


def test_tooling_settings_allowed():
    r = run_hook(edit(str(PROJ / ".claude/settings.json")))
    check(".claude/settings.json -> allow", r is None, r)


def test_daemon_file_allowed():
    r = run_hook(edit(str(PROJ / ".claude/daemon/moves.md"), tool="Write"))
    check(".claude/daemon file -> allow", r is None, r)


def test_worktree_file_allowed():
    r = run_hook(edit(str(WT / "fix-foo/crates/manifold-app/src/ui_root.rs")))
    check("file inside a worktree -> allow", r is None, r)


def test_relative_from_worktree_cwd_allowed():
    # Session working inside a worktree; relative path resolves under it.
    r = run_hook(edit("crates/manifold-app/src/ui_root.rs", cwd=str(WT / "fix-foo")))
    check("relative source, cwd inside worktree -> allow", r is None, r)


def test_outside_repo_allowed():
    r = run_hook(edit("/tmp/scratch.rs"))
    check("path outside the repo -> allow", r is None, r)


def test_home_memory_allowed():
    r = run_hook(edit(str(Path.home() / ".claude/projects/x/memory/foo.md"), tool="Write"))
    check("repo memory (outside project dir) -> allow", r is None, r)


def test_non_edit_tool_silent():
    r = run_hook({"tool_name": "Bash", "tool_input": {"command": "ls"}})
    check("non-edit tool -> silent", r is None, r)


def test_missing_file_path_silent():
    r = run_hook({"tool_name": "Edit", "tool_input": {}})
    check("missing file_path -> silent (fail open)", r is None, r)


def test_malformed_payload_silent():
    stdin = io.StringIO("not json")
    stdout = io.StringIO()
    o_in, o_out = sys.stdin, sys.stdout
    sys.stdin = stdin
    try:
        with redirect_stdout(stdout):
            rc = hook.main()
    finally:
        sys.stdin, sys.stdout = o_in, o_out
    check("malformed stdin -> silent, rc 0", rc == 0 and stdout.getvalue().strip() == "")


def test_multiedit_main_denies():
    r = run_hook(edit(str(PROJ / "crates/manifold-ui/src/tree.rs"), tool="MultiEdit"))
    check("MultiEdit on main source -> deny", r is not None, r)


def test_deny_reason_names_worktree_command():
    r = run_hook(edit(str(PROJ / "crates/foo/src/lib.rs")))
    check("deny reason includes the worktree command", r and "git worktree add" in r, r)


def test_merge_conflicted_file_allowed():
    target = PROJ / "docs/BUG_BACKLOG.md"
    orig = hook.merge_conflict_paths
    hook.merge_conflict_paths = lambda: {target.resolve()}
    try:
        r = run_hook(edit(str(target)))
    finally:
        hook.merge_conflict_paths = orig
    check("unmerged file during live merge -> allow", r is None, r)


def test_merge_nonconflicted_file_still_denies():
    orig = hook.merge_conflict_paths
    hook.merge_conflict_paths = lambda: {(PROJ / "crates/other.rs").resolve()}
    try:
        r = run_hook(edit(str(PROJ / "crates/manifold-app/src/ui_root.rs")))
    finally:
        hook.merge_conflict_paths = orig
    check("non-conflicted file during live merge -> deny", r is not None, r)


def test_no_merge_denies():
    orig = hook.merge_conflict_paths
    hook.merge_conflict_paths = lambda: set()
    try:
        r = run_hook(edit(str(PROJ / "crates/manifold-app/src/ui_root.rs")))
    finally:
        hook.merge_conflict_paths = orig
    check("no merge in progress -> deny unchanged", r is not None, r)


def main():
    for fn in [
        test_main_source_absolute_denies,
        test_main_source_relative_denies,
        test_docs_deny,
        test_docs_design_doc_now_allowed,
        test_docs_non_md_still_denies,
        test_cargo_toml_denies,
        test_tooling_hook_allowed,
        test_tooling_settings_allowed,
        test_daemon_file_allowed,
        test_worktree_file_allowed,
        test_relative_from_worktree_cwd_allowed,
        test_outside_repo_allowed,
        test_home_memory_allowed,
        test_non_edit_tool_silent,
        test_missing_file_path_silent,
        test_malformed_payload_silent,
        test_multiedit_main_denies,
        test_deny_reason_names_worktree_command,
        test_merge_conflicted_file_allowed,
        test_merge_nonconflicted_file_still_denies,
        test_no_merge_denies,
    ]:
        fn()

    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
