# Sleep pass 2 — consolidated agenda (~July 12, Opus)

Compiled by Fable 2026-07-07 (final Fable session) from every pass-2 item
scattered across the daemon memory, DESIGN.md, and RUNBOOK notes — so the pass
starts from one file. Procedure: RUNBOOK.md. Authoring/editing moves:
MOVE_AUTHORING.md first. This agenda is ordered; items 1–3 are the pass's
reason to exist.

1. **Headline metric (Peter, 2026-07-07): corrections land before his next
   message.** From telemetry.jsonl injected records, compute the in-turn
   (valve PostToolUse/Stop) vs next-prompt (valve UserPromptSubmit) split for
   corrective fires (anchor/coaching/escalate — exclude advice + primers).
   Baseline 07-05→07-07: 52 in-turn vs 17 late (~25%). The 07-07 forensics
   (DESIGN.md §2h.6) classified all 17: 11 capped-wait classifier races (8 =
   done-claim family), 5 snapshot-race defect (fixed), 1 anomaly. Three fixes
   shipped same day: snapshot double-stat, cap 6s→10s, and two deterministic
   Stop-tier moves. Verify the split actually moved; measurement oracle: the
   transcript's `stop_hook_summary.hookInfos[].durationMs` field is the
   measured Stop wall time — use it, don't infer. Also check the per-turn
   latency tax (the double-stat adds ~0.2s to every turn end; the 10s cap
   binds only when a classification is in flight).
2. **Score the 07-07 additions** — BOTH batches. The final authoring pass
   (same day, later) added 7 more moves + 5 sharpenings mined from
   eval/observations.session.jsonl; per-move oracles in DESIGN.md §2i, triage
   dispositions in §Triage below. Note anchor/unheeded-warning is DORMANT
   until TICKETS.md T10 ships — its non-fires are not misses. The morning
   batch, by their DESIGN.md §2h oracles:
   ungrounded-chat-claim (grounding read or unverified-restatement within ~10
   events), unverified-done-claim (verification-class event within ~10 events
   or claim restated unverified — crude by design, PULL it if noisy),
   landing-doc-reflex (Status-line edit / docs/landings/ write around
   the fire), workflow-agent observation (workflow agents discovered in
   observer logs + phase telemetry carries their agent_id — DELIVERY is off
   the table: the 07-07 probe proved the harness runs no hooks for workflow
   agents, see DESIGN.md §2h.3; first agent_id self-grade already exists:
   af65698430e9470ac seq 2, written during the build itself), worker
   self-grade uptake (more agent_id-bearing grade lines), worker review
   threshold (20 events — placeholder, tune from data).
3. **Score the pass-1 rework + mute expiry:** verify-claim post-rework fires
   only (pre-rework grades don't count); scope-drift mute expires ~07-12 —
   decide: attribution-aware rework using §2f session facts, fold into the
   §2d phase tier, or extend the mute. Do not unmute unchanged.
4. **§2d shadow telemetry** (`phase_fire` records, accumulating since 07-05):
   hand-grade against transcripts (slice_fires.py recipe), flip delivery
   per-rule at ≥60% shadow precision; tune the placeholders (span 40 /
   flips 3).
5. **Gate telemetry reads:** ask_gate (now tier-tagged; watch the 10s Haiku
   budget under fleet throttling — n=2 real invocations, 1 timeout, as of
   07-05) and workflow_gate (zero real launches as of 07-05 — still
   unvalidated, not broken).
6. **Advice tier, pass-level only:** reasoning-primer / design-primer /
   unread-edit graded from downstream behavior; `effective: unclear` is the
   expected common case; exclude advice from precision denominators or report
   separately. Read fatigue ordinals; grade heededness as its own column;
   evaluate the 300-event advice recurrence number from refire telemetry.
7. **Worker fires:** manual grading per RUNBOOK (grade format carries
   agent_id); the §2b clock started at the first orchestration after the
   07-04 discovery fix. Disable rule: worker precision <60% at any pass pulls
   the worker-nudges flag.
8. **Specific graded-window debts:** session 10977941 (param-storage
   orchestration 07-05: circling ×2 → checkpoint → circling again, ended
   no-commits — leans TP, grade it); ignore null-move_id injected records
   before 07-05 ~10:00 (pre-fix residue). Fires from the 07-07 final-window
   sessions (timeline-ux run + the meta/daemon session) are ungraded material.
9. **Recall honesty:** pass-1 miss-hunt closed EMPTY (Peter had none to
   report) — recall stands at "insufficient data", not zero-miss. Ask Peter
   for specimens again; if none, say so again.
10. **Standing residues (check, don't build):** §2g bounds tier = unbuilt
    Sonnet ticket; 3-arm falsification experiment deferred by Peter, still
    owed eventually; classifier throttling — if 180s timeouts recur under
    fleets, next lever is logging rate-limit vs hang distinctly (07-04
    incident note); worker whispers still not persisted to worker transcript
    jsonls (mailbox/observer-log recovery only); hook-ordered context switches
    still invisible to TASK attribution (HARNESS_TEXT_PREFIXES exclusion —
    named residue on scope-drift's mute); ack/backstop inconsistency —
    build_block's supervised grade-ask is appended to EVERY delivered move
    including mechanical ones, but the grade backstop only counts
    anchor/coaching/escalate, so mechanical fires are asked for grades
    nobody chases (found 07-07; decide: exempt mechanical from the ack, or
    count them in the backstop); worker Stop events reuse
    data["transcript_path"] as-is — whether the harness populates the
    subagent's OWN transcript there is an inherited, unverified assumption
    under announced-not-started and the 07-07 worker review flows (the
    workflow/worker probe should settle it).
11. **Numeric placeholders that are yours to tune** (marked Opus-tunable at
    handoff): §2d oscillation span/flips, §2g card-limit bounds, §2h.4 worker
    review threshold, OBSERVATION_PROMPT_MIN_EVENTS (40, main) if grading
    shows it mis-set.

12. **Check for the 07-07-night Fable grading session's output first.** A
    dedicated grading session was planned for the window's final night
    (reconciling the ~180 self-grade lines and scoring ungraded telemetry
    fires). If eval/live_grades* carries a 07-07/07-08 pass stamp, build on
    it — verify, don't redo. If it never ran, its scope folds into items 1–3
    and 8 here.

## Triage of eval/observations.session.jsonl (Fable, 2026-07-07 late — final authoring pass)

All 39 records were read and dispatched; do NOT re-triage. By entry number
(line order in the jsonl):

- **Authored as new moves (7)** — oracles in DESIGN.md §2i: #2
  coaching/deduction-loop; #6+#18+#20 anchor/circular-oracle; #10
  anchor/asserted-values; #13 coaching/explain-with-their-artifact; #26
  anchor/premature-capture; #39 anchor/unheeded-warning (dormant on T10);
  #8+#14 mechanical/stale-brief (advice-kind).
- **Sharpened existing moves** — #4+#12 circling (two never-fire clauses);
  #23 ungrounded-resolution (memory-file reads are not artifact provenance —
  the rest of #23's candidate was NOT re-authored as a move: the chat form is
  ungrounded-chat-claim's territory once T3 widens its vocabulary); #3
  git-landing payload (escape hatch); #11 confessed-stopgap contract
  (self-disposal exemption; runtime = T2); #1+#19 design-primer payload
  (termination-condition + unification-bias).
- **Ticketed for Sonnet** (TICKETS.md): #3→T1, #5+#11→T2, #23→T3,
  #24+#31→T4, #35→T5, #38→T6, #27→T7, #9→T8, #8/#14→T9, #39→T10.
- **Rule-doc fixes shipped same pass:** #16+#28+#29+#33 untracked-file
  exception (CLAUDE.md rule + GIT_TREE_DISCIPLINE §3b); #16 inverse-sweep
  hazard (§3b); #38 standalone landing merges (§3b); #36 worktree handoff
  files (§3b); #17 affordance-legibility gate line (DESIGN_DOC_STANDARD §5);
  #22's meta-lesson = the hook-tier placement rule (MOVE_AUTHORING §2).
- **Pass-2 watch/grade, deliberately no artifact tonight:** #5 verify-claim
  honest-hedge FPs (blocked behind scoring the pass-1 rework first — one
  change, one measurement); #7 trim primers before detectors if tier budget
  ever needs trimming; #15+#25 ask-gate calibration (generalized past its
  intended class twice, net-positive both times — consider a value-framing
  rule for (Recommended) tags, and "decidable from doctrine → decide, don't
  ask" as its own deny reason); #21 recall-before-recommend (one specimen,
  waits for a second per MOVE_AUTHORING §1); #22 consider narrowing
  agent-model-discipline now agent-model-guard.py denies at launch; #30
  PNG-alone-verification whisper (one specimen, waits); #32 verify-claim
  cadence-fire audit (does the trigger require a nearby claim, or fire
  periodically in tool-heavy stretches?); #34+#37 orchestration hazards
  (gate commands verified runnable before dispatch; background long workers,
  user answers in the turn's final message — also appended to the
  agent-execution-playbook memory).

Raw jsonl entries left in place for RUNBOOK step 8's normal clear.

Done = grades written per RUNBOOK, gate numbers reported per family (never
aggregate-only — the pass-1 bimodal lesson), actions (mutes/rewords/flag
pulls) committed with dated notes in moves.md/DESIGN.md, this file updated to
pass-3 state.
