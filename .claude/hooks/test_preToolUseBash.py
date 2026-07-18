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


# --- worktree-ring guard (2026-07-15: pool capped at 6 slots; raw
# `git worktree add` denied in every mode so the ring can't be bypassed) ---

def test_worktree_add_denied_all_modes():
    for mode in ("default", "auto", "bypassPermissions"):
        out = run_hook_main({
            "tool_input": {"command": "git worktree add -b feat/x .claude/worktrees/x HEAD"},
            "cwd": MAIN_CWD,
            "permission_mode": mode,
        })
        check(f"worktree add ({mode} mode) -> deny", '"deny"' in out and "slot ring" in out, out)


def test_worktree_add_in_compound_denied():
    out = run_hook_main({
        "tool_input": {"command": "git fetch origin main && git worktree add wt feat/y"},
        "cwd": MAIN_CWD,
        "permission_mode": "auto",
    })
    check("worktree add inside compound -> deny", '"deny"' in out, out)


def test_worktree_read_and_remove_unaffected():
    for cmd in ("git worktree list", "git worktree prune",
                "git worktree remove --force .claude/worktrees/slot-0"):
        out = run_hook_main({
            "tool_input": {"command": cmd},
            "cwd": MAIN_CWD,
            "permission_mode": "default",
        })
        check(f"`{cmd}` -> not denied", '"deny"' not in out, out)


PIPEY_CMD = "python3 scripts/frob.py | tee /Users/peterkiemann/out.txt"


def test_cc_fleet_lane_workflow_preapproved():
    # K3 lane workflow (2026-07-18 routing directive): spawn/poll auto-allow.
    check(
        "cc-fleet subagent spawn is pre-approved",
        hook.is_preapproved_command(
            "cc-fleet subagent kimi-code --prompt-file /tmp/b.md --background"
        ),
    )
    check(
        "ccf alias + status polling is pre-approved",
        hook.is_preapproved_command("ccf subagent-status abc123"),
    )
    # Provider mutation and key material still prompt.
    check(
        "cc-fleet add is NOT pre-approved",
        not hook.is_preapproved_command("cc-fleet add evil --api-key-stdin"),
    )
    check(
        "cc-fleet keyget is NOT pre-approved",
        not hook.is_preapproved_command("cc-fleet keyget kimi-code"),
    )


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


def test_rg_replace_bundled_rn_fires():
    reason = hook.rg_replace_lint("rg -rn pattern file")
    check("rg -rn (bundled) -> warns", reason is not None, reason)


def test_rg_replace_bundled_rl_fires():
    reason = hook.rg_replace_lint("rg -rl pattern")
    check("rg -rl (bundled) -> warns", reason is not None, reason)


def test_rg_replace_standalone_fires():
    reason = hook.rg_replace_lint("rg -r 'x' file")
    check("rg -r 'x' (standalone) -> warns", reason is not None, reason)


def test_rg_plain_n_does_not_fire():
    reason = hook.rg_replace_lint("rg -n pattern file")
    check("rg -n (no r) -> no warning", reason is None, reason)


def test_rg_plain_no_flags_does_not_fire():
    reason = hook.rg_replace_lint("rg pattern file")
    check("rg pattern file (no flags) -> no warning", reason is None, reason)


def test_rg_replace_non_rg_command_does_not_fire():
    reason = hook.rg_replace_lint("grep -rn pattern file")
    check("non-rg command with -rn -> no warning", reason is None, reason)


def test_masked_exit_status_pipe_then_echo_dollar_status_fires():
    reason = hook.masked_exit_status_lint("cargo test | rg FAIL; echo exit: $?")
    check("cargo test | rg ...; echo $? -> warns", reason is not None, reason)


def test_masked_exit_status_and_chain_does_not_fire():
    reason = hook.masked_exit_status_lint("cargo test -p foo --lib && cargo clippy")
    check("cargo test && cargo clippy (no pipe-into-filter) -> no warning", reason is None, reason)


def test_masked_exit_status_pytest_head_echo_fires():
    reason = hook.masked_exit_status_lint("pytest | head -20; echo GATE_DONE")
    check("pytest | head ...; echo GATE_DONE -> warns", reason is not None, reason)


def test_masked_exit_status_no_trailing_echo_does_not_fire():
    reason = hook.masked_exit_status_lint("cargo test | rg FAIL")
    check("cargo test | rg FAIL alone (no trailing echo/$?) -> no warning", reason is None, reason)


def test_masked_exit_status_non_runner_head_does_not_fire():
    reason = hook.masked_exit_status_lint("rg foo | head")
    check("rg foo | head (no test runner) -> no warning", reason is None, reason)


def test_trailing_comment_swallow_fires():
    reason = hook.trailing_comment_swallow_lint("rg foo #grep-ok && echo done-grading")
    check("comment followed by && -> warns", reason is not None, reason)
    check("warning names the swallowed text", reason and "done-grading" in reason, reason)


def test_trailing_comment_no_operator_does_not_fire():
    reason = hook.trailing_comment_swallow_lint("rg foo # just a note")
    check("comment with no trailing operator -> no warning", reason is None, reason)


def test_trailing_comment_no_hash_does_not_fire():
    reason = hook.trailing_comment_swallow_lint("rg foo")
    check("no `#` at all -> no warning", reason is None, reason)


def test_trailing_comment_hash_inside_quotes_does_not_fire():
    reason = hook.trailing_comment_swallow_lint('echo "price: #1" && echo done')
    check("`#` inside quoted string -> no warning", reason is None, reason)


def test_compound_landing_merge_unverified_denies():
    orig = hook._current_branch
    hook._current_branch = lambda cwd: "main"
    try:
        cmd = "git fetch && git merge origin/main && git merge --no-ff feat/x && git push"
        reason = hook.detect_unverified_compound_landing_merge(cmd, MAIN_CWD)
        check("unverified compound landing merge -> denies", reason is not None, reason)
    finally:
        hook._current_branch = orig


def test_compound_landing_merge_verified_in_between_unaffected():
    orig = hook._current_branch
    hook._current_branch = lambda cwd: "main"
    try:
        cmd = ("git fetch && git merge origin/main && git branch --show-current "
               "&& git merge --no-ff feat/x && git push")
        reason = hook.detect_unverified_compound_landing_merge(cmd, MAIN_CWD)
        check("verify segment in between -> unaffected", reason is None, reason)
    finally:
        hook._current_branch = orig


def test_single_landing_merge_not_compound_unaffected():
    orig = hook._current_branch
    hook._current_branch = lambda cwd: "main"
    try:
        reason = hook.detect_unverified_compound_landing_merge("git merge --no-ff feat/x", MAIN_CWD)
        check("single (non-compound) landing merge -> unaffected", reason is None, reason)
    finally:
        hook._current_branch = orig


def test_compound_landing_merge_worktree_unaffected():
    orig = hook._current_branch
    hook._current_branch = lambda cwd: "main"
    try:
        cmd = (f'git -C "{WORKTREE_CWD}" fetch && git -C "{WORKTREE_CWD}" merge origin/main '
               f'&& git -C "{WORKTREE_CWD}" merge --no-ff feat/x && git -C "{WORKTREE_CWD}" push')
        reason = hook.detect_unverified_compound_landing_merge(cmd, MAIN_CWD)
        check("compound targeting a worktree dir -> unaffected", reason is None, reason)
    finally:
        hook._current_branch = orig


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
    test_cc_fleet_lane_workflow_preapproved()
    test_pipe_deny_active_in_default_mode()
    test_pipe_deny_skipped_in_auto_mode()
    test_pipe_deny_active_when_mode_missing()
    test_landing_ask_survives_auto_mode()

    test_rg_replace_bundled_rn_fires()
    test_rg_replace_bundled_rl_fires()
    test_rg_replace_standalone_fires()
    test_rg_plain_n_does_not_fire()
    test_rg_plain_no_flags_does_not_fire()
    test_rg_replace_non_rg_command_does_not_fire()

    test_masked_exit_status_pipe_then_echo_dollar_status_fires()
    test_masked_exit_status_and_chain_does_not_fire()
    test_masked_exit_status_pytest_head_echo_fires()
    test_masked_exit_status_no_trailing_echo_does_not_fire()
    test_masked_exit_status_non_runner_head_does_not_fire()

    test_trailing_comment_swallow_fires()
    test_trailing_comment_no_operator_does_not_fire()
    test_trailing_comment_no_hash_does_not_fire()
    test_trailing_comment_hash_inside_quotes_does_not_fire()

    test_compound_landing_merge_unverified_denies()
    test_compound_landing_merge_verified_in_between_unaffected()
    test_single_landing_merge_not_compound_unaffected()
    test_compound_landing_merge_worktree_unaffected()

    test_worktree_add_denied_all_modes()
    test_worktree_add_in_compound_denied()
    test_worktree_read_and_remove_unaffected()

    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
