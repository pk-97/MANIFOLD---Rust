# Landing reports — the durable record of every landing

<!-- index: One committed file per landing (gate output, level reached, click-script, deviations, VD refs). Rule: DESIGN_DOC_STANDARD.md §8.10. -->

One file per landing, named `YYYY-MM-DD-<slug>.md`, committed in the same push
as the landing itself (DESIGN_DOC_STANDARD.md §8.10). The chat-side landing
report is a summary plus a pointer here. `docs/VERIFICATION_DEBT.md` entries
and BUG_BACKLOG `Escaped:` lines reference these files.

Template — every section present, `none` written out explicitly rather than
omitted:

```markdown
# <wave/phase> — landed YYYY-MM-DD @ <merge SHA>

**Branch:** <branch> · **Level reached:** L<0–4> / target L<n> (§10)
**Doc status line (quoted verbatim):** <the new Status: line>

## Gate results (verbatim)
<clippy + test command lines and their actual output tails — pasted, not paraphrased>

## Deviations from brief
<every departure, or "none">

## Shortcuts confessed (rolled up from phase reports)
<union of the phases' `Shortcuts taken:` fields, or "none">

## Verification debt
<VD-NNN opened / VD-NNN carried (why) / "none opened, none carried">

## Click-script for Peter (≤2 minutes)
1. <step> — expect: <observation>
2. ...
```
