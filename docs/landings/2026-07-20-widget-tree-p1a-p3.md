# Landing — WIDGET_TREE P1a + P3 (batched) · 2026-07-20

**Merged:** `lane/widget-tree-p1a` (`1153bc73` + doc-status commit) and `lane/widget-tree-p3` (`cb2347b6`), both `--no-ff`, pushed as `f95fd5a4` lineage. Design: `docs/WIDGET_TREE_DESIGN.md` (APPROVED same day; status marker update to "P1a+P3 landed" rides the P1b landing — *_DESIGN.md edits need a worktree).

## What landed

- **P1a (Sonnet lane):** the parallel-vec disease's storage half is dead — `ParamCardConfig`'s 14 per-param vecs → `rows_mod: Vec<RowMod>`; `AudioCardState`'s 15 → `rows: Vec<AudioRowState>`; state_sync's private `CardModulation` assembler (a 4th twin, lane-surfaced escalation, accepted) eliminated. 7 files, +317/−442.
- **P3 (Sonnet lane):** geometry monopoly proven — `compute_height` audit: every production call site is build-time layout (classified list in lane report), zero post-build consumers; 4 new pure-math tests incl. the INV-3 pin `inv3_drag_targets_follow_live_bounds_after_in_place_scroll` (drives scroll-in-place → drag → dispatched `EffectReorder`) and a non-contiguous `end_card_drag` index pin. +213 test lines, zero production change.

## Gates (run by orchestrator, main checkout)

- `cargo nextest run --workspace` → `Summary [150.223s] 3836 tests run: 3836 passed (10 slow), 13 skipped`
- `cargo clippy --workspace -- -D warnings` → clean · `cargo deny check bans` → `bans ok`
- `cargo xtask ui-snap timeline --script scripts/ui-flows/drag-clip.json` → all 7 steps ok (clip 230→314px through the real input path) — **L3**
- `cargo xtask ui-snap inspector --script scripts/ui-flows/select-and-inspect.json` → all steps ok
- Negative: no `Vec<bool>`-style parallel modulation fields remain on `ParamCardConfig`/`AudioCardState` (`rg 'driver_active|trim_min|target_norm|env_decay'` hits only per-row struct fields, `ParamModState` runtime state, and core model fields).

**Level reached:** P1a **L1** (interior refactor pinned by 1168 untouched suite tests — the design doc's own characterization); P3 **L1 math + L3 generic drag flow**. Gap: the card-drag flow variant → **VD-034**, owed to P5's flow sweep.

## Click-script for Peter (≤2 min)

1. Open a project with a layer carrying 2+ effects. Open the inspector.
2. Scrub any effect slider → value follows, one undo entry (Cmd+Z restores).
3. Arm a driver (D) on a param → badge lights, drawer opens with beat-division buttons (P1a moved all this state per-row).
4. Scroll the inspector, then drag a card by its handle → the blue indicator lands where the cursor is, drop reorders correctly (W2-B + P3's pins).

## Deviations / notes

- P1a lane touched 4 files beyond the brief's 3 (lib.rs export, ui_root.rs fixture, inspector.rs test helper, audio_trigger_section.rs consumer) — all forced by the re-point, reported, reviewed, accepted.
- P1a eliminated `CardModulation` (not in brief) — the lane applied the brief's own clean-fit criterion and reported it; accepted as within D3's intent.
- `Shortcuts taken:` none (both lanes).

**VD opened:** VD-034 (card-drag flow variant → P5). **VD carried:** none touched.
