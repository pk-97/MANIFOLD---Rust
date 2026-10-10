#!/usr/bin/env python3
"""The water crate edge census holds at zero: no water file reaches up or
sideways across the crate lines of docs/WATER_CRATES_DESIGN.md section 3."""
import contextlib
import io
import tempfile
import unittest
from pathlib import Path

import water_crate_edges as edges


class WaterCrateEdgeTests(unittest.TestCase):
    def test_the_crate_has_no_cut_rows_and_every_file_is_placed(self):
        files, prod, _ = edges.census(edges.DEFAULT_SRC)
        self.assertEqual(sorted(f for f, a in files.items() if a == "UNPLACED"), [])
        cuts = {k: sorted(v) for k, v in prod.items() if edges.is_cut(*k)}
        self.assertEqual(cuts, {}, "a water file reaches up or sideways; run scripts/dev.py water-edges")

    def test_a_sideways_reach_is_a_cut_and_fails_check(self):
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp)
            (src / "primitives").mkdir()
            (src / "primitives" / "gpu_flip_step.rs").write_text("pub fn face_bytes() {}\n")
            (src / "primitives" / "whitewater_step.rs").write_text("use crate::primitives::gpu_flip_step::face_bytes;\n")
            (src / "liquid.rs").write_text("pub const X: u32 = 1;\n")
            (src / "primitives" / "matter_fill.rs").write_text("use crate::liquid::X;\n")
            _, prod, _ = edges.census(src)
            self.assertTrue(edges.is_cut("whitewater", "gpuflip"))
            self.assertIn(("whitewater", "gpuflip"), prod)
            self.assertFalse(edges.is_cut("matter", "liquid"))
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(edges.main([str(src), "--check"]), 1)


if __name__ == "__main__":
    unittest.main()
