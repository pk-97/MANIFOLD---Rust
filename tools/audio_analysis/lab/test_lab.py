#!/usr/bin/env python3
"""The lab's invariant tests (docs/DETECTOR_LAB_DESIGN.md section 4). Run directly: test_lab.py"""
import hashlib
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

import numpy as np  # noqa: E402

from tools.audio_analysis.lab import audit, cache  # noqa: E402
from tools.audio_analysis.lab.registry import Song, sha256_file  # noqa: E402


def song(id_, path):
    return Song(id=id_, kind='project', source=dict(how='wip', paths=[str(path)]), source_sha256=(sha256_file(path),),
                sample_rate=48000, offset_s=0.0, proof='wip:proven', families=('snare',))


class LabTests(unittest.TestCase):
    def test_recycled_name_misses_cache(self):
        """The same song id over different audio must rebuild, never reuse (the linn_* trap)."""
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            a, b = d / 'a.npy', d / 'b.npy'
            np.save(a, np.zeros(10, np.float32))
            np.save(b, np.ones(10, np.float32))
            builds = []

            def build(v):
                return lambda: builds.append(v) or np.full(3, v, np.float16)
            first = cache.artifact(song('linn_1', a), 'spectrum', dict(bands=64), build(0), root=d / 'c')
            again = cache.artifact(song('linn_1', a), 'spectrum', dict(bands=64), build(9), root=d / 'c')
            other = cache.artifact(song('linn_1', b), 'spectrum', dict(bands=64), build(1), root=d / 'c')
            setting = cache.artifact(song('linn_1', a), 'spectrum', dict(bands=128), build(2), root=d / 'c')
            self.assertEqual(builds, [0, 1, 2])
            self.assertEqual((first[0], again[0], other[0], setting[0]), (0, 0, 1, 2))

    def test_recipe_conflict_exits(self):
        old = os.environ.get('KICK_GOAL_TRUTH')
        os.environ['KICK_GOAL_TRUTH'] = 'strict'
        try:
            with self.assertRaises(SystemExit):
                recipe.apply()
        finally:
            os.environ['KICK_GOAL_TRUTH'] = old if old is not None else 'v3'

    def test_audit_keeps_labels_sha(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / 'x_labels.json'
            p.write_text(json.dumps({'s': {'positives': [1.0]}}))
            before = hashlib.sha256(p.read_bytes()).hexdigest()
            side = audit.write_sidecar(p, {'s': {'pass': True}})
            self.assertEqual(hashlib.sha256(p.read_bytes()).hexdigest(), before)
            self.assertEqual(json.loads(side.read_text())['labels_sha256'], before)


if __name__ == '__main__':
    unittest.main()
