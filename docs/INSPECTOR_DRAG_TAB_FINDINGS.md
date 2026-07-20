# Inspector Card Drag + Tab Scope — Investigation Findings

**Date:** 2026-07-20 · **Investigator:** K3 (read-only investigation, no code changed) · **Status:** FINDINGS ONLY — pending Fable review before implementation. Do not implement from this doc until Fable signs off.

Tracked as BUG-265 (drag indicator), BUG-266 (tab pin), BUG-267 (duplicated card lists).

Symptoms reported by Peter: the blue drop indicator is often wrong; cards are hard to move; inspector tabs change at unexpected times (noticed when adding effects). Both break nearly every time the inspector/card UI is touched.

---

## BUG-265 — Card drag indicator/target wrong after scroll or animation

### Root cause 1: stale `card_y` after in-place scroll (primary)

`crates/manifold-ui/src/panels/inspector.rs:1692-1708` (`update_card_drag`) hit-tests the cursor against `card.card_y()`, a field snapshot written only during `build()` (`param_card.rs:2213` and `param_card.rs:2977`). Wheel and scrollbar scrolling move the actual tree nodes in place via `ScrollContainer::offset_content` (called at `inspector.rs:1477`) and deliberately skip a rebuild (`scrolled_in_place` path). Nothing updates `card_y` on that path.

Result: indicator Y and drop-target index are off by exactly the scroll delta accumulated since the last rebuild. Correct right after a rebuild, wrong after any scroll — matches "often wrong, not always."

### Root cause 2: height from a second, live source

The same loop uses `card.compute_height()` (`param_card.rs:1611`), which re-derives height from animated state (`collapse_frac()`, `animated_drawer_height()`) instead of the laid-out rect. Mid-tween, hit-test geometry disagrees with the screen. Two parallel implementations (`compute_height_effect` `param_card.rs:1618`, `compute_height_generator` `param_card.rs:1690`) must each mirror the build draw loop exactly — BUG-108 was already this exact drift, and the param-drawer unification (b0124dc3) added more animated height to keep in sync.

### Root cause 3 (latent): drop-index assumes contiguous tail

`inspector.rs:1762-1769` (`end_card_drag`): `to_fx = cards[to_card].effect_index()`, else `last.effect_index() + 1`. Assumes the tab's cards are a contiguous run in the flat effects list. True today; any future per-tab filtering breaks it silently.

### Structural note

Card geometry lives in three places that must agree by hand: build-time layout, `card_y` snapshot, live `compute_height()`. This is why the bug recurs on every card-UI change.

### Fix shape (for Fable to weigh)

Preferred: hit-test against actual tree node bounds (the only scroll-current source) instead of `card_y`/`compute_height`. Cheaper stopgap: update `card_y` in the in-place scroll path. Root fix removes the class.

---

## BUG-266 — Inspector tab pin dies on incidental selection changes

### Mechanism

- Tab click → `pin_scope` records `(tab, selection_version)` (`crates/manifold-ui/src/ui_state.rs:203-209`).
- `pinned_scope()` returns the pin only if the version still matches (`ui_state.rs:193-197`). **Any `selection_version` bump clears it.**
- Every state sync recomputes the active tab (`crates/manifold-app/src/ui_bridge/state_sync.rs:2293-2310`): pin if alive and in the derived tab set, else default = Layer (or Group) whenever a layer exists. Peter confirmed 2026-07-20: the Layer default is correct and stays.

### Why tabs "randomly" change

Adding an effect mutates the selection behind the scenes → version bump → pin silently dies → snap back to Layer. "Sometimes" because only selection-touching add paths trigger it. Second path: the pin is filtered against a freshly recomputed tab set (state_sync.rs:2295); if the selection drifts to another layer, the pinned tab can vanish from the set without any version bump.

### Fix shape

Make the pin sticky: clear only on explicit user action (clicking another tab, or a genuine user selection change in the timeline), not on command side effects. Likely means decoupling pin invalidation from `selection_version` and keying it to selection-identity changes. Scope of "genuine" is a Fable call.

---

## BUG-267 — Duplicated card lists (master_effects / layer_effects)

`inspector.rs` keeps two `Vec<ParamCardPanel>` with parallel match arms at every touchpoint: `cards_for_tab` / `cards_for_tab_mut` (:1836-1848), `find_drag_handle` (:1810-1834), selection sets (:1101-1125), `skip_to_settled` (:349-353), press routing (:1492-1501, :1530-1539). Layer/Group/Clip all alias `layer_effects`. Every new card behavior must be written twice; "fixed for Master, forgot Layer" is a standing bug class. Peter flagged this as a huge concern 2026-07-20.

### Fix shape

One vec keyed by scope (or one vec + scope field on the card). Mechanical but wide — worth a dedicated lane, not folded into BUG-265.

---

## Smaller improvements (fold in or defer per Fable)

1. **Auto-scroll during card drag.** `update_card_drag` clamps the ghost to the viewport but never scrolls; long effect lists are unreachable drop targets. Edge-proximity scroll while `card_drag_active`; the in-place scroll path already exists.
2. **Multi-select drop footprint.** Multi drags dim all selected cards but show a single 2px insertion line (`DRAG_INDICATOR_H`, inspector.rs:93). Size/tint the gap for the group.
3. **No drag cancel.** `end_card_drag` always reorders unless a perfect no-op (inspector.rs:1801-1805). Esc-to-cancel or drop-outside-cancels is nearly free.
4. **Geometry unit tests.** Cursor-Y → target-index is pure math; test scrolled / animating / mixed-height layouts. This is the missing "breaks every time we touch it" detector. Same for pin survival across add-effect. Note BUG-263: no app-level harness for gesture paths — unit level may be the only cheap option.

## Verification notes

- Read-only investigation; root causes confirmed by code reading, not yet reproduced on screen. A headless repro (scroll → drag → indicator offset = scroll delta) would confirm before fixing.
- BUG-108 (compute_height drift) and the b0124dc3 param-drawer unification are the relevant prior art.
