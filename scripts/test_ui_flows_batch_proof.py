#!/usr/bin/env python3
"""Batch proof: a flow whose verdict or artifacts change under batching is
red, a flow that differs between two solo runs is only listed, and a flow the
batch didn't actually run is red. No Cargo, no GPU: the fake binary from
test_run_ui_flows speaks the batch protocol and writes one PNG per flow."""
import contextlib
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import run_ui_flows
import test_run_ui_flows
import ui_flows_batch_proof as proof


class ProofTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.binary = self.tmp / "manifold"
        self.binary.write_text(test_run_ui_flows.FAKE_BINARY.format(python=sys.executable))
        self.binary.chmod(self.binary.stat().st_mode | stat.S_IXUSR)
        self.out = self.tmp / "snapshots"

    def tearDown(self):
        subprocess.run(["rm", "-rf", str(self.tmp)], check=False)

    def prove(self, flows, behaviour=None):
        (self.tmp / "manifest.json").write_text(json.dumps({"flows": flows}))
        (self.tmp / "behaviour.json").write_text(json.dumps(behaviour or {}))
        with contextlib.ExitStack() as stack:
            output = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(patch.object(run_ui_flows, "MANIFEST", str(self.tmp / "manifest.json")))
            stack.enter_context(patch.object(run_ui_flows, "build_binary", return_value=str(self.binary)))
            stack.enter_context(patch.object(proof.gpu_queue, "hold",
                                             side_effect=lambda *a, **k: contextlib.nullcontext()))
            stack.enter_context(patch.object(
                proof, "run_dir", side_effect=lambda name, scene: str(self.out / scene / f"run-{name}")))
            stack.enter_context(patch.object(sys, "argv", ["ui_flows_batch_proof.py"]))
            stack.enter_context(patch.dict(os.environ, {
                "FAKE_FLOWS": str(self.tmp / "behaviour.json"), "FAKE_LOG": str(self.tmp / "log"),
                "FAKE_OUT": str(self.out), "FAKE_RELEASE": str(self.tmp / "release")}))
            code = proof.main()
        return code, output.getvalue()

    def test_identical_modes_pass(self):
        stale = self.out / "timeline" / "run-flow-a" / "stale.png"
        stale.parent.mkdir(parents=True)
        stale.write_text("left over from last week")
        code, output = self.prove({"flow-a": "timeline", "flow-b": "inspector"},
                                  {"flow-b": "fail"})
        self.assertEqual(code, 0, output)
        self.assertIn("2/2 flows identical", output)
        self.assertFalse(stale.exists())

    def test_artifact_that_changes_under_batching_is_red(self):
        code, output = self.prove({"flow-a": "timeline", "flow-b": "inspector"},
                                  {"_art_flow-b": "batchdiff"})
        self.assertEqual(code, 1)
        self.assertIn("BATCH DIFF   flow-b: 01.png hash differs", output)
        self.assertIn("1 batch-induced", output)

    def test_verdict_that_changes_under_batching_is_red(self):
        code, output = self.prove({"flow-a": "timeline"}, {"flow-a": "batchfail"})
        self.assertEqual(code, 1)
        self.assertIn("BATCH DIFF   flow-a: exit 0 vs 1", output)

    def test_flow_unstable_on_its_own_is_listed_not_blamed(self):
        code, output = self.prove({"flow-a": "timeline"}, {"_art_flow-a": "unstable"})
        self.assertEqual(code, 0, output)
        self.assertIn("UNSTABLE     flow-a", output)

    def test_flow_the_batch_did_not_run_is_red(self):
        code, output = self.prove({"flow-a": "timeline", "flow-b": "inspector"},
                                  {"flow-a": "die"})
        self.assertEqual(code, 1)
        self.assertIn("RAN SOLO     flow-a: batch died, exit -9", output)


if __name__ == "__main__":
    unittest.main()
