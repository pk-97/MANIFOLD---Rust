import json, tempfile, unittest, sys, types
from pathlib import Path
import codex_prepare

ROOT = Path(__file__).resolve().parents[1]

class PrepareTests(unittest.TestCase):
    def test_mapping_union_and_brief(self):
        r = codex_prepare.build(ROOT, ["crates/manifold-playback/src/transport_sync.rs", "crates/manifold-ui/src/param_surface.rs"], "fix sync", "observed", "tests pass")
        self.assertEqual({s["name"] for s in r["subsystems"]}, {"transport-midi", "parameter-ui"})
        self.assertIn("Do not delegate", r["instructions"])
        self.assertIn("docs/CORE_ENGINE_MAP.md", r["references"])

    def test_unknown_is_explicit(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as d:
            p = Path(d) / "unknown.rs"; p.touch()
            r = codex_prepare.build(ROOT, [str(p)], "task", "", "accept")
        self.assertEqual(r["subsystems"], [])

    def test_traversal_and_missing_reference(self):
        with self.assertRaises(ValueError): codex_prepare.build(ROOT, ["../secret"], "t", "f", "a")
        old = codex_prepare._load_manifest
        codex_prepare._load_manifest = lambda: {"subsystems": [{"name":"x", "prefixes":["scripts/"], "entry_points":["missing"], "docs":[], "invariants":"x"}]}
        try:
            with self.assertRaisesRegex(ValueError, "manifest reference missing"): codex_prepare.build(ROOT, ["scripts/codex_prepare.py"], "t", "f", "a")
        finally: codex_prepare._load_manifest = old

    def test_new_file_and_exact_file_prefix(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as d:
            new = Path(d) / "new.rs"
            self.assertEqual(codex_prepare.select_context(ROOT, [str(new)])["subsystems"], [])
        self.assertEqual(codex_prepare.select_context(ROOT, ["crates/manifold-core/src/midi.rs.bak"])["subsystems"], [])

    def test_cli_emits_real_commands(self):
        import contextlib, io
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(codex_prepare.main(["--repo", str(ROOT), "--path", "scripts/codex_prepare.py", "--task", "t", "--findings", "f", "--acceptance", "a"]), 0)
        self.assertIn("test_codex_prepare.py", output.getvalue())
        self.assertIn("Check (cwd", output.getvalue())

    def test_all_manifest_references_exist(self):
        for subsystem in codex_prepare._load_manifest()["subsystems"]:
            for reference in subsystem["entry_points"] + subsystem["docs"]:
                self.assertTrue((ROOT / reference).is_file(), reference)

if __name__ == "__main__": unittest.main()
