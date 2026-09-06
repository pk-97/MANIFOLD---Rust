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

    def event(self, tool, args, worker=False, cwd=None):
        return {"hook_event_name": "PreToolUse", "session_id": "test",
                "model": guard.LUNA if worker else "gpt-6-astra", "cwd": str(cwd or self.root),
                "tool_name": tool, "tool_input": args}

    def spawn(self, files=None, **overrides):
        scope = {"worktree": str(self.slot), "files": files if files is not None else ["src/a.rs"]}
        args = {"model": guard.LUNA, "reasoning_effort": "low",
                "message": "Implement the decided fix.\nMANIFOLD_SCOPE: " + json.dumps(scope)}
        args.update(overrides)
        return guard.evaluate(self.event("spawn_agent", args))

    def shell_call(self, command, worker=False, cwd=None):
        return guard.evaluate(self.event("Bash", {"command": command}, worker, cwd))

    def patch_call(self, paths, worker=False):
        command = "*** Begin Patch\n" + "\n".join(paths) + "\n*** End Patch"
        return guard.evaluate(self.event("apply_patch", {"command": command}, worker))

    def test_spawn_requires_model_effort_and_scope(self):
        self.assertIsNone(self.spawn())
        self.assertTrue(self.spawn(model=None))
        self.assertTrue(self.spawn(reasoning_effort="high"))
        with self.assertRaises(ValueError):
            self.spawn(message="unscoped")
        self.assertTrue(guard.evaluate(self.event("spawn_agent", {}, True)))

    def test_native_dispatch_registers_read_only_scope(self):
        config = json.loads((REAL_ROOT / ".codex/hooks.json").read_text())
        matcher = config["hooks"]["PreToolUse"][0]["matcher"]
        for tool in ("spawn_agent", "collaborationspawn_agent", "Agent"):
            with self.subTest(tool=tool):
                self.assertIsNotNone(re.search(matcher, tool))
                state = self.root / "scope.json"
                state.unlink(missing_ok=True)
                args = {"model": guard.LUNA, "reasoning_effort": "low",
                        "message": "MANIFOLD_SCOPE: " + json.dumps(
                            {"worktree": str(self.root), "files": []})}
                if tool == "collaborationspawn_agent":
                    guard.prepare_lane("test", "readonly", str(self.root), [])
                    args.update(task_name="readonly", message="gAAAA_encrypted_brief")
                self.assertIsNone(guard.evaluate(self.event(tool, args)))
                self.assertEqual(json.loads(state.read_text()),
                                 {"worktree": str(self.root), "files": []})
                for command in ("pwd", "git rev-parse --short HEAD", "cat .codex/README.md"):
                    self.assertIsNone(self.shell_call(command, True))
                self.assertTrue(self.patch_call(["*** Add File: forbidden.rs"], True))
                self.assertTrue(self.shell_call("cargo test", True))
                self.assertTrue(guard.evaluate(self.event(tool, args, True)))

    def test_native_scope_is_required_bound_expiring_and_single_use(self):
        args = {"model": guard.LUNA, "reasoning_effort": "low",
                "task_name": "readonly", "message": "gAAAA_encrypted_brief"}
        event = self.event("collaborationspawn_agent", args)
        with self.assertRaisesRegex(ValueError, "Prepare this lane"):
            guard.evaluate(event)
        guard.prepare_lane("test", "different_task", str(self.root), [])
        with self.assertRaisesRegex(ValueError, "task name"):
            guard.evaluate(event)
        with patch.object(guard.time, "time", return_value=100):
            guard.prepare_lane("test", "readonly", str(self.root), [])
        with patch.object(guard.time, "time", return_value=701):
            with self.assertRaisesRegex(ValueError, "expired"):
                guard.evaluate(event)
        guard.prepare_lane("test", "readonly", str(self.root), [])
        self.assertIsNone(guard.evaluate(event))
        with self.assertRaisesRegex(ValueError, "Prepare this lane"):
            guard.evaluate(event)

    def test_native_scope_revalidates_lease_at_dispatch(self):
        guard.prepare_lane("test", "write_lane", str(self.slot), ["src/a.rs"])
        (self.slot / ".worktree-lease.json").unlink()
        args = {"model": guard.LUNA, "reasoning_effort": "low",
                "task_name": "write_lane", "message": "gAAAA_encrypted_brief"}
        with self.assertRaisesRegex(ValueError, "Acquire the slot"):
            guard.evaluate(self.event("collaborationspawn_agent", args))

    def test_scope_rejects_escape_and_unleased_slot(self):
        for name in ("../a.rs", "/tmp/a.rs", "src/*.rs", ".codex/config.toml"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.spawn([name])
        (self.slot / "outside").symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.spawn(["outside/AGENTS.md"])
        (self.slot / ".worktree-lease.json").unlink()
        with self.assertRaises(ValueError):
            self.spawn()

    def test_main_and_cc_protection(self):
        for name in ("crates/app.rs", "CLAUDE.md", ".claude/settings.json"):
            self.assertTrue(self.patch_call([f"*** Update File: {name}"]))
        for name in ("AGENTS.md", ".codex/config.toml", "docs/TEST.md"):
            self.assertIsNone(self.patch_call([f"*** Update File: {name}"]))

    def test_lane_scope_includes_rename_destination(self):
        self.spawn()
        src = self.slot / "src/a.rs"
        self.assertIsNone(self.patch_call([f"*** Add File: {src}"], True))
        self.assertTrue(self.patch_call([f"*** Update File: {src}", "*** Move to: /tmp/escape.rs"], True))
        self.assertTrue(self.patch_call(["*** Delete File: AGENTS.md"], True))
        self.spawn([])
        self.assertTrue(self.patch_call([f"*** Add File: {src}"], True))

    def test_symlink_changed_after_dispatch(self):
        self.spawn()
        (self.slot / "src").symlink_to(self.root, target_is_directory=True)
        self.assertTrue(self.patch_call([f"*** Add File: {self.slot / 'src/a.rs'}"], True))

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

    def test_worker_shell(self):
        self.spawn()
        for command in ("git commit -m fix -- src/a.rs", "git push origin main", "rm src/a.rs",
                        "python3 -c 'print(1)'", "cat a > b", "bash", "cargo run",
                        "scripts/land_branch.py lane/test"):
            with self.subTest(command=command):
                self.assertTrue(self.shell_call(command, True, self.slot))
        self.assertIsNone(self.shell_call("cargo clippy -p manifold-core -- -D warnings", True, self.slot))
        self.assertIsNone(self.shell_call("rg 'test' src", True, self.slot))
        self.assertTrue(self.shell_call("cargo test", True))


if __name__ == "__main__":
    unittest.main()
