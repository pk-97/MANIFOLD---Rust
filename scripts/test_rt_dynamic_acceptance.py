#!/usr/bin/env python3
"""Static self-tests for the RT dynamic acceptance dispatcher.

These tests exercise the runner's evidence policy without invoking Cargo,
Metal, ffmpeg, or an export.  In particular, they protect the two failure
modes that can otherwise make a green report meaningless: an acceptance test
being ignored, and correctness groups being run in separate gate processes.
"""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "rt_dynamic_acceptance", HERE / "rt_dynamic_acceptance.py"
)
runner = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(runner)


class AcceptanceRunnerTests(unittest.TestCase):
    @staticmethod
    def _static_reports(current_p95=3.9):
        baseline = {
            "status": "measured_static_rt",
            "sourceCommit": "713cd9bb",
            "hardware": {"gpu": "Apple M4 Max"},
            "settings": {"measuredFrames": 120, "resolution": [1280, 720]},
            "fixture": {"sha256": "fixture-hash"},
            "gpuFrame": {"count": 120, "p95Ms": 3.882708348},
            "rtDispatchWitness": {"observed": True, "channels": [{"label": "refl_raw"}]},
        }
        current = {
            "hardware": {"gpu": "Apple M4 Max"},
            "referenceProjectHash": "fixture-hash",
            "productionConfigurations": [{
                "name": "static_rt",
                "status": "measured_production_frame",
                "measuredFrames": 120,
                "resolution": [1280, 720],
                "anyDispatch": True,
                "dispatchFrames": 120,
                "gpuFrame": {"count": 120, "p95Ms": current_p95},
            }],
        }
        return baseline, current

    def test_static_baseline_match_passes(self):
        baseline, current = self._static_reports()
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "static-baseline.json"
            path.write_text(json.dumps(baseline))
            result = runner.validate_static_baseline(path, current)
        self.assertEqual(result["status"], "pass")
        self.assertAlmostEqual(result["deltaMs"], 0.017291652, places=6)
        self.assertEqual(result["allowedDeltaMs"], 0.2)

    def test_static_baseline_gpu_mismatch_fails(self):
        baseline, current = self._static_reports()
        current["hardware"]["gpu"] = "Apple M3 Max"
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "static-baseline.json"
            path.write_text(json.dumps(baseline))
            result = runner.validate_static_baseline(path, current)
        self.assertEqual(result["status"], "fail")
        self.assertIn("GPU mismatch", result["reason"])

    def test_static_baseline_regression_fails(self):
        baseline, current = self._static_reports(current_p95=4.2)
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "static-baseline.json"
            path.write_text(json.dumps(baseline))
            result = runner.validate_static_baseline(path, current)
        self.assertEqual(result["status"], "fail")
        self.assertGreater(result["deltaMs"], result["allowedDeltaMs"])

    def test_static_baseline_malformed_fields_fail(self):
        for field in ("hardware", "gpuFrame", "settings", "fixture"):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as td:
                baseline, current = self._static_reports()
                baseline[field] = ["invalid"]
                path = Path(td) / "baseline.json"
                path.write_text(json.dumps(baseline))
                self.assertEqual(runner.validate_static_baseline(path, current)["status"], "fail")
        baseline, current = self._static_reports(current_p95=-1)
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "baseline.json"
            path.write_text(json.dumps(baseline))
            self.assertEqual(runner.validate_static_baseline(path, current)["status"], "fail")
            current["productionConfigurations"] = 7
            self.assertEqual(runner.validate_static_baseline(path, current)["status"], "fail")

    def test_static_baseline_missing_blocks(self):
        _, current = self._static_reports()
        result = runner.validate_static_baseline(None, current)
        self.assertEqual(result["status"], "blocked")
        self.assertIn("not provided", result["reason"])

    def test_perf_report_loader_rejects_missing_and_malformed_json(self):
        with tempfile.TemporaryDirectory() as td:
            missing = Path(td) / "missing.json"
            report, error = runner._load_json_report(missing, "reference")
            self.assertIsNone(report)
            self.assertIn("missing", error)

            malformed = Path(td) / "malformed.json"
            malformed.write_text("not-json")
            report, error = runner._load_json_report(malformed, "held-out")
            self.assertIsNone(report)
            self.assertIn("malformed", error)

    def test_perf_qualification_separates_live_budget_from_static_baseline(self):
        production = [
            {"name": name, "status": "measured_production_frame",
             "gpuFrame": {"p95Ms": 10.0}, "cpuEncode": {"p95Ms": 10.0}}
            for name in ("static_rt", "dynamic_selective_refit", "fresh_build_reference")
        ]
        reference = {
            "productionConfigurations": production,
            "configurations": [{"name": "dynamic_selective_refit",
                                 "gpuAsMaintenance": {"p95Ms": 1.0}}],
            "gates": {"staticRegression": "compare with saved baseline"},
        }
        held_out = {
            "status": "measured",
            "cpuWall": {"p95Ms": 999.0},
            "enforced": {
                "completeFrames": True,
                "noGpuFaults": True,
                "zeroPostWarmupBufferAllocations": True,
                "zeroPostWarmupAccelerationStructureAllocations": True,
                "rtDispatched": True,
                "profilingClean": True,
            },
        }
        reference_content = {
            "status": "measured",
            "cpuWall": {"p95Ms": 10.0},
            "enforced": held_out["enforced"].copy(),
        }
        qualification, problems = runner._perf_qualification(
            reference, held_out, reference_content)
        self.assertFalse(problems)
        self.assertEqual(qualification["correctness"], "pass")
        self.assertEqual(qualification["resource"], "pass")
        self.assertEqual(qualification["liveBudget"], "pass")
        self.assertEqual(qualification["staticBaseline"], "missing")
        self.assertEqual(qualification["heldOutContentFrameP95Ms"], 999.0)
        self.assertEqual(qualification["referenceContentFrameP95Ms"], 10.0)
        self.assertEqual(qualification["overall"], "blocked")
        reference["gates"]["staticRegression"] = {"passed": True}
        qualified, _ = runner._perf_qualification(reference, held_out, reference_content)
        self.assertEqual(qualified["overall"], "pass")

    def test_ignored_selected_test_is_a_reported_failure(self):
        tests = runner.parse_test_results(
            "test rt_dynamic_export_first_frame_and_state_steps ... ignored\n"
            "test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out"
        )
        self.assertEqual(len(tests), 1)
        self.assertEqual(tests[0]["status"], "fail")
        self.assertEqual(tests[0]["observed"], "ignored")
        self.assertEqual(tests[0]["required"], "selected non-ignored test")
        self.assertEqual(runner.parse_result_summary(
            "test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out"
        )["ignored"], 1)

    def test_gpu_correctness_groups_share_one_gate_invocation(self):
        groups = ["rt_dynamic_oracle", "rt_dynamic_fusion", "rt_dynamic_ordering"]
        listed = {f"rt_dynamic_{name.split('_')[-1]}::proof" for name in
                  ("oracle", "fusion", "ordering")}
        with tempfile.TemporaryDirectory() as td:
            artifact_dir = Path(td)
            calls = []

            def fake_run(cmd, cwd, log_path):
                calls.append(cmd)
                log_path.write_text(
                    "test rt_dynamic_oracle::proof ... ok\n"
                    "test rt_dynamic_fusion::proof ... ok\n"
                    "test rt_dynamic_ordering::proof ... ok\n"
                    "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
                )
                return 0, 0.1

            with patch.object(runner, "_list_gpu_tests", return_value=(listed, 0, {
                "cmd": "cargo test --list", "exitCode": 0,
                "durationSec": 0.0, "log": str(artifact_dir / "list.log")
            })), patch.object(runner, "run_streamed", side_effect=fake_run):
                rc, tests, metrics, commands = runner._mode_gpu_groups(
                    artifact_dir, artifact_dir / "Cargo.toml", artifact_dir, groups
                )

        self.assertEqual(rc, 0)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0].count("--filter"), 3)
        self.assertEqual(metrics["passed"], 3)
        self.assertEqual(len(tests), 3)
        self.assertEqual(len(commands), 2)  # listing + one batched gate

    def test_missing_group_blocks_but_present_groups_still_batch(self):
        groups = ["rt_dynamic_oracle", "rt_dynamic_fusion"]
        listed = {"rt_dynamic_oracle::proof"}
        with tempfile.TemporaryDirectory() as td:
            artifact_dir = Path(td)
            calls = []

            def fake_run(cmd, cwd, log_path):
                calls.append(cmd)
                log_path.write_text(
                    "test rt_dynamic_oracle::proof ... ok\n"
                    "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
                )
                return 0, 0.1

            with patch.object(runner, "_list_gpu_tests", return_value=(listed, 0, {
                "cmd": "cargo test --list", "exitCode": 0,
                "durationSec": 0.0, "log": str(artifact_dir / "list.log")
            })), patch.object(runner, "run_streamed", side_effect=fake_run):
                rc, tests, metrics, _ = runner._mode_gpu_groups(
                    artifact_dir, artifact_dir / "Cargo.toml", artifact_dir, groups
                )

        self.assertEqual(rc, 1)
        self.assertEqual(len(calls), 1)
        self.assertEqual(metrics["blocked"], 1)
        self.assertTrue(any(t["name"] == "rt_dynamic_fusion" and
                            t["status"] == "blocked" for t in tests))


if __name__ == "__main__":
    unittest.main()
