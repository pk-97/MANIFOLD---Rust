# P-F2a census — `ui_root.rs` (4,080 lines) method partition

**Wave:** god-file Wave 1, WS2 · **Design:** `docs/UI_FUNNEL_DECOMPOSITION_DESIGN.md` P-F2 · **Scope fence (D-16):** `ui_root.rs` only; `app.rs` / `app_render.rs` / `ui_bridge/` untouched.

Split target: `ui_root.rs` → `ui_root/{mod,overlay,drag,dropdowns,events}.rs`. `mod.rs` keeps the `UIRoot` struct, ctor, and panel wiring; submodules take `impl UIRoot` blocks (pure moves). The `impl UIRoot` block holds **83 methods**; the file also has 6 top-level free fns and 4 `#[cfg(test)]` modules (the "107 fn" figure counts test/helper fns and the `OverlayId`/`ScrollDirty` impls too).

## Partition

**`mod.rs` (stays) — panel build/wiring + input forwarding + resize handles + per-frame update (~53 methods):**
`new`, `set_display_resolutions`, `panel_cache_info`, `apply_project_layout`, `build`, `build_inspector_in_rect`, `route_inspector_events`, `try_inspector_scroll`, `rebuild_scroll_panels`, `build_scroll_panels`, `build_viewport_panels`, `resize`, `pointer_event`, `right_click`, `key_event`, `set_clip_detect_layers`, `is_near_inspector_edge`, `begin/update/end_inspector_resize`, `is_near_audio_setup_edge`, `begin/update/end_audio_setup_resize`, `toggle_audio_dock`, `set_audio_setup_handle_hover/drag/idle`, `is_near_scene_setup_edge`, `begin/update/end_scene_setup_resize`, `toggle_scene_dock`, `set_scene_setup_handle_hover/drag/idle`, `set_handle_color`, `set_split_handle_hover/drag/idle`, `set_inspector_handle_hover/drag/idle`, `tick_inspector`, `update`, `update_audio_meters`, `update_fire_meters`, `open_fire_mode_drawer_send/band`, `update_audio_scope_readout`, `update_audio_scope_bands`, `audio_band_dragging`, `update_audio_band_meters`. Plus shared free fns `trace_worthy`/`trace_kind` (used by both `overlay` and `events` → kept in the ancestor so no cross-wall bump).

**`overlay.rs` — overlay driver (14 methods + `overlay_redraw_needed_tests`):**
`overlay_mut`, `overlay_is_open`, `current_overlay_open_mask`, `detect_overlay_open_change`, `overlay_redraw_needed`, `build_overlays`, `build_overlays_for_screen`, `route_overlay_event`, `overlay_contains_point`, `note_overlay_closed_if`, `take_closed_overlays`, `drain_overlay_selections`, `escape_overlays`, `intercept_overlay_actions`.

**`drag.rs` — drag-capture ownership (6 methods + `drag_capture_tests`):**
`resolve_drag_owner`, `fire_gesture_end_hooks`, `broadcast_gesture_end`, `any_overlay_wants_immediate_drag`, `should_stash_for_tracks`, `is_event_in_tracks_area`.

**`dropdowns.rs` — dropdown/picker builders (6 methods + 5 free fns):**
methods `open_dropdown_typed`, `sync_embedded_presets`, `build_preset_picker_items`, `try_open_dropdown`, `try_open_dropdown_inner`, `dropdown_color_to_action`; free fns `build_picker_session`, `send_channels_action`, `push_channel_pair_rows`, `build_tap_channel_dropdown`, `build_channel_dropdown`.

**`events.rs` — event pump (4 methods):**
`repopulate_intents`, `resolve_intent`, `process_events`, `drain_viewport_events`.

## Cross-wall visibility (fn → pub(crate) fn, verifier "visibility pairs")

Only `mod.rs`- or sibling-called private methods widen; a child calling an ancestor's private fn does not (Rust privacy: an item is visible to its module and all descendants).

- overlay: `overlay_mut`, `build_overlays`, `route_overlay_event`, `drain_overlay_selections`.
- drag: `resolve_drag_owner`, `fire_gesture_end_hooks`, `broadcast_gesture_end`, `any_overlay_wants_immediate_drag`, `should_stash_for_tracks`.
- dropdowns: `try_open_dropdown`, `dropdown_color_to_action`.
- events: `repopulate_intents`, `resolve_intent`.

Commit order overlay → drag → dropdowns → events keeps every intermediate compiling: a submodule that calls a not-yet-moved method reaches it as an ancestor (`mod.rs`) private; the bump lands in the same commit that turns the call into a sibling call.

## No method resisted a clean move

Every method fell into exactly one bucket. `should_stash_for_tracks`/`is_event_in_tracks_area` are event-pump-adjacent but read `drag_owner` and are covered by `drag_capture_tests`, so they ride with `drag.rs` (its test module's subjects stay together). `trace_worthy`/`trace_kind` are the one genuinely-shared pair — kept in `mod.rs` as ancestor-visible helpers rather than forced into one submodule.
