#!/usr/bin/env python3
"""Fake-only tests for feature-matrix build admission."""

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import feature_matrix


class FeatureMatrixTests(unittest.TestCase):
    def test_reservation_during_matrix_defers_before_next_build(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            launched = []

            class Child:
                returncode = 0

                def communicate(self, timeout=None):
                    # The first fake build creates the reservation after its
                    # admission; the next matrix row must see it atomically.
                    feature_matrix.gpu_queue.reserve("campaign", "window", 60,
                                                     directory=directory)
                    return "", ""

            def fake_popen(command, **kwargs):
                launched.append(command)
                return Child()

            with patch.object(feature_matrix, "workspace_features",
                              return_value=[("one", "feature-one"), ("two", "feature-two")]), \
                    patch.object(feature_matrix.gpu_queue, "queue_dir", return_value=directory), \
                    patch.object(feature_matrix.gpu_queue.subprocess, "Popen",
                                 side_effect=fake_popen), \
                    patch.object(sys, "argv", ["feature_matrix.py"]), \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(feature_matrix.main(), 0)
            feature_matrix.gpu_queue.clear_reservation(directory)

        self.assertEqual(len(launched), 1)
        self.assertEqual(launched[0][0:2], ["cargo", "clippy"])
        self.assertIn("DEFER", output.getvalue())

    def test_feature_coverage_is_metadata_only(self):
        with patch.object(feature_matrix, "workspace_features", return_value=[("one", "feature-one")]), \
                patch.object(feature_matrix.gpu_queue, "run_admitted",
                             side_effect=AssertionError("coverage must not build")), \
                patch.object(sys, "argv", ["feature_matrix.py", "--check-coverage"]):
            self.assertEqual(feature_matrix.main(), 0)


if __name__ == "__main__":
    unittest.main()
