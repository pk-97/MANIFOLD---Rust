#!/usr/bin/env python3
"""
Standalone test runner for preToolUseBash.py's guards (landing-protocol,
worktree-ring, pre-land verdict coverage, compound-landing-merge, shell
lints). Invokes the hook's functions directly with synthetic stdin — never
spawns a real hook subprocess against a live session (per DESIGN.md: "test
hooks by invoking them directly with synthetic stdin, not by observing your
own session").

Run: python3 .claude/hooks/test_preToolUseBash.py
"""
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest.mock
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


def with_orch_verdicts_dir(fn):
    """Run `fn(orch_verdicts_dir)` with hook._ORCH_VERDICTS_DIR patched to a
    scratch temp dir, restoring it afterward regardless of outcome."""
    orig = hook._ORCH_VERDICTS_DIR
    with tempfile.TemporaryDirectory() as td:
        hook._ORCH_VERDICTS_DIR = Path(td)
        try:
            fn(Path(td))
        finally:
            hook._ORCH_VERDICTS_DIR = orig


MAIN_CWD = str(hook._PROJECT_DIR)
WORKTREE_CWD = str(hook._WORKTREES_DIR / "some-branch")


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
    system)."""
    orig_in, orig_out = sys.stdin, sys.stdout
    sys.stdin = io.StringIO(json.dumps(payload))
    sys.stdout = io.StringIO()
    try:
        hook.main()
        return sys.stdout.getvalue()
    finally:
        sys.stdin, sys.stdout = orig_in, orig_out


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
    for cmd in ("git worktree list", "git worktree prune"):
        out = run_hook_main({
            "tool_input": {"command": cmd},
            "cwd": MAIN_CWD,
            "permission_mode": "default",
        })
        check(f"`{cmd}` -> not denied", '"deny"' not in out, out)
    # `worktree remove` denied since 25056b1d — releases go through the slot ring
    out = run_hook_main({
        "tool_input": {"command": "git worktree remove --force .claude/worktrees/slot-0"},
        "cwd": MAIN_CWD,
        "permission_mode": "default",
    })
    check("`git worktree remove --force` -> deny (slot ring)",
          '"deny"' in out and "slot ring" in out, out)


# ---------------------------------------------------------------------------
# Pre-land verdict-coverage guard (I1) — merge_verdict_guard
# ---------------------------------------------------------------------------

def _make_verdict(task_id, kind="gate", pass_value=True):
    """Create a schema-1 verdict dict for testing."""
    return {
        "schema": 1, "task": task_id, "phase": "per-lane", "brief": "",
        "branch": "lane/test", "commit": "abc123",
        "gates": [] if kind == "no-gate" else [{"cmd": "true", "exit": 0, "duration_s": 0.1, "tail": ""}],
        "scope": {"files_changed": [], "in_scope": True},
        "pass": pass_value, "kind": kind, "reason": None if kind == "gate" else "test bypass",
        "runner": "gate_runner.py@lead", "ts": "2026-07-25T12:00:00Z",
    }


def _setup_mock_run(diff_output="crates/foo/src/lib.rs\n", log_output="feat\n\nBUG-abc123\n"):
    """Create a configured subprocess.run mock returning specific diff/log."""
    mock_run = unittest.mock.MagicMock()

    def side_effect(cmd, *args, **kwargs):
        result = unittest.mock.MagicMock()
        result.returncode = 0
        cmd_str = " ".join(cmd) if isinstance(cmd, list) else cmd
        if "diff" in cmd_str:
            result.stdout = diff_output
        elif "log" in cmd_str:
            result.stdout = log_output
        else:
            result.stdout = ""
        return result

    mock_run.side_effect = side_effect
    return mock_run


def test_merge_denied_missing_verdict():
    """Merge blocked when source branch BUG- ids lack passing verdicts."""
    def run(orch_vd):
        mock_run = _setup_mock_run(
            diff_output="crates/foo/src/lib.rs\n",
            log_output="feat: add X\n\nBUG-abc123\n",
        )
        orig_branch = hook._current_branch
        hook._current_branch = lambda cwd: "main"
        mock_patcher = unittest.mock.patch.object(hook.subprocess, 'run', mock_run)
        mock_patcher.start()
        try:
            reason, context = hook.merge_verdict_guard(
                "git merge --no-ff lane/feat-x", MAIN_CWD
            )
            check("merge denied when verdict missing", reason is not None, reason)
            check("deny names missing task", reason and "BUG-abc123" in reason, reason)
            check("deny mentions gate_runner no-gate", reason and "no-gate" in reason, reason)
        finally:
            mock_patcher.stop()
            hook._current_branch = orig_branch
    with_orch_verdicts_dir(run)


def test_merge_passes_with_gate_verdict():
    """Merge passes when a passing gate verdict exists in the trail."""
    def run(orch_vd):
        # Write a passing gate verdict
        vpath = orch_vd / "BUG-abc123.jsonl"
        vpath.write_text(json.dumps(_make_verdict("BUG-abc123", "gate", True)) + "\n")

        mock_run = _setup_mock_run(
            diff_output="crates/foo/src/lib.rs\n",
            log_output="feat: add X\n\nBUG-abc123\n",
        )
        orig_branch = hook._current_branch
        hook._current_branch = lambda cwd: "main"
        mock_patcher = unittest.mock.patch.object(hook.subprocess, 'run', mock_run)
        mock_patcher.start()
        try:
            reason, context = hook.merge_verdict_guard(
                "git merge --no-ff lane/feat-x", MAIN_CWD
            )
            check("merge passes with gate verdict", reason is None, reason)
            check("context confirms passing", context and "passing" in context, context)
        finally:
            mock_patcher.stop()
            hook._current_branch = orig_branch
    with_orch_verdicts_dir(run)


def test_merge_passes_with_no_gate_verdict():
    """Merge passes when a no-gate verdict exists in the trail."""
    def run(orch_vd):
        # Write a no-gate verdict
        vpath = orch_vd / "BUG-abc123.jsonl"
        vpath.write_text(json.dumps(_make_verdict("BUG-abc123", "no-gate", True)) + "\n")

        mock_run = _setup_mock_run(
            diff_output="crates/foo/src/lib.rs\n",
            log_output="feat: add X\n\nBUG-abc123\n",
        )
        orig_branch = hook._current_branch
        hook._current_branch = lambda cwd: "main"
        mock_patcher = unittest.mock.patch.object(hook.subprocess, 'run', mock_run)
        mock_patcher.start()
        try:
            reason, context = hook.merge_verdict_guard(
                "git merge --no-ff lane/feat-x", MAIN_CWD
            )
            check("merge passes with no-gate verdict", reason is None, reason)
            check("context confirms passing", context and "passing" in context, context)
        finally:
            mock_patcher.stop()
            hook._current_branch = orig_branch
    with_orch_verdicts_dir(run)


def test_merge_passes_no_bug_ids():
    """Merge passes when merged branch has no BUG- ids in log."""
    def run(orch_vd):
        mock_run = _setup_mock_run(
            diff_output="crates/foo/src/lib.rs\n",
            log_output="feat: add X\n\nNo bug ids here.\n",
        )
        orig_branch = hook._current_branch
        hook._current_branch = lambda cwd: "main"
        mock_patcher = unittest.mock.patch.object(hook.subprocess, 'run', mock_run)
        mock_patcher.start()
        try:
            reason, context = hook.merge_verdict_guard(
                "git merge --no-ff lane/infra-fix", MAIN_CWD
            )
            check("merge passes with no BUG- ids", reason is None, reason)
            check("context mentions pre-trail", context and "pre-trail" in context, context)
        finally:
            mock_patcher.stop()
            hook._current_branch = orig_branch
    with_orch_verdicts_dir(run)


def test_merge_passes_docs_only():
    """Docs-only merge passes without verdict check."""
    def run(orch_vd):
        mock_run = _setup_mock_run(
            diff_output="docs/GATE_RUNTIME_DESIGN.md\n",
            log_output="",  # never reached
        )
        orig_branch = hook._current_branch
        hook._current_branch = lambda cwd: "main"
        mock_patcher = unittest.mock.patch.object(hook.subprocess, 'run', mock_run)
        mock_patcher.start()
        try:
            reason, context = hook.merge_verdict_guard(
                "git merge --no-ff lane/doc-fix", MAIN_CWD
            )
            check("docs-only merge passes", reason is None, reason)
            check("context names docs-only", context and "Docs-only" in context, context)
        finally:
            mock_patcher.stop()
            hook._current_branch = orig_branch
    with_orch_verdicts_dir(run)


PIPEY_CMD = "python3 scripts/frob.py | tee /Users/peterkiemann/out.txt"


def test_cc_fleet_lane_workflow_preapproved():
    # K3 lane workflow (2026-07-18 routing directive): spawn/poll auto-allow.
    check(
        "cc-fleet subagent spawn is pre-approved",
        hook.is_preapproved_command(
            "cc-fleet subagent kimi --prompt-file /tmp/b.md --background"
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
        not hook.is_preapproved_command("cc-fleet keyget kimi"),
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


def test_cd_guard():
    g = hook.persistent_cd_guard
    slot = str(hook._WORKTREES_DIR / "slot-1")

    # The 2026-07-27 incident shape: no-op cd into a worktree
    check("cd worktree + true -> deny",
          g(f'cd "{slot}/scripts" 2>/dev/null; true', MAIN_CWD) is not None)
    # Bare cd (lands in $HOME) -> deny
    check("bare cd -> deny", g("cd", MAIN_CWD) is not None)
    check("cd /tmp -> deny", g("cd /tmp && ls", MAIN_CWD) is not None)
    check("cd - -> deny", g("cd -", MAIN_CWD) is not None)
    # Recovery moves allowed
    check("cd project root -> allowed",
          g(f'cd "{MAIN_CWD}" && pwd', MAIN_CWD) is None)
    check("cd slot root -> allowed", g(f'cd "{slot}"', MAIN_CWD) is None)
    # Non-persistent forms exempt
    check("subshell cd -> exempt", g("(cd /tmp && make)", MAIN_CWD) is None)
    check("substitution cd -> exempt",
          g('echo "$(cd /tmp && pwd)"', MAIN_CWD) is None)
    # cd as text, not command
    check("quoted cd text -> exempt",
          g("git commit -m 'retire cd /tmp habit' -- a.rs", MAIN_CWD) is None)
    check("plain command -> exempt", g("git status", MAIN_CWD) is None)
    # Mid-chain cd
    check("chain-tail cd -> deny",
          g("git fetch && cd /tmp", MAIN_CWD) is not None)


def test_sed_write_guard_asks_on_w_command():
    for cmd in ["sed -n 'w /tmp/x' f", "sed -n '1,5w /etc/pwn' f",
                "sed -n 'p;w out' f", "sed 's/a/b/w out' f",
                "sed -n w/tmp/x f"]:
        check(f"sed_write_guard asks: {cmd}",
              hook.sed_write_guard(cmd) is not None, cmd)


def test_sed_write_guard_ignores_read_only_sed():
    for cmd in ["sed -n '5,10p' f", "sed -n 's/a/b/p' f",
                "sed -n '440,460p' file.rs", "sed -n 's/wide/w2/' f",
                "sed -n 'p' wide.rs", "cat f | sed -n '3p'",
                "rg -n 'w ' file", "git log --oneline"]:
        check(f"sed_write_guard silent: {cmd}",
              hook.sed_write_guard(cmd) is None, cmd)


def main():
    test_cd_guard()
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

    test_sed_write_guard_asks_on_w_command()
    test_sed_write_guard_ignores_read_only_sed()

    test_merge_denied_missing_verdict()
    test_merge_passes_with_gate_verdict()
    test_merge_passes_with_no_gate_verdict()
    test_merge_passes_no_bug_ids()
    test_merge_passes_docs_only()

    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
