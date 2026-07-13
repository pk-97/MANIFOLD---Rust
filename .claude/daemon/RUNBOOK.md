# Sleep-pass runbook

Procedure for consolidation passes. Written by Fable 2026-07-04 so passes run
on whatever large model is available after July 7 (Opus). Run weekly, or when
telemetry has gained ≥10 new injections since the last pass. A pass that only
grades and changes nothing is a valid pass; bias toward silence.

## Inputs
- `.claude/daemon/telemetry.jsonl` — injected / scored / observer_spawn events
- session transcripts: `~/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/<session>.jsonl`
- `.claude/daemon/moves.md`, `rubric.md`, `DESIGN.md` (§4 gates, §4b/4c dials)
- prior grades: `.claude/daemon/eval/live_grades.jsonl` (append-only, the durable
  pass-graded corpus — the only tracked grade file; this pass writes it)
- provisional session self-grades: `.claude/daemon/eval/live_grades.session*.jsonl`
  (gitignored, `grader:session`; a session appends its own fires here so the tracked
  corpus never goes dirty mid-session — see step 2)
- retrospective session findings: `.claude/daemon/eval/observations.session.jsonl`
  (gitignored; a miss noticed on end-of-session review, or a note that doesn't fit
  a fire — never a second grade line for an already-graded fire — see step 2/3)

## Procedure

1. **Join.** For each `injected` record since the last pass: pair with its
   `scored` record by (session_id, seq) if one exists; locate the injected
   block in the transcript (`rg '<daemon(-advice)? move=' <transcript>`).
   Pre-§4b records lack move_id in telemetry — the transcript block is
   authoritative.
2. **Grade each injection.** Read the transcript around it (the window before,
   ~40 events after). Two labels, human-judgment level, mechanical score is
   input not verdict: `correct` — did the named drift actually exist? Canonical
   values ONLY: `true`, `false`, `"miss"` (step 3), or `"unclear"` — never
   free-text like `"TP"`/`"FP"`/`"y"`/`"n"`. Use `"unclear"` for `correct` only
   when the drift's *existence* is genuinely undeterminable from the evidence
   (a worker fire whose move_id/window is lost to mailbox overwrite or colliding
   attribution); it is excluded from precision denominators, so never reach for
   it to dodge a call you can make. `effective` — did behavior change in the
   direction the payload asks? Canonical values ONLY: `true`, `false`,
   `"unclear"`, or `"n/a"` — where `"n/a"` is used ONLY on a `correct:"miss"`
   record (a false negative delivered no payload, so effectiveness is undefined,
   not merely unknown). Enforce mechanically: `python3 eval/check_grades.py`
   before grading (baseline) and after (gate — a grading session may not add
   violations).
   Append to `eval/live_grades.jsonl`:
   `{ts, session_id, seq, move_id, correct, effective, ordinal, notes}` —
   plus `agent_id` when the fire was a worker nudge: (session_id, seq) alone
   collides across workers (pass-1 lesson: colliding grades attached to the
   wrong fires and cost real attribution work).
   Sessions self-grade at fire time since 2026-07-04 (records with
   `"grader": "session"`, prompted by the supervised-mode sentence; since
   2026-07-13 that sentence names a one-shot command — `python3
   .claude/daemon/log_grade.py <seq> <move_id> <correct> <effective>
   "<notes>" [--agent-id ID]` — which owns the record format, normalizes
   stray vocabulary, resolves the MAIN checkout's session file even from a
   worktree copy, and lints with `check_grades.py` at write time;
   hand-written records remain valid input. The sentence has named
   the fire's own `seq` directly since 2026-07-05 — `"seq"` is
   REQUIRED on every session-graded record; a record without one can only be
   joined by move_id, and reads as AMBIGUOUS whenever that move fired more than
   once in the session). Since 2026-07-05 those land in
   `eval/live_grades.session*.jsonl` (gitignored), not the tracked corpus —
   read every session file for them. `slice_fires.py`'s reader
   (`load_grades`/`join_grades`) normalizes stray vocabulary onto the canonical
   values (`TP`/`true`/`y` → `true`, `FP`/`false`/`n` → `false`; anything else
   passes through untouched rather than guessing) and joins by
   (session_id, seq) when seq is present, falling back to (session_id, move_id)
   — flagged AMBIGUOUS — only when seq is missing; it prints the
   normalization/ambiguity counts before you trust the joined corpus, and never
   silently drops a record. Advice-kind fires (`<daemon-advice>` blocks, DESIGN
   §2e) carry no ack and no session self-grade BY DESIGN — grade them
   pass-level only, on whether the session's downstream behavior shows the
   payload's patterns, and expect `effective: unclear` to be common rather than
   a defect. Treat session self-grades as provisional input, not verdicts —
   confirm or override each with your own transcript read; on disagreement
   write a pass-graded record for the same (session_id, seq) into
   `live_grades.jsonl` rather than editing the session's line. End-of-session
   retrospective findings (a miss noticed on review, a note that doesn't fit a
   fire) are NOT a second grade line for an already-graded fire — they go to
   `eval/observations.session.jsonl` instead
   (`{ts, session_id, kind: "miss-candidate"|"note", move_id or expect_family,
   evidence, note}`); step 3 folds these in alongside Peter's corrections.
3. **Count misses.** Recall can't be read off telemetry. Scan the graded
   week's human messages for correction-shaped turns (the user catching
   drift the daemon didn't flag); each is a FN with a timestamp. Read every
   `eval/observations.session.jsonl` `"miss-candidate"` record from the graded
   window alongside these — sessions log a miss there the moment they notice
   one on end-of-session review, rather than waiting for the pass. Ask Peter
   for ones that never hit the transcript. Log FNs in live_grades with
   `correct: "miss"` and which move SHOULD have fired (or `family: null` —
   those are new-move candidates). Once folded in, delete the consumed
   `observations.session.jsonl` records the same way step 8 clears session
   grade files.
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
   listing gates + actions. Once the session grades are folded into
   `live_grades.jsonl`, delete the consumed `eval/live_grades.session*.jsonl`
   and `eval/observations.session.jsonl` records (gitignored, so nothing to
   commit) so they don't re-grade / re-surface next pass.
   SIGTERM all `verdicts/*.pid` so daemons reload. Update the `daemon` memory
   (gates, catalog count, open decisions).

## Rules
- Authoring a NEW move, or editing any signature/payload: read
  MOVE_AUTHORING.md first, whole. It is the method behind the catalog
  (specimen rule, tier choice, signature evidence contract, payload voice,
  scoring story, tuning discipline) — grading is only half the pass.
- Fail toward silence: when a grade is genuinely unclear, `unclear`, not TP.
- Never tune from invalid data (classifier error rate > 5% in the graded
  window = don't trust it; see replay run-validity precedent).
- Product-level judgment calls (a move about UX/design behavior, anything
  touching Peter's working style) escalate to Peter, not decided solo.
- Two consecutive passes failing the same gate → stop tuning, bring Peter and
  the largest available model to look at rubric + windowing jointly (the
  round-1/2 lesson: the binding constraint is usually observation, not wording).
