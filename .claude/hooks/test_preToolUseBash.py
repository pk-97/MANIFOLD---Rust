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
        check("reminder points at land_branch.py", "land_branch.py" in (context or ""), context)
    finally:
        hook._current_branch = orig


def test_merge_trunk_into_main_no_reminder():
    orig = hook._current_branch
    hook._current_branch = lambda cwd: "main"
    try:
        reason, context = hook.landing_protocol_guard("git merge --ff-only origin/main", MAIN_CWD)
        check("merge origin/main while on main -> no reminder", reason is None and context is None, (reason, context))
    finally:
        hook._current_branch = orig


def test_lane_merging_main_in_worktree_no_reminder():
    cmd = f'git -C "{WORKTREE_CWD}" merge origin/main --no-edit'
    reason, context = hook.landing_protocol_guard(cmd, MAIN_CWD)
    check("lane merging origin/main in its worktree -> no reminder", reason is None and context is None, (reason, context))


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


def test_settings_allowed_segments_compose():
    """BUG-ls9y (Bash hook chain parity): a chain whose every segment is
    allowed alone by permissions.allow is pre-approved; anything else in the
    chain still decides."""
    orig = hook._SETTINGS_ALLOW
    hook._SETTINGS_ALLOW = [
        (["cargo", "check"], True),
        (["cargo", "nextest", "run"], True),
        (["bd"], True),
        (["scripts/agent-worktree.py", "list"], False),
        (["git", "-C"], True),
    ]
    try:
        allowed = [
            'cargo check --manifest-path "x/Cargo.toml" -p a 2>&1 | tail -5',
            "cargo nextest run -p a 2>&1 | rg 'FAIL|Summary'",
            "bd show BUG-x 2>&1 | head -40; bd ready -n 5",
            "scripts/agent-worktree.py list | head",
        ]
        for cmd in allowed:
            check(f"chain of allowed segments passes: {cmd}", hook.is_preapproved_command(cmd))
        refused = [
            "cargo check -p a; rm -rf target",  # rm is not allowed alone
            "cargo check -p a > crates/out.txt",  # write redirect to repo path
            "cargo check",  # wildcard rule needs an argument, like the harness
            "cargo build -p a | tail",  # no rule for cargo build here
            "scripts/agent-worktree.py list --all | head",  # exact rule, extra arg
            "ls; git -C x reset --hard",  # git keeps its own classification
            "bd show X | sh",  # sh is not allowed
        ]
        for cmd in refused:
            check(f"chain with an unallowed part is refused: {cmd}", not hook.is_preapproved_command(cmd))
    finally:
        hook._SETTINGS_ALLOW = orig


def test_settings_allow_parser_shapes():
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        (root / ".claude").mkdir()
        (root / ".claude" / "settings.json").write_text(json.dumps({"permissions": {"allow": [
            "Bash(cargo check *)",
            "Bash(pkill -f rust-analyzer)",
            'Bash(pkill -f "zola.*serve")',
            "Read(//tmp/**)",
        ]}}))
        with unittest.mock.patch.object(hook, "_main_checkout_path", return_value=root):
            rules = hook._load_settings_allow()
    check("prefix rule parsed", (["cargo", "check"], True) in rules, rules)
    check("exact rule parsed", (["pkill", "-f", "rust-analyzer"], False) in rules, rules)
    check("inner-star rule skipped", all("zola" not in " ".join(t) for t, _ in rules), rules)
    check("non-Bash rule skipped", len(rules) == 2, rules)


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


# --- shared-checkout commit hygiene (CLAUDE.md: pathspec, never the index;
# no substitution inside a double-quoted message) ---

def test_index_add_denied_all_modes():
    for cmd in ("git add -A", "git add .", "git add --all", "git add -u",
                "git -C /x add -A && git commit -m 'x'", "git commit -am 'x'",
                "git commit -a -m 'x'", "git commit --all -m 'x'"):
        for mode in ("default", "auto"):
            out = run_hook_main({"tool_input": {"command": cmd}, "cwd": MAIN_CWD,
                                 "permission_mode": mode})
            check(f"{cmd} ({mode}) -> deny", '"deny"' in out and "pathspec" in out, out)


def test_pathspec_add_and_commit_unaffected():
    for cmd in ("git add -- scripts/dev.py", "git add scripts/dev.py docs/x.md",
                "git commit -m 'msg' -- scripts/dev.py", "git commit -q -F - -- a.py",
                "git add -p", "git status --porcelain"):
        check(f"{cmd} -> no index deny", hook.index_add_guard(cmd) is None, cmd)


def test_commit_message_substitution_denied():
    for cmd in ('git commit -m "fix `x`" -- a.py', 'git commit -m "run $(date)" -- a.py',
                'git commit --message="see `y`" -- a.py'):
        out = run_hook_main({"tool_input": {"command": cmd}, "cwd": MAIN_CWD,
                             "permission_mode": "auto"})
        check(f"{cmd} -> deny", '"deny"' in out and "single-quote" in out.lower(), out)


def test_commit_message_quoted_safely_unaffected():
    for cmd in ("git commit -m 'fix `x` and $(y)' -- a.py", 'git commit -m "plain message" -- a.py',
                "git commit -F - -- a.py <<'EOF'\nuses `code`\nEOF"):
        check(f"{cmd!r} -> no substitution deny", hook.commit_substitution_guard(cmd) is None, cmd)


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


def test_sed_write_guard_file_operands_are_not_scripts():
    """The three false-positive shapes that prompted Peter 2026-09-29..10-01:
    an unquoted `$W/path` file operand (`$` address + `W` command), a BSD
    `sed -i ''` empty suffix shadowing the script, and a heredoc body with an
    apostrophe after a sed segment (shlex failure read as 'unparseable')."""
    silent = [
        'W="/x/slot-2"; sed -n 1,99p $W/particle_tests.rs; rg -n busy $W/../ -l | head',
        'W=".claude/worktrees/slot-1"; rg -n fn -A40 $W/crates/a.rs | sed -n 10,40p',
        'sed -n 720,790p $W/crates/manifold-nodes-scene/src/node_graph/gltf_import/object_group.rs',
        "sed -i '' 's/a/b/' $W/f.rs",
        "sed -i '' 's/^## Owed before any engine work$/## Owed before engine work/' docs/X.md; "
        "git commit -q -F - -- docs/X.md <<'EOF'\nP4 is\nPeter's call.\nEOF\ngit push -q origin main",
        "git commit -m 'hook: quoted \"$VAR\" is not a sed w command'",
        'rg -n "var w = |var acc" "$R/swash_tests.rs" | sed -n 1,40p',
        "git worktree list | rg -i 'fft'; for w in $(git worktree list --porcelain | cut -d' ' -f2-); do :; done",
        # merge markers inside a perl script are not heredoc openers
        "F='/x/t.rs'; perl -0pi -e 's/<<<<<<< HEAD\\n.*?=======\\n//s' \"$F\"; sed -n '38,75p' \"$F\"",
        # shlex-unparseable (`\\'` inside double quotes) falls back to span scan, stays silent
        "OUT=\"$S/x.wgsl\"; { printf '%s\\n' \"// FLIP Fluids\\' tables\" \"const A = 1;\"; } > /tmp/x; sed -n 1,5p $W/a.rs",
    ]
    for cmd in silent:
        check(f"sed_write_guard silent (operand/heredoc): {cmd[:60]}",
              hook.sed_write_guard(cmd) is None, cmd)
    asks = [
        "sed -i '' 'w /tmp/x' f",
        "sed -i '' -e p -e '1w out' f",
        "sed -n w/tmp/x docs/README.md",
        "sed -f script.sed f",
        "rg x | sed -n '$w out'",
        "sed -n 'p' f <<'EOF'\nbody\nEOF\nsed -n 'w /tmp/y' g",
        # operator glued to the previous word must still start a new segment
        "cat x; sed -n 'w /tmp/out' f",
        "rg a f|sed -n 'w /tmp/out'",
        "echo \"it\\'s\"; sed -n 'w /tmp/out' f",
        # unparseable quoting + a real quoted w command still asks (fallback path)
        "sed -n 'w /tmp/out' f; echo \"unbalanced",
    ]
    for cmd in asks:
        check(f"sed_write_guard asks (real w): {cmd[:60]}",
              hook.sed_write_guard(cmd) is not None, cmd)
    check("heredoc bodies stripped before parsing",
          "Peter's" not in hook._strip_heredoc_bodies("x <<'EOF'\nPeter's call\nEOF\ny"))


# ---------------------------------------------------------------------------
# Pre-land flow-gate guard — flow_gate_guard
# ---------------------------------------------------------------------------

TIP_SHA = "a" * 40


def _run_flow_gate(diff_output, marker=None, cmd="git merge --no-ff lane/x"):
    """Run flow_gate_guard with mocked git + temp manifest/marker paths."""
    mock_run = unittest.mock.MagicMock()

    def side_effect(argv, *args, **kwargs):
        result = unittest.mock.MagicMock()
        result.returncode = 0
        joined = " ".join(argv) if isinstance(argv, list) else argv
        if "diff" in joined:
            result.stdout = diff_output
        elif "rev-parse" in joined:
            result.stdout = TIP_SHA + "\n"
        else:
            result.stdout = ""
        return result

    mock_run.side_effect = side_effect
    orig_branch = hook._current_branch
    orig_marker = hook._FLOW_MARKER_PATH
    orig_manifest = hook._FLOW_MANIFEST_PATH
    hook._current_branch = lambda cwd: "main"
    with tempfile.TemporaryDirectory() as td:
        manifest_path = Path(td) / "manifest.json"
        manifest_path.write_text(json.dumps(
            {"path_triggers": {"crates/manifold-ui/": ["scene-setup"]}}))
        hook._FLOW_MANIFEST_PATH = manifest_path
        marker_path = Path(td) / "flow-gate-marker.json"
        if marker is not None:
            marker_path.write_text(json.dumps(marker))
        hook._FLOW_MARKER_PATH = marker_path
        patcher = unittest.mock.patch.object(hook.subprocess, "run", mock_run)
        patcher.start()
        try:
            return hook.flow_gate_guard(cmd, MAIN_CWD)
        finally:
            patcher.stop()
            hook._current_branch = orig_branch
            hook._FLOW_MARKER_PATH = orig_marker
            hook._FLOW_MANIFEST_PATH = orig_manifest


def test_flow_gate_unmapped_branch_unaffected():
    reason, context = _run_flow_gate("crates/manifold-core/src/lib.rs\n")
    check("flow gate: unmapped branch -> silent",
          reason is None and context is None, (reason, context))


def test_flow_gate_denies_missing_marker():
    reason, _ = _run_flow_gate("crates/manifold-ui/src/panels/foo.rs\n")
    check("flow gate: mapped + no marker -> deny", reason is not None, reason)
    check("flow gate: deny names run_ui_flows",
          reason and "run_ui_flows.py --touched" in reason, reason)


def test_flow_gate_denies_stale_marker():
    reason, _ = _run_flow_gate(
        "crates/manifold-ui/src/panels/foo.rs\n",
        marker={"head": "b" * 40, "pass": True})
    check("flow gate: stale marker -> deny",
          reason is not None and "stale" in reason, reason)


def test_flow_gate_denies_red_marker():
    reason, _ = _run_flow_gate(
        "crates/manifold-ui/src/panels/foo.rs\n",
        marker={"head": TIP_SHA, "pass": False})
    check("flow gate: red marker -> deny",
          reason is not None and "RED" in reason, reason)


def test_flow_gate_passes_green_marker_at_tip():
    reason, context = _run_flow_gate(
        "crates/manifold-ui/src/panels/foo.rs\n",
        marker={"head": TIP_SHA, "pass": True})
    check("flow gate: green marker at tip -> allow", reason is None, reason)
    check("flow gate: allow carries context",
          context is not None and "green" in context.lower(), context)


def test_flow_gate_touched_flow_file_is_mapped():
    reason, _ = _run_flow_gate("scripts/ui-flows/some-flow.json\n")
    check("flow gate: touched flow file counts as mapped",
          reason is not None, reason)


def test_inline_python_heredoc_denied():
    reason = hook.inline_python_guard("python3 - << 'EOF'\nopen('x','w')\nEOF")
    check("inline python: heredoc denied", reason is not None, reason)


def test_inline_python_dash_c_denied():
    reason = hook.inline_python_guard("python3 -c 'import os'")
    check("inline python: -c denied", reason is not None, reason)
    reason2 = hook.inline_python_guard("python3 - <<'PY'\nprint(1)\nPY")
    check("inline python: dash-stdin heredoc denied", reason2 is not None, reason2)


def test_inline_python_script_path_unaffected():
    reason = hook.inline_python_guard("python3 scripts/landing_gate.py")
    check("inline python: script path NOT denied", reason is None, reason)
    reason2 = hook.inline_python_guard("python3 .claude/hooks/design_status.py --lifecycle-check")
    check("inline python: hook script path NOT denied", reason2 is None, reason2)
    reason3 = hook.inline_python_guard("rg foo crates/")
    check("inline python: non-python segment unaffected", reason3 is None, reason3)



def test_sed_guard_ignores_quoted_shell_variable():
    r = hook.sed_write_guard('W="/tmp/a.rs"; sed -n \'/^mod tests {/,/fn x/p\' "$W"')
    check("sed guard: quoted $W variable is not a w command", r is None, r)
    r2 = hook.sed_write_guard("sed -n '$w /tmp/out' file")
    check("sed guard: real $w command still asks", r2 is not None)


def test_pgrep_and_command_prefix_pre_approved():
    check("pgrep pre-approved", hook.is_preapproved_command("pgrep -fl cargo | head -3"))
    check("command ls pre-approved", hook.is_preapproved_command("command ls -la .claude/"))
    check("command rm NOT pre-approved", not hook.is_preapproved_command("command rm -rf x"))


def test_xargs_gated_on_its_command():
    check("xargs read-only command pre-approved",
          hook.is_preapproved_command("fd -e rs . crates | xargs wc -l"))
    check("xargs -n1 read-only command pre-approved",
          hook.is_preapproved_command("fd -e rs . crates | xargs -n 1 head -1"))
    check("xargs rm NOT pre-approved",
          not hook.is_preapproved_command("fd -e tmp . | xargs rm"))
    check("bare xargs NOT pre-approved",
          not hook.is_preapproved_command("cat list | xargs"))


def _git(repo, *args):
    import subprocess
    out = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True)
    assert out.returncode == 0, (args, out.stderr)
    return out.stdout.strip()


def _land_repo(td):
    """Bare origin + clone on main; branch feat/code changes src/, feat/docs
    changes docs/ only. origin/main is current."""
    origin, repo = Path(td) / "origin.git", Path(td) / "repo"
    _git(td, "init", "-q", "--bare", "-b", "main", str(origin))
    _git(td, "clone", "-q", str(origin), str(repo))
    for k, v in (("user.email", "t@t"), ("user.name", "t")):
        _git(repo, "config", k, v)
    _git(repo, "checkout", "-q", "-b", "main")
    (repo / "src").mkdir()
    (repo / "docs").mkdir()
    (repo / "src/a.rs").write_text("a\n")
    (repo / "docs/d.md").write_text("d\n")
    _git(repo, "add", "--", "src", "docs")
    _git(repo, "commit", "-q", "-m", "base", "--", "src", "docs")
    _git(repo, "push", "-q", "origin", "main")
    _git(repo, "checkout", "-q", "-b", "feat/code")
    (repo / "src/a.rs").write_text("b\n")
    _git(repo, "commit", "-q", "-m", "code", "--", "src/a.rs")
    _git(repo, "checkout", "-q", "main")
    _git(repo, "checkout", "-q", "-b", "feat/docs")
    (repo / "docs/d.md").write_text("e\n")
    _git(repo, "commit", "-q", "-m", "docs", "--", "docs/d.md")
    _git(repo, "checkout", "-q", "main")
    return repo


def _record_pass(repo, tree):
    common = (repo / _git(repo, "rev-parse", "--git-common-dir")).resolve()
    (common / hook.LAND_PASS_DIR).mkdir(exist_ok=True)
    (common / hook.LAND_PASS_DIR / f"{tree}.json").write_text("{}")


def test_land_pass_guard_denies_bypass_shapes():
    with tempfile.TemporaryDirectory() as td:
        repo = _land_repo(td)
        other = Path(td) / "elsewhere"
        other.mkdir()
        shapes = {
            "git merge while on main": ("git merge feat/code", str(repo)),
            "git merge --no-ff while on main": ("git merge --no-ff -m 'x y' feat/code", str(repo)),
            "git -C <path> merge": (f'git -C "{repo}" merge feat/code', str(other)),
            "checkout main && merge": ("git checkout feat/code && git checkout main && git merge feat/code", str(repo)),
            "push origin HEAD:main": ("git push origin HEAD:main", None),
            "push origin <branch>:main": ("git push origin feat/code:main", str(repo)),
            "push origin <branch>:refs/heads/main": ("git push origin feat/code:refs/heads/main", str(repo)),
            "git -C <path> push <br>:main": (f'git -C "{repo}" push origin feat/code:main', str(other)),
            "gh pr merge": ("gh pr merge 12 --squash", str(repo)),
            "update-ref main": ("git update-ref refs/heads/main feat/code", str(repo)),
        }
        for name, (cmd, cwd) in shapes.items():
            if name == "push origin HEAD:main":
                _git(repo, "checkout", "-q", "feat/code")
                cwd = str(repo)
            reason = hook.land_pass_guard(cmd, cwd)
            check(f"land pass: {name} -> denied", reason is not None, reason)
            if name == "push origin HEAD:main":
                _git(repo, "checkout", "-q", "main")
        _git(repo, "checkout", "-q", "feat/code")
        check("land pass: bare push on main denied",
              hook.land_pass_guard("git push", str(repo)) is None, "feat branch bare push is not main")
        _git(repo, "checkout", "-q", "main")
        _git(repo, "merge", "-q", "--ff-only", "feat/code")
        check("land pass: push --all with unpassed main commits denied",
              hook.land_pass_guard("git push --all origin", str(repo)) is not None)
        check("land pass: bare push on main with unpassed commits denied",
              hook.land_pass_guard("git push", str(repo)) is not None)


def test_land_pass_guard_allowed_paths():
    with tempfile.TemporaryDirectory() as td:
        repo = _land_repo(td)
        c = str(repo)
        check("land pass: merge of a docs-only branch allowed",
              hook.land_pass_guard("git merge --no-ff feat/docs", c) is None)
        check("land pass: push of a docs-only branch to main allowed",
              hook.land_pass_guard("git push origin feat/docs:main", c) is None)
        check("land pass: merge on a feature branch allowed",
              hook.land_pass_guard("git checkout feat/docs && git merge main", c) is None)
        check("land pass: push of a feature branch to itself allowed",
              hook.land_pass_guard("git push origin feat/code", c) is None)
        check("land pass: merge --abort allowed",
              hook.land_pass_guard("git merge --abort", c) is None)
        check("land pass: push of main already on origin allowed",
              hook.land_pass_guard("git push origin main", c) is None)
        check("land pass: unrelated git commands untouched",
              hook.land_pass_guard("git status && git log -3", c) is None)
        # a recorded pass for the merged tree opens exactly that merge and push
        tree = _git(repo, "rev-parse", "feat/code^{tree}")
        check("land pass: unrecorded code push denied first",
              hook.land_pass_guard("git push origin feat/code:main", c) is not None)
        _record_pass(repo, tree)
        check("land pass: recorded tree push allowed",
              hook.land_pass_guard("git push origin feat/code:main", c) is None)
        check("land pass: recorded tree merge allowed (ff result tree matches)",
              hook.land_pass_guard("git merge feat/code", c) is None)
        check("land pass: recorded tree update-ref allowed",
              hook.land_pass_guard("git update-ref refs/heads/main feat/code", c) is None)


def test_land_pass_guard_reads_land_branch_record():
    """The writer in scripts/land_branch.py and this reader agree on the format."""
    land_spec = importlib.util.spec_from_file_location(
        "land_branch", HOOK_PATH.parents[2] / "scripts" / "land_branch.py")
    sys.path.insert(0, str(HOOK_PATH.parents[2] / "scripts"))
    try:
        land = importlib.util.module_from_spec(land_spec)
        land_spec.loader.exec_module(land)
    finally:
        sys.path.pop(0)
    check("land pass: writer and reader share the directory name",
          land.LAND_PASS_DIR == hook.LAND_PASS_DIR, (land.LAND_PASS_DIR, hook.LAND_PASS_DIR))
    with tempfile.TemporaryDirectory() as td:
        repo = _land_repo(td)
        tree = _git(repo, "rev-parse", "feat/code^{tree}")
        common = (repo / _git(repo, "rev-parse", "--git-common-dir")).resolve()
        land.lock_path = lambda: common / land.LOCK_NAME
        land.record_land_pass(tree, "abc", "feat/code", None)
        check("land pass: record written by land_branch opens the push",
              hook.land_pass_guard("git push origin feat/code:main", str(repo)) is None)


def test_land_pass_tamper_guard():
    d = ".git/land-passes-v1"
    for name, cmd in {
        "redirect": f"echo {{}} > {d}/abc.json",
        "append": f"echo x >> {d}/abc.json",
        "cp": f"cp /tmp/x {d}/abc.json",
        "mv": f"mv /tmp/x {d}/abc.json",
        "tee": f"echo x | tee {d}/abc.json",
        "touch": f"touch {d}/abc.json",
        "mkdir": f"mkdir -p {d}",
        "sed -i": f"sed -i s/a/b/ {d}/abc.json",
        "python": f"/tmp/w.py {d}/abc.json",
        "redirect after cat": f"cat {d}/x > /tmp/y; echo a > {d}/z",
    }.items():
        check(f"land pass tamper: {name} -> denied", hook.land_pass_tamper_guard(cmd) is not None, cmd)
    for cmd in (f"ls {d}", f"cat {d}/abc.json", f"rg tree {d}", "scripts/land_branch.py feat/x --worktree /w --message m"):
        check(f"land pass tamper: read allowed: {cmd[:30]}", hook.land_pass_tamper_guard(cmd) is None, cmd)


# BUG-zezjf.21 (python heredoc edited settings.json): the lane's command,
# shortened. The apostrophe in the body made shlex raise, the old guard read
# that as "no segments" and let it through.
ZEZJF21_BYPASS = (
    'S="/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-5"; '
    "/usr/bin/python3 - \"$S\" <<'EOF'\n"
    "import sys,pathlib\n"
    "S=pathlib.Path(sys.argv[1])\n"
    "p=S/\".claude/settings.json\"; t=p.read_text()\n"
    "a='      \"Bash(scripts/gpu_queue.py -- scripts/landing_gate.py *)\",\\n'\n"
    "t=t.replace(a,a+'      \"Bash(scripts/land_branch.py *)\",\\n'); p.write_text(t)\n"
    "new='''def g(cmd):\n"
    "    \"\"\"both stay Peter's call every time\"\"\"'''\n"
    "EOF"
)


def test_zezjf21_bypass_denied():
    import shlex as _shlex
    try:
        _shlex.split(ZEZJF21_BYPASS)
        unparseable = False
    except ValueError:
        unparseable = True
    check("zezjf.21: bypass body still breaks plain shlex (repro is faithful)", unparseable)
    check("zezjf.21: exact bypass denied as inline python",
          hook.inline_python_guard(ZEZJF21_BYPASS) is not None)
    check("zezjf.21: exact bypass also flagged as a config write",
          hook.config_write_guard(ZEZJF21_BYPASS) is not None)
    out = run_hook_main({"tool_input": {"command": ZEZJF21_BYPASS}, "cwd": MAIN_CWD,
                         "permission_mode": "auto"})
    check("zezjf.21: main() denies the bypass in auto mode", '"deny"' in out, out)


def test_inline_python_spellings_denied():
    for cmd in (
        "true;python3 -c 'print(1)'",
        "rg -n x f&&python3 -c 'print(1)'",
        "echo hi\npython3 -c 'print(1)'",
        "python3.12 -c 'print(1)'",
        "/opt/homebrew/bin/python3.13 -c 'print(1)'",
        "\"python3\" -c 'print(1)'",
        "env PYTHONPATH=x python3 -c 'print(1)'",
        "nice -n 10 python3 - <<'EOF'\nprint('it''s')\nEOF",
        "timeout 30 python3 -c 'print(1)'",
        "uv run --with rich python -c 'print(1)'",
        "printf 'print(1)' | python3",
        "python3 <<'EOF'\nprint(\"Peter's\")\nEOF",
        "python3 /dev/stdin <<'EOF'\nprint(1)\nEOF",
        "python3 <<< 'print(1)'",
        "cat <<'EOF' | python3\nprint(\"don't\")\nEOF",
        "bash -c \"python3 -c 'print(1)'\"",
        "bash <<'EOF'\npython3 -c 'print(1)'\nEOF",
        "eval \"python3 -c 'print(1)'\"",
        "(python3 -c 'print(1)')",
        "echo \"$(python3 -c 'print(1)')\"",
        "P=/usr/bin/python3; $P -c 'print(1)'",
        "$(command -v python3) -c 'print(1)'",
        "scripts/gpu_queue.py -- python3 -c 'print(1)'",
        "find . -name x -exec python3 -c 'print(1)' \\;",
        "python3 -c 'print(1)'; echo \"unbalanced",
    ):
        check(f"inline python spelling denied: {cmd[:60]!r}",
              hook.inline_python_guard(cmd) is not None, cmd)


def test_inline_python_reads_allowed():
    for cmd in (
        "python3 .claude/hooks/test_preToolUseBash.py",
        "nice -n 10 python3 \"$S/.claude/hooks/test_preToolUseBash.py\" 2>&1 | rg 'FAIL|passed'",
        "rg -n 'python3 -c' .claude/hooks/preToolUseBash.py",
        "echo python3 -c x",
        "command -v python3",
        "which python3",
        "git log --oneline -- python3",
        "git commit -q -F - -- a.py <<'EOF'\nDeny python3 -c and Peter's heredocs\nEOF",
        "cat <<'EOF' > /tmp/x.py\nprint('hi')\nEOF",
        "find . -name '*.py' -exec python3 {} \\;",
        "scripts/gpu_queue.py -- cargo test -p x",
    ):
        check(f"inline python guard silent: {cmd[:60]!r}",
              hook.inline_python_guard(cmd) is None, cmd)


def test_config_writes_ask():
    for cmd in (
        "cat <<'EOF' > .claude/settings.json\n{}\nEOF",
        "cat > .claude/settings.json <<'EOF'\n{\"it's\": 1}\nEOF",
        "echo x >> \"$S/.claude/hooks/preToolUseBash.py\"",
        "printf '{}' | tee .claude/settings.local.json",
        "cp /tmp/settings.json \"/Users/peterkiemann/MANIFOLD - Rust/.claude/settings.json\"",
        "cp /tmp/x/settings.json .claude/",
        "cp -R /tmp/work/.claude \"/Users/peterkiemann/MANIFOLD - Rust/\"",
        "install -m 755 /tmp/h.py .claude/hooks/h.py",
        "mv .claude/hooks/preToolUseBash.py /tmp/",
        "ln -sf /tmp/evil.json ~/.claude/settings.json",
        "rm -rf .claude/hooks",
        "chmod -x .claude/hooks/preToolUseBash.py",
        "dd if=/tmp/x of=.claude/settings.json",
        "sed -i '' 's/a/b/' .claude/settings.json",
        "perl -pi -e 's/a/b/' .claude/hooks/preToolUseBash.py",
        "node -e \"require('fs').writeFileSync('.claude/settings.json','{}')\"",
        "awk '{print > \".claude/settings.json\"}' /tmp/in",
        "bash <<'EOF'\necho '{}' > .claude/settings.json\nEOF",
        "git checkout origin/main -- .claude/settings.json",
        "git -C .claude/worktrees/slot-5 restore .claude/hooks/x.py",
        "scripts/x.py --out .claude/settings.json",
        "S=/repo; cp /tmp/x \"$S/.claude/hooks/y.py\"",
        "true;cp /tmp/x .claude/settings.json",
    ):
        check(f"config write asks: {cmd[:60]!r}",
              hook.config_write_guard(cmd) is not None, cmd)


def test_config_reads_silent():
    for cmd in (
        "rg -n '\"allow\"' .claude/settings.json .claude/settings.local.json",
        "sed -n 1,12p \"$S/.claude/settings.json\"",
        "jq .permissions .claude/settings.json",
        "cat .claude/settings.json | head",
        "ls -la .claude/hooks/",
        "diff .claude/settings.json /tmp/s.json",
        "git diff -- .claude/settings.json",
        "git -C /w show origin/main:.claude/settings.json > /tmp/s.json",
        "cp .claude/settings.json /tmp/s.json",
        "D=/private/tmp/x; cp -R \"/repo/.claude\" $D/work/",
        "git add -- .claude/hooks/x.py",
        "git commit -m 'Hook: tighten' -- .claude/hooks/preToolUseBash.py",
        "git restore --staged .claude/hooks/x.py",
        ".claude/hooks/test_preToolUseBash.py 2>&1 | tail -4",
        ".claude/hooks/design_status_check.py origin/main HEAD",
        "python3 .claude/hooks/design_status.py --lifecycle-check",
        "echo x > /tmp/w/.claude/settings.json",
        "rg -n 'tee .claude/settings.json' docs/",
        "rm -rf .claude/worktrees/slot-5/target/tmp",
    ):
        check(f"config read silent: {cmd[:60]!r}",
              hook.config_write_guard(cmd) is None, cmd)


def test_config_write_asks_in_every_mode():
    for mode in ("default", "auto", "bypassPermissions", "acceptEdits"):
        out = run_hook_main({"tool_input": {"command": "cat .claude/settings.json > .claude/settings.local.json"},
                             "cwd": MAIN_CWD, "permission_mode": mode})
        check(f"config write asks in {mode} mode", '"ask"' in out, out)


def test_heredoc_tail_redirect_not_preapproved():
    check("cat heredoc with a tail redirect to a repo path is NOT pre-approved",
          not hook.is_preapproved_command("cat <<'EOF' > /Users/x/repo/f.rs\nfn x() {}\nEOF"))
    check("commit heredoc with an apostrophe still pre-approved",
          hook.is_preapproved_command("git commit -q -F - -- a.py <<'EOF'\nPeter's call\nEOF"))


# BUG-zezjf.21 review: a heredoc fed to a shell is a program, a delimiter is a
# whole shell word, an unreadable heredoc fails closed, and $'...' is quoting.
def _decision(cmd, mode):
    out = run_hook_main({"tool_input": {"command": cmd}, "cwd": MAIN_CWD,
                         "permission_mode": mode})
    if not out:
        return None, ""
    d = json.loads(out).get("hookSpecificOutput", {})
    return d.get("permissionDecision"), d.get("permissionDecisionReason", "")


WORKTREE_DENY = "git worktree add"
INDEX_DENY = "Staging the whole index"
PY_DENY = "Inline python"


def test_heredoc_shell_program_keeps_denies():
    for cmd, want in (
        ("bash <<'EOF'\ntrue && git worktree add -b x ../y HEAD\nEOF", WORKTREE_DENY),
        ("bash <<'EOF'\ngit worktree add -b x ../y HEAD\nEOF", WORKTREE_DENY),
        ("bash <<'EOF'\ntrue && git add -A\nEOF", INDEX_DENY),
        ("bash <<'EOF'\ntrue && python3 -c 'print(1)'\nEOF", PY_DENY),
        ("cat <<EOF | bash\ntrue && git add -A\nEOF", INDEX_DENY),
        ("cat <<'EOF' | bash\ntrue && python3 -c 'print(1)'\nEOF", PY_DENY),
        ("cat <<'EOF' | bash\npython3 -c 'print(1)'\nEOF", PY_DENY),
        ("cat <<EOF | sed s/a/b/ | sh\ntrue && git add -A\nEOF", INDEX_DENY),
        ("env bash -o pipefail <<'EOF'\ngit add -A\nEOF", INDEX_DENY),
        ("cat <<EOF > /tmp/x\n$(python3 -c 'print(1)')\nEOF", PY_DENY),
    ):
        for mode in ("default", "auto"):
            dec, why = _decision(cmd, mode)
            check(f"heredoc program ({mode}) denied: {cmd[:50]!r}",
                  dec == "deny" and want in why, (dec, why[:80]))
    for cmd in (
        "sh <<'EOF'\ntrue && git push --force origin main\nEOF",
        "bash -s <<'EOF'\ntrue && git push --force origin main\nEOF",
        "cat <<EOF | sh\ntrue && git push --force origin main\nEOF",
    ):
        plain = _decision("git push --force origin main", "auto")
        for mode in ("default", "auto"):
            dec, why = _decision(cmd, mode)
            check(f"heredoc force-push ({mode}) decided like the plain push: {cmd[:50]!r}",
                  (dec, why) == plain, ((dec, why[:60]), plain[0]))


def test_heredoc_data_is_not_a_program():
    for cmd in (
        "git commit -q -F - -- a.rs <<'EOF'\ngit add -A\npython3 -c 'x'\nEOF",
        "cat <<'EOF' > /tmp/notes.md\ngit worktree add -b x ../y HEAD\nEOF",
        "bash scripts/x.sh <<'EOF'\ngit add -A\nEOF",
        "bash -c 'echo hi' <<'EOF'\ngit add -A\nEOF",
        "cat <<'EOF' > /tmp/x\n$(python3 -c 'print(1)')\nEOF",
    ):
        for mode in ("default", "auto"):
            dec, why = _decision(cmd, mode)
            check(f"heredoc data ({mode}) not denied: {cmd[:50]!r}", dec != "deny", why[:80])


def test_heredoc_delimiter_is_a_whole_word():
    for cmd, want in (
        ("cat <<EOF.x\ndata\nEOF.x\ntrue && python3 -c 'print(1)'", PY_DENY),
        ("cat <<EOF.x\ndata\nEOF.x\npython3 -c 'print(1)'", PY_DENY),
        ("cat <<EOF.x\ndata\nEOF.x\ntrue && git worktree add -b x ../y HEAD", WORKTREE_DENY),
        ("cat <<EOF-1\ndata\nEOF-1\ntrue && git add -A", INDEX_DENY),
        ("cat <<'EOF'x\ndata\nEOFx\ntrue && git add -A", INDEX_DENY),
        ("cat <<\"E\"O'F'-1\ndata\nEOF-1\ntrue && git add -A", INDEX_DENY),
        ("cat <<EOF\ndata\n  EOF\ntrue && git add -A\nEOF", INDEX_DENY),
        ("echo $((a<<b))\ntrue && git add -A\nb", INDEX_DENY),
        ("git commit -F - -- a.rs <<'MSG.txt'\nit's\nMSG.txt\ntrue && git add -A", INDEX_DENY),
        ("cat <<END_OF_MSG\nhi\nEND_OF_MSG\ngit add -A", INDEX_DENY),
    ):
        for mode in ("default", "auto"):
            dec, why = _decision(cmd, mode)
            check(f"heredoc delimiter ({mode}) read whole: {cmd[:50]!r}",
                  dec == "deny" and want in why, (dec, why[:80]))
    cmd = "cat <<E\\OF\ndata\nEOF\ntrue && git push --force origin main"
    dec, _ = _decision(cmd, "auto")
    check("heredoc delimiter E\\OF reads as EOF; the push after it is seen", dec in ("ask", "deny"), dec)
    for opener, delim in (("<<EOF.x", "EOF.x"), ("<<'EOF'x", "EOFx"), ("<<E\\OF", "EOF"),
                          ("<<-EOF", "EOF"), ("<< \"A B\"", "A B"), ("<<EOF-1", "EOF-1")):
        res = hook._split_heredocs(f"cat {opener}\nbody\n{delim}\n")
        bodies, ok = res[1], len(res) > 2 and res[2]
        check(f"heredoc opener {opener} -> delimiter {delim!r}", ok and bodies == ["body\n"], (ok, bodies))


SETTINGS_WRITE_PY = ("/usr/bin/python3 - <<'EOF'\nimport pathlib; "
                     "pathlib.Path(\".claude/settings.json\").write_text(\"{}\")\nEOF")


def test_heredoc_unreadable_fails_closed():
    for cmd in (
        "true <<EOF.\nEOF.\n" + SETTINGS_WRITE_PY,
        "cat <<EOF-1\ndata\nEOF-1\n" + SETTINGS_WRITE_PY,
    ):
        for mode in ("default", "auto", "bypassPermissions", "acceptEdits"):
            dec, why = _decision(cmd, mode)
            check(f"settings write after a dotted heredoc ({mode}) denied: {cmd[:30]!r}",
                  dec == "deny" and PY_DENY in why, (dec, why[:80]))
    dec, _ = _decision("cat <<EOF-1\ndata\nEOF-1\necho '{}' > .claude/settings.json", "auto")
    check("settings redirect after a dashed heredoc asks", dec == "ask", dec)
    for cmd in (
        "cat <<EOF\nit's data\ntrue && git add -A",
        "cat <<'EOF\nit's data\nEOF\ngit add -A",
        "cat <<EOF",
    ):
        for mode in ("default", "auto", "bypassPermissions"):
            dec, why = _decision(cmd, mode)
            check(f"unreadable heredoc ({mode}) fails closed: {cmd[:40]!r}",
                  dec in ("ask", "deny"), (dec, why[:80]))
        check(f"unreadable heredoc is not pre-approved: {cmd[:40]!r}",
              not hook.is_preapproved_command(cmd))


def test_ansi_c_quoting_is_text():
    note = ("bd create -t chore -p 3 -l low,open -d $'Peter\\'s hooks note: python3 -c is denied "
            "in .claude/hooks/preToolUseBash.py' 'hook note'")
    for mode in ("default", "auto"):
        dec, why = _decision(note, mode)
        # bd is pre-approved on main, so "allow" is also correct here.
        check(f"$'...' note text ({mode}) is not a command", dec in (None, "allow"), (dec, why[:80]))
    check("$'...' note: inline python guard silent", hook.inline_python_guard(note) is None)
    check("$'...' note: config guard silent", hook.config_write_guard(note) is None)
    for cmd in ("echo $'it\\'s' && python3 -c 'print(1)'", "echo $'a\\'b'\npython3 -c 'x'"):
        dec, why = _decision(cmd, "auto")
        check(f"python after $'...' still denied: {cmd[:40]!r}",
              dec == "deny" and PY_DENY in why, (dec, why[:80]))
    dec, why = _decision("echo $'it\\'s' ; git add -A", "auto")
    check("git add -A after $'...' still denied", dec == "deny" and INDEX_DENY in why, (dec, why[:80]))


# BUG-zezjf.15 (one status screen): foreground wait loops are denied; the
# same command with run_in_background passes this guard.
WAIT_DENY = "Foreground wait"


def _wait_decision(cmd, mode, background=False):
    tool_input = {"command": cmd}
    if background:
        tool_input["run_in_background"] = True
    out = run_hook_main({"tool_input": tool_input, "cwd": MAIN_CWD,
                         "permission_mode": mode})
    if not out:
        return None, ""
    d = json.loads(out).get("hookSpecificOutput", {})
    return d.get("permissionDecision"), d.get("permissionDecisionReason", "")


WAIT_LOOPS = (
    "while kill -0 12345 2>/dev/null; do sleep 10; done; echo finished",
    "until rg -q 'landing gate:' /tmp/gate.log; do sleep 15; done",
    "while true; do if [ -f /tmp/done ]; then break; fi; sleep 30; done",
    "while pgrep -f landing_gate >/dev/null\ndo\n  sleep 20\ndone\ntail -5 /tmp/gate.log",
    'PID=123; while kill -0 "$PID" 2>/dev/null; do sleep 5; done && tail -5 /tmp/log',
    "while kill -0 4242; do :; done",
    "until ! kill -0 4242 2>/dev/null; do echo waiting; done",
    "timeout 600 bash -c 'until test -f /tmp/done; do sleep 5; done'",
    "bash <<'EOF'\nwhile kill -0 4242; do sleep 10; done\nEOF",
    "while :; do sleep 1; done",
    "sleep 60; tail -20 /tmp/gate.log",
    "sleep 120 && cat /tmp/out.txt",
    'sleep 30 ; tail -n 50 "/Users/peterkiemann/MANIFOLD - Rust/target/gate.log"',
    "sleep 300",
    "sleep 2m; tail /tmp/x",
    "sleep 5 5 5; cat /tmp/x",
    "sleep 11",
    'sleep "$WAIT"; cat /tmp/x',
)


def test_wait_loops_denied_in_foreground():
    for cmd in WAIT_LOOPS:
        for mode in ("default", "auto"):
            dec, why = _wait_decision(cmd, mode)
            check(f"wait loop ({mode}) denies: {cmd[:50]!r}",
                  dec == "deny" and WAIT_DENY in why, (dec, why[:80]))
    _, why = _wait_decision("sleep 300", "auto")
    check("wait deny names watch_land.py and Monitor",
          "scripts/watch_land.py" in why and "Monitor" in why, why)


def test_wait_loops_silent_in_background():
    for cmd in WAIT_LOOPS:
        dec, why = _wait_decision(cmd, "auto", background=True)
        check(f"wait loop in background passes: {cmd[:50]!r}",
              WAIT_DENY not in why, (dec, why[:80]))


def test_short_waits_and_non_polls_silent():
    for cmd in (
        "scripts/dev.py status && sleep 1 && tail -5 /tmp/dev.log",
        "sleep 5",
        "sleep 10",
        "( target/debug/deps/x --list >/dev/null 2>&1 & pid=$!; sleep 6; kill $pid )",
        "sleep 0.5; cat /tmp/x",
        "sleep 3s && rg -n ready /tmp/server.log",
        "sleep 600 &",
        "kill -0 1234 && echo alive",
        "while read -r f; do echo \"$f\"; done < /tmp/list",
        "for f in a b c; do echo $f; sleep 1; done",
    ):
        for mode in ("default", "auto"):
            dec, why = _wait_decision(cmd, mode)
            check(f"short wait / non-poll silent ({mode}): {cmd[:50]!r}",
                  WAIT_DENY not in why, (dec, why[:80]))


EVERYDAY = (
    'git -C "/Users/peterkiemann/MANIFOLD - Rust" status --short',
    "git -C /w log --oneline -5",
    'cargo check --manifest-path "/Users/peterkiemann/MANIFOLD - Rust/crates/manifold-core/Cargo.toml"',
    "cargo nextest run -p manifold-core --manifest-path /w/Cargo.toml sync",
    "rg -n 'sleep' crates/manifold-playback/src",
    'rg -n "while true; do sleep" .claude/hooks',
    "jq '.hooks' .claude/settings.json",
    "bd show BUG-zezjf.15",
    "bd create -t bug -p 2 -l minor,open -d 'wait loop: while kill -0 PID; do sleep 5; done blocked the lane'",
    "scripts/landing_gate.py --cheap",
    "scripts/watch_land.py --pid 4242 --log /tmp/gate.log",
    "scripts/gpu_queue.py -- cargo test -p manifold-gpu --features gpu-proofs",
    "git commit -F - -- .claude/hooks/x.py <<'EOF'\nDeny foreground wait loops\n\n"
    "A lane ran sleep 300; tail -20 gate.log and blocked for five minutes.\n"
    "while kill -0 $PID; do sleep 5; done is the other shape.\nEOF",
    "gh pr create --title 'Wait-loop guard' --body \"$(cat <<'EOF'\n"
    "Denies foreground while loops that sleep, e.g.\nuntil test -f done; do sleep 10; done\nEOF\n)\"",
    "git -C /w diff --stat origin/main...HEAD && cargo clippy --manifest-path /w/Cargo.toml -p manifold-ui -- -D warnings",
    "ps -p 4242 -o pid,etime,command",
    "git commit -m 'hook: deny sleep 60 polls' -- .claude/hooks/preToolUseBash.py",
)


def test_everyday_commands_unchanged():
    check("everyday corpus has at least 15 commands", len(EVERYDAY) >= 15, len(EVERYDAY))
    for cmd in EVERYDAY:
        for mode in ("default", "auto"):
            fg = _wait_decision(cmd, mode)
            bg = _wait_decision(cmd, mode, background=True)
            check(f"everyday unchanged ({mode}): {cmd[:50]!r}",
                  fg == bg and WAIT_DENY not in fg[1], (fg[0], fg[1][:80], bg[0]))


def main():
    test_wait_loops_denied_in_foreground()
    test_wait_loops_silent_in_background()
    test_short_waits_and_non_polls_silent()
    test_everyday_commands_unchanged()
    test_heredoc_shell_program_keeps_denies()
    test_heredoc_data_is_not_a_program()
    test_heredoc_delimiter_is_a_whole_word()
    test_heredoc_unreadable_fails_closed()
    test_ansi_c_quoting_is_text()
    test_zezjf21_bypass_denied()
    test_inline_python_spellings_denied()
    test_inline_python_reads_allowed()
    test_config_writes_ask()
    test_config_reads_silent()
    test_config_write_asks_in_every_mode()
    test_heredoc_tail_redirect_not_preapproved()
    test_sed_guard_ignores_quoted_shell_variable()
    test_xargs_gated_on_its_command()
    test_pgrep_and_command_prefix_pre_approved()
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
    test_merge_trunk_into_main_no_reminder()
    test_lane_merging_main_in_worktree_no_reminder()
    test_bare_push_on_main_branch_reminds()
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
    test_sed_write_guard_file_operands_are_not_scripts()

    test_merge_denied_missing_verdict()
    test_merge_passes_with_gate_verdict()
    test_merge_passes_with_no_gate_verdict()
    test_merge_passes_no_bug_ids()
    test_merge_passes_docs_only()

    test_flow_gate_unmapped_branch_unaffected()
    test_flow_gate_denies_missing_marker()
    test_flow_gate_denies_stale_marker()
    test_flow_gate_denies_red_marker()
    test_flow_gate_passes_green_marker_at_tip()
    test_flow_gate_touched_flow_file_is_mapped()

    test_settings_allowed_segments_compose()
    test_settings_allow_parser_shapes()

    test_inline_python_heredoc_denied()
    test_inline_python_dash_c_denied()
    test_inline_python_script_path_unaffected()

    test_land_pass_guard_denies_bypass_shapes()
    test_land_pass_guard_allowed_paths()
    test_land_pass_guard_reads_land_branch_record()
    test_land_pass_tamper_guard()

    test_index_add_denied_all_modes()
    test_pathspec_add_and_commit_unaffected()
    test_commit_message_substitution_denied()
    test_commit_message_quoted_safely_unaffected()


    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
