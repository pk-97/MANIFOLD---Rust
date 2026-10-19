# Coupling census, app and UI side (evidence for the BUG-brw4f (app and UI decomposition) design pass)

Base commit: 7eae46856 (branch lane/overnight-coupling-app-ui, on top of 25af1c932). Epic: BUG-brw4f (app and UI decomposition). The epic names no candidate subsystems and has no child beads, so the groups below are the directory and module-name groupings in the code. No recommendations here; the design pass makes those. Rendering side: coupling_rendering.md in this directory (BUG-bfktg (rendering-side decomposition)).

## Method (all read-only, from the worktree)

- Sizes: `fd -e rs . crates/manifold-<c>/src` then `wc -l`, tests files (`tests.rs`, `/tests/`, `*_test(s).rs`) dropped; kept files over 1,500 lines.
- Production lines: `/tmp/.../prod.awk` (scratchpad, not committed) drops every col-0 `#[cfg(test)]` item and, for `mod x {`, everything to the next col-0 `}`. Gaps: `cfg(all(test, ..))` is caught only at col 0; feature-gated harness files are not subtracted (marked H). All edge counts below run on this stream (comment lines skipped).
- Items: `rg -n '^(pub )?(fn|struct|enum|impl|trait|const|mod|type) |^    (pub )?fn '`, then item length = next item line minus this line (items of 45-100+ lines listed). Hierarchy inside `impl` blocks is not resolved.
- app intra-crate edges: `rg -o 'crate::[a-z_0-9]+'` per source file, source and target mapped to the groups in section 2 by an awk table. ui edges: same with `crate::panels::<sub>`. playback edges: `crate::<mod>` per source module. No grouped `use crate::{` exists in app; ui has 18 grouped lines and 5 `use crate::panels::{` blocks (counted as `panels::` with empty sub, undercounted). `super::` is not in the matrices (counts in section 4).
- Cross-crate edges: `rg -o 'manifold_<crate>::[A-Za-z_0-9]+'` per consumer; grouped `use manifold_x::{..}` expanded with `rg -U -o 'use manifold_x::\{[^;]*\};'` then identifier split (nested braces approximate). Crate lines from Cargo.toml `^manifold-`.

## Not covered

- No type resolution (no cargo, no rust-analyzer): symbols are path or grouped-import names, so a symbol reached only through `use x::*` or a re-export (`pub use`) is missed. Counts are lower bounds.
- Method-call coupling (field access on `Application`, `ContentThread`, `UIRoot`, `PlaybackEngine` from sibling `impl` blocks) is not countable by path; only the `impl` spread is listed.
- Item hierarchy inside giant `impl` blocks, runtime behaviour, shaders, docs, bin targets (`manifold-app/src/bin`, 3,587 lines) and files at or under 1,500 lines (except where a table needs them) are not read. Four test-only app harness files (`scene_modifier_journey`, `corridor_acceptance`, `rt_dynamic_*`, `scene_modifier_performance`) and `ui_bridge/inspector.rs` (production 12 lines, the rest is a test module) are not production.

## 1. Production files over 1,500 lines (eight crates)

Total, Prod = total minus test blocks. H = feature-gated harness (`journey-proofs`, `perf-soak`, `ui-snapshot`), not in the default build.

| File | Total | Prod | Notes |
|---|---|---|---|
| ui/interaction_overlay.rs | 6876 | 3818 | clip drag + automation gestures + click/select (sec. 3) |
| app/content_pipeline.rs | 4797 | 4353 | `render_content_native` 1436 lines; `struct ContentPipeline` 387 |
| playback/engine.rs | 4781 | 3840 | `PlaybackEngine` 203-line struct; `sync_clips_to_time` 233 |
| playback/modulation.rs | 4667 | 1406 | tests 3260 |
| ui/panels/param_card/mod.rs | 4383 | 1328 | `ParamCardPanel` struct 291 |
| audio/analysis.rs | 3925 | 2209 | onset/ridge/send analysis (sec. 5) |
| app/ui_bridge/inspector.rs | 3803 | 12 | test-only |
| ui/panels/layer_header.rs | 3581 | 2739 | `build_layer_row` 567, `compute_layer_row` 253 |
| ui/panels/inspector/mod.rs | 3561 | 1572 | `InspectorCompositePanel` struct 213 |
| playback/ableton_bridge.rs | 3534 | 2887 | |
| app/ui_bridge/project.rs | 3418 | 1475 | `dispatch_project` 1134 lines |
| app/ui_snapshot/script.rs | 3396 | 3321 H | flow runner; `step` 345 |
| editing/commands/graph/scene.rs | 3288 | 3284 | 12 `impl Command`; plus `scene/` dir 10,641 prod lines |
| app/app_render.rs | 3225 | 2933 | `tick_and_render` 2412 lines |
| app/editor_bridge.rs | 2882 | 2195 | `present_graph_editor` 704 |
| ui/panels/viewport.rs | 2838 | 1772 | `TimelineViewportPanel` struct 202 |
| ui/panels/audio_setup_panel.rs | 2811 | 2142 | `build_nodes` 602 |
| app/app.rs | 2687 | 2687 | `Application` struct 490; `window_event` 366; `fs_main` shader 440 |
| app/scene_modifier_journey.rs | 2570 | 2570 test-only | |
| app/input_host.rs | 2561 | 1664 | |
| ui/panels/param_card/render.rs | 2540 | 2378 | |
| app/ui_root/dropdowns.rs | 2496 | 2139 | `try_open_dropdown_inner` 1442 |
| ui/panels/param_slider_shared/builders.rs | 2493 | 2493 | |
| ui-paint/ui_renderer.rs | 2491 | 2491 | `draw_node` 359, `prepare_with_offset` 458 |
| app/content_thread.rs | 2476 | 2229 | `tick_frame_at` 638, `tick_sync_controllers` 254 |
| app/ui_bridge/projection/inspector.rs | 2461 | 1847 | `sync_inspector_data` 1171 |
| ui/panels/scene_setup_panel.rs | 2453 | 2442 | `handle_event` 427, `ScenePanel` struct 195 |
| editing/commands/effects.rs | 2405 | 1573 | |
| core/effects/instance.rs | 2345 | 1657 | `PresetInstance` + 4 impl blocks |
| app/ui_bridge/scrub.rs | 2328 | 2328 | `dispatch_scrub` 1596 |
| app/content_commands.rs | 2227 | 2093 | `handle_command` 1319, `run_warmup` 483 |
| io/collect.rs | 2198 | 950 | asset collect + re-point |
| editing/commands/graph/expose.rs | 2146 | 772 | |
| ui-paint/native_text.rs | 2089 | 1892 | `NativeTextRenderer::prepare` 339 |

Remaining files over 1,500 lines (total/prod): editing/commands/effect_groups.rs 2041/1309; ui/panels/scene_setup_panel/material_inspector.rs 2032/1279; core/scene_modifier_preset.rs 2022/1097; app/project_io.rs 2017/1440; app/perf_soak.rs 2008/1755 H; app/ui_snapshot/mod.rs 1993/1092 H; app/window_input.rs 1955/1886; app/ui_root/mod.rs 1944/1736; ui/tree.rs 1939/1304; editing/commands/settings.rs 1902/1467; playback/percussion_orchestrator.rs 1885/1366; editing/service.rs 1856/1643; ui/panels/browser_popup.rs 1851/1556; playback/automation.rs 1828/920; app/content_export.rs 1828/1450; ui/graph_canvas/model.rs 1810/1810; app/ui_bridge/projection/cards.rs 1800/1086; ui/graph_canvas/mapping_popover.rs 1691/1235; core/effect_graph_def.rs 1690/1023; ui/panels/param_slider_shared/row_host.rs 1688/1688; core/layer.rs 1648/1166; app/app_lifecycle.rs 1643/1551; ui/graph_canvas/render.rs 1601/1601; app/ui_bridge/dispatch/params.rs 1557/1131; editing/commands/graph/scene/physics.rs 1557/1557; editing/commands/automation.rs 1548/746; editing/commands/graph/skin.rs 1530/703; app/journey_proof.rs 1527/1527 H; app/frame_plan.rs 1526/865; core/scene_exposure.rs 1508/548; 

Counts of files over 1,500 lines total (all crates, test-only files included): app 23, ui 16, editing 9, core 5, playback 4, audio 3, ui-paint 2, io 1 (same as the epic baseline). Non-test files over 3,000 prod lines: content_pipeline 4353, playback/engine 3840, interaction_overlay 3818, ui_snapshot/script 3321 (H), editing graph/scene 3284, app_render 2933. Crate production sizes: app 137,943 total (105,892 prod in this stream, 24,055 of it H), ui 100,952 (panels 58,259; graph_canvas 11,823), core 47,447, editing 53,584 (commands 50,147; commands/graph 32,100), playback 40,359, audio 12,745, io 11,767, ui-paint 7,739.

## 2. app: groups and edges

Groups (module names), prod lines: UIBRIDGE `ui_bridge/` 16,992 (dispatch/ 3,636 total, projection/ 8,750 total); CONTENT `content_thread, content_pipeline/, content_commands, content_command, content_export, content_state, content_reactions, frame/, offline_audio_mod, audio_mod_runtime, rt_capture, import_worker, frame_timer, present_feedback` 16,669; UIROOT `ui_root/, ui_frame, ui_frame_profile, ui_translate, tree_passes, frame_plan, live_ui` 9,145; APP `app, app_lifecycle, app_render, main, window_registry, workspace` 8,535; INPUT `input_host/, window_input, input_handler, viewport_input, text_input, text_session, drag_hover, drag_interpose, menu` 8,140; SCENEEDIT `scene_modifier_edit/, scene_modifier_transfer, scene_item_transfer, scene_camera_edit, scene_view_copy, object_modifier_transfer, fluid_domain_edit, generator_change, modifier_preset, scene_viewport, physics_overlay, edit_selection, editing_host` 5,500; EDITOR `editor_bridge, editor_frame, editor_graph_cache, editor_input, graph_dump, graph_snapshot, graph_target, gesture_preview` 4,862; PROJECT `project_io, autosave, user_library, user_prefs, file_dialogs, session_log` 2,679; PERFORM `perform_mode/` 1,868; HARNESS (ui_snapshot 9,919, journey_proof, perf_soak, probes, demos) 24,055; OTHER (alerts, shared_texture, clip_atlas, clip_filmstrip, blender_import, breadcrumb, edr_surface, display_link, bridge-adjacent) 7,447.

Cross-group `crate::` references (source > target, refs; production, HARNESS rows only where large):

| Edge | Refs | Modules crossed (count) |
|---|---|---|
| UIBRIDGE > CONTENT | 99 | content_command 76, content_state 20, content_thread 3 |
| UIBRIDGE > UIROOT | 64 | ui_translate 41, ui_root 23 |
| UIROOT > UIBRIDGE | 25 | ui_bridge 25 (two-way) |
| UIBRIDGE > SCENEEDIT | 51 | scene_modifier_edit 17, editing_host 14, scene_item_transfer 7, edit_selection 7, modifier_preset 3 |
| UIBRIDGE > PROJECT | 21 | file_dialogs 13, user_library 4, user_prefs 3 |
| UIBRIDGE > APP | 17 | app 11, app_render 6 |
| INPUT > CONTENT | 46 | content_command 44 |
| APP > CONTENT | 45 | import_worker 12, content_command 11, content_pipeline 6, content_state 6 |
| APP > INPUT | 47 | text_input 33, menu 6 |
| APP > PROJECT | 41 | file_dialogs 21, project_io 14 |
| APP > UIBRIDGE | 36 | ui_bridge 36 |
| CONTENT > SCENEEDIT | 34 | object_modifier_transfer 7, scene_viewport 6, scene_modifier_edit 7, edit_selection 4 |
| CONTENT > EDITOR | 15 | graph_snapshot 5, graph_dump 4 |
| SCENEEDIT > CONTENT | 22 | content_command 20 |
| EDITOR > INPUT | 27 | text_input 13, viewport_input 8 |
| HARNESS > CONTENT / UIROOT / EDITOR / UIBRIDGE | 117 / 51 / 48 / 44 | harness reaches every group |

`ContentCommand::` references by source file: ui_bridge 261, content_commands 126, input_host 106, scene_modifier_journey 78 (test), app_render 39, ui_snapshot 27, app_lifecycle 24, editing_host 18, app 17, content_thread 16, editor_bridge 13, window_input 11. `enum ContentCommand`: about 95 variants, content_command.rs 432 lines. `ContentState` struct at content_state.rs:118, file 1,412 lines; read by UIBRIDGE 20, EDITOR 7, SCENEEDIT 2.

`impl` spread (state shared across files): `impl Application` in 12 files (app.rs 3, window_input, app_render, app_lifecycle, editor_bridge, breadcrumb, autosave, live_ui, frame/present, perform_mode/render, lifecycle, input); `impl ContentThread` in 4 files (content_thread, content_commands, content_reactions, content_export).

app external-crate reference lines per group (non-comment `manifold_<crate>::` occurrences; HARNESS is the large harness set):

| Group | core | ui | editing | nodes_scene | node_engine | gpu | playback | io | ui_paint |
|---|---|---|---|---|---|---|---|---|---|
| UIBRIDGE | 547 | 254 | 176 | 84 | 11 | 1 | 9 | 6 | 0 |
| CONTENT | 281 | 12 | 22 | 3 | 71 | 148 | 60 | 1 | 5 |
| UIROOT | 120 | 212 | 4 | 1 | 63 | 15 | 3 | 0 | 18 |
| APP | 107 | 182 | 36 | 12 | 6 | 59 | 21 | 5 | 12 |
| INPUT | 70 | 105 | 51 | 16 | 1 | 0 | 3 | 0 | 0 |
| EDITOR | 143 | 85 | 9 | 5 | 26 | 12 | 0 | 0 | 4 |
| SCENEEDIT | 73 | 18 | 28 | 27 | 18 | 5 | 0 | 0 | 3 |
| PROJECT | 57 | 1 | 4 | 6 | 6 | 0 | 3 | 14 | 0 |
| HARNESS | 376 | 149 | 32 | 16 | 58 | 69 | 25 | 50 | 17 |

ui_bridge reaches `manifold_nodes_scene::node_graph` in `project.rs` 19, `projection/inspector.rs` 24, `projection/cards.rs` 10; `manifold_node_engine::load` in `projection/cards.rs` 4 and `ui_snapshot/mod.rs` 10.

## 3. Item outlines by domain (largest files)

| File (prod) | Domains and item sizes |
|---|---|
| interaction_overlay.rs (3818) | Drag state types 149-800 (`DragMode`, 11 automation drag state structs, `TrimDrag`, `RegionDrag`, `MoveDrag`); automation draw helpers 384-700 (`apply_draw_point`, `shape_bend`, `insert_preview_point`); `InteractionOverlay` 824-3800: pointer events 1018-1115 and 1414-1600 (`on_pointer_click` 188); animation/feedback `tick, lift_amount, ghost_alpha, settle_dx_px, error_shake_offset_px` 1115-1230; automation gestures 1602-2750 (16 fns: click, begin/handle/commit for dot, segment, bend, group, marquee, draw; 919 of the file's lines mention "utomation"); clip drag 2754-3800 (`on_begin_drag` 122, `on_end_drag` 198, `handle_move_drag` 137, trim, region, `begin_move_drag` 114) |
| content_pipeline.rs (4353) | Visibility and skip helpers 50-207; node-atlas and clip-atlas constants and fill/restore 208-745 (`fill_clip_atlas` 148, `restore_clip_atlas` 103); `ContentPipeline` struct 746-1133; embedded WGSL strings 1435-1660; `impl` 1144-4400: profiling setters 1301-1360, surface and present tracking 1739-1950, preview/atlas/node-preview setters 1942-2060, `apply_trigger_pulses/apply_sim_step/apply_rt_quality` 2084-2220, `render_content_native` 2339-3775, resize 3775-3890, workspace preview 3887-3957, prewarm 4058-4152, export textures and readback 4203-4399 |
| content_thread.rs (2229) | `ContentThread` struct 161 lines; `drain_commands` 76; `tick_frame_at` 638; `tick_sync_controllers` 254; `IssuedPresents`, `DetectionWrite` |
| app_render.rs (2933) | `intake_content_state` 143; `tick_and_render` 2412; `render_text_input_overlay` 89; `BoundNodeParamDrag`, `MiniTimelineCache` |
| app.rs (2687) | `Application` struct 490 (fields); `new` 211; `handle_text_input_commit` 298; `convert_key` 83; `window_event` 366; `about_to_wait` 140; inline `fs_main` shader 440 |
| ui_bridge/project.rs (1475) | `dispatch_project` 1134; `dispatch_material_batch` 82; tests 1941 |
| ui_bridge/projection/inspector.rs (1847) | `sync_inspector_data` 1171; `build_card_modulation` 85; `sync_audio_dock` 123 |
| editor_bridge.rs (2195) | key types `NodeInspectorKey, CardMapping, MappingHost`; `resolve_canvas_binding` 90; `dispatch` 123; `present_graph_editor` 704 |
| project_io.rs (1440) | `ProjectIOAction` 146; `migrate_legacy_math_views` 218; `ProjectIOService` 631+: `open_project_from_path` 128, `save_project_as_to_path` 95, `process_dir` 253 |
| panels/param_card/mod.rs (1328) | `ParamCardPanel` struct 291; `new` 95; `effect_config` 96; `gen_config` 134 (type configs in one file) |
| panels/layer_header.rs (2739) | `LayerControl` enum 46; row data/ids 303-960; `compute_layer_row` 253; `build_layer_row` 567; `build` 110 |
| panels/inspector/mod.rs (1572) | `InspectorCompositePanel` struct 213; impl 414-1560 |
| panels/viewport.rs (1772) | `TimelineViewportPanel` struct 202; `rebuild_automation_lane_screens` 124; `build` 162 |
| panels/audio_setup_panel.rs (2142) | `AudioSetupPanel` struct 103; `build_nodes` 602; `handle_click_inner` 95; `handle_event` 168 |
| panels/scene_setup_panel.rs (2442) | row/lattice/scene/card state types 187-950; `ScenePanel` struct 195; `build_nodes` 100; `build_item_properties` 133; `handle_event` 427 |
| ui-paint/ui_renderer.rs (2491) | vertex/command/batch structs 21-420; WGSL UI + image shaders 35-400; `UIRenderer` struct 114; `new` 147; `draw_node` 359; `prepare_with_offset` 458; `render_in_pass` 95; `impl manifold_ui::draw::Painter` 2261 |
| ui-paint/native_text.rs (1892) | atlas constants and glyph types 44-140; `FontManager` 157-247; `GlyphAtlas` 247-470 (`rasterize_glyph` 113); icon generation 464-920 (`generate_cog_icon`, `generate_single_waveform`); `NativeTextRenderer` 994+ (`prepare` 339, `render_depth_in_pass` 54); measure cache 1727-1830 |
| playback/engine.rs (3840) | `PlaybackNotifier` trait; `PlaybackEngine` struct 203; `new` 97; `tick*` 1219-1540; `sync_clips_to_time` 233; MIDI/audio-trigger ticks 2735-2860; `check_preparing_clips`, `correct_video_drift` 122, `compute_prewarm_candidates` 131; impls of `LiveClipHost`, `SyncTarget`, `SyncArbiterTarget` 3689-3830 |
| playback/ableton_bridge.rs (2887) | `OneEuroFilter` 170; `DiscoveryState` 294; `AbletonBridge` struct 326 (74); connect/update/apply 494-900; message handlers 922-1500; mapping validation 1864-2145 (`validate_single_mapping` 152); `rebuild_listeners` 218; transport drain 2494-2630; fast parse 2844 |
| audio/analysis.rs (2209) | worker + ring I/O 41-460 (`GainBank, MonoReader, AudioFeatureWorker, SendState, MonoWorkerLoop, LinearResampler`); onset detection constants 514-600; salience/peak tracking 629-860; `RidgeTracker` 872-1330 (`update` 243); band reduce 1432-1700; `StreamingSendAnalyzer` 1766-2210 (`push_with_hops` 172) |
| editing/commands/graph/scene.rs (3284) | 12 `impl Command`: AddSceneObject (`do_execute` 270), AddSceneLight, RemoveSceneObject (246), RemoveSceneSubmesh, DuplicateSceneSubmesh, RemoveSceneLight, AddSceneEnvironment, AddSceneFog, AddObjectTransform, ImportModelIntoScene, RenameSceneObject, SetNodeHandle; sibling dir `scene/` clone_*, duplicate 562, fluid 601 + `fluid/roles` 979 + `lifecycle` 869, physics 1557, physics_match 760, split 751 |
| core/effects/instance.rs (1657) | `PresetInstance` struct 35-163; impl 163-1437 (about 1,270 lines); `ParamSource` impl 1597 |

## 4. ui crate: groups and edges

Groups (prod lines): CLIPTL `interaction_overlay` 3,818 + clip_hit_tester, automation_hit_tester, coordinate_mapper, snap, drag, cursor_nav, timeline_*host, mini_timeline, automation, waveform_*, bitmap_*; PARAMCARD `panels/param_card` 4,821 + `param_slider_shared` 5,148; INSPECTOR `panels/inspector` 4,106 + macros_panel, drawer; LAYERHDR `layer_header` 2,739 + layer/clip/master chrome, header; SCENESETUP `panels/scene_setup_panel` 6,059 (+ actions, skin); VIEWPORT `panels/viewport` 4,080; AUDIOSETUP `audio_setup_panel` 2,142 + audio_trigger_section, ableton_picker; POPUPS browser_popup, dropdown, picker_core, popup_shell, settings_popup, toast, overlay; GRAPHCANVAS `graph_canvas/` 8,416 + `panels/graph_editor`; ACTIONS `panels/actions.rs` 1,304 (`PanelAction` at panels/mod.rs:198, `RootAction` at actions.rs:890); STATE ui_state 1,255, intent, view, graph_view, graph_edit, param_surface; CHROME `chrome/` 2,003; WIDGETS tree, node, input, slider, text_edit, text, scroll_container, color, layout, types, hit*, transform*, icons, draw, anim, stepper, dock, fmt.

| Edge (source > target) | Refs | Modules crossed |
|---|---|---|
| SCENESETUP > STATE | 76 | param_surface 69, view 6, intent 1 |
| PARAMCARD > WIDGETS | 72 | types 37, slider 18, node 4, tree 3, anim 3 |
| GRAPHCANVAS > WIDGETS | 70 | node 19, anim 14, slider 13, draw 8, hit_targets 4 |
| GRAPHCANVAS > STATE | 46 | graph_view 42, intent 2, view 2 |
| POPUPS > WIDGETS | 47 | color 11, input 10, node 8, tree 7, types 5 |
| CLIPTL > WIDGETS | 42 | node 17, color 8, input 4, hit_targets 3 |
| PARAMCARD > STATE | 34 | param_surface 19, view 9, intent 6 |
| PARAMCARD > INSPECTOR | 31 | panels::drawer 31 |
| PARAMCARD / INSPECTOR / LAYERHDR / AUDIOSETUP > CHROME | 10 / 7 / 9 / 4 | chrome 30 |
| ACTIONS > STATE | 23 | view 19, param_surface 4 |
| VIEWPORT > STATE / CLIPTL | 15 / 15 | view 11, param_surface 4; bitmap_renderer 5, clip_hit_tester 3, waveform_renderer 2, snap 2 |
| CLIPTL > VIEWPORT | 11 | panels::viewport (two-way) |

`super::` references (ui, production) top files: audio_trigger_section 9, actions 9, scene_setup_panel 8, inspector/mod 8, param_slider_shared/row_host 7, audio_setup_panel 7, browser_popup 5, graph_canvas/interaction 5.

ui > foundation: 138 lines; symbols NodeId 45, ParamId 38, scene_surface 14, work_count 6, EffectGroupId 6, settings 5, LayerId 5, AudioSendId 5, RELIGHT_FEATURE_ENABLED 4, Beats 4. Declared deps: foundation only (ui has no `manifold_core` / editing / playback / io references outside comments).

`PanelAction` references: ui 698 lines, app 422. ui-defined mirror types `Ui*` (12 `pub enum|struct Ui*`; 292 reference lines in ui, 410 in app); app-side translation file `ui_translate.rs` 959 lines (UIBRIDGE > UIROOT 41 refs).

## 5. Other crates

Cargo `[dependencies]` (manifold-*): app > audio, compositor, core, editing, fluids, foundation, gpu, io, led, media, node-engine, nodes, nodes-image, nodes-scene, nodes-water, physics, playback, profiler, recording, spectral, ui, ui-paint, water-liquid, water-rigid. ui > foundation; ui-paint > foundation, gpu, ui; editing > core, io; playback > core, editing, io; audio > core, playback, spectral; io > core; core > foundation. Declared but no production reference: editing > io, playback > io, audio > playback.

Non-comment reference lines per crate pair (production):

| From > To | Lines |
|---|---|
| app > core / ui / editing / gpu / node-engine / nodes-scene / playback / io / ui-paint / audio / compositor | 1,754 / 991 / 362 / 333 / 264 / 164 / 132 / 80 / 62 / 22 / 86 |
| ui-paint > ui / foundation / gpu | 42 / 8 / 9 (grouped gpu imports expand to about 22 distinct Gpu* types) |
| playback > editing / core | 43 / 259 |
| editing > core | 514 |
| io > core / audio > core | 51 / 11 |
| core > foundation | 28 |

app > ui module use (paths, ranked): panels 280, input 110, node 83, color 72, view 53, graph_view 37, InspectorTab 31, GraphEditCommand 31, graph_canvas 21, RootAction 19, param_surface 18, automation 14, PanelAction 13, interaction_overlay 9; `manifold_ui::input::Key` 77, `panels::scene_setup_panel` 64, `panels::actions` 38, `view::UiGraphTarget` 22, `panels::browser_popup` 18, `graph_view::GraphSnapshot` 18, `panels::picker_core` 16. 18 grouped `use manifold_ui::{..}`: PanelAction 14, ParamsAction 10, Rect 9, RootAction 6, UITree 5, InspectorTab 5, GraphParamTarget 4, EditingAction 4.
app > editing: commands 232 (commands::graph 86, effects 30, settings 23, layer 18, effect_target 15, clip 13, effect_groups 9, automation 9, drivers 5), command 107, service 18, clipboard 4. app > playback: engine 19, ableton_bridge 13, rig_log 10, renderer 8, modulation 8, automation 7. app > io: loader 41, saver 25, preset_file 8, project_folder 4. app > audio: directory 8, analysis 6, permission 5. app > ui-paint: ui_cache_manager 17, ui_renderer 13, clip_draw 12, clip_thumb_gpu 8, layer_bitmap_gpu 5.
ui-paint > ui: node 5, icons::Icon 5, Color32 5, panels::viewport 4, waveform_painter::draw_waveform 2, AutomationFeedback 2, `impl manifold_ui::draw::Painter for UIRenderer`; grouped: transform2d, text (TextMeasure), node, color, Vec2, UIState, FontWeight, Affine2.

editing: top `crate::` targets commands 66, command 38, graph_edit_touches 19, graph 13, effect_target 7; shared constants in commands/mod.rs: WHOLE_INSTANCES 27 uses, GRAPH_EDIT 21, graph_edit_touches 20. Prod lines by file group (commands/): graph/scene dir 10,641; effects 1,719; service 1,643; graph/node_edit 1,476; settings 1,467; clip 1,382; effect_groups 1,309; layer 1,090; graph/expose 947; preset 867; graph/modifiers 759; automation 746; graph/skin 703; material 578; session_commands 565; graph/modifier_stack 558; graph/groups 465; drivers 457; graph/layer_plane 400; audio_setup 377; stage 370; audio_mod 357; envelopes 305; trigger_source 253; ableton 192; marker 151. 199 `impl Command for` total. editing > core modules: change 93, effect_graph_def 67, effects 66, project 50, GraphTarget 39, NodeId 29, PresetTypeId 22, params 18, scene_exposure 11, scene_modifier_preset 10, liquid_domain 8.
playback: flat modules (engine 4781, modulation 4667, ableton_bridge 3534, percussion_orchestrator 1885, automation 1828, live_clip_manager 1311, midi_input 1154, audio_layer_playback 1163, midi_clock_sync 994, session_state 958, osc_sync 892, transport_sync 836, audio_mixdown 806). Intra edges (crate::): engine > automation 25, sync > rig_log 8, live_clip_manager > scheduler 7, engine > modulation 7, ableton_bridge > rig_log 6, engine > video_time 5, ableton_bridge > transport_sync 5, engine > live_clip_manager 4. playback > editing by file: engine 13, percussion_orchestrator 12, transport_controller 8, percussion_import 3, live_clip_manager 3; symbols EditingService 10, Command 8, ProjectEdit 5, commands::settings 5, commands::clip 4, tick_write/TickField 4. playback > core: Beats 28, project 24, device_guard 22, Seconds 22, LayerId 21, types 19, effects 14, work_count 12, audio_trigger 11, audio_mod 11. audio: analysis uses core audio_stream 4, audio_setup 2, audio_features 1, AudioBand 1; app uses audio directory 8, analysis (`MAX_SENDS` 3, `StreamingSendAnalyzer` 2), permission 5, detectors 2; `trained/` 6,035 lines in 8 files at depth 1. io: `migrate` 14 intra refs, `migrations/` 4,032 lines; `retired_cpu_flip` 13 core refs; app uses `loader::load_project` 19, `saver::save_project_v1` 19.

## 6. Shared types that would have to cross a crate boundary

| Type or surface | Defined | Used by (lines, lower bounds) |
|---|---|---|
| `PanelAction`, `RootAction` and 20+ `*Action` enums | ui panels/mod.rs, actions.rs | ui 698, app 422 (UIBRIDGE dispatch, INPUT, UIROOT, EDITOR, ui_snapshot) |
| `Ui*` mirror types (12) and `ui_translate.rs` | ui | ui 292, app 410 |
| `UITree`, `Rect`, `Vec2`, `Color32`, `NodeId` (ui node), `Key`, `UIEvent` | ui | app 991 lines total; ui-paint 42 |
| `ContentCommand` (95 variants), `ContentState` | app (content_command.rs, content_state.rs) | app only; ui_bridge 261 command refs, content_commands 126, input_host 106 |
| `PresetInstance` | core effects/instance.rs | app 124, editing 88, playback 29, io 15, core 145 |
| `EffectGraphDef`, `ParamSpecDef`, `BindingTarget` | core effect_graph_def.rs | app 180, editing 268, io 21, core 136 |
| `Project` | core | app 568, editing 639, playback 121, io 50 |
| `GraphTarget`, `NodeId`, `PresetTypeId`, `EffectId`, `LayerId` | core | editing 39/29/22/8/7, app (ui_bridge 23 GraphTarget), playback LayerId 21 |
| `Command`, `CompositeCommand`, `EditingService`, `ProjectEdit`, `TickField` | editing | app 107+18+..., playback (EditingService 10, Command 8, ProjectEdit 5) |
| `EditingHost` / `TimelineEditingHost` | editing service.rs:17 / ui timeline_editing_host | impl in app editing_host.rs:169 (`TimelineEditingHost`) and ui test hosts |
| Scene modifier types `SceneNodeRef`, `SceneModifierRecipe`, scene_exposure fns | core | editing scene_modifier_preset 10, scene_exposure 11, app scene_modifier_preset 45, nodes-scene implements `NodeParamProvider` |
| Foundation ids (`NodeId`, `ParamId`, `LayerId`, `AudioSendId`, `EffectGroupId`) | foundation | ui 138 lines, core 28, app via core |
| `AudioFeatures`, `audio_stream`, `audio_setup`, `audio_trigger`, `device_guard`, `work_count` | core | playback 10/-/5/11/22/12, audio 1/5/2/-/6/-, app 3/-/23/23/-/- |
