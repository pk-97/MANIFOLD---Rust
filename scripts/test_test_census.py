#!/usr/bin/env python3
"""Canned nextest JSON and CLI checks; never invokes Cargo."""
from collections import Counter
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import test_census as census


def listing(crate, name, tests, kind="lib"):
    return {"rust-suites": {f"{crate}::{name}": {
        "binary-name": name, "kind": kind, "package-id": crate,
        "testcases": {test: {"ignored": False, "filter-match": {"status": "matches"}}
                      for test in tests},
    }}}


class CensusTests(unittest.TestCase):
    def compare(self, before, after, *maps):
        with tempfile.TemporaryDirectory() as directory:
            paths = [Path(directory) / name for name in ("before.json", "after.json")]
            for path, data in zip(paths, (before, after)):
                path.write_text(json.dumps({"version": 1, "identities": census.identities(data)}))
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                result = census.main(["diff", *map(str, paths), *maps])
            return result, output.getvalue()

    def test_cross_crate_move(self):
        code, out = self.compare(
            listing("renderer", "renderer", ["ui_renderer::tests::foo"]),
            listing("ui-paint", "ui_paint", ["ui_renderer::tests::foo"]))
        self.assertEqual(code, 0, out)

    def test_folded_integration_binary(self):
        code, out = self.compare(
            listing("renderer", "foo", ["nested::roundtrip"], "test"),
            listing("nodes", "main", ["foo::nested::roundtrip"], "test"))
        self.assertEqual(code, 0, out)

    def test_deleted_and_added_test(self):
        code, out = self.compare(listing("a", "a", ["lost", "kept"]),
                                 listing("b", "b", ["new", "kept"]))
        self.assertEqual(code, 1)
        self.assertIn("vanished lost: 1 (before 1, after 0)", out)
        self.assertIn("appeared new: 1 (before 0, after 1)", out)

    def test_explicit_prefix_map(self):
        before = listing("a", "a", ["node_graph::tests::foo"])
        after = listing("b", "b", ["tests::foo"])
        self.assertEqual(self.compare(before, after)[0], 1)
        self.assertEqual(self.compare(before, after, "--map", "node_graph::=")[0], 0)

    def test_duplicate_counts_and_map_collisions(self):
        data = listing("a", "a", ["tests::foo"])
        data["rust-suites"].update(listing("b", "b", ["tests::foo"])["rust-suites"])
        self.assertEqual(census.identities(data), Counter({"tests::foo": 2}))
        code, out = self.compare(data, listing("a", "a", ["tests::foo"]))
        self.assertEqual(code, 1)
        self.assertIn("before 2, after 1", out)
        self.assertEqual(census.renamed(Counter({"a::x": 1, "x": 1}), [("a::", "")]),
                         Counter({"x": 2}))

    def test_record_command_and_output(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "census.json"
            with patch.object(census.subprocess, "run") as run:
                run.return_value.returncode = 0
                run.return_value.stderr = ""
                run.return_value.stdout = json.dumps(listing("a", "a", ["foo"]))
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(census.main(["record", str(path)]), 0)
                    self.assertIn("--workspace", run.call_args.args[0])
                    self.assertEqual(census.main(["record", str(path), "-p", "a", "-p", "b",
                                                  "--features", "x", "--features", "y"]), 0)
                command = run.call_args.args[0]
                self.assertNotIn("--workspace", command)
                self.assertEqual(command[-8:], ["-p", "a", "-p", "b", "--features", "x", "--features", "y"])
                self.assertEqual(run.call_args.kwargs["env"]["CARGO_BUILD_JOBS"], "4")
                self.assertEqual(census.read_census(path), Counter({"foo": 1}))


if __name__ == "__main__":
    unittest.main()
