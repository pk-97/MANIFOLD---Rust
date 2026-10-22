#!/usr/bin/env python3
"""The citation scanner skips globs, placeholders and prose."""

import unittest

from docs_cited_paths import cited_paths


class CitedPathsTests(unittest.TestCase):
    def test_skips_globs_placeholders_and_prose(self):
        text = ("see `scripts/ui-flows/*.json`, `scripts/x.py`, docs/comments, "
                "`docs/landings/2026-07-12-batch-`, `crates/<crate>/src`, "
                "~/.claude/hooks/token-guard.py, /abs/docs/FOO.md, "
                "`scripts/dev.py`, (docs/AGENT_ROUTING.md). and scripts/ui-flows/")
        self.assertEqual(cited_paths(text),
                         ["scripts/dev.py", "docs/AGENT_ROUTING.md", "scripts/ui-flows/"])


if __name__ == "__main__":
    unittest.main()
