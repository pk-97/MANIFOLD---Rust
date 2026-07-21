# Landing: UI_FUNNEL_DECOMPOSITION P-I — the unified scrub wire

**Date:** 2026-07-22 · **Branch:** `lane/ws1-scrub` → main · **Executors:** four rotating Opus seats (ws1-scrub .. ws1-scrub-4), Opus-direct per the delicate-work ruling; **Lander:** ws1 orchestrator.

## What landed (17 commits)
- `PanelAction::Scrub(ValueRef, ScrubPhase)` — ONE gesture wire for 15 scrub families (all Snapshot/Changed/Commit trios dead) + the Marker guard + the Fork-2 frame-resident fold. `ScrubPhase{Begin, Move(ScrubValue), Commit}`, value on Move only; `ScrubValue::{Scalar, Range}`.
- `ActiveInspectorDrag` EXTINCT (zero live references) along with ALL ten `Application` scrub-snapshot fields and the interim `ScrubState` Options — the 18-arg-dispatch disease's last organ. One `ScrubState.active` slot; single-active invariant machine-checked at both entry points.
- D8 direct-set flow verb: `AutomationAction::SetParam` = Begin/Move/Commit through the REAL dispatch path; flow `set-param-layer-opacity` 8/8 green incl. undo.
- D4 amendment applied (two-role wire/resolved split; frame-resident honest-cost; wire-carries-what-the-panel-knows precedent).

## Gates
Per-family (in every commit message): `undo_baseline` clean+stomp GREEN with ASSERTIONS UNMODIFIED across the whole phase — the parity contract held 17/17 commits; nextest 1171/1171 per commit; clippy both flavors clean. Phase close: oracle set 64/64. Full sweep + flows quoted in the push-time addendum.

## Verification level & debt
L1+L3. **VD-037:** the single-active-gesture invariant (cross-window overlap case) rests on reasoning + field assertions (debug_assert/dev, loud warn/release), not an exercised test — burn down: morning interactive smoke (drag in editor + main window simultaneously) or a scripted two-window flow when the harness gains one. Consciously carried, named by the executing seat.

## Stage note
Every scrubbable control now shares one gesture engine: identical feel, exactly one undo entry per drag, everywhere — including type-in (which now also arms touch-to-select, an accepted behavior change). This is P-I's payoff for the instrument.

## Push-time addendum
Full sweep on final merged main: `Summary [176.314s] 3851 tests run: 3851 passed (21 slow), 13 skipped`; clippy workspace clean; deny `bans ok`; flows **34/34 required passed (incl. the new D8 `set-param-layer-opacity`), 7 xfail, 41/41 accounted**. One landing-time catch: the renderer swatch test still spelled the deleted MasterOpacity trio (the P-D third-crate lesson, missed by the P-I seat briefs) — ported to the wire in `18d…` fix commit; the seat-brief template gains "renderer test targets in gate scope" permanently.
