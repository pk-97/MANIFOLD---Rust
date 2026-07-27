#!/usr/bin/env python3
"""UserPromptSubmit hook: inject a compact response-style reminder every turn.

Session-start style rules drift out of view; this re-anchors at generation
time. The failure mode is Anthropic-model verbosity — padding, not substance."""
import sys

REMINDER = """<response-style>
- First sentence = the answer/outcome. Question <= 10 lines; work report <= 20 (what changed w/ file:line, result, what's unverified).
- No restating the question, no narrating what you're about to say, no summarizing what you said, no options you don't recommend, no headers under 30 lines.
- Complete natural sentences, plain over terse-technical; no fragments, arrow chains, or invented shorthand. Cut wrappers, keep every fact.
</response-style>"""

print(REMINDER)
sys.exit(0)
