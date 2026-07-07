# Sleep pass 2 — consolidated agenda (~July 12, Opus)

Compiled by Fable 2026-07-07 (final Fable session) from every pass-2 item
scattered across the daemon memory, DESIGN.md, and RUNBOOK notes — so the pass
starts from one file. Procedure: RUNBOOK.md. Authoring/editing moves:
MOVE_AUTHORING.md first. This agenda is ordered; items 1–3 are the pass's
reason to exist.

## Night-half results (Fable grading session, 2026-07-07 late — item 12's answer)

The planned final-night grading session RAN. Every corrective fire since
07-05 10:00 was pass-graded from timestamp-anchored transcript windows (154
pass grades + 17 folded miss lines + 16 folded mechanical session grades,
all stamped "[pass 2026-07-07 Fable night]" in eval/live_grades.jsonl).
Verify these, don't redo them. Per-family precision (TP/FP/unclear):

| family | TP/FP/u | precision |
|---|---|---|
| ungrounded-resolution | 31/14/0 | 69% |
| verify-claim | 24/25/1 | **49%** (mains 23/19 ≈ 55%, workers 1/6) |
| circling | 0/10/0 | **0%** |
| checkpoint | 2/7/0 | 22% |
| skim | 5/4/0 | 56% |
| permission-creep | 2/4/0 | 33% |
| unpackaged-deliverable | 3/1/0 | 75% |
| differential, attack-the-story | 2/0 each | 100% |
| agent-model-discipline | 1/0 | TP |
| thrash, invariant-frame, symptom-suppression, model-first | 0/1 each | FP |
| ALL corrective | 72/69/1 | 51% |

Gates (RUNBOOK step 4): precision 51% (n=141 graded corrective fires — fails
the 80% gate, bimodal exactly as pass 1); noise 0.69 corrective FP per
observed session (69 FP / 100 sessions — passes <1, up from pass 1's 0.26);
recall insufficient-data-leaning-good (72 TP vs ~15 unique logged FNs — a
floor-quality estimate, see item 9).

Addendum (~20:50, same night — Peter flagged stragglers): +4 records for
fires/grades that arrived while the pass ran. Three corrective TPs (incl.
**coaching/price-the-fork's FIRST-ever fire — coaching is now 5/0 this
window**) and the chat-claim FP→TP reversal above. Corrective totals become
75/69/1 ≈ 52%. Two a2c972aa fires (seq 4 git-landing, seq 5) left for that
session's own end-of-session grades / pass 3. One process casualty: the
pass cleared live_grades.session.jsonl at 20:26 and destroyed a 20:12
self-grade it hadn't folded (a2c972aa seq 2 — re-graded from transcript).
Pass-3 procedure: defer the session-file clear to the pass's very last
step, and clear only lines whose fire predates the pass's telemetry
snapshot. Test-residue purge #3 also ran (9 records, from the pass's own
suite runs — T11 is the root fix).

Actions taken by the night-half (dated notes in the files): **worker-nudges
flag PULLED** (2/10, disable rule; return path in DESIGN §2b note — includes
the read-only-worker whisper-refusal framing defect, 93150901/a8287d);
**mechanical/unverified-done-claim MUTED 7 days** (0/3; both defect shapes in
the moves.md note and mute file); **escalate/checkpoint never-fire clause**
added (2/7; fire-count ≠ repeated drift — the one signature edit, evidence
in the clause); test-session telemetry residue purged. NOT touched (frozen
one-change-one-measurement): circling, ungrounded-resolution,
confessed-stopgap, git-landing, verify-claim.

Grading-infrastructure defects the night-half hit, for pass 3's method:
(a) sessions before the mid-07-05 seq-in-sentence fix self-graded with their
OWN corrective-only numbering (a5b78b70 "seq 1/2" = telemetry seq 2/3), so
even exact-seq joins misattach — join by note-content when the vocab predates
the fix; (b) five c9e4d45d self-grade lines were malformed JSON from
`date +%s.%N` (macOS date has no %N → literal "N" in ts) — recovered by
regex; consider naming the exact command in build_block's grade-ask;
(c) grade lines without agent_id collide across (session, seq) exactly as
pass 1 warned — the af6569 agent_id-bearing line was the only clean worker
join; (d) compaction re-injects attachment history, so slice_fires
order-matching drifts mid-session — anchor by block timestamp instead
(cut_windows.py recipe in the pass-2 grading session's scratchpad, trivially
re-derivable: dedupe (text,ts) pairs, match nearest fire ts).

Two enforcement gaps found while ACTING, both part-fixed, remainder yours:
(e) **mutes were classifier-tier only** — daemon-stop.py never read
verdicts/mutes/, so muting a hook-fired mechanical move did nothing. Fixed
minimally same night: `_move_muted()` added to daemon-stop.py and gated on
mechanical/unverified-done-claim (the move actually muted; 120/120 stop-valve
tests green, gate verified against the live mute file). Gating
announced-not-started + ungrounded-chat-claim the same way was DENIED by the
auto-mode classifier as beyond the grading mandate — finish the generic
version (every hook-fired move checks `_move_muted`) in pass 2 so "muted"
means the same thing at every tier. (f) **escalate/checkpoint's new
never-fire clause is CONTRACT only**: escalate/* is excluded from the
classifier catalog and checkpoint actually fires from observer.py's
fire_count-based escalation — the code counts fires, not same-drift
recurrence, which IS the graded 7-FP defect. The clause states the contract;
the observer escalation change (require same-move recurrence post-correction)
is a code edit for this pass, one concern, small blast radius.

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
   **MEASURED 2026-07-07 night (Fable), durationMs oracle:** post-fix split =
   **32 in-turn / 3 next-prompt (~9% late, from 25%)**. The 3 late: ebade1fd
   s6 (TP; wait engaged, ran the FULL cap, 10685ms — capped class survives at
   10s), f4b895d7 s2 (TP; capped class), 84a58ca5 s8 (FP; 29ms skip with the
   verdict landing in the idle gap — residual VERDICT-AFTER-TURN, 1 specimen).
   **But the latency-tax finding overturns 2h.6b's theory: of 114 post-fix
   Stop invocations, 75% ran ≥10.5s and ZERO landed in 0.1–9.5s.** Bimodal =
   the wait is not racing a slow classifier; it waits for something that
   structurally does not arrive mid-wait (the observer doesn't classify
   turn-final text until the next event pokes it). The cap bump converted
   nothing and 1.75×'d a tax now paid on three-quarters of turn ends (~8s
   mean). Top plumbing item for this pass: either make the Stop wait CONVERT
   (poke the observer to classify the final text, then wait on that), or
   revert the cap toward 6s / skip the wait when no flag is already pending.
   The measured double-stat cost is invisible (28 skips all <100ms — check
   whether the double-stat path engages at all on the skip branch).
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
   **PARTIALLY MEASURED (night-half):** unverified-done-claim **0/3 → MUTED**
   (see moves.md note); ungrounded-chat-claim **1 TP / 1 FP** — the pass
   first graded the f4b895d7 fire FP (two named artifacts had earlier
   in-session reads) then REVERSED it to TP on the session's own evidence
   (the OTHER named artifacts were unverified recall; rechecking found
   render_ui_to_png bypasses UICacheManager and changed the plan — the
   pass had sampled the named artifacts instead of checking all of them;
   reversal record in the corpus). The 5363065f FP (system-context +
   earlier tool-result provenance) still motivates T3;
   landing-doc-reflex **1/1 + 1 FP-lean** (4340cb05 TP produced the missing
   report; 5e1aca3d fixtures-freeze had its doc trace in a memory update —
   consider a landing-size/class threshold, n=2, no edit). Sharpened
   confessed-stopgap graded against the CONTRACT: 1 TP (c9e4d45d allow-excuse)
   / 4 FP — two are the self-disposal class T2 will exempt at runtime,
   but TWO are classifier misidentifications with NO stopgap marker in the
   flagged edit at all (a5d63eee coverage-gap "yet", 1f5cc037 grading-script
   write) — that second defect is not covered by T2; needs its own look.
   Tonight's 7 NEW moves: **zero live fires** (catalog went live at d7c80f78
   19:33, observers revive naturally) — all still UNVALIDATED, nothing to
   grade, unheeded-warning dormant on T10 as specced.
3. **Score the pass-1 rework + mute expiry:** verify-claim post-rework fires
   only (pre-rework grades don't count); scope-drift mute expires ~07-12 —
   decide: attribution-aware rework using §2f session facts, fold into the
   §2d phase tier, or extend the mute. Do not unmute unchanged.
   **MEASURED (night-half): verify-claim post-rework = 49% (24/25/1, n=50)
   — the rework did NOT clear the gate.** FP taxonomy for the next single
   change, from the graded notes: (a) ~11 fires with NO claim in view (recon
   reads, task-start UserPromptSubmit, waiting-on-TaskOutput — the #32
   cadence-fire suspicion is CONFIRMED); (b) ~10 fires on claims already
   verified in-turn/same-compound (workers worst: 6/7 FP on literal command
   output); (c) ~4 on rephrase/report-of-verified-work turns. Class (a) is
   the biggest and cleanest: require a stated claim in the window. Move
   stays frozen until Opus makes that one change. scope-drift decision
   still Opus's (~07-12).
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
   **DONE (night-half): 2/10 → flag pulled, then RESTORED same night in
   OBSERVATION-ONLY mode at Peter's direction** (the pull had killed worker
   observation entirely, not just whispers). Corrective fires on worker
   mailboxes now log `worker_shadow_fire` telemetry instead of delivering
   (observer.py `_resolve_fire`, tested); advice + mechanical still deliver.
   Your item: grade the accumulated `worker_shadow_fire` records at pass 2/3,
   build the worker payload variant + verify-claim precondition, and flip
   corrective delivery back on per-move at the §2b bar.
8. **Specific graded-window debts:** ~~session 10977941~~ **DONE (night-half),
   and the lean was WRONG:** the circling ×2 → checkpoint → circling chain
   graded all-FP from timestamp-anchored transcript windows — the session was
   a monotonic compiler-driven migration whose "ended no-commits" premise is
   refuted in-transcript (P2 landed @ fe363d86, P5b @ 6cf04bcf). The
   telemetry fire-shape suggested stuck; the transcript proved converging —
   the same observation-vs-inference lesson as the round-1/2 windowing one.
   All 17 main + 1 worker fires graded (8 TP / 9 FP main). Null-move_id
   pre-07-05 records ignored as specced; the 07-07 final-window sessions are
   graded through ~20:10 (fires after this pass's write are pass-3 material).
9. **Recall honesty:** pass-1 miss-hunt closed EMPTY (Peter had none to
   report) — recall stands at "insufficient data", not zero-miss. Ask Peter
   for specimens again; if none, say so again.
   **Night-half input:** 17 session-logged miss lines folded into
   live_grades.jsonl (correct:"miss") — Peter-caught and self-caught FNs,
   several already consumed as tonight's new-move specimens (root-fix-retreat
   ×2, false-fork, unchecked-negative-claim, name-the-blocker,
   communication-register). Recall over the window: 72 TP / ~15 unique FN ≈
   0.83 by the file, but treat as a floor-quality estimate, not a gate pass —
   misses only surface when someone notices.
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

12. **The 07-07-night Fable grading session RAN — its output is the
    "Night-half results" section above** plus the "[pass 2026-07-07 Fable
    night]"-stamped records in eval/live_grades.jsonl. Build on it; verify,
    don't redo. What it deliberately left for you: every frozen-move
    measurement (circling clauses, ungrounded-resolution memory-provenance,
    git-landing escape hatch, verify-claim next change per item 3),
    checkpoint's new clause (measure it), the Stop-wait structural fix
    (item 1), the worker re-enable decision (item 7), scope-drift expiry
    (item 3), and everything in §Triage marked pass-2 watch. New small
    residues found while grading: skim's FP class is conclusions resting on
    structurally-gated evidence (&&-chained ancestry checks, captured run
    output — 84a58ca5 s7 is the clean specimen); permission-creep keys on
    Peter-ORDERED asks (99ea1793) and genuine taste forks (0d9e8fba); an
    85b5962e note/harness-upgrade-followup session line (atlas cell-picking
    unverified headless) turned out to be already tracked as BUG-034, whose
    BUG-033 gate is now open — nothing lost by the session-file clear.

**TICKETS.md T1–T11 are DONE as of 2026-07-07 night (Sonnet, 3-lane worktree
session)** — T1–T6, T8–T11 shipped and landed on main; T7 SKIPPED with
discovery findings recorded in the ticket (no hook/skill governs MEMORY.md
compaction in this repo). Grade the now-live detectors against their
tickets in the July-12 pass: git-landing's command-position anchoring (T1),
confessed-stopgap's self-disposal exemption (T2) plus its resolved a5d63eee
audit (the grading note misattributed the artifact — see the ticket close
and the code comment above `DISPOSAL_TRIGGER_RE` in common.py — the
mesh_pipeline.rs `#[allow(unreachable_code)]` edit itself was never
actually graded and remains an open TP/FP question), ungrounded-chat-claim's
widened vocabulary (T3, plus a root-caused pre-existing bug: `_EXT_ARTIFACT_RE`
matched bare filenames with no existence check before this pass), three new
preToolUseBash lints (T4/T5/T8, warn-only) and one narrow compound-landing
DENY (T6), mechanical/stale-brief's first live fires (T9),
anchor/unheeded-warning's now-populated hook-warning ledger annotation (T10
— it can finally fire; still needs its first real grade), and T11's
telemetry-leak fix (verify no new test-residue records land in
telemetry.jsonl across the next few sessions' suite runs).

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
