# Landing — WIDGET_TREE P4 · 2026-07-21

**Merged:** `lane/widget-tree-p4` (`a5778752` lane · `c70831ea` doc correction), `--no-ff`. One code file touched: `ui_bridge/inspector.rs`, +508/−0.

## What landed — and what turned out not to need landing

**The consolidation was already done.** The lane verified (rather than assumed) each dual-edit target and found the effect/generator twins were collapsed over `GraphParamTarget` by `79905d63` (2026-06-08, "unify all UI dispatch arms") — a month before the design's audit. The design doc's §1 transcription-#4 row is now corrected in place. `resolve_param_range` already reads the manifest slot spec. Zero `ParamCardKind::` forks in `ui_bridge/inspector.rs` (negative gate quoted in lane report).

**What did land: the `row_dispatch` Harness family (closing P2's owed gaps #2/#3)** — 14 bridge-level tests, each `PanelAction` dispatched against BOTH a master-effect and a layer-effect target through the real `dispatch_inspector` path (via the editor-context entry, real production code), commands executed + undone through a real `EditingService`. This is the standing "fixed for Master, forgot Layer" detector. Two real semantics discovered and pinned as such, not forced symmetric:
- `EnvelopeToggle` on a MASTER effect is intentionally inert (effects are clip-timed; documented no-op) — pinned as a zero-commands assertion.
- `AbletonInvertToggle` is deliberately NOT undo-tracked (`MutateProject`, mirroring `TrimChanged`'s Ableton branch) — pinned as a both-sides-flip, no-Execute assertion.

## Gates (orchestrator-run)

- Worktree: `nextest -p manifold-app` → `324 passed, 3 skipped` (all 3 exemplar suites green untouched; 14 new tests named). Clippy `-p manifold-app -D warnings` clean.
- Main post-merge: `cargo nextest run --workspace` → **`3861 passed (10 slow), 13 skipped`** · clippy `--workspace` clean · `bans ok`. (No flows re-run: zero production-code change in this landing.)
- **BUG-283 opened:** the lane found `clippy --tests` fails on three pre-existing unrelated files (lint drift invisible to the standard gates — they never compile test targets). Logged with fix shape; gate-policy question flagged for Peter.

**Level:** L1 by design (test-only landing; the tests ARE the deliverable). `Shortcuts taken:` lane used the editor-context dispatch entry rather than simulating ambient tab clicks — real production path, stated openly.

**VD:** VD-034 carried (card-drag flow → P5). Remaining phase: P5 only.
