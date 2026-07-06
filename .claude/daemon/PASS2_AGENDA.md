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
2. **Score the 07-07 additions** by their DESIGN.md §2h oracles:
   ungrounded-chat-claim (grounding read or unverified-restatement within ~10
   events), unverified-done-claim (verification-class event within ~10 events
   or claim restated unverified — crude by design, PULL it if noisy),
   landing-doc-reflex (Status-line edit / docs/landings/ write around
   the fire), workflow-agent observation (workflow agents discovered + fires
   carry their agent_id), worker self-grade uptake (agent_id-bearing grade
   lines rise from all-time zero), worker review threshold (20 events —
   placeholder, tune from data).
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

Done = grades written per RUNBOOK, gate numbers reported per family (never
aggregate-only — the pass-1 bimodal lesson), actions (mutes/rewords/flag
pulls) committed with dated notes in moves.md/DESIGN.md, this file updated to
pass-3 state.
