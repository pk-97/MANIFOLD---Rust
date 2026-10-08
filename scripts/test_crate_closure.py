#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path

import crate_closure


class ClosureSourceTests(unittest.TestCase):
    def test_production_text_selects_production_testkit_arm(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.rs"
            source.write_text(
                'testkit_visible! {\n'
                '    testkit { pub struct TestOnly; }\n'
                '    production { pub struct ProductionOnly; }\n'
                '}\n'
            )
            text = crate_closure.production_text(str(source))
            self.assertIn("ProductionOnly", text)
            self.assertNotIn("TestOnly", text)


if __name__ == "__main__":
    unittest.main()
