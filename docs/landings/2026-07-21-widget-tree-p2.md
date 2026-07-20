# Landing — WIDGET_TREE P2 · 2026-07-21

**Merged:** `lane/widget-tree-p2` (`28b26412` seams Fable · `7a0b42ca` routing swap Sonnet lane · `31ccf0d8` card-root identity Fable · `45789fd6` sweep fixup), `--no-ff`. 15 files, ~+1,550/−900.

## What landed

**Routing is a lookup.** `RowIndex: WidgetId → (row, RowRole)` populated at build from the same rows being rendered; ONE `row_action` for both card kinds (target derived — the effect/generator click twins are deleted: `handle_click_effect`, `handle_click_generator`, `pid_at` gone from `ParamCardPanel`). Bundle sub-elements resolve through the bundles' own `resolve(NodeId)` methods (widget-contract split). `register_intents` re-pointed at rows; right-click contracts untouched.

**Identity is keyed end-to-end (D4).** Rows: `stable_key(param_id) << 8 | role` on row roots/elements; drawers keyed via a new `key: Option<u64>` on `BitmapSlider::build`/`drawer::build` (all pre-existing callers pass `None`). Card ROOTS: new opt-in `View::identity(u64)` pins the durable WidgetId through both ChromeHost mint paths — `stable_key(EffectId)` for effects, layer-derived for generators. **Deliberate deviation from a naive reading of D4:** `View::key` was NOT threaded into WidgetId salts — keys are only host-unique and hosts share tree parents (several panels use constants `1,2,3…`); global threading would have made cross-panel collisions a standing trap. The opt-in `identity` field keeps the trap impossible. The tree's duplicate-WidgetId assert validated this the hard way: it caught two test fixtures minting sibling cards with one `EffectId` (impossible in production — instance ids are unique) and two tests double-building without the region truncation the live path always does. Fixtures fixed, documented in place.

**Enforcement (§5b) shipped:** `crates/manifold-ui/tests/no_bespoke_row_infra.rs` (allowlist scan: `BitmapSlider` construction + `Vec<Option<NodeId>>` fields outside sanctioned modules FAIL THE BUILD, message points at §5b); CLAUDE.md hard-rule line added; the five-step recipe already lives as `param_surface.rs`'s module doc. The hook deny-pattern rides the approved hook-trim pass (not this landing).

**Tests:** keyed-identity family (`row_identity_survives_an_earlier_row_arming_a_drawer`, `card_identity_survives_effect_chain_reorder`), `stable_key_is_pinned` (cross-process hash pin — updating it is a breaking change to the automation surface, says so in place), + new role-dispatch tests (EnvelopeBtn, RowCatcher, Label/OSC, enum value cell, toggle-row label); pre-existing dispatch tests all green through the new path (byte-identical `PanelAction` output).

## Gates (orchestrator-run)

- Worktree: `nextest -p manifold-ui -p manifold-app` → `1184 passed, 3 skipped`; clippy clean.
- Main post-merge: **`3847 passed (15 slow), 13 skipped`** · clippy `--workspace -D warnings` clean · `bans ok`.
- Flows: `select-and-inspect` + `drag-clip` exit 0 — **L3**.
- Negative: `handle_click_effect|handle_click_generator` → 0; `pid_at` → 0 in param_card (scene's own separate `pid_at` remains, its file is the convergence lane's); no row-array id-scans in `handle_*` bodies (single-scalar card-chrome checks + the 3-element relight loop remain, by design).
- Workspace sweep caught one focused-gate escape (renderer swatches test caller missing the new args) — fixed on the lane before push. Lesson: signature changes gate on the workspace sweep, not `-p` scopes.

**Level:** L3. **Performer gesture verified by suite:** arming a driver on one row no longer renumbers any other row's widget identity mid-set; reordering the effect chain keeps every row's identity.

## Carried gaps (owned, not hidden)

1. **`match_param_row_click` survives as a documented delegation shim** for `scene_setup_panel.rs` only — body delegates to the same bundle `resolve`s `row_action` uses (can't drift). Dies when convergence P2 deletes its consumer; P5 gates on it.
2. **Bridge-level `row_dispatch` Harness tests** (ui_bridge) — owed to **P4** (it's ui_bridge work; added to its deliverables).
3. **Drawer-internal role dispatch tests** (DriverConfig/EnvelopeConfig/AudioConfig/AbletonConfig) — indirect coverage only; owed with #2.

## Click-script for Peter (≤2 min)

1. Layer with 3+ effects: arm a driver on effect 1's first param — badge lights, drawer opens.
2. Scroll, then drag effect 3 above effect 1 — indicator tracks the cursor, drop lands.
3. After the reorder: hover/click effect 3's D/E/A buttons — all still route correctly (keyed identity).
4. Right-click any slider track — resets to default (contract path untouched).

`Shortcuts taken:` none beyond the carried gaps above.
**VD:** VD-034 carried (card-drag flow → P5).
