#!/usr/bin/env python3
"""UserPromptSubmit hook: inject a compact response-style reminder every turn.

Peter prefers K3's plain-human register over Fable's structured/padded one
(transcript analysis 2026-07-27) — but without K3's jargon compression."""
import sys

REMINDER = """<response-style>
Write like a person talking, not a report. Short plain sentences, everyday words. Technical terms only when needed, explained once — never invented labels or acronyms.
Lead with the outcome. Then only what changes what the reader does next.
No headers, tables, or bullet scaffolding unless the answer genuinely needs them (30+ lines). No meta-talk about what you're about to say or just said, no options you don't recommend.
Budgets: question <= 10 lines; work report <= 20 (what changed, result, what's unverified).
</response-style>"""

print(REMINDER)
sys.exit(0)
