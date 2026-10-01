#!/usr/bin/env python3
"""
Standalone test runner for preToolUseBash.py's guards (landing-protocol,
worktree-ring, landing-gate marker, compound-landing-merge, shell
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
# Landing-gate marker guard — merge_marker_guard
# ---------------------------------------------------------------------------

def _marker(tree="treeA", **over):
    record = {"schema": 1, "tree": tree, "pass": True, "failing_tests": [],
              "pre_existing_tests": [], "head": "h", "branch": "lane/x", "base": "b",
              "skipped": [], "ts": "2026-10-01T00:00:00Z"}
    record.update(over)
    return record


def _run_marker_guard(marker=None, tree="treeA", in_origin_main=False,
                      cmd="git merge --no-ff lane/feat-x"):
    """Run merge_marker_guard with the marker file, git and the branch mocked.
    marker=None means no marker file exists."""
    def side_effect(argv, *args, **kwargs):
        result = unittest.mock.MagicMock()
        result.stderr = ""
        if "merge-base" in argv:
            result.returncode = 0 if in_origin_main else 1
            result.stdout = ""
        elif "rev-parse" in argv:
            result.returncode = 0
            result.stdout = tree + "\n"
        else:
            result.returncode = 0
            result.stdout = ""
        return result

    orig_path, orig_branch = hook._LANDING_MARKER_PATH, hook._current_branch
    with tempfile.TemporaryDirectory() as td:
        path = Path(td) / "landing-gate-marker.json"
        if marker is not None:
            path.write_text(json.dumps(marker))
        hook._LANDING_MARKER_PATH = path
        hook._current_branch = lambda cwd: "main"
        try:
            with unittest.mock.patch.object(hook.subprocess, "run", side_effect=side_effect):
                return hook.merge_marker_guard(cmd, MAIN_CWD)
        finally:
            hook._LANDING_MARKER_PATH = orig_path
            hook._current_branch = orig_branch


def test_marker_merge_denied_without_a_marker():
    reason, _ = _run_marker_guard(marker=None)
    check("merge denied with no marker", reason is not None and "no landing-gate marker" in reason, reason)
    check("deny points at running the gate", reason and "scripts/landing_gate.py --repo" in reason, reason)


def test_marker_merge_denied_on_tree_mismatch():
    reason, _ = _run_marker_guard(marker=_marker(tree="staleTree"), tree="treeA")
    check("merge denied when the marker is for another tree",
          reason is not None and "marker is for tree" in reason, reason)


def test_marker_merge_denied_on_red_marker():
    reason, _ = _run_marker_guard(marker=_marker(**{"pass": False}))
    check("merge denied on a red marker", reason is not None and "RED" in reason, reason)


def test_marker_merge_denied_when_failing_tests_listed():
    reason, _ = _run_marker_guard(marker=_marker(failing_tests=["manifold-gpu core::hang"]))
    check("merge denied when failing tests are listed",
          reason is not None and "failing tests" in reason, reason)


def test_marker_merge_passes_with_pre_existing_failure_that_has_a_bead():
    marker = _marker(pre_existing_tests=[{"test": "gpu-proofs proofs::flip", "bead": "BUG-xyz"}])
    reason, context = _run_marker_guard(marker=marker)
    check("merge passes with a pre-existing failure that has a bead", reason is None, reason)
    check("context confirms the green marker", context and "green marker" in context, context)


def test_marker_merge_denied_when_pre_existing_failure_has_no_bead():
    reason, _ = _run_marker_guard(marker=_marker(pre_existing_tests=[{"test": "a b"}]))
    check("merge denied when a pre-existing failure has no bead",
          reason is not None and "no bead" in reason, reason)


def test_marker_merge_passes_with_green_marker_at_tip():
    reason, context = _run_marker_guard(marker=_marker())
    check("merge passes with a green marker for the tip's tree", reason is None, reason)
    check("context confirms the green marker", context and "green marker" in context, context)


def test_marker_branch_with_no_bug_ids_still_needs_the_marker():
    # The old guard waved through any branch whose log named no bug id.
    reason, _ = _run_marker_guard(marker=None, cmd="git merge --no-ff lane/infra-fix")
    check("a branch with no bug ids still needs the marker", reason is not None, reason)


def test_marker_docs_only_merge_still_needs_the_marker():
    # The old guard exempted docs-only merges; the gate runs for them too.
    reason, _ = _run_marker_guard(marker=None, cmd="git merge --no-ff lane/doc-fix")
    check("a docs-only merge still needs the marker", reason is not None, reason)


def test_marker_merge_of_a_branch_already_in_origin_main_passes():
    # `git merge origin/main`-style pulls land nothing new, so they cannot need a gate run.
    reason, context = _run_marker_guard(marker=None, in_origin_main=True)
    check("merge of a branch already in origin/main passes", reason is None and context is None,
          (reason, context))


def test_marker_guard_denies_when_the_marker_module_cannot_load():
    orig = hook._LANDING_MARKER_MODULE
    hook._LANDING_MARKER_MODULE = Path("/nonexistent/landing_marker.py")
    try:
        reason, _ = _run_marker_guard(marker=_marker())
    finally:
        hook._LANDING_MARKER_MODULE = orig
    check("guard denies when it cannot load the marker check",
          reason is not None and "could not load" in reason, reason)


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


def main():
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

    test_marker_merge_denied_without_a_marker()
    test_marker_merge_denied_on_tree_mismatch()
    test_marker_merge_denied_on_red_marker()
    test_marker_merge_denied_when_failing_tests_listed()
    test_marker_merge_passes_with_pre_existing_failure_that_has_a_bead()
    test_marker_merge_denied_when_pre_existing_failure_has_no_bead()
    test_marker_merge_passes_with_green_marker_at_tip()
    test_marker_branch_with_no_bug_ids_still_needs_the_marker()
    test_marker_docs_only_merge_still_needs_the_marker()
    test_marker_merge_of_a_branch_already_in_origin_main_passes()
    test_marker_guard_denies_when_the_marker_module_cannot_load()

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

    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
