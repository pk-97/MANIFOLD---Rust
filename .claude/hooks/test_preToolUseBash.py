#!/usr/bin/env python3
"""
Standalone test runner for preToolUseBash.py's shared-checkout guard
(.claude/GIT_TREE_DISCIPLINE.md §1). Invokes the hook's functions directly
with synthetic pidfiles under a temp verdicts dir — never touches the real
`.claude/daemon/verdicts/` or spawns a real hook subprocess against a live
session (per DESIGN.md: "test hooks by invoking them directly with synthetic
stdin, not by observing your own session").

Run: python3 .claude/hooks/test_preToolUseBash.py
"""
import importlib.util
import io
import json
import os
import sys
import tempfile
from pathlib import Path

HOOK_PATH = Path(__file__).resolve().parent / "preToolUseBash.py"

spec = importlib.util.spec_from_file_location("preToolUseBash", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

PASS = []
FAIL = []


def check(name, cond, detail=""):
    if cond:
        PASS.append(name)
    else:
        FAIL.append((name, detail))


def with_verdicts_dir(fn):
    """Run `fn(verdicts_dir)` with hook._VERDICTS_DIR patched to a scratch
    temp dir, restoring it afterward regardless of outcome."""
    orig = hook._VERDICTS_DIR
    with tempfile.TemporaryDirectory() as td:
        hook._VERDICTS_DIR = Path(td)
        try:
            fn(Path(td))
        finally:
            hook._VERDICTS_DIR = orig


def make_pidfile(verdicts_dir, session_id, pid_text):
    (verdicts_dir / f"{session_id}.pid").write_text(pid_text)


LIVE_PID = str(os.getpid())  # our own process is guaranteed alive
DEAD_PID = "999999"  # extremely unlikely to be a live pid on any dev box
MAIN_CWD = str(hook._PROJECT_DIR)
WORKTREE_CWD = str(hook._WORKTREES_DIR / "some-branch")


def test_bare_checkout_foreign_live():
    def run(vd):
        make_pidfile(vd, "other-session", LIVE_PID)
        reason = hook.shared_checkout_guard("git checkout main", "my-session", MAIN_CWD)
        check("bare checkout, foreign live pidfile -> ask", reason is not None, reason)
        check("ask reason names the foreign session", reason and "other-session" in reason, reason)
    with_verdicts_dir(run)


def test_bare_checkout_only_own_session():
    def run(vd):
        make_pidfile(vd, "my-session", LIVE_PID)
        reason = hook.shared_checkout_guard("git checkout main", "my-session", MAIN_CWD)
        check("bare checkout, only own-session pidfile -> no guard", reason is None, reason)
    with_verdicts_dir(run)


def test_bare_checkout_no_pidfiles():
    def run(vd):
        reason = hook.shared_checkout_guard("git checkout main", "my-session", MAIN_CWD)
        check("bare checkout, solo (no pidfiles) -> no guard", reason is None, reason)
    with_verdicts_dir(run)


def test_worktree_checkout_foreign_live():
    def run(vd):
        make_pidfile(vd, "other-session", LIVE_PID)
        # Quoted because the real repo path contains a space
        # ("MANIFOLD - Rust") -- this also exercises that shlex, not the
        # placeholder-collapsing `sanitize`, is what resolves -C's value.
        cmd = f'git -C "{hook._WORKTREES_DIR / "some-branch"}" checkout main'
        reason = hook.shared_checkout_guard(cmd, "my-session", MAIN_CWD)
        check("git -C worktree checkout, foreign live -> unaffected (no guard)", reason is None, reason)
    with_verdicts_dir(run)


def test_dead_pid_pidfile():
    def run(vd):
        make_pidfile(vd, "other-session", DEAD_PID)
        reason = hook.shared_checkout_guard("git checkout main", "my-session", MAIN_CWD)
        check("dead-pid foreign pidfile -> treated as absent, no guard", reason is None, reason)
    with_verdicts_dir(run)


def test_malformed_pidfile():
    def run(vd):
        make_pidfile(vd, "other-session", "not-a-pid")
        reason = hook.shared_checkout_guard("git checkout main", "my-session", MAIN_CWD)
        check("malformed pidfile -> treated as absent, no guard", reason is None, reason)
    with_verdicts_dir(run)


def test_switch_variant():
    def run(vd):
        make_pidfile(vd, "other-session", LIVE_PID)
        reason = hook.shared_checkout_guard("git switch feature-branch", "my-session", MAIN_CWD)
        check("git switch, foreign live -> ask", reason is not None, reason)
    with_verdicts_dir(run)


def test_merge_variant():
    def run(vd):
        make_pidfile(vd, "other-session", LIVE_PID)
        reason = hook.shared_checkout_guard("git merge feature-branch", "my-session", MAIN_CWD)
        check("git merge, foreign live -> ask", reason is not None, reason)
    with_verdicts_dir(run)


def test_checkout_bare_new_branch():
    def run(vd):
        make_pidfile(vd, "other-session", LIVE_PID)
        reason = hook.shared_checkout_guard("git checkout -b new-thing", "my-session", MAIN_CWD)
        check("git checkout -b, foreign live -> ask", reason is not None, reason)
    with_verdicts_dir(run)


def test_checkout_dashdash_paths_unchanged():
    def run(vd):
        make_pidfile(vd, "other-session", LIVE_PID)
        reason = hook.shared_checkout_guard("git checkout -- src/main.rs", "my-session", MAIN_CWD)
        check("git checkout -- <paths> (file restore) -> unaffected, no guard", reason is None, reason)
    with_verdicts_dir(run)


def test_solo_still_reaches_allow_path():
    """No pidfiles at all: guard stays out of the way AND job-1 pre-approval
    still fires for a plain `git checkout main` (regression guard on job 1).
    Deliberately calls the two functions directly rather than shelling out to
    a subprocess against the REAL verdicts dir — this session's own live
    pidfile (and any concurrent session's) would make an out-of-process,
    unpatched invocation observe genuine foreign daemons and correctly `ask`,
    which would misreport as a regression. Per DESIGN.md: test hooks with
    synthetic stdin, not by observing your own session."""
    def run(vd):
        cmd = "git checkout main"
        reason = hook.shared_checkout_guard(cmd, "my-session", MAIN_CWD)
        check("solo: guard step yields no ask", reason is None, reason)
        check("solo: job-1 pre-approval still allows", hook.is_preapproved_command(cmd))
    with_verdicts_dir(run)


def test_branch_force_main_asks():
    reason, context = hook.landing_protocol_guard("git branch -f main abc123", MAIN_CWD)
    check("branch -f main -> ask", reason is not None, reason)
    check("branch -f main -> no context", context is None, context)


def test_branch_force_main_worktree_unaffected():
    cmd = f'git -C "{WORKTREE_CWD}" branch -f main abc123'
    reason, context = hook.landing_protocol_guard(cmd, MAIN_CWD)
    check("branch -f main in worktree -> unaffected", reason is None and context is None, (reason, context))


def test_branch_force_non_main_unaffected():
    reason, context = hook.landing_protocol_guard("git branch -f other-branch abc123", MAIN_CWD)
    check("branch -f other-branch -> unaffected", reason is None and context is None, (reason, context))


def test_force_push_explicit_main_asks():
    reason, context = hook.landing_protocol_guard("git push --force origin main", MAIN_CWD)
    check("push --force origin main -> ask", reason is not None, reason)
    check("push --force origin main -> no context", context is None, context)


def test_force_push_refspec_main_asks():
    reason, context = hook.landing_protocol_guard("git push -f origin abc123:main", MAIN_CWD)
    check("push -f origin <sha>:main -> ask", reason is not None, reason)


def test_force_push_non_main_unaffected():
    reason, context = hook.landing_protocol_guard("git push --force origin some-branch", MAIN_CWD)
    check("push --force origin some-branch -> unaffected", reason is None and context is None, (reason, context))


def test_nonforce_push_explicit_main_reminds():
    reason, context = hook.landing_protocol_guard("git push origin main", MAIN_CWD)
    check("push origin main (no force) -> no ask", reason is None, reason)
    check("push origin main (no force) -> reminder attached", context is not None, context)


def test_nonforce_push_non_main_unaffected():
    reason, context = hook.landing_protocol_guard("git push origin some-branch", MAIN_CWD)
    check("push origin some-branch -> unaffected", reason is None and context is None, (reason, context))


def test_push_worktree_unaffected():
    cmd = f'git -C "{WORKTREE_CWD}" push --force origin main'
    reason, context = hook.landing_protocol_guard(cmd, MAIN_CWD)
    check("force-push-to-main from a worktree cwd -> unaffected", reason is None and context is None, (reason, context))


def test_merge_while_on_main_reminds():
    orig = hook._current_branch
    hook._current_branch = lambda cwd: "main"
    try:
        reason, context = hook.landing_protocol_guard("git merge feature-branch", MAIN_CWD)
        check("merge while on main -> no ask", reason is None, reason)
        check("merge while on main -> reminder attached", context is not None, context)
    finally:
        hook._current_branch = orig


def test_merge_while_on_other_branch_unaffected():
    orig = hook._current_branch
    hook._current_branch = lambda cwd: "feature-branch"
    try:
        reason, context = hook.landing_protocol_guard("git merge other-thing", MAIN_CWD)
        check("merge while on non-main branch -> unaffected", reason is None and context is None, (reason, context))
    finally:
        hook._current_branch = orig


def test_bare_push_on_main_branch_reminds():
    """No explicit refspec at all: falls back to checking the current branch."""
    orig = hook._current_branch
    hook._current_branch = lambda cwd: "main"
    try:
        reason, context = hook.landing_protocol_guard("git push", MAIN_CWD)
        check("bare push while on main -> reminder attached", context is not None, context)
    finally:
        hook._current_branch = orig


def run_hook_main(payload):
    """Drive hook.main() end-to-end with synthetic stdin, returning what it
    wrote to stdout ("" = no decision, fell through to the permission
    system). Wrapped in a patched verdicts dir so the shared-checkout guard
    never observes this session's real pidfile."""
    result = {}

    def run(vd):
        orig_in, orig_out = sys.stdin, sys.stdout
        sys.stdin = io.StringIO(json.dumps(payload))
        sys.stdout = io.StringIO()
        try:
            hook.main()
            result["out"] = sys.stdout.getvalue()
        finally:
            sys.stdin, sys.stdout = orig_in, orig_out

    with_verdicts_dir(run)
    return result["out"]


PIPEY_CMD = "python3 scripts/frob.py | tee /Users/peterkiemann/out.txt"


def test_pipe_deny_active_in_default_mode():
    check("pipey test cmd is not pre-approved", not hook.is_preapproved_command(PIPEY_CMD))
    out = run_hook_main({
        "tool_input": {"command": PIPEY_CMD},
        "cwd": MAIN_CWD,
        "permission_mode": "default",
    })
    check("default mode: non-pre-approved pipe -> deny", '"deny"' in out, out)


def test_pipe_deny_skipped_in_auto_mode():
    for mode in ("auto", "bypassPermissions"):
        out = run_hook_main({
            "tool_input": {"command": PIPEY_CMD},
            "cwd": MAIN_CWD,
            "permission_mode": mode,
        })
        check(f"{mode} mode: non-pre-approved pipe -> no decision", out == "", out)


def test_pipe_deny_active_when_mode_missing():
    out = run_hook_main({
        "tool_input": {"command": PIPEY_CMD},
        "cwd": MAIN_CWD,
    })
    check("missing permission_mode: deny stays (safe default)", '"deny"' in out, out)


def test_landing_ask_survives_auto_mode():
    out = run_hook_main({
        "tool_input": {"command": "git push --force origin main"},
        "cwd": MAIN_CWD,
        "permission_mode": "auto",
    })
    check("auto mode: force-push to main still asks", '"ask"' in out, out)


def main():
    test_bare_checkout_foreign_live()
    test_bare_checkout_only_own_session()
    test_bare_checkout_no_pidfiles()
    test_worktree_checkout_foreign_live()
    test_dead_pid_pidfile()
    test_malformed_pidfile()
    test_switch_variant()
    test_merge_variant()
    test_checkout_bare_new_branch()
    test_checkout_dashdash_paths_unchanged()
    test_solo_still_reaches_allow_path()
    test_branch_force_main_asks()
    test_branch_force_main_worktree_unaffected()
    test_branch_force_non_main_unaffected()
    test_force_push_explicit_main_asks()
    test_force_push_refspec_main_asks()
    test_force_push_non_main_unaffected()
    test_nonforce_push_explicit_main_reminds()
    test_nonforce_push_non_main_unaffected()
    test_push_worktree_unaffected()
    test_merge_while_on_main_reminds()
    test_merge_while_on_other_branch_unaffected()
    test_bare_push_on_main_branch_reminds()
    test_pipe_deny_active_in_default_mode()
    test_pipe_deny_skipped_in_auto_mode()
    test_pipe_deny_active_when_mode_missing()
    test_landing_ask_survives_auto_mode()

    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
