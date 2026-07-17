# Scene Panel UX — the Scene Setup dock becomes an instrument, not a form

**Status:** IN PROGRESS — UX-P1 landed (selection responds same-frame + outliner unification); UX-P2 (properties rows on the card family) not started · 2026-07-17 · Fable
**Prerequisites:** SCENE_OBJECT_AND_PANEL_V2 (SHIPPED `e78d97d2`). Independent of REALTIME_3D P5/P6 (viewport/gizmos) — the two land in the same wave but share no code seam.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter, 2026-07-17, looking at the shipped v2 panel against a real GLB import: *"It's also really weird to select objects, it only updates once you start moving the params. We should reuse the effect card and widget infra, unified style system etc. It needs to be more intuitive."* The governing insight: the v2 wave shipped the panel's **data contract** (scene VM, value-cell gestures, command routing) correctly, and its **presentation** as bespoke minimal rows. Every fix in this design is presentation and event plumbing — zero model changes, zero new state, zero new crates. On stage this is the difference between "scene editing exists" and "click an object, see its properties instantly, grab a value and pull" — the panel must respond at click speed mid-set, not at next-mutation speed.

Binding constraints checked (DESIGN_AUTHORING §1): no hot path (panel rebuild is event-gated, stays event-gated); thread residency unchanged (UI-local panel + `PanelAction` dispatch, house model); no time model involvement; **no persistence** (`SceneSelection` is explicitly never serialized — scene_setup_panel.rs:559); performance surface — yes, the whole point: selection latency and scrub affordance are performer-facing.

## 1. Audit — what exists (verified 2026-07-17, against `624d34a4`)

| Piece | Where | State |
|---|---|---|
| Scene Setup panel | `crates/manifold-ui/src/panels/scene_setup_panel.rs` (~3,900 lines) | Bespoke rows throughout; outliner + properties + modifier chips |
| Outliner click → selection | scene_setup_panel.rs:2432 | Sets UI-local `self.selection`, returns `(true, Vec::new())` — **no action emitted**, nothing triggers a rebuild |
| Panel rebuild | `sync_inspector_data` (state_sync.rs:1435, scene section) | Rebuilt from scratch per sync — but sync is EVENT-GATED: fires only on layer-selection version change, structural mutation, or active-layer change (app_render.rs:3326-3378). This gap IS the selection-lag bug. |
| Resync trigger mechanism | `DispatchResult.structural_change` (ui_bridge/mod.rs:32-34) | Exists, shipping — a dispatched `PanelAction` returning `structural_change: true` re-runs `sync_project_data` + `sync_inspector_data` the same frame |
| Value-cell gesture contract | `crates/manifold-ui/src/value_cell.rs` (D8, v2 P4) | SHIPPED: drag-scrub / Shift-fine / double-click-type / right-click-reset, pinned by `intent_for_pins_the_full_contract_table`. **Gestures work; the cells render no affordance for them** |
| Unified card row family | `crates/manifold-ui/src/panels/param_card.rs` + `param_slider_shared.rs` | SHIPPED: the effect/generator card contract — `BitmapSlider` rows, `SliderColors`, header/chevron chrome. The style system Peter means by "effect card and widget infra" |
| Slider widget | `crates/manifold-ui/src/slider.rs` (`BitmapSlider`, `SliderColors`, `SliderZones`) | SHIPPED, D2 of UI_WIDGET_UNIFICATION — pure `intent_for`, host-stateful drags |
| Dropdown | `crates/manifold-ui/src/panels/dropdown.rs` | SHIPPED — the precedent for the Add-modifier menu |
| Color swatch | audio_setup_panel.rs:687-693 (identity swatch row) | Nearest precedent. **No color-picker widget exists anywhere** (`rg -l 'ColorPicker|color_picker' crates/manifold-ui/src` → 0 hits, 2026-07-17) |
| Scene VM hierarchy | `scene_vm.rs:115-175` (`SceneVm.objects/lights`, `SceneObjectVm.material: MaterialVm`) | Material is already a CHILD of its object in the VM; the outliner renders it flat |
| Flow driver | `scripts/ui-flows/` + `cargo xtask ui-snap` | SHIPPED (UI_AUTOMATION P1-P2) — `scene-setup-modifier-stack.json` already drives this panel; L3 gates are cheap |

Extend, don't redesign. The audit's classification: everything below is *exists* or *one wire away* — the only genuinely-new pixels are the scrub-affordance chrome and the color swatch, both small draws inside existing row builders.

Out of scope, owned elsewhere: BUG-218 (modifier commands' stale splice point — manifold-editing, fix shape in BUG_BACKLOG.md), BUG-212 (Duplicate drops string bindings — same), REALTIME_3D P5/P6 (viewport + gizmos, briefed in that doc).

## 2. Decisions

**D1 — Selection updates ride the existing dispatch loop, not a new mechanism.** The outliner row click (scene_setup_panel.rs:2432) keeps setting `self.selection` locally AND now also returns a new `PanelAction::SceneSetupSelectionChanged(LayerId)` (payload: the layer whose selection moved — the panel key; the selection itself stays panel-internal, D7 of SCENE_SETUP_PANEL). `dispatch_inspector` handles it by returning `DispatchResult { structural_change: true, ..handled() }`. That single flag re-runs `sync_inspector_data` in the same frame (app_render.rs:3341) and the panel rebuilds with the new selection. Same-frame Properties update, no polling, no per-frame rebuild.
*Rejected: rebuilding the scene panel every frame* — violates the event-gated sync doctrine for a panel that's open during live sets; *rejected: panel-internal partial rebuild* — the panel has no `Project` access by design (`ui` doesn't depend on `core`); the VM must come from state_sync.
Consequences, stated honestly: `structural_change: true` also re-runs `sync_project_data` — heavier than strictly needed for a selection click. It's a click-rate event, not per-frame; measured cost of a full sync is far under a frame. A dedicated lighter flag is not worth a new field on `DispatchResult` (dont-cascade-redesign).

**D2 — Bounded-range material scalars become real `BitmapSlider` rows.** Metallic, Roughness (and any future 0..1 material scalar the VM exposes) render as the same slider rows effect cards use — `BitmapSlider` + `SliderColors`, fill bar, label left / value right — replacing the `[−] value [+]` stepper triplets. Shape it like param_card.rs's scalar rows. The steppers were v2's placeholder; a slider is what the same parameter looks like one panel over, and unification is the point.
*Rejected: keeping steppers with better styling* — the app already has exactly one way to render a bounded scalar; a second styled way is the drift this design exists to remove.

**D3 — Unbounded transform cells stay value cells and gain visible affordance.** Position/Rotation/Scale can't be sliders (unbounded). They keep the D8 gesture contract and get chrome that advertises it: (a) hover state — cell background lightens and the cursor switches to horizontal-resize (`crate::cursors`), (b) during a scrub, a thin accent underline animates with the drag (the `MOD_TAB_INK_H`-scale hairline idiom, param_card.rs:47), (c) the cell's value text uses the same font/weight/alignment tokens as slider value labels. No gesture changes — value_cell.rs is untouched (its contract test pins that).

**D4 — Color row = live swatch + the three existing scrub cells.** A square swatch rendered from the row's current RGB sits left of the R/G/B cells, updating live during scrubs. Shape the swatch like the audio dock's identity swatch (audio_setup_panel.rs:687). The swatch is display-only in v1 — clicking it does nothing. A full picker popover is **Deferred** (below).
*Rejected: shipping a color-picker popover now* — no picker widget exists in the codebase; inventing one mid-wave is scope widening. The cells already give full precision input.

**D5 — Outliner rows unify on one template; objects stay a flat list.** Every row renders `[type icon | name | trailing affordance]` with identical metrics. Trailing affordance: the eye toggle for every object row that has a `visible` param; a DISABLED (dimmed, non-interactive) eye glyph for rows that don't — never a different button in that slot (per `feedback_no_conditionally_visible_ui`, the slot's meaning may not change per row). Section labels ("Scene" for Camera/World, "Lights", "Objects") replace the current undifferentiated run of rows; selected row keeps the full-width highlight. **Objects are NOT nested** — REALTIME_3D "Decided — do not reopen" §1 pins the scene as a flat object list; the material belongs to the Properties body (where v2 already shows it), not to outliner hierarchy.
*Rejected: parent/child indentation of materials under objects* — re-opens a settled decision and misleads: outliner rows are selectable scene items; the material is a property of one.

**D6 — The three stacked action buttons become one compact row; the modifier chip grid becomes a dropdown.** `+ Object · + Light · Import…` render as three equal-width buttons on ONE row (same `PanelAction`s, same handlers). The seven permanent modifier chips are replaced by a single `+ Add Modifier` control opening the existing dropdown (panels/dropdown.rs) listing the same seven entries, dispatching the same `SceneSetupAddModifier` action. Reclaims ~200px of permanent panel height.
Consequences, stated honestly: adding a modifier becomes two clicks instead of one. Correct trade — modifiers are added occasionally, and the reclaimed height keeps Properties above the fold, which is read constantly. (Note: until BUG-218 lands in lane A, the dropdown dispatches into a silent no-op exactly as the chips do today — this design neither fixes nor worsens that; the wave lands both.)

**D7 — Style tokens come from the card family, not new constants.** Backgrounds, hairlines, label/value typography, row heights, and hover states reuse `color.rs` constants and the param_card/slider idioms already on screen in the inspector. The panel introduces **zero new color or metric constants** except the swatch size. A negative gate enforces this (§4).

## 3. Seam brief — the one API change

`PanelAction` (manifold-ui) gains one variant:

```rust
/// Scene Setup outliner selection moved (D1 of SCENE_PANEL_UX_DESIGN.md).
/// The panel has already updated its UI-local selection; this action's only
/// job is to ride the dispatch loop back as `structural_change: true` so
/// `sync_inspector_data` rebuilds the panel this same frame.
SceneSetupSelectionChanged(LayerId),
```

Handled in `dispatch_inspector` (ui_bridge/mod.rs) next to the other `SceneSetup*` arms: `PanelAction::SceneSetupSelectionChanged(_) => DispatchResult { structural_change: true, ..DispatchResult::handled() }` (match the file's actual constructor idiom at implementation time). No old symbol is removed; this is additive, so no deletion gate — the seam's completeness check is the exhaustive-match compile error the new variant produces at every `PanelAction` match site.

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| Outliner click updates Properties the same frame, without any project mutation | L3 flow script `scripts/ui-flows/scene-setup-select-updates.json` (UX-P1 gate): click a second object's row, assert the Properties header text changed, with zero param writes in the flow |
| Value-cell gesture table unchanged | existing `value_cell::tests::intent_for_pins_the_full_contract_table` stays green untouched |
| No new style constants outside the token set | negative gate: `rg -n 'const .*Color|rgb\(|rgba\(' crates/manifold-ui/src/panels/scene_setup_panel.rs` returns no NEW hits vs. the pre-phase count (record both counts in the phase report); swatch-size const exempt by name |
| No new shared state | `rg -n 'Arc<Mutex|Arc<RwLock' crates/manifold-ui/src crates/manifold-app/src/ui_bridge` → zero new hits |
| Trailing-affordance slot uniform across outliner rows | UX-P1 PNG review checks every row's trailing slot is an eye (live or dimmed), nothing else |

## 5. Phasing

Both phases live in `manifold-ui` + the `dispatch_inspector`/state_sync seams in `manifold-app`. Test scope per phase: `cargo nextest run -p manifold-ui -p manifold-app` focused; clippy `-p manifold-ui -p manifold-app`; no GPU runs (no shader/kernel touched — if a phase somehow touches one, that's a scope violation, stop). Full workspace sweep at landing, in the main checkout, per standard §5.

### UX-P1 — selection responds + outliner unification (one session)

- **Entry state:** `624d34a4` or later on the wave branch. Verify anchors: scene_setup_panel.rs:2432 (click handler returns empty actions), app_render.rs:3326 (event-gated sync), ui_bridge/mod.rs:32 (`structural_change`). If any moved, re-locate before coding; if the mechanism changed, escalate.
- **Read-back:** this doc §2 D1/D5/D6 + §3; DESIGN_DOC_STANDARD §5-§6; scene_setup_panel.rs's own module docs (top 300 lines). Restate the decisions and forbidden moves before coding.
- **Deliverables:** `PanelAction::SceneSetupSelectionChanged` + dispatch arm (§3); outliner row template per D5 (icons, section labels, uniform trailing slot); compact action row per D6 (buttons only — the modifier dropdown is UX-P2); flow script `scripts/ui-flows/scene-setup-select-updates.json`; the invariant checks in §4 rows 1 and 5.
- **Gate:** *Positive:* the new flow script passes via `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-select-updates.json` (drives the real `cc0__*.glb` fixture scene); focused nextest green; PNG artifact of the rebuilt outliner. *Negative:* §4 style-constant and shared-state gates.
- **Acceptance demo:** L3 — the flow script above, plus the PNG read by the orchestrator (affordance check: every row's trailing slot, section labels legible, selected-row highlight).
- **Performer gesture:** mid-set, tap through three objects in a row and watch Properties follow each tap instantly — the flow script drives exactly this (three sequential row clicks, three header assertions).
- **Forbidden moves:** adding a per-frame rebuild path "to be safe" (the action IS the mechanism); a second selection store outside the panel; changing `SceneSelection` semantics or serializing it; touching value_cell.rs; nesting outliner rows (D5's rejected alternative).

### UX-P2 — properties rows on the card family (one session)

- **Entry state:** UX-P1 merged on the wave branch; its flow script green. Verify anchors: param_card.rs scalar-row builder (the precedent to copy), slider.rs `BitmapSlider`, dropdown.rs public shape, audio_setup_panel.rs:687 swatch draw.
- **Read-back:** this doc §2 D2/D3/D4/D6/D7; param_card.rs module docs; UI_WIDGET_UNIFICATION_DESIGN.md D2 (slider contract).
- **Deliverables:** Metallic/Roughness as `BitmapSlider` rows (D2); transform value-cell affordance chrome (D3: hover state, resize cursor, scrub hairline, token-matched value text); color swatch + live update during scrub (D4); `+ Add Modifier` dropdown replacing the chip grid (D6); §4 row-3 negative gate counts recorded.
- **Gate:** *Positive:* focused nextest green; flow script `scripts/ui-flows/scene-setup-modifier-stack.json` still passes with the dropdown in place of chips (edit the flow's selectors to the dropdown path — the flow proves dispatch parity, not the BUG-218 outcome); PNG pair of the Properties body before/after a scripted scrub showing the hairline + swatch change. *Negative:* chip-grid builder deleted — `rg -n 'modifier_chip' crates/manifold-ui/src/panels/scene_setup_panel.rs` → zero hits (re-derive the actual symbol name at read-back; the gate is "old chip-grid path gone", not this literal string).
- **Acceptance demo:** L3 — the two flow scripts; PNG reviewed for affordance legibility: sliders read as sliders, cells read as scrubbable (hover state captured in one frame of the flow), swatch shows the row's color.
- **Performer gesture:** grab Roughness and sweep it full-range in one drag (slider), then Shift-scrub a Position cell for a fine nudge (cell) — the flow script performs both and asserts both values moved.
- **Forbidden moves:** inventing a color picker (D4's rejected alternative); new style constants (D7); replacing value cells with sliders on unbounded params; keeping the chip grid alive next to the dropdown (parallel old path); touching scene_vm.rs or any command.

## 6. Decided — do not reopen

1. Selection resync rides `DispatchResult.structural_change` — no new flag, no per-frame rebuild (D1).
2. Bounded scalars = `BitmapSlider`; unbounded = value cells with affordance chrome (D2/D3).
3. Color swatch is display-only in v1 (D4).
4. Outliner stays flat — no nesting (D5; inherited from REALTIME_3D decided §1).
5. Modifier add is a dropdown, two clicks, and that's the accepted trade (D6).
6. Zero new style constants (D7).

## 7. Deferred

- **Color-picker popover** — revive when a second consumer needs color input (e.g. light color rows in REALTIME_3D P6+ or the material system's albedo), at which point it's a shared widget, designed once.
- **Outliner rename-in-place** (double-click a row to rename) — revive on Peter's ask; the Properties name field covers rename today.
- **Panel-width/responsive layout pass** — revive with the app-shell design; the dock's width is that design's property.
