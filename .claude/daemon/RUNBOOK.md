# Sleep-pass runbook

Procedure for consolidation passes. Written by Fable 2026-07-04 so passes run
on whatever large model is available after July 7 (Opus). Run weekly, or when
telemetry has gained ≥10 new injections since the last pass. A pass that only
grades and changes nothing is a valid pass; bias toward silence.

## Inputs
- `.claude/daemon/telemetry.jsonl` — injected / scored / observer_spawn events
- session transcripts: `~/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/<session>.jsonl`
- `.claude/daemon/moves.md`, `rubric.md`, `DESIGN.md` (§4 gates, §4b/4c dials)
- prior grades: `.claude/daemon/eval/live_grades.jsonl` (append-only, created by pass 1)

## Procedure

1. **Join.** For each `injected` record since the last pass: pair with its
   `scored` record by (session_id, seq) if one exists; locate the injected
   block in the transcript (`rg '<daemon move=' <transcript>`). Pre-§4b
   records lack move_id in telemetry — the transcript block is authoritative.
2. **Grade each injection.** Read the transcript around it (the window before,
   ~40 events after). Two labels, human-judgment level, mechanical score is
   input not verdict: `correct` (did the named drift actually exist? TP/FP)
   and `effective` (did behavior change in the direction the payload asks?
   y/n/unclear). Append to `eval/live_grades.jsonl`:
   `{ts, session_id, seq, move_id, correct, effective, ordinal, notes}`.
3. **Count misses.** Recall can't be read off telemetry. Scan the graded
   week's human messages for correction-shaped turns (the user catching
   drift the daemon didn't flag); each is a FN with a timestamp. Ask Peter
   for ones that never hit the transcript. Log FNs in live_grades with
   `correct: "miss"` and which move SHOULD have fired (or `family: null` —
   those are new-move candidates).
4. **Score the gates** (DESIGN §4): precision = TP/(TP+FP) over graded fires;
   noise = FP per clean session; recall = TP/(TP+FN). State them with the n.
   With n < 10, write "insufficient data", don't tune wording from it.
5. **Fatigue read** (§4c-3b): success-by-ordinal per move. Decay → wording
   tiers become this pass's authoring agenda; flat → note it, move on.
6. **Act.** In strict order of preference: do nothing; adjust bounded dials
   (§4b: cooldowns, confidence thresholds, within floors); retire or mute a
   move that fires wrong repeatedly; reword a signature (quote the FP/FN
   windows as evidence in the commit); author a new move (only from ≥1
   concrete specimen; follow moves.md voice: payload arrives as a thought,
   signature is observable markers only). Wording edits happen HERE and
   nowhere else. One concern per edit.
7. **Decisions due each pass:** worker-nudge flag per DESIGN §2b enable rule;
   review §4b auto-mutes (confirm or lift); check the delivery-gap ledger
   (how many flags were raised on turn-final text and delivered a turn late —
   informs the Stop-valve's value once built).
8. **Close.** Commit everything (moves.md, dial changes, live_grades.jsonl,
   this file if procedure changed) with a `daemon: sleep pass N` message
   listing gates + actions. SIGTERM all `verdicts/*.pid` so daemons reload.
   Update the `daemon` memory (gates, catalog count, open decisions).

## Rules
- Fail toward silence: when a grade is genuinely unclear, `unclear`, not TP.
- Never tune from invalid data (classifier error rate > 5% in the graded
  window = don't trust it; see replay run-validity precedent).
- Product-level judgment calls (a move about UX/design behavior, anything
  touching Peter's working style) escalate to Peter, not decided solo.
- Two consecutive passes failing the same gate → stop tuning, bring Peter and
  the largest available model to look at rubric + windowing jointly (the
  round-1/2 lesson: the binding constraint is usually observation, not wording).
