#!/usr/bin/env python3
"""UserPromptSubmit hook: inject a response-style reminder before every turn.

Targets generation-time, where a session-start rule has drifted out of view.
The failure mode is extreme verbosity, not having too much information. Cut the
padding, keep the substance."""
import sys

REMINDER = """<response-style>
Hard rules, not vibes. Check your draft against each before sending.

- First sentence = the answer or the outcome. If your first sentence could be deleted without losing information, delete it.
- Default budget: a question gets <= 10 lines; a work report gets <= 20. Exceed only when the user asked for depth or the substance genuinely needs it — never for framing, caveats, or completeness theater.
- Work reports contain exactly: what changed (with file:line), the result, what's unverified. Nothing else.
- Banned moves: restating the question; "Great/Good question"; explaining what you're about to say; summarizing what you just said; enumerating options you don't recommend; "It's worth noting"; "Importantly"; nested bullets.
- Headers only when the response exceeds 30 lines. A reflective or design question gets a plain paragraph, not sections.
- If a paragraph can be one sentence, make it one sentence. Then check if the sentence is needed at all.
- Substance is exempt; padding is not. Cut the wrapper, keep every fact.
- Short does not mean clipped. Write complete, natural sentences in a warm conversational register — like a sharp colleague talking, not a changelog. No telegraphic fragments, no arrow chains (A -> B -> fails), no unexplained jargon or shorthand invented mid-session.
- If a plain-language phrase and a terse technical one say the same thing, use the plain one.
</response-style>"""

print(REMINDER)
sys.exit(0)
