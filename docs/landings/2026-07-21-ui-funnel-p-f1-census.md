# P-F1 census — `app_render.rs` (6,548 lines) partition

**Wave:** god-file Wave 1, WS2 · **Design:** `docs/UI_FUNNEL_DECOMPOSITION_DESIGN.md` D7 · **Claim:** D-25. **Scope fence:** `app_render.rs` only; `ui_bridge/`, `app.rs`, `ui_root/` untouched; WGSL-out-of-app.rs is P-F2, not here.

Base re-derived on `lane/ws2-frame` (post P-B). The file is: top-level free fns (mapping/graph helpers + present/debug helpers), two drag structs, one giant `impl Application` block (`tick_and_render` + graph-editor-bridge methods + present methods), and four `#[cfg(test)]` modules.

## The load-bearing finding: 4 of the 6 D7 targets are NOT pure-movable

D7 lists six modules: `frame/{drain,events,sync,push,present}.rs` + `editor_bridge.rs`. But **`drain`, `events`, `sync`, `push` are inline segments of the single `tick_and_render` method (839–4086), not separate methods**:

- `// 1. Drain state` 858–1221 · `// 2. Process UI events and dispatch` 1222–3717 · `// 3. Rebuild if needed` 3718–3751 · `// 4. Push engine state` 3752–3785 · `// 5/6` perf+lightweight 3786–4034 · present 4035–4053.
- The segments **thread locals across their boundaries**: `needs_structural_sync` (declared 1585, written throughout §2, read in §3), plus `seg`/`frame_t0` profiler cursors. Extracting a segment into `frame/drain.rs` etc. means creating a new function boundary and passing those locals in/out — the moved lines change (`self`-body → param references), so `move_identity_check` cannot see them as moves. **Such a slice fails INV-G1 (residue 0) by construction.** It is *semantic function extraction*, forbidden as a pure move by D-7 and by the brief's "new glue shape → STOP and park" rule; INV-G4 also fences any tick-sequence change.

**→ `frame/{drain,events,sync,push}.rs` are PARKED** (see `parked.md`, `P-F1-frame-body`): they require a daytime semantic extraction phase (thread `needs_structural_sync`/`seg` explicitly, or make the stages `&mut self` methods and prove behavior-identity by the flow suite), not an overnight pure move. `tick_and_render` therefore stays whole this phase; the "orchestrator under one page" end-state is gated on that semantic phase.

## Pure-movable this phase (whole methods / fns / structs / tests)

**`editor_bridge.rs`** — the graph-editor bridge cluster (`:26–838` + `:4087–4852`):
- Methods: `mapping_target`, `scope_hover_uv`, `watched_reshape`, `watched_full_reshape`, `watched_binding_for_node_param`, `watched_current_node_param_value`, `watched_node_param_is_wired`, `seed_def_for`, `watched_def_cloned`, `copy_selected_graph_nodes`, `confirm_remove_node_orphans`, `watched_value`, `preview_mapping`, `commit_mapping`, `commit_mapping_with_reverse`, `resolve_effect_card_id`, `watch_effect_graph`, `watch_generator_graph`, `begin_save_preset_prompt`, `begin_rename_preset_prompt`, `editor_canvas_viewport`, `editor_ui_snapshot`, `present_graph_editor_window`.
- Structs: ~~`BoundNodeParamDrag`, `UnboundNodeParamDrag` (+ their impls)~~ **PARKED — stay in app_render.rs** (field-widening not provable residue-0; see `parked.md` P-F1-drag-structs).
- Free fns: `build_mapping_command`, `seed_def_for_project`, `build_mapping_command_with_reverse`, `descend_def_level`, `full_reshape_from_def`, `descend_level_ref`, `binding_for_node_param`, `node_param_is_wired`, `node_param_value`, `serialized_value_as_f32`, `find_snapshot_node`, `resolve_preview_target`, `resolve_boundary_node`, `resolve_producer`, `producer_into`, `primary_texture_port`, `non_empty_node_id`, `resolve_canvas_binding`.
- Tests: `preview_target_tests`, `binding_reroute_tests` (moved); ~~`bound_node_param_drag_tests`, `unbound_node_param_drag_tests`~~ **PARKED with the structs**.
- Re-exports (external/staying callers): `build_mapping_command`, `seed_def_for_project` (`app.rs`), `resolve_canvas_binding` (`window_input.rs`), `serialized_value_as_f32` keep their `crate::app_render::` path via `pub(crate) use`.

**Landed (verified):** editor_bridge `57f17529` (residue 0, 319 tests) · present `bc42e9a2` (residue 0). app_render.rs 6548 → 3774 lines. `tick_and_render` stays the ~3,250-line inline monolith (111–~3360) — the drain/events/sync/push body split is the parked semantic work, so the "orchestrator under one page" end-state is NOT reached this phase.

**P-F1 CLOSED (D-28).** Both pure-move clusters my census identified are landed — nothing pure-movable remains. Dispositions: drain/events/sync/push → daytime semantic phase **P-F1b** (FrameCtx-shaped, its own brief); `Bound`/`UnboundNodeParamDrag` → **stay** as P-I deletion targets (die into `ScrubState`, design D4); `mini_timeline_data`/`render_text_input_overlay` → stay (shared helpers).

**`frame/present.rs`** — `present_all_windows` (4853–5530), `represent_cached_offscreen` (5531–5587) + present-path private helpers `bug060_dump_every`, `bug060_dump_png`, `format_scope_readout`, `fmt_table_cell_seed`.

## Parked, not forced (brief: "anything not fitting cleanly → park")

- `frame/{drain,events,sync,push}.rs` — semantic extraction (above).
- `mini_timeline_data`, `render_text_input_overlay` — present-adjacent free fns but with broad cross-module callers (`ui_snapshot/`, `window_input.rs`, `editor_frame.rs`, `tree_passes.rs`, `app.rs`); they are shared UI helpers, not `present`-internal. Moving them would sprinkle re-exports for no boundary gain — left in `app_render.rs`.

## Visibility / method paths

Inherent `impl Application` methods keep their path regardless of module (Application's fields are `pub(crate)`, which is why `app_render.rs` already holds an out-of-`app` impl). Only cross-module *private* method calls widen `fn` → `pub(crate) fn` (verifier visibility pairs); only *free fns* with external callers need re-exports. Commit order: `editor_bridge` then `frame/present` (independent clusters).
