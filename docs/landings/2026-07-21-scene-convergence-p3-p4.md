# 2026-07-21 — Scene Panel Exposure Convergence P3 + P4 landing

Closes the two remaining phases of `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md`: P3 (outliner/discovery slimming) and P4 (supersession sweep of the four prior scene design docs).

## P3 — scene_vm slimming (main, `7ee0a887`)

- **`scene_vm.rs` 2137 → 1769 lines.** The per-family param-TRANSCRIPTION fields (value/addr/`_driven`) on `MaterialColorRow`/`MetallicRoughnessRow`/`LightRow`/`LensRow`/`OrbitCameraRow`/`FreeCameraRow`/`LookAtCameraRow`/`ImporterEnvironmentRow`/`BareEnvironmentRow`/`AtmosphereRow` and `ModifierVm.params`/`driven` are deleted — P2 already made the panel read the exposed manifest, so these were dead weight. Every struct keeps its `node_doc_id` identity (command routing needs it).
- **`TransformVm` kept WHOLE.** It is a load-bearing dual consumer: `viewport_gizmo.rs` (`gizmo_target_for`/`pick_object`/`drag_write`, dispatched from `window_input.rs:978-993`) reads `pos_value` + the `ParamAddr` + the `_driven` axis-lock flags for 3D gizmo drag-to-move/rotate/scale and object pick. Not superseded by the manifest; deleting its fields would have broken viewport dragging.
- **New `scene_vm::is_param_driven(def, node_doc_id, param_id)`** — the exact structural sibling of `is_param_exposed`: recurses nodes + group bodies to the node's level, returns whether a wire feeds `(node_doc_id, param_id)` there. It is now the SOLE driven-state source for the panel's manifest rows (47 call sites in `state_sync.rs`), replacing the deleted per-struct `_driven` fields — including the eye toggle's `visible_driven`. Reuses the existing wire model; no new shared state.
- **`state_sync.rs` (~213 lines changed):** the material/object/light/lens/camera/environment/atmosphere transcription blocks now feed each row value via the existing `display_value`/`row`/`scoped_row` manifest closures and driven-state via `is_param_driven`, taking only identity (`node_doc_id` + param name + scope) from the VM. The `transform_row` block is unchanged (TransformVm stays). Round-trip test re-pointed to read the manifest; `viewport_gizmo` test fixtures updated to the slimmed shapes.
- **Gate at landing:** workspace `clippy -D warnings` clean, `cargo deny check bans` ok, `cargo nextest run --workspace` **3846/3846**. Independently reverified before merge: gizmo demo tests (`viewport_p5c_demo`/`viewport_p6_demo`, incl. `move_gizmo_drag_moves_the_rendered_object` + `pick_object_highlights_the_clicked_object`), `scene_setup_round_trip` (2/2), and the `scene-setup-eye-toggle` + `scene-setup-light-cast-shadows-toggle` flows all green.

## P4 — supersession sweep

Status-header banners added to the four prior scene design docs pointing at `SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` as the current authority for the param mechanism: `SCENE_SETUP_PANEL_DESIGN.md`, `SCENE_OBJECT_AND_PANEL_V2_DESIGN.md`, `SCENE_PANEL_UX_DESIGN.md`, `SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md`. Each banner names what still stands (outliner/selection/structural verbs; the `node.scene_object` vocabulary; the card-row unification) vs what the convergence removed underneath it (synthetic scene-ids, resolution funnels, `scene_bound_slot`'s dual write path, hand metadata tables).

## VD-035 — re-diagnosed, still open, no code bug

The `scene-setup-modifier-stack` flow (the 1/15 non-green) was thought to be a below-fold layout bug. Instrumenting `ScenePanel::handle_scroll` disproves that: the panel measures its content correctly (`content_height ≈ 3384`, `max_scroll = 2286`, overflows). The real cause is the `--script` scroll delta **sign** — `apply_scroll_delta` does `offset -= delta`, so the flow's positive `y` scroll clamped to 0; a negative delta reaches the row (proved `offset 0→900→1800→2286`). The live app is unaffected (real mouse wheel feeds the correct sign). With the scroll fixed, the flow then trips on **stale pre-convergence assertion names** (`scene_setup.modifier.param1_*`, gone since P2 — the rows are converged card rows now, params present, no regression). Closing it is the design §6 P4 "rewrite the flow suite against the real rows" task — a flow-suite re-authoring, deferred beyond this docs-only sweep. VD-035 stays OPEN, fully diagnosed; no new BUG-NNN.

## Open behind this close

- **VD-035** — flow-suite re-authoring for `scene-setup-modifier-stack` (negative-delta scroll + re-target modifier drag/driver/reorder asserts at converged card rows). Recipe recorded in the VD-035 entry.
- The design's §7 named-not-fixed items (UI density §5.4, emoji glyphs, BUG-239 harness live-modulation gap, BUG-240 flow rot) are unchanged by this landing.
