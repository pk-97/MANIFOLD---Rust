#!/usr/bin/env python3
"""Exercise denials and normal workflows against disposable git repositories."""
import importlib.util
import json
import re
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("guard", Path(__file__).with_name("guard.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)
REAL_ROOT = Path(__file__).resolve().parents[2]


class Guards(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="codex-guard-test-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        def run(*args):
            subprocess.run(["git", "-C", str(self.root), *args], check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        run("init", "-b", "main")
        (self.root / "AGENTS.md").write_text("test\n")
        run("add", "AGENTS.md")
        run("-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-m", "init")
        run("update-ref", "refs/remotes/origin/main", "HEAD")
        self.slot = self.root / ".claude/worktrees/slot-1"
        self.slot.parent.mkdir(parents=True)
        run("worktree", "add", "-b", "lane/test", str(self.slot))
        (self.slot / ".worktree-lease.json").write_text("{}")
        self.paths = guard.load("test_cc_paths", REAL_ROOT / ".claude/hooks/worktree-guard.py")
        self.shell = guard.load("test_cc_shell", REAL_ROOT / ".claude/hooks/preToolUseBash.py")
        self.paths._PROJECT_DIR = self.root
        self.paths._WORKTREES_DIR = self.root / ".claude/worktrees"
        self.paths._CLAUDE_DIR = self.root / ".claude"
        self.paths._DOCS_DIR = self.root / "docs"
        self.root_patch = patch.object(guard, "ROOT", self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)
        loader = patch.object(guard, "load", side_effect=lambda name, path: self.paths if name == "cc_paths" else self.shell)
        loader.start()
        self.addCleanup(loader.stop)
        state = patch.object(guard, "state_path", return_value=self.root / "scope.json")
        state.start()
        self.addCleanup(state.stop)
        permits = patch.object(guard, "permit_path", return_value=self.root / "permits.json")
        permits.start()
        self.addCleanup(permits.stop)

    def event(self, tool, args, worker=False, cwd=None):
        return {"hook_event_name": "PreToolUse", "session_id": "test",
                "model": "gpt-5.6-luna" if worker else "gpt-6-astra", "cwd": str(cwd or self.root),
                "tool_name": tool, "tool_input": args}

    def shell_call(self, command, worker=False, cwd=None):
        return guard.evaluate(self.event("Bash", {"command": command}, worker, cwd))

    def patch_call(self, paths, worker=False):
        command = "*** Begin Patch\n" + "\n".join(paths) + "\n*** End Patch"
        return guard.evaluate(self.event("apply_patch", {"command": command}, worker))

    def test_dispatch_requires_no_registration(self):
        for tool in ("spawn_agent", "collaborationspawn_agent", "Agent"):
            for worker in (False, True):
                self.assertIsNone(guard.evaluate(self.event(tool, {}, worker)))
        self.assertFalse((self.root / "scope.json").exists())

    def test_cargo_fmt_file_argument_still_formats_workspace(self):
        for command in ("cargo fmt", "cargo fmt -- crates/manifold-playback/src/midi_input.rs",
                        "cargo +nightly fmt --all", "env RUST_LOG=warn cargo fmt --manifest-path Cargo.toml",
                        "with-build-lock.sh cargo fmt"):
            with self.subTest(command=command):
                self.assertIn("cargo fmt", self.shell_call(command))
        self.assertIsNone(self.shell_call("cargo fmt --check"))
        self.assertIsNone(self.shell_call("rustfmt --config skip_children=true crates/manifold-playback/src/midi_input.rs"))
        self.assertIsNone(self.shell_call("printf '%s' 'cargo fmt'"))

    def test_luna_without_registration_uses_normal_tools(self):
        for command in ("pwd", "git status", "python3 -c 'print(1)'",
                        "sed -n '1p' AGENTS.md", "cat a > b", "cargo run",
                        "git add -- AGENTS.md", "git commit -m fix -- AGENTS.md",
                        "git push origin main", "scripts/land_branch.py lane/test"):
            with self.subTest(command=command):
                self.assertIsNone(self.shell_call(command, True))
        for tool in ("exec_command", "Bash"):
            self.assertIsNone(guard.evaluate(self.event(tool, {"cmd": "pwd"}, True)))
        self.assertIsNone(self.patch_call([f"*** Add File: {self.slot / 'src/a.rs'}"], True))
        self.assertFalse((self.root / "scope.json").exists())

    def test_luna_retains_shared_protections(self):
        for command in ("git reset --hard", "git add .", "git push --force origin main", "cargo test"):
            self.assertTrue(self.shell_call(command, True))
        for name in ("crates/app.rs", "CLAUDE.md", ".claude/settings.json"):
            self.assertTrue(self.patch_call([f"*** Update File: {name}"], True))
        self.assertTrue(self.patch_call([f"*** Update File: {self.slot / 'src/a.rs'}",
                                         "*** Move to: crates/app.rs"], True))

    def test_main_and_cc_protection(self):
        for name in ("crates/app.rs", "CLAUDE.md", ".claude/settings.json"):
            self.assertTrue(self.patch_call([f"*** Update File: {name}"]))
        for name in ("AGENTS.md", ".codex/config.toml", "docs/TEST.md"):
            self.assertIsNone(self.patch_call([f"*** Update File: {name}"]))

    def test_git_unsafe_forms_and_compounds(self):
        for command in ("git reset --hard", "git clean -fd", "git push --force origin main",
                        "git add .", "git add -A", "git commit -am fix", "git commit -m fix -- .",
                        "git worktree add /tmp/unpooled", "git branch -D feature",
                        "git status;git reset --hard", "git status\ngit reset --hard", "git status && git add .",
                        "git merge --no-ff lane/test", "git push origin HEAD:main"):
            with self.subTest(command=command):
                self.assertTrue(self.shell_call(command))

    def test_normal_git_and_gate_path(self):
        for command in ("git status --short", "git diff --check", "git add -- AGENTS.md",
                        "git commit -m 'docs only' -- AGENTS.md", "git push origin main",
                        "scripts/land_branch.py lane/test --worktree /tmp/slot --message fix --lead astra"):
            with self.subTest(command=command):
                self.assertIsNone(self.shell_call(command))
        self.assertTrue(self.shell_call("git commit -m fix -- crates/app.rs"))
        self.assertTrue(self.shell_call("scripts/land_branch.py lane/test --named-red BUG-x --reason skip"))
        self.assertTrue(self.shell_call("git push origin main", cwd=self.slot))

    def test_focused_attempt_budget_and_read_only_calls(self):
        command = "cargo test -p manifold-ui mapping"
        self.assertIsNone(self.shell_call(command))
        self.assertIsNone(self.shell_call(command))
        self.assertIn("Execution budget", self.shell_call(command))
        self.assertIsNone(self.shell_call("git status --short"))
        self.assertIsNone(self.shell_call(command, cwd=self.slot))

    def test_prepared_merge_commit_is_limited_to_clean_slot_index(self):
        def run(cwd, *args):
            subprocess.run(["git", "-C", str(cwd), "-c", "user.name=Test",
                            "-c", "user.email=test@example.test", *args], check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.assertTrue(self.shell_call("git commit --no-edit", cwd=self.slot))
        (self.root / "source.txt").write_text("new content\n")
        run(self.root, "add", "source.txt")
        run(self.root, "commit", "-m", "source change")
        run(self.slot, "merge", "--no-ff", "--no-commit", "main")
        self.assertIsNone(self.shell_call("git commit --no-edit", cwd=self.slot))
        for command in ("git commit --no-edit", "git commit -am merge", "git commit --amend"):
            self.assertTrue(self.shell_call(command, cwd=self.root))
        self.assertTrue(self.shell_call("git commit -am merge", cwd=self.slot))
        (self.slot / "source.txt").write_text("unreviewed changes\n")
        self.assertTrue(self.shell_call("git commit --no-edit", cwd=self.slot))
        run(self.slot, "add", "source.txt")
        self.assertIsNone(self.shell_call("git commit --no-edit", cwd=self.slot))
        with patch.object(guard, "git", side_effect=subprocess.CalledProcessError(1, "git")):
            self.assertFalse(guard.prepared_slot_merge(self.slot, ["--no-edit"]))

    def test_broad_wrapped_and_visual_checks_need_exception(self):
        for command in ("cargo test", "cargo test -p manifold-ui --workspace",
                        "RUSTC_WRAPPER= cargo clippy --workspace",
                        "bash .claude/scripts/with-build-lock.sh cargo test --workspace",
                        "python3 -B scripts/trunk_health.py",
                        "python3 scripts/launch_live_ui.py", "cargo xtask perf-soak"):
            with self.subTest(command=command):
                self.assertIn("Execution budget", self.shell_call(command))
        self.assertIsNone(self.shell_call("python3 scripts/landing_gate.py"))
        self.assertIsNone(self.shell_call("rg 'cargo test' docs"))

    def test_budget_uses_executed_program_not_argument_basenames(self):
        for command in (
                "cat scripts/trunk_health.py",
                "rg trunk_health.py logs/permit-command.txt",
                "sed -n '1p' scripts/feature_matrix.py",
                "printf 'python3 scripts/gpu_proofs_gate.py' > permit.log"):
            with self.subTest(command=command):
                self.assertIsNone(self.shell_call(command))
        for command in ("python3 scripts/trunk_health.py",
                        "bash .claude/scripts/with-build-lock.sh cargo test --workspace"):
            with self.subTest(command=command):
                self.assertTrue(self.shell_call(command))
        self.assertIn("Execution budget", self.shell_call("cargo build --features perf-soak"))
        wrapped = "env FOO=bar python3 -B scripts/gpu_proofs_gate.py"
        self.assertIsNone(self.shell_call(wrapped))
        self.assertIsNone(self.shell_call(wrapped))
        self.assertTrue(self.shell_call(wrapped))
        self.assertIsNone(self.shell_call("cargo run --features perf-soak"))
        self.assertTrue(self.shell_call("cargo xtask perf-soak"))

    def test_execution_wrappers_and_redirections_preserve_limits(self):
        for command in (
                "env -i FOO=bar python3 -B scripts/trunk_health.py",
                "env -u FOO cargo +stable test --workspace",
                "cargo --config config.toml test --workspace",
                ".claude/scripts/with-build-lock.sh cargo test --workspace",
                "bash -lc 'cargo test --workspace'",
                "cat README.md | python3 scripts/trunk_health.py",
                "cat README.md; cargo test --workspace",
                "for i in 1 2; do cargo test --workspace; done",
                "cargo run -p manifold-app --features perf-soak -- perf-soak show.manifold",
                "cargo run -p manifold-app --features perf-soak -- rt-capture show.manifold"):
            with self.subTest(command=command):
                self.assertIn("broad", list(guard.expensive_checks(command)))
        for command in (
                "cat README.md > scripts/trunk_health.py",
                "cat < scripts/trunk_health.py",
                "python3 -c 'print(\"trunk_health.py\")'",
                "command -v cargo",
                "python3 .codex/hooks/guard.py permit-check --command 'cargo test --workspace'",
                "rg 'scripts/trunk_health.py' README.md"):
            with self.subTest(command=command):
                self.assertEqual([], list(guard.expensive_checks(command)))
        self.assertEqual(["focused"], list(guard.expensive_checks(
            "cargo build -p manifold-app --features perf-soak")))

    def test_required_gpu_proof_gate_has_normal_bounded_attempts(self):
        command = "python3 -B scripts/gpu_proofs_gate.py"
        self.assertIsNone(self.shell_call(command))
        self.assertIsNone(self.shell_call(command))
        self.assertIn("Execution budget", self.shell_call(command))

    def test_exception_is_exact_bounded_and_expiring(self):
        command = "cargo test --workspace"
        guard.permit_check("test", command, str(self.root), "Required regression", 1)
        self.assertTrue(self.shell_call(command, cwd=self.slot))
        self.assertTrue(self.shell_call(command + " --lib"))
        self.assertIsNone(self.shell_call(command))
        self.assertTrue(self.shell_call(command))
        with patch.object(guard.time, "time", return_value=100):
            guard.permit_check("test", command, str(self.root), "New evidence", 1)
        with patch.object(guard.time, "time", return_value=1901):
            self.assertTrue(self.shell_call(command))
        with self.assertRaises(ValueError):
            guard.permit_check("test", command, str(self.root), "", 1)

    def test_exception_survives_hook_session_alias_mismatch(self):
        command = "cargo test --workspace"
        guard.permit_check("cli-thread-id", command, str(self.slot), "Required regression", 1)
        event = self.event("Bash", {"command": command, "workdir": str(self.root)})
        event["session_id"] = "desktop-hook-session-id"
        self.assertIsNone(guard.evaluate(event))
        self.assertIn("Execution budget", guard.evaluate(event))

    def test_missing_workdir_fallback_refuses_ambiguous_permits(self):
        command = "cargo test --workspace"
        guard.permit_check("one", command, str(self.slot), "Slot regression", 1)
        guard.permit_check("two", command, str(self.root / "other-slot"), "Other regression", 1)
        self.assertIn("Execution budget", self.shell_call(command))

    def test_exec_command_uses_budget(self):
        matcher = json.loads((REAL_ROOT / ".codex/hooks.json").read_text())["hooks"]["PreToolUse"][0]["matcher"]
        self.assertIsNotNone(re.search(matcher, "exec_command"))
        self.assertTrue(guard.evaluate(self.event("exec_command", {"cmd": "cargo test"})))


if __name__ == "__main__":
    unittest.main()
