# Move authoring — how to write a daemon move

Authored by Claude Fable 5, 2026-07-07, the final Fable session. This is the
method behind every move in `moves.md`, written down so the next author (Opus,
from pass 2 on) inherits the judgment and not just the artifacts. RUNBOOK.md
teaches grading; DESIGN.md specs the plumbing; `paper.html` explains the system.
This file is the missing fourth thing: what to do when a NEW drift class
appears and the catalog has no move for it.

Every rule below was learned from a real incident, named where it matters.
Nothing here is speculative advice.

## 1. A move is a compiled specimen, not a category

A move = a detection signature + a fixed payload, written once, fired forever
at the right moment. Haiku (or a regex) detects; it never composes. The payload
is the author's judgment, frozen.

The unit of origin is the **specimen**: a real transcript moment where an
intervention, arriving right then, would have changed the outcome. Every move
in the catalog names one (735b0dc6 for ungrounded-resolution, the glTF P1c fork
for price-the-fork, Peter's "I NEVER want the quick fix" pair for the stopgap
moves). If you cannot point at the transcript moment, you do not have a move —
you have a worry. Log it in `eval/labels.jsonl` with `expect_family: null` and
wait for a second specimen (precedent: the hedging-on-conclusive-observation
gap, moves.md header 2026-07-05 — still waiting, correctly).

Authoring starts by answering two questions about the specimen, in this order:
- What did the transcript **observably look like** at the moment intervention
  would have helped? (That becomes the signature.)
- What single thought, arriving at that moment, would have changed what
  happened next? (That becomes the payload.)

## 2. Choose the tier by tempo and evidence type

Three tiers exist. Pick by what the evidence IS and when the fire must land,
never by which tier is easiest to write.

- **Deterministic** (`mechanical/*`, valve- or observer-selected): the evidence
  is a literal transcript artifact — a marker string added by an edit, a
  command shape, a toolless turn naming a path. Zero latency, zero cost, no
  confidence question. Its limit: it catches only the confessed/literal form
  of the failure (confessed-stopgap catches "for now"; the competent unconfessed
  shallow fix stays invisible — DESIGN.md §2c's honest residue).
- **Classifier** (`anchor/*`, `coaching/*`): the evidence needs reading
  comprehension over a window. Pays the async-latency race and the rubric's
  evidence contract (see §3). Use only when the deterministic form genuinely
  cannot express the signature.
- **Synchronous gate** (ask-gate pattern, DESIGN.md §2c-ask): for events that
  are rare, already blocking, and where damage precedes any async delivery
  (a question to the human, a workflow launch). A sync Haiku call is
  affordable exactly because the event blocks anyway. Hard timeout, fail open.

Two placement rules with measured teeth:
- **A move that must fire on a text-only turn must be deterministic.** Chat
  turns are where the classifier races the Stop catch-up wait and loses — 16
  of 68 corrective fires landed a prompt late in the 07-05→07-07 window, and
  text-only turns were the largest class. This is why ungrounded-chat-claim
  exists as a mechanical sibling of the classifier anchor.
- **Predictive coverage never goes to the classifier.** A predictive
  situation's evidence is usually an ABSENCE (the read that didn't happen, the
  investigation that never occurred), and the rubric requires verbatim quotes —
  you cannot quote an absence, so Haiku self-censors and the move silently
  never fires (pass-1 finding: coaching starved at detection while
  define-done, given a surface proxy instead, produced 47 noise fires in one
  replay round). Absences go to the phase-transition tier (§2d) or a
  deterministic rule with a concrete proxy, or they wait.

## 3. Writing a classifier signature — you are writing for Haiku

The signature is parsed out of moves.md into the rubric at runtime; its reader
is a small model under a strict evidence contract (rubric.md "read as law":
verbatim quote required; confidence < 0.8 → clear; observable markers only,
no mental states; when torn, null). Write accordingly:

- **Observable markers only.** Test each clause: could a reader with only
  TASK / LEDGER / RECENT *quote* the evidence? "The agent is overconfident" is
  a mental state; "success claimed with no verifying event between the change
  and the claim" is a marker.
- **Write the never-fire list in the signature itself,** as concretely as the
  fire condition. The catalog's precision history is mostly never-fire clauses
  added after false positives: define-done's short-TASK exclusion (its absence
  = 47/496 noise fires, replay round 1), verify-claim's
  evidence-checks-the-same-claim and LEDGER-verifying-event and restatement
  exclusions (each one a graded FP class), attack-the-story's "TASK quoting
  past reasoning is not an assertion". Budget as much text for when NOT to
  fire as for when to fire.
- **One signature, one failure shape.** If it needs three "or"s, it is two
  moves. (Verify-claim's bundled-claim clause is the allowed form: same
  failure — claim without its own evidence — in a second costume.)
- **Know the window's horizon.** The classifier sees TASK, a LEDGER of tool
  events since the last window, RECENT (last two prose texts), and the §2f
  SESSION FACTS block (verification events, context switches, per-path
  read/edit counts carried past the horizon). A signature that depends on
  something older than all of these cannot work as worded — scope-drift's
  attribution blindness (1/11 precision, muted) is the standing specimen.
  Check whether a session fact already carries what you need; if not, the fix
  is a windowing/facts change (DESIGN.md), not cleverer signature prose.

For a deterministic move, the signature text is the **implementation
contract**: state the ALL-hold conditions, the never-fire list, and the
exclusions precisely enough that a Sonnet builder can implement it without
interpretation (confessed-stopgap and ungrounded-chat-claim are the models).

## 4. Writing the payload — it arrives as a thought

The voice rule from the moves.md header, unpacked:

- Short, concrete, second person, zero preamble. It interrupts someone
  mid-work; it must repay the interruption in the first line.
- **Name the exact next action**, not the principle alone. "Run the
  verification in the medium where failure would actually show" beats "be more
  careful about verification".
- **Always include the escape hatch:** every anchor payload ends with the
  legitimate-case out ("if the guard genuinely is the right contract, say why
  in one sentence"; "if you can't verify it here, say exactly that instead").
  The escape hatch is what makes a wrong fire cheap — the agent spends one
  sentence and moves on — and cheap wrong fires are what keep trust alive
  (rubric law 2 is about the detector; this is the payload's half of the same
  bargain).
- State the repo rule when one exists (fix-at-the-root, §8.9, tree
  discipline) — the payload is allowed to be an enforcement surface; cite the
  doc so the agent can read the whole rule.
- **Advice-kind moves are a different genre** (`kind: advice`, §2e): a helping
  hand, not a correction. Nothing-is-wrong preamble, no acknowledgment, never
  escalates, expected common grade "unclear". Use for priming; never dress a
  correction as advice or advice as an alert — the alert wrapper on
  non-corrections trains alert-blindness that bleeds into the real anchors
  (Peter's diagnosis, 2026-07-05, which created the tier).

## 5. Every move ships with its scoring story

Before shipping, answer: within ~10 tool events of delivery, what observable
event counts as success, what counts as fail? If the answer is mechanical
(a verification event follows verify-claim; a grounding read follows
ungrounded-chat-claim; a Status-line edit follows landing-doc-reflex), record
it — §4b or the pass can score it. If there is no mechanical oracle, mark the
move **unscored** explicitly and let the sleep pass grade it from transcripts
(RUNBOOK step 2). Never invent a proxy oracle to make a move look scoreable —
"unscored" is honest; a fake oracle poisons auto-mute.

## 6. Ship discipline

- moves.md is the single source of truth: the classifier catalog, the
  deterministic contracts, and the cooldown classes are all parsed from it.
  Add the dated header note naming the specimen — the header is the catalog's
  provenance ledger.
- Grep the catalog AND the gate deny-reasons before authoring: the failure
  mode of authoring is writing a move that already exists under another name.
  Specimen: "mispriced-fork" sat on the sleep-pass agenda for three days
  while coaching/price-the-fork (the same pattern, same glTF specimen) sat in
  the catalog, plus the ask-gate's "mispriced" deny reason. Caught 2026-07-07
  during final-window review; cost only reading time, but only because it was
  caught before authoring began.
- Detection wiring is Sonnet work with tests. Warn the builder explicitly
  about the explicit-dispatch test convention: a test function not added to
  the file's main() dispatch list silently never runs (the stop-wait v1 tests
  reported green while never executing — caught same commit, 2026-07-05).
- New/edited moves are UNVALIDATED until a pass grades live fires. Do not
  claim them working; the marker exists for this.
- Live observers pick up catalog changes on natural idle-exit revive.
  Cross-session SIGTERM sweeps are permission-gated — ask, don't sweep
  (auto-mode classifier precedent, 2026-07-05).

## 7. Tuning discipline — when a move underperforms

- **Two wording rounds, then stop.** Replay tuning went 8% → 42% recall on
  wording alone, and the third round was correctly refused: the residue was
  windowing geometry, not language (2026-07-03 escalation, verdict confirmed
  by the fixes that followed). If two honest rewords don't clear the gate, the
  gap is structural — windowing, session facts, tier placement — and belongs
  in DESIGN.md, not in more adjectives.
- **Never stack a reword on an unscored reword.** Verify-claim was resharpened
  at pass 1; the 07-07 session deliberately left it alone because the rework
  hadn't been scored yet. One change, one measurement, then the next change.
- **Mute beats delete.** A move with a real specimen but bad precision gets a
  time-boxed mute and a rework condition (scope-drift: muted 7 days,
  attribution-aware rework named as the unmute gate). Deletion is for moves
  whose specimen turned out to be miscompiled.
- Precision is judged per family, not per catalog: pass 1's 51% overall was
  bimodal — ungrounded-resolution 10/12 and every coaching fire a win, while
  scope-drift sat at 1/11. The aggregate number would have told you to panic;
  the per-move numbers told you exactly what to mute.

## 8. Current state pointers (verify before trusting — this file does not update itself)

- Catalog size, families, cooldown classes: moves.md header + `common.py`'s
  parser. Live gate numbers: latest sleep-pass grades in `eval/live_grades*`
  and RUNBOOK's procedure. Delivery-latency split (in-turn vs next-prompt):
  telemetry.jsonl `valve` field on injected records — the standing metric
  Peter set 2026-07-07: corrections must land before his next message.
- Open numeric placeholders deliberately left for pass tuning: §2d oscillation
  span/flips, §2g card-limit bounds, the worker review threshold (§2h.4).
