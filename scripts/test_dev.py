#!/usr/bin/env python3
"""Every script is a verb or named internal; every verb points at something real."""
import contextlib
import io
import sys
import unittest
from pathlib import Path

import dev


class InventoryTests(unittest.TestCase):
    def test_every_script_is_a_verb_or_named_internal(self):
        on_disk = {p.name for p in dev.SCRIPTS.iterdir()
                   if p.suffix in {".py", ".sh"} and not p.name.startswith("test_")}
        listed = {t for _, _, t, _ in dev.VERBS if isinstance(t, str)} | dev.INTERNAL
        self.assertEqual(sorted(on_disk - listed), [],
                         "new script: add a verb to scripts/dev.py VERBS (or INTERNAL with a reason)")
        self.assertEqual(sorted(listed - on_disk), [], "verb points at a script that no longer exists")

    def test_cargo_verbs_name_real_targets(self):
        manifests = "\n".join(p.read_text() for p in (dev.ROOT / "crates").glob("*/Cargo.toml"))
        bins = {p.stem for p in (dev.ROOT / "crates").glob("*/src/bin/*.rs")}
        examples = {p.stem for p in (dev.ROOT / "crates").glob("*/examples/*.rs")}
        for _, verb, target, _ in dev.VERBS:
            if isinstance(target, str):
                continue
            with self.subTest(verb=verb):
                if "--example" in target:
                    self.assertIn(target[target.index("--example") + 1], examples)
                else:
                    name = target[target.index("--bin") + 1]
                    self.assertTrue(f'name = "{name}"' in manifests or name in bins, name)

    def test_verbs_are_unique_and_help_lists_them_all(self):
        verbs = [v for _, v, _, _ in dev.VERBS]
        self.assertEqual(len(verbs), len(set(verbs)))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(dev.main(["--help"]), 0)
        for verb in verbs:
            self.assertIn(f"  {verb} ", out.getvalue())

    def test_index_file_matches_the_catalog(self):
        # scripts/TOOLS.md is the zero-prompt way to read the inventory (cat).
        self.assertEqual(dev.INDEX.read_text(), dev.catalog(),
                         "regenerate: scripts/dev.py --write-index")

    def test_unknown_verb_points_at_help(self):
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(dev.main(["no-such-verb"]), 2)
        self.assertIn("--help", err.getvalue())

    def test_commands_resolve_to_absolute_scripts_or_queued_cargo(self):
        self.assertEqual(dev.command_for("gate", ["--repo", "x"]),
                         [str(dev.SCRIPTS / "landing_gate.py"), "--repo", "x"])
        capture = dev.command_for("capture", ["out"])
        self.assertEqual(capture[:2], [str(dev.SCRIPTS / "gpu_queue.py"), "--"])
        self.assertEqual(capture[-3:], ["fluid_capture", "--", "out"])
        self.assertEqual(dev.command_for("graph-tool", [])[0], "cargo")


if __name__ == "__main__":
    unittest.main()
