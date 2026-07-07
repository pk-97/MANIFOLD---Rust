# wave/transport-sync-anchor-fix (BUG-050 partial) — landed 2026-07-07 @ 6bf0c23b

**Branch:** wave/transport-sync-anchor-fix · **Level reached:** L1 / target L4 (rig confirmation, Peter — same VD-013 checklist)
**Doc status line (quoted verbatim):** unchanged — "**Status:** P1–P3 SHIPPED 2026-07-07 (same-session build, Fable) · P4 landing complete except the L4 live checklist (§6 P4 demo — Peter-owned, the real acceptance gate)" — plus deviation 5 (moving-anchor amendment) added.

Same-day follow-up to `2026-07-07-ableton-transport-sync.md`: Peter's first L4 run
(checklist step 1) failed — BUG-050. This landing fixes the proven half and
instruments the unproven half.

## What changed
- `transport_sync.rs`: `Expectation` gains `anchored_at`; playing acks accept
  [beat−ε, beat+elapsed·tempo+ε] (interval, not point); T7 retransmits and the
  D11 deferred seek carry the engine's CURRENT beat and re-anchor (no more
  backward yanks); `[ABL-SYNC]` info/warn logs on gesture-retry/ack/degrade,
  each with the observed snapshot (or UNOBSERVED markers).
- Harness: `FakeAbleton` gains `listener_cadence`; `f2` asserts playhead
  convergence + never-behind-gesture (anchor-fix semantics); new `f8`
  (174 BPM, 0.5s cadence) pins no-yank-back + slow-listener convergence, with
  an honesty note that it does NOT bite pre-fix code.
- Drive-by: one pre-existing clippy `len_zero` in
  `project_local_preset_reload.rs` (another session's file, blocked the gate).

## Gate results (verbatim)
```
cargo test -p manifold-playback (all targets): 158 + 9 + 8 + 19 passed, 0 failed
cargo clippy -p manifold-playback -p manifold-app --tests -- -D warnings: Finished (clean)
```
Bite-proof: `t7b_retransmit_seeks_current_beat_not_stale_anchor` (and the f2
convergence change) observed red against the pre-fix machine during
development; `f8` verified to PASS against the pre-fix machine in a detached
proof worktree — recorded honestly in its doc comment rather than claimed.

## Deviations from brief
This landing IS a deviation record — design doc deviation 5.

## Shortcuts confessed
- The real-rig ack starvation is UNDIAGNOSED. The fix removes the two provable
  defects (razor ack window, stale-anchor re-seek) and both plausibly account
  for the symptom, but the harness could not reproduce the multi-retry
  starvation, and retry queries should have acked even pre-fix. The
  `[ABL-SYNC]` logs are the instrument; claiming this landing fixes BUG-050
  outright would be overclaiming, so the bug stays OPEN pending one rig run.

## Verification debt
VD-013 carried (unchanged owner/steps — step 1 now also reads the `[ABL-SYNC]`
lines). No new entry: BUG-050 tracks the residual.

## Click-script for Peter (≤1 min)
1. Pull/rebuild, open the project, AbletonOSC + CLK up as before.
2. Repeat the failing gesture: play in MANIFOLD at a cursor away from
   Ableton's playhead.
3. Expected: both land at the cursor, no repeated Ableton snap-backs, chip
   green "ABL" after a brief "ABL…".
4. Either way, grab the `[ABL-SYNC]` lines from the terminal (they're info
   level) — one gesture's worth answers the open question if it still fights.
