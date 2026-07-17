# Landing report — wave/scene-panel-ux (SCENE_PANEL_UX UX-P1 + UX-P2) · 2026-07-17

**Landed:** merge of `wave/scene-panel-ux` onto main (SHA in merge commit). Two orchestrated Sonnet sessions (slot-0), Fable orchestrating + PNG-reviewing per phase. Design: `docs/SCENE_PANEL_UX_DESIGN.md` (authored + landed same day, `d465f9ef`).

## What landed

**UX-P1 — selection responds + outliner unification**
- `PanelAction::SceneSetupSelectionChanged(LayerId)` (panels/mod.rs) + dispatch arm returning `structural_change: true` (ui_bridge/mod.rs) — outliner clicks rebuild the panel the same frame. Kills the "panel only updates once you move a param" bug at its mechanism (event-gated sync never fired on panel-local selection).
- Outliner: Scene/Lights/Objects section labels, one row template `[icon | name | eye]` (live eye on object rows, dimmed non-interactive elsewhere — uniform trailing slot), compact single-row +Object/+Light/Import.
- Flow: `scripts/ui-flows/scene-setup-select-updates.json` (L3) — three selection changes, header asserted after each, zero param writes.

**UX-P2 — properties rows on the card family**
- Metallic/Roughness: real `BitmapSlider` rows (mirrors layer_header's gain slider), replacing steppers.
- Transform cells: hover state + `ResizeHorizontal` cursor (app.rs `update_cursor_for_position`), scrub hairline, slider-token value text. `value_cell.rs` untouched (contract pinned).
- Color: 14px live swatch (`SWATCH_W`, the one new style constant) left of R/G/B scrub cells; display-only per D4.
- Modifier chip grid deleted → one `+ Add Modifier` dropdown (`SceneSetupAddModifierClicked` action, `MESH_MODIFIER_CHOICES` items); `rg modifier_chip` → 0.
- Flows: `scene-setup-modifier-stack.json` re-pathed to the dropdown; new `scene-setup-ux-p2-demo.json`.

## Gates at landing (main checkout, post-merge)

- Full workspace sweep: clippy `--workspace -D warnings` clean · `cargo deny check bans` ok · `cargo nextest run --workspace` green (counts in push log).
- Acceptance demos re-run in main checkout: `scene-setup-select-updates.json` + `scene-setup-ux-p2-demo.json` — pass; PNGs reviewed by the orchestrator both at phase gates and at landing.

**Verification level: L3** (both phases). **Verification debt:** the mid-scrub hairline has no PNG proof — the ui-snap `Drag` gesture is atomic (no mid-drag snapshot point); covered by unit test `roughness_slider_sweeps_full_range_and_value_box_opens_typein` + code read. One line added to `docs/VERIFICATION_DEBT.md`.

## Click-script for Peter (~2 min)

1. Open a project with an imported GLB layer → Scene Setup dock.
2. Click between object rows in the outliner. Expect: Properties follows each click INSTANTLY (no param nudge needed).
3. Note the outliner: Scene/Lights/Objects sections, every row has an eye (dimmed on Camera/World/sun).
4. Drag the Roughness slider full range in one sweep. Expect: card-style fill bar, live value.
5. Hover a Position cell. Expect: horizontal-resize cursor + lightened cell. Drag to scrub; Shift-drag for fine.
6. Scrub a Color cell. Expect: the swatch left of R/G/B tracks the color live.
7. Click "+ Add Modifier" → pick Twist. Expect: dropdown (not a chip grid); a Twist stack appears (BUG-218 fix landed earlier today).

## Follow-ups (recorded, not debt)

- UX-P3 amendment coming: scene param rows become param_card rows 1:1 (drawers + modulation buttons) — Peter's directive this session ("Maybe reusing the card sliders and drawer and modulation buttons etc 1:1 would make sense here? One surface to learn?").
- Swatch polish (size/border) folds into UX-P3.
- Scroll + close-button fixes are a parallel lane (lane/panel-interaction-bugs), landing separately.
