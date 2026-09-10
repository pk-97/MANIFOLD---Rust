import json
from pathlib import Path
import tempfile
import unittest

import codex_regressions

ROOT = Path(__file__).resolve().parents[1]


class RegressionInventoryTests(unittest.TestCase):
    def test_all_five_categories_have_executable_references(self):
        rows = codex_regressions.inventory(ROOT)
        self.assertEqual(len(rows), 5)
        gpu = next(r for r in rows if r["kind"] == "gpu")
        self.assertIn("oracle_catches_wrong_fusion", gpu["tests"])
        self.assertNotIn("nextest", str(gpu["commands"]))

    def test_selection_is_subsystem_scoped(self):
        rows = codex_regressions.inventory(ROOT, ["crates/manifold-playback/src/midi_input.rs"])
        self.assertEqual([r["name"] for r in rows], ["midi-ordering-channel"])

    def test_removed_or_disabled_test_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "scripts").mkdir()
            (root / "check.rs").write_text("fn formerly_a_test() {}")
            (root / "scripts/codex_regressions.json").write_text(json.dumps({
                "fixture": {"source": "check.rs", "kind": "cpu", "package": "x", "target": "lib", "tests": ["formerly_a_test"], "triggers": []}}))
            with self.assertRaisesRegex(ValueError, "missing executable test"):
                codex_regressions.inventory(root)


if __name__ == "__main__":
    unittest.main()
