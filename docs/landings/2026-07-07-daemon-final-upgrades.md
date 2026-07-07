# Daemon final-window upgrades — landed 2026-07-07 @ <filled at merge>

**Branch:** feat/daemon-final-upgrades · **Level reached:** L2 (full automated
suite; no GUI surface exists for this change class) / target L2
**Doc status line (quoted verbatim):** `## 2h. Final-window extensions (SPECCED
+ BUILT 2026-07-07, Fable authoring + two Sonnet builders, same day — all
sections shipped; workflow-agent DELIVERY unproven pending the live probe,
observation tested against real layouts)` (.claude/daemon/DESIGN.md)

## What landed
Fable-authored: three new mechanical moves (ungrounded-chat-claim,
unverified-done-claim, landing-doc-reflex — catalog 31→34), DESIGN.md §2h
build contract + §2h.6 late-fire forensics, MOVE_AUTHORING.md (the authoring
method for the Opus handover), PASS2_AGENDA.md (consolidated pass-2
checklist), RUNBOOK pointer. Sonnet-built: chat-tier + done-claim detection at
the Stop valve; snapshot-race defect fix in the catch-up wait (double-stat,
~200ms); STOP_WAIT_CAP_S 6→10; worker self-grade ack + worker grade
backstop/observation prompt (threshold 20 events, once per worker, escape
valve); landing-doc-reflex in the observer (transcript gitBranch field, no git
subprocess); workflow-agent discovery (subagents/workflows/*/); verdicts/
hygiene sweep at observer start.

## Gate results (verbatim tails)
Every test file under .claude/daemon/ and .claude/hooks/ run individually in
the worktree, 2026-07-07:
test_fatigue_ordinal 11 · test_git_landing_detection 21 · test_habit_memory 22
· test_hygiene_sweep 19 · test_landing_doc_reflex 41 · test_phase_transitions
24 · test_priming_tier 44 · test_richer_windows 17 · test_scoring 19 ·
test_session_facts 38 · test_slice_fires 57 · test_stop_valve 120 ·
test_stopgap_detection 33 · test_worker_nudges 25 — all "N passed, 0 failed";
test_window_caps PASS-line convention, green. Hooks: test_ask_question_guard
46 · test_dead_code_suppression_hook 10 · test_lsp_nudge 24 ·
test_preToolUseBash 35 · test_workflow_gate 27 — all green. No Rust touched;
no cargo gate applicable.

## Deviations from brief
Worker A: Builds 1+2 share one commit (shared main() control flow); Build 3
committed at old cap, cap bumped separately; read-only-Bash detection is a
first-token regex (fails toward firing); markdown-decoration lstrip in
sentence matching. Worker B: landing detection uses the transcript's per-event
gitBranch instead of a git subprocess (better than spec); `-C <other-dir>` git
commands abstain; LANDING_DOCS_WINDOW_EVENTS=30 introduced as a pass-2-tunable
placeholder; "pre-v2 residue" deletion NOT implemented (undefined file class —
refused rather than guessed). Orchestrator: mispriced-fork agenda item found
already covered (price-the-fork + ask-gate) — not re-authored.

## Shortcuts confessed (rolled up)
Worker Stop events reuse data["transcript_path"] as-is — inherited, unverified
assumption (probe owed). python3-anything counts as script-run verification
(§2f table crudeness) — possible FN source for unverified-done-claim.
Stop-tier mechanical moves get per-turn stopblock dedup, not a true 20-event
cooldown (pre-existing pattern, inherited by the two new Stop moves).

## Verification debt
Workflow-agent DELIVERY unproven (observation tested; the live 1-agent probe
workflow needs Peter's explicit go — recorded in DESIGN.md §2h.3 and
PASS2_AGENDA). Live observers pick up observer.py changes only on natural
idle-exit revive (cross-session SIGTERM is permission-gated); Stop-hook
changes are live immediately per-call once on main. In-turn delivery split
must be re-measured by pass 2 (baseline 52/17, oracle: stop_hook_summary
durationMs).
