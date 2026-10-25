#!/usr/bin/env python3
"""Fake-only tests for feature-matrix coverage."""

import sys
import unittest
from unittest.mock import patch

import feature_matrix


class FeatureMatrixTests(unittest.TestCase):
    def test_feature_coverage_is_metadata_only(self):
        with patch.object(feature_matrix, "workspace_features", return_value=[("one", "feature-one")]), \
                patch.object(feature_matrix.subprocess, "run",
                             side_effect=AssertionError("coverage must not build")), \
                patch.object(sys, "argv", ["feature_matrix.py", "--check-coverage"]):
            self.assertEqual(feature_matrix.main(), 0)


if __name__ == "__main__":
    unittest.main()
