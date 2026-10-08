#!/usr/bin/env python3
"""Focused fake-file tests for watch_land.py."""

import contextlib
import io
import sys
import tempfile
import threading
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import watch_land  # noqa: E402
import landing_gate


class FakeWatch(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.log = self.root / "outer.log"
        self.now = 0.0

    def tearDown(self):
        self.directory.cleanup()

    def clock(self):
        return self.now

    def run_watch(self, *, alive=lambda _pid: True, hang=5.0, poll=1.0, kind="gate", step=None):
        def sleeper(seconds):
            self.now += seconds
            if step:
                step()

        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = watch_land.watch(
                123, self.log, hang, poll, kind,
                clock=self.clock, sleeper=sleeper, alive=alive,
            )
        return code, output.getvalue()

    def test_red_in_active_transcript(self):
        leg = self.root / "leg.log"
        self.log.write_text(f"[RUN] tests  (live transcript: {leg})\n")
        leg.write_text("[FAIL] test failed\n")

        code, output = self.run_watch()

        self.assertEqual(code, 1)
        self.assertIn("leg=tests failure=red", output)
        self.assertIn(str(leg), output)

    def test_outer_red_keeps_failed_leg_after_next_run(self):
        failed = self.root / 'failed.log'
        current = self.root / 'current.log'
        self.log.write_text(f'[RUN] clippy (live transcript: {failed})\n'
                            '[FAIL] clippy (1s)\n'
                            f'[RUN] tests (live transcript: {current})\n')
        code, output = self.run_watch()
        self.assertEqual(code, 1)
        self.assertIn('leg=clippy failure=red', output)
        self.assertIn(str(failed), output)

    def test_hang(self):
        leg = self.root / "leg.log"
        self.log.write_text(f"[RUN] tests  (live transcript: {leg})\n")
        leg.write_text("still running\n")

        code, output = self.run_watch(hang=2.0)

        self.assertEqual(code, 1)
        self.assertIn("leg=tests failure=hang", output)

    def test_outer_chatter_cannot_mask_active_hang(self):
        leg = self.root / "leg.log"
        self.log.write_text(f"[RUN] tests  (live transcript: {leg})\n")
        leg.write_text("started\n")
        chatter = 0

        def write_chatter():
            nonlocal chatter
            chatter += 1
            with self.log.open("a") as stream:
                stream.write(f"[land] progress update {chatter}\n")

        code, output = self.run_watch(hang=2.0, step=write_chatter)

        self.assertEqual(code, 1)
        self.assertIn("failure=hang", output)
        self.assertGreaterEqual(chatter, 2)

    def test_active_transcript_refresh_avoids_hang(self):
        leg = self.root / "leg.log"
        self.log.write_text(f"[RUN] tests  (live transcript: {leg})\n")
        leg.write_text("started\n")
        writes = 0

        def refresh():
            nonlocal writes
            if writes < 3:
                writes += 1
                with leg.open("a") as stream:
                    stream.write(f"progress {writes}\n")
            if writes == 3:
                self.log.write_text("landing gate: 1 passed, 0 failed, 0 skipped\n")

        code, output = self.run_watch(hang=2.0, step=refresh)

        self.assertEqual(code, 0)
        self.assertIn("failure=none", output)
        self.assertEqual(writes, 3)

    def test_actual_nextest_red_is_reported(self):
        leg = self.root / "leg.log"
        self.log.write_text(f"[RUN] tests  (live transcript: {leg})\n")
        leg.write_text("FAIL [ 0.01s] manifold::tests::broken\n")

        code, output = self.run_watch()

        self.assertEqual(code, 1)
        self.assertIn("failure=red", output)
        self.assertIn("manifold::tests::broken", output)

    def test_normal_land_waits_for_done(self):
        self.log.write_text("landing gate: 1 passed, 0 failed, 0 skipped\n")
        steps = 0

        def finish():
            nonlocal steps
            steps += 1
            if steps == 2:
                self.log.write_text("[land] DONE: branch landed. lead\n")

        code, output = self.run_watch(kind="land", step=finish)

        self.assertEqual(code, 0)
        self.assertIn("leg=landing failure=none", output)
        self.assertEqual(steps, 2)

    def test_new_land_step_wins_over_nested_gate_leg(self):
        gate = self.root / "gate.log"
        land_step = self.root / "land-step.log"
        gate.write_text(
            f"[RUN] old-gate-leg  (live transcript: {gate}.leg)\n"
            "[PASS] old-gate-leg (1s)\n"
        )
        self.log.write_text(
            f"[land] complete landing gate transcript: {gate}\n"
            f"[RUN] land-step  (live transcript: {land_step})\n"
        )
        land_step.write_text("merging\n")

        def finish():
            self.log.write_text(self.log.read_text() + "[land] DONE: branch landed. lead\n")

        code, output = self.run_watch(kind="land", step=finish)

        self.assertEqual(code, 0)
        self.assertIn("leg=landing failure=none", output)
        self.assertNotIn("old-gate-leg", output)

    def test_refusal(self):
        self.log.write_text("[INCOMPLETE] landing gate: cancelled by SIGTERM\n")

        code, output = self.run_watch()

        self.assertEqual(code, 1)
        self.assertIn("failure=refusal", output)
        self.assertIn("cancelled by SIGTERM", output)

    def test_unexpected_exit(self):
        self.log.write_text("[RUN] tests  (live transcript: /tmp/missing-leg.log)\n")

        code, output = self.run_watch(alive=lambda _pid: False)

        self.assertEqual(code, 1)
        self.assertIn("failure=unexpected-exit", output)

    def test_proof_output_reaches_watcher_before_fake_binary_exits(self):
        # Exercise both subprocess pipes: fake test -> proof runner -> gate's
        # live transcript. The fake cannot exit until the watcher reports.
        # run_gate is called directly: no Cargo, build admission or GPU lock.
        for marker, failure in (("test proof::broken ... FAILED", "red"),
                                ("GPU-PROOFS GATE: HUNG proof::stuck", "hang")):
            with self.subTest(marker=marker):
                release = self.root / 'release'
                release.unlink(missing_ok=True)
                live = self.root / 'gpu-proofs.log'
                live.unlink(missing_ok=True)
                self.log.write_text(f'[RUN] gpu-proofs (live transcript: {live})\n')
                fake = (
                    'import pathlib, sys, time\n'
                    f'sys.stdout.write({marker!r}); sys.stdout.flush()\n'
                    f'while not pathlib.Path({str(release)!r}).exists(): time.sleep(.01)\n'
                    'sys.exit(1)\n')
                runner = (
                    'import sys\nfrom pathlib import Path\n'
                    f'sys.path.insert(0, {str(Path(__file__).resolve().parent)!r})\n'
                    'import gpu_proofs_gate as g\n'
                    f'g.cargo_test_cmd = lambda *args: [sys.executable, "-u", "-c", {fake!r}]\n'
                    'sys.exit(g.run_gate(Path("/tmp/fake-Cargo.toml"), [], [])[0])\n')
                result = []
                worker = threading.Thread(target=lambda: result.append(landing_gate.run_cmd(
                    [sys.executable, '-u', '-c', runner], self.root, 10, live_log=live)))
                worker.start()
                try:
                    output = io.StringIO()
                    with contextlib.redirect_stdout(output):
                        code = watch_land.watch(123, self.log, hang_seconds=5,
                                                poll_seconds=.01, alive=lambda _: worker.is_alive())
                    self.assertEqual(code, 1)
                    self.assertIn(f'leg=gpu-proofs failure={failure}', output.getvalue())
                    self.assertIn(marker, output.getvalue())
                    self.assertTrue(worker.is_alive(), 'fake binary exited before watcher fired')
                    self.assertIn(marker, live.read_text())
                finally:
                    release.touch()
                    worker.join(15)
                self.assertFalse(worker.is_alive())
                self.assertEqual(result[0][0], 1)


if __name__ == "__main__":
    unittest.main()
