# rescreen: proof

Commands are run from the scratchpad against `/Users/peterkiemann/MANIFOLD - Rust` at main 37a9c180a. `RG` below is `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F NAME "/Users/peterkiemann/MANIFOLD - Rust"`; `CORPUS` is `rg -l -w -F NAME /tmp/simplification_census/corpus/projects` (182 projects; a loading check only, so 0 means no load path needs to survive the deletion); `GIT` is `git -C "/Users/peterkiemann/MANIFOLD - Rust" log -S NAME --format='%h %ad %s' --date=short -- crates | head -4` (substring match, so a short name also matches longer ones). The summary after each arrow is what the command printed. Verdict tags come from `raw/pub_refs_v2/`.

### rescreen-001

Claim: 5 thin wrappers over the fn the product calls, each called only by tests of its own file (2, 2, 12, 7 and 8 test sites); the product calls `drain_layer_tap_stamped`, `render_export_audio` + `write_export_wav`, `evaluate_all_automation_at`, `resolve_refs_with_controls` and `prepare_control_bases` + `compose_prepared_controls`; 0 corpus; gap: the tests are repointed, and the WAV byte-identity test stays

- `drain_layer_tap` (TEST-ONLY fn, manifold-playback/src/audio_layer_playback.rs:612)
  - RG `drain_layer_tap` -> 5 hits: 3 in other files (crates/manifold-playback/src/audio_layer_playback.rs x3); 1 in comments (crates/manifold-playback/src/audio_layer_playback.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `drain_layer_tap` -> 0 project files (loading check only)
  - GIT `drain_layer_tap` -> 4+ commits, newest: 51bef3b5a 2026-09-27 Preserve audio source positions and report input gaps
- `render_export_mix` (TEST-ONLY fn, manifold-playback/src/audio_mixdown.rs:303)
  - RG `render_export_mix` -> 16 hits: 5 in other files (crates/manifold-playback/src/audio_mixdown.rs x5); 4 in comments (crates/manifold-playback/src/audio_mixdown.rs x3, crates/manifold-app/src/content_export.rs); 7 in docs (docs/AUDIO_ENGINE_DIRECTION.md, docs/OFFLINE_AUDIO_REACTIVE_EXPORT_DESIGN.md x5, docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `render_export_mix` -> 0 project files (loading check only)
  - GIT `render_export_mix` -> 3 commits, newest: d207f94ad 2026-07-07 feat(audio): P1 offline-export mixdown seam - render_export_audio + ExportAudio
- `evaluate_all_automation` (TEST-ONLY fn, manifold-playback/src/automation.rs:440)
  - RG `evaluate_all_automation` -> 21 hits: 13 in other files (crates/manifold-playback/src/automation.rs x13); 4 in comments (crates/manifold-playback/src/engine.rs, crates/manifold-playback/src/automation.rs, crates/manifold-app/src/ui_translate.rs, crates/manifold-app/src/content_thread.rs); 4 in docs (docs/AUTOMATION_AUTHORING_AUDIT.md, docs/PARAM_STEP_ACTIONS_DESIGN.md, docs/AUTOMATION_LANES_DESIGN.md, docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `evaluate_all_automation` -> 0 project files (loading check only)
  - GIT `evaluate_all_automation` -> 4+ commits, newest: e284b843b 2026-10-15 Automation recording: release-ended takes, held-still writing, thinning
- `resolve_refs` (TEST-ONLY fn, manifold-playback/src/session_state.rs:333)
  - RG `resolve_refs` -> 18 hits: 8 in other files (crates/manifold-playback/src/session_state.rs x8); 7 in comments (crates/manifold-playback/src/engine.rs, crates/manifold-playback/src/scheduler.rs, crates/manifold-playback/src/session_state.rs x5); 3 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md, docs/CORE_ENGINE_MAP.md, docs/DESIRED_STATE_RECONCILIATION_DESIGN.md)
  - CORPUS `resolve_refs` -> 0 project files (loading check only)
  - GIT `resolve_refs` -> 2 commits, newest: 269c45a95 2026-10-09 Retain live and session control intervals with source-timed delivery
- `compose_controls` (TEST-ONLY fn, manifold-playback/src/modulation/composition.rs:67)
  - RG `compose_controls` -> 18 hits: 9 in other files (crates/manifold-playback/src/modulation/composition.rs x9); 1 in comments (crates/manifold-playback/src/modulation/composition.rs); 8 in docs (docs/FLUID_ENGINE_INTEGRATION_PLAN.md, TRIGGER_LANES_DESIGN.md (deleted) x6, docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `compose_controls` -> 0 project files (loading check only)
  - GIT `compose_controls` -> 4+ commits, newest: 2586c916d 2026-10-14 SCENE_SURFACES P8: automation and modulation stay inside the slider range
- `drain_layer_tap` (audio_layer_playback.rs:612): its own doc calls it a legacy API that logs and drops gaps. The one product reader is `drain_layer_tap_stamped` at manifold-app/src/audio_mod_runtime.rs:505. Callers: audio_layer_playback.rs:752 and :804, both in `mod layer_tap_tests` (cfg(test)).
- `render_export_mix` (audio_mixdown.rs:303): body is `render_export_audio` then `write_export_wav`. manifold-app/src/content_export.rs:730 and :741 call those two directly. Callers: audio_mixdown.rs:529 and :558, inside `mod tests` (cfg(test) at 367). `render_export_mix_byte_identity_fixture` pins a WAV hash and must stay; repoint it.
- `evaluate_all_automation` (automation.rs:440): calls `evaluate_all_automation_with_mode(.., Playback)`, which calls `evaluate_all_automation_at`. The engine calls `_at` (engine.rs:1431). Callers: 12 sites, all after the cfg(test) at automation.rs:921. One is the test helper `eval_unarmed` (automation.rs:989; its doc calls it the P1-era call shape kept as a thin wrapper), which 16 more tests call. engine.rs:107 names it in a doc comment.
- `resolve_refs` (session_state.rs:333): calls `resolve_refs_with_controls(Beats(..), .., None)`. The engine's `resolve_session_refs` calls the `_with_controls` form. Callers: 7 sites in session_state.rs tests (601, 704, 731, 737, 746, 774, 778; cfg(test) at 535). Comments at scheduler.rs:266 and engine.rs:2238 name it.
- `compose_controls` (modulation/composition.rs:67): runs `prepare_control_bases` then `compose_prepared_controls`. modulation.rs calls those two directly (257, 606, 650 and 204, 343, 608, 1164). Callers: 8 sites in composition.rs tests (cfg(test) at 318).
- Lines: 105 (estimate). Spans read with span.sh (doc and attributes included): 14 + 16 + 28 + 19 + 24 = 101, plus blank lines. The test edits remove nothing.

### rescreen-002

Claim: 3 staged evaluators with no product caller: the engine and the app run `evaluate_modulation` -> `compose_project_controls`; the staged ones are called by the module's own tests (fixture wrappers, 46 audio sites, the staged-vs-composed equivalence test) and by 4 manifold-app test files; the private `evaluate_instance_drivers` and `apply_instance_envelopes` serve only them and those tests; 0 corpus; gap: 4 app test files run `evaluate_modulation` instead

- `evaluate_all_drivers` (TEST-ONLY fn, manifold-playback/src/modulation.rs:299)
  - RG `evaluate_all_drivers` -> 23 hits: 13 in other files (water_family.rs x4, crates/manifold-playback/src/modulation.rs x7, crates/manifold-app/src/ui_bridge/inspector.rs x2); 3 in comments (crates/manifold-playback/src/modulation.rs x3); 7 in docs (docs/SCENE_PANEL_UX_DESIGN.md, docs/BUG_BACKLOG.md x4, docs/PARAM_STEP_ACTIONS_DESIGN.md, docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `evaluate_all_drivers` -> 0 project files (loading check only)
  - GIT `evaluate_all_drivers` -> 4+ commits, newest: 58ace37c4 2026-10-09 Unify snapshot and retained parameter composition
- `evaluate_all_envelopes` (TEST-ONLY fn, manifold-playback/src/modulation.rs:373)
  - RG `evaluate_all_envelopes` -> 18 hits: 5 in other files (crates/manifold-playback/src/modulation.rs x5); 1 in comments (crates/manifold-playback/src/modulation.rs); 12 in docs (docs/BUG_BACKLOG.md x3, PRESET_UNIFICATION_PLAN.md (deleted) x2, TRIGGER_LANES_DESIGN.md (deleted) x4, PRESET_FORK_INVENTORY.md (deleted), ...)
  - CORPUS `evaluate_all_envelopes` -> 0 project files (loading check only)
  - GIT `evaluate_all_envelopes` -> 4+ commits, newest: 58ace37c4 2026-10-09 Unify snapshot and retained parameter composition
- `evaluate_all_audio_mods` (TEST-ONLY fn, manifold-playback/src/modulation.rs:1197)
  - RG `evaluate_all_audio_mods` -> 24 hits: 5 in other files (crates/manifold-app/src/ui_bridge/projection/cards.rs, crates/manifold-app/src/ui_bridge/projection/inspector.rs x2, crates/manifold-playback/src/modulation.rs x2); 6 in comments (crates/manifold-app/src/ui_bridge/projection/inspector.rs x2, crates/manifold-playback/src/modulation.rs x2, crates/manifold-playback/src/automation.rs x2); 10 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md, docs/BUG_BACKLOG.md, TRIGGER_LANES_DESIGN.md (deleted) x3, AUDIO_LAYER_DESIGN.md (deleted) x2, ...); 3 in beads
  - CORPUS `evaluate_all_audio_mods` -> 0 project files (loading check only)
  - GIT `evaluate_all_audio_mods` -> 4+ commits, newest: 58ace37c4 2026-10-09 Unify snapshot and retained parameter composition
- Product path: engine.rs:1475 and :1617, manifold-app/src/content_pipeline/trigger_targets.rs:331 and offline_audio_mod.rs:1028 call `evaluate_modulation` (modulation.rs:413), which calls `compose_project_controls` (:447) -> `compose_instance_controls` (:590).
- `evaluate_all_drivers`: 0 product callers. Test callers: modulation.rs:2492 (the staged side of `retained_composition_preserves_staged_values_meters_and_envelope_edges`, :2426) and :4346; manifold-app/src/ui_bridge/project/tests/water_family.rs:233, :236, :428, :431 and manifold-app/src/ui_bridge/inspector.rs:327, :340 (inside `mod scene_card_convergence_tests`).
- `evaluate_all_envelopes`: callers are `evaluate_all_envelopes_fixture` (modulation.rs:1457, inside `mod tests` from :1406) and tests at :1925, :1951, :2495.
- `evaluate_all_audio_mods`: runs `advance_audio_hops` or `advance_snapshot_audio` then `apply_audio_controls`, a partial pipeline the engine never runs on its own. Callers: `evaluate_all_audio_mods_fixture` (:1460, 46 test sites), manifold-app/src/ui_bridge/projection/cards.rs:1326 (after its cfg(test) at :1227) and inspector.rs:2027 and :2154 (cfg(test) at :1889).
- `evaluate_instance_drivers` (:340) is called by `evaluate_all_drivers` and by tests at :4279, :4313, :4368. `apply_instance_envelopes` (:195) is called by `evaluate_all_envelopes` only. Both move with them.
- The test that keeps the evaluators honest compares staged against composed output. After the move it stays as it is, with the staged side local to the test module.
- Lines: 175 (estimate). Spans: 40 + 44 + 33 + 17 + 31 = 165, plus the three phase banner comments (about 12). These lines leave product code and stay as test code; nothing is deleted outright.

### rescreen-003

Claim: `LiveClipManager::clear_on_seek` has 3 callers, all tests (tests/live_clip.rs:483 and :523, live_clip_manager.rs:1269); the engine's seek path (engine.rs:1169-1190) does its own collect-ids, `clear_all`, stop loop and says why; 0 corpus

- `clear_on_seek` (TEST-ONLY fn, manifold-playback/src/live_clip_manager.rs:245)
  - RG `clear_on_seek` -> 7 hits: 4 in other files (crates/manifold-playback/src/live_clip_manager.rs x2, crates/manifold-playback/tests/live_clip.rs x2); 1 in comments (crates/manifold-playback/src/engine.rs); 2 in docs (docs/SESSION_MODE_DESIGN.md, docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `clear_on_seek` -> 0 project files (loading check only)
  - GIT `clear_on_seek` -> 3 commits, newest: 269c45a95 2026-10-09 Retain live and session control intervals with source-timed delivery
- engine.rs:1169 comment: `live_clip_manager.clear_on_seek needs a stop callback` and cannot be called for a borrow conflict. The code below it reads `live_slots_list()`, calls `clear_all()`, then stops each clip through the renderer. `clear_all` clears more than `clear_on_seek` does (it also clears slot_creation_times, clip_starts and others), so the two disagree today and the engine's one wins.
- Not checked: whether an engine-level test pins the seek eviction. The deleted tests pin the unused method, not the engine path.
- A Unity port (`Port of C# LiveClipManager.ClearOnSeek`, per its doc) that was never wired.
- Lines: 89 (estimate). fn span 16 + blank, tests 460-528 (69 lines with their blank), 3-line check at :1269.

### rescreen-004

Claim: `LinearResampler::is_identity` is read by 2 asserts in `resampler_identity_passes_rate_through` and `resampler_halves_count_when_downsampling_2to1` (analysis.rs:2258, :2276) and a doc line; both product users compare the rates before building the resampler (trained/input.rs:34, manifold-app audio_mod_runtime.rs:631); 0 corpus

- `is_identity` (TEST-ONLY fn, manifold-audio/src/analysis.rs:422)
  - RG `is_identity` -> 5 hits: 3 in other files (crates/manifold-audio/src/analysis.rs x3); 1 in comments (crates/manifold-audio/src/analysis.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `is_identity` -> 0 project files (loading check only)
  - GIT `is_identity` -> 4+ commits, newest: 712f18f49 2026-09-29 Renderer test prune: collapse per-node checks, delete implementation pins
- analysis.rs:400-401 says `the runtime skips this entirely (see Self::is_identity)`. No runtime code calls it. trained/input.rs:34 builds `(input_rate != MODEL_RATE).then(|| LinearResampler::new(..))`; audio_mod_runtime.rs:631 tests `layer_rate != Some(canonical)` before it constructs one.
- The rest of both tests (output counts for 1:1 and 2:1) stays.
- Lines: 7. Doc line, 3-line fn, blank, 2 asserts.

### rescreen-005

Claim: `VideoRenderer::pre_warm_clips` has 1 hit, the definition, and a stale doc line at manifold-playback engine.rs:102; the live path is `pre_warm_from_candidates` (content_thread.rs:738); 0 corpus

- `pre_warm_clips` (DEAD fn, manifold-media/src/video_renderer.rs:403)
  - RG `pre_warm_clips` -> 3 hits: 1 in other files (crates/manifold-media/src/video_renderer.rs); 1 in comments (crates/manifold-playback/src/engine.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `pre_warm_clips` -> 0 project files (loading check only)
  - GIT `pre_warm_clips` -> 2 commits, newest: 3ff133dba 2026-03-28 fix: wire video decoder prewarm — eliminate black frames at clip start
- `pre_warm_from_candidates` (video_renderer.rs:425) takes the engine's `PrewarmCandidate` map and is called at manifold-app/src/content_thread.rs:738. `pre_warm_clips` takes a map of `VideoClip` that nothing builds.
- Next to engine-runtime-041 (`request_clip_poster`, the same file, also dead). Related prewarm items in the engine-runtime record: engine-runtime-002 (7 uncalled methods in playback engine.rs) and engine-runtime-007 (`append_live_prewarm_candidates` stub).
- Lines: 21. Span 402-421 (20 lines with doc), plus a blank.

### rescreen-006

Claim: `generate_sheet_particles` is a 1-line wrapper (`Ok(trace_sheet_particles(..)?.seeds)`) with 1 hit, the definition, and a doc link from `trace_sheet_particles`; the tests call `trace_sheet_particles` directly; 0 corpus

- `generate_sheet_particles` (DEAD fn, manifold-fluids/src/sheeter.rs:233)
  - RG `generate_sheet_particles` -> 3 hits: 1 in other files (crates/manifold-fluids/src/sheeter.rs); 1 in comments (crates/manifold-fluids/src/sheeter.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `generate_sheet_particles` -> 0 project files (loading check only)
  - GIT `generate_sheet_particles` -> 3 commits, newest: cbeae4ef5 2026-10-06 Sheeting oracle and port: exact decisions, isolation proof, replay proof
- The rest of sheeter.rs (592 lines) is the CPU reference for the GPU sheeting tests: `trace_sheet_particles` and `fixtures::splash` are used by manifold-water-gpu-flip and manifold-water-whitewater GPU tests and by `sheet_oracle.rs`. Not judged here; `sheet_oracle.rs` is in water-019 (the whitewater and sheet oracle feature).
- Lines: 13. Span 230-241 (12 lines with doc), plus a blank.

### rescreen-007

Claim: 4 fns with no caller anywhere (`.material_slot(`, `.transform_slot(`, `install_texture_2d`, `.walk_wires_into(` find 0 call sites); the siblings (`texture_2d_slot`, `light_slot`, `walk_wires`, `walk_wires_from`, `swap_texture_2d`) have callers; `transform_slot` reads TEST-ONLY only because local variables with that name sit in water-rigid; 0 corpus

- `material_slot` (DEAD fn, manifold-node-engine/src/bindings.rs:370)
  - RG `material_slot` -> 4 hits: 1 in other files (crates/manifold-node-engine/src/bindings.rs); 2 in comments (crates/manifold-nodes-scene/tests/gpu_proofs/render_scene_glass.rs, crates/manifold-node-engine/src/bindings.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `material_slot` -> 0 project files (loading check only)
  - GIT `material_slot` -> 3 commits, newest: a4f101802 2026-07-17 feat(scene-object-v2): P2 — render_scene v2 surface + migration, amended for 21 map fields (SCENE_OBJECT_AND_PANEL_V2_DESIGN)
- `transform_slot` (TEST-ONLY fn, manifold-node-engine/src/bindings.rs:376)
  - RG `transform_slot` -> 47 hits: 44 in other files (crates/manifold-node-engine/src/bindings.rs, crates/manifold-water-rigid/src/primitives/rigid_body.rs x16, crates/manifold-water-liquid/src/primitives/fluid_role_source.rs x21, crates/manifold-water-liquid/src/testkit/fluid_role_source.rs x3, ...); 2 in comments (crates/manifold-node-engine/src/bindings.rs, crates/manifold-nodes-scene/tests/gpu_proofs/render_scene_glass.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `transform_slot` -> 0 project files (loading check only)
  - GIT `transform_slot` -> 4+ commits, newest: 913871356 2026-10-08 P1w residual: moved water files use engine-local paths and includes; extent rules for test-fixture primitives register under test
- `install_texture_2d` (DEAD fn, manifold-node-engine/src/exec/metal_backend.rs:309)
  - RG `install_texture_2d` -> 7 hits: 1 in other files (crates/manifold-node-engine/src/exec/metal_backend.rs); 3 in comments (crates/manifold-node-engine/src/exec/metal_backend.rs x3); 3 in docs (docs/SCENE_OBJECT_AND_PANEL_V2_DESIGN.md, INFRARED_BLACKOUT_HANDOFF_2026_06_12.md (deleted), docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `install_texture_2d` -> 0 project files (loading check only)
  - GIT `install_texture_2d` -> 4+ commits, newest: 1101db6a3 2026-05-12 chain-graph: close the perf gap vs native — 6 hot-path fixes
- `walk_wires_into` (DEAD fn, manifold-node-engine/src/graph.rs:694)
  - RG `walk_wires_into` -> 4 hits: 1 in other files (crates/manifold-node-engine/src/graph.rs); 1 in comments (crates/manifold-node-engine/src/graph.rs); 2 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md, docs/evidence/simplification_census_2026-10/graph.proof.md)
  - CORPUS `walk_wires_into` -> 0 project files (loading check only)
  - GIT `walk_wires_into` -> 1 commits, newest: 307a31ea8 2026-05-24 node_graph: unified WireWalkMode API + migrate cycle / topo passes
- `rg '\.(material_slot|transform_slot)\('` over crates: 0 call sites. The `transform_slot` hits in manifold-water-rigid/src/primitives/rigid_body.rs (525, 532, 570, 580, 597, 602, 610, 621) are a parameter and a local with that name.
- `install_texture_2d` (metal_backend.rs:309): 0 callers. Three doc comments (:332, :348, :357) use it as the thing the live `replace_texture_2d` and `swap_texture_2d` avoid; they need rewording.
- `walk_wires_into` (graph.rs:694): the `walk_wires` trio shares one predicate. `walk_wires` has 3 callers (substeps.rs:176, node_pairs.rs:147, validation.rs:899) and `walk_wires_from` has 2 (validation.rs:913, :1001). `walk_wires_into` has none.
- Lines: 32. Spans: 5 + 5 + 8 + 10 = 28, plus blanks.

### rescreen-008

Claim: `StateStore::cleanup_owner` is called by its own test `cleanup_owner_drops_only_matching_entries` (state_store.rs:381) and nowhere else; the docs at :40 and :54 say the host calls it from a cleanup hook, and no such hook exists; the live teardown is `cleanup_all` and `cleanup_nodes`; 0 corpus; gap: not traced whether one runtime ever sees two owner keys

- `cleanup_owner` (TEST-ONLY fn, manifold-node-engine/src/state_store.rs:156)
  - RG `cleanup_owner` -> 9 hits: 2 in other files (crates/manifold-node-engine/src/state_store.rs x2); 2 in comments (crates/manifold-node-engine/src/state_store.rs x2); 5 in docs (.claude/skills/effect-development/SKILL.md, docs/MULTI_DISPLAY_DESIGN.md, docs/EFFECT_RUNTIME_UNIFICATION.md x2, docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `cleanup_owner` -> 0 project files (loading check only)
  - GIT `cleanup_owner` -> 4+ commits, newest: 7d3d713e3 2026-05-23 feedback: decompose node.feedback into pure 1-frame delay + primitive vocabulary
- runtime/core.rs:163 holds one `state_store: StateStore` per `PresetRuntime`; teardown calls are `cleanup_all` (core.rs:1989, runtime/resize.rs:98, runtime/lifecycle.rs:26) and `cleanup_nodes` (core.rs:2026). Nothing calls `cleanup_owner`.
- The owner key is set per chain by the compositor (`layer_id_owner_key`, `group_id_owner_key`, the LED master key in manifold-compositor/src/layer_compositor.rs:260-275, :674, :724) and passed in as `ctx.owner_key` (core.rs:1715). If one runtime can serve two keys, state for a vanished key would stay until `cleanup_all`; then the right fix is to call this fn where the owner ends, not to delete it. The delete itself changes no behavior today.
- Docs that name it and would go stale: docs/EFFECT_RUNTIME_UNIFICATION.md:760 (the StateStore API list), docs/MULTI_DISPLAY_DESIGN.md:296 (says cleanup sites compute keys from layer and clip identity), .claude/skills/effect-development/SKILL.md:143 (the legacy StatefulEffect hook).
- Lines: 50 (estimate). fn span 13 + blank, test 352-387 (36 lines).

### rescreen-009

Claim: 2 lookups the code has moved past, each called only by tests: `node_id_by_handle` (graph.rs:897, :908; persistence.rs:928-1039; graph_loader.rs:2159) is named in graph.rs:294 as the predecessor of `instance_by_node_id`; `catalog_graph_def_for` (1 in-file test, 3 nodes contract-test sites) has a module doc that says editing commands need it, and none call it; 0 corpus; gap: those tests use `Graph::handles()` and `catalog_source::preset_def` instead

- `node_id_by_handle` (TEST-ONLY fn, manifold-node-engine/src/graph.rs:278)
  - RG `node_id_by_handle` -> 35 hits: 31 in other files (crates/manifold-node-engine/src/persistence.rs x4, crates/manifold-node-engine/src/graph.rs x3, crates/manifold-node-engine/src/load/graph_loader.rs, crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/persistence.rs x2, ...); 3 in comments (crates/manifold-node-engine/src/graph.rs x2, crates/manifold-nodes/tests/contracts/node_graph/image_tests/fusion_proof.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `node_id_by_handle` -> 0 project files (loading check only)
  - GIT `node_id_by_handle` -> 4+ commits, newest: 0cf0b783a 2026-10-10 fix(tests): keep water contracts in the linked catalog harness
- `catalog_graph_def_for` (TEST-ONLY fn, manifold-node-engine/src/palette.rs:146)
  - RG `catalog_graph_def_for` -> 7 hits: 5 in other files (crates/manifold-node-engine/src/palette.rs x2, crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/palette.rs x2, crates/manifold-nodes/tests/contracts/palette.rs); 1 in comments (crates/manifold-node-engine/src/palette.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `catalog_graph_def_for` -> 0 project files (loading check only)
  - GIT `catalog_graph_def_for` -> 4+ commits, newest: 86d5a8c13 2026-10-08 P1 residual: engine crate compiles and passes alone
- `node_id_by_handle` is a 3-line read of the `handles` map. The map itself stays: `add_node_named` is called by graph_loader.rs:645 and composites/soft_focus.rs:31-32, and `handles()` (graph.rs:354) has product readers at exec/resource_allocation.rs:250, persistence.rs:502 and param_binding.rs:798.
- `catalog_graph_def_for` (palette.rs:146) is `bundled_preset_def(effect_type).map(|def| def.as_ref().clone())`. palette.rs:8-14 names `PresetInstance.graph` lifting in editing commands as its purpose; `rg catalog_graph_def_for crates` outside tests and palette.rs finds nothing. Callers: manifold-nodes/tests/contracts/node_graph/catalog_tests/palette.rs:1 and :12, tests/contracts/palette.rs:164.
- Lines: 36 (estimate). graph.rs span 5 + blank; palette.rs fn span 11 + blank, test module 10, module doc paragraph about 8.

### rescreen-010

Claim: `CONVERSION_DEBT_LEDGER` lists one id, `node.watercolor`, and is read only by the meta-test `every_boundary_atom_declares_its_reason` (manifold-nodes/tests/contracts/freeze/classify.rs:46, :56); `ConversionDebt` has one holder (watercolor.rs:340) and the ledger's own comment says watercolor is a bundle awaiting decomposition, which is `FusedBundle`; the string `boundary:conversion_debt` appears in the generated docs/node_catalog.json:48514 only; 0 corpus

- `CONVERSION_DEBT_LEDGER` (TEST-ONLY const, manifold-node-engine/src/freeze/classify.rs:418)
  - RG `CONVERSION_DEBT_LEDGER` -> 12 hits: 6 in other files (crates/manifold-nodes/tests/contracts/freeze/classify.rs x5, crates/manifold-node-engine/src/freeze/classify.rs); 2 in comments (crates/manifold-nodes/tests/contracts/freeze/region.rs, crates/manifold-node-engine/src/freeze/classify.rs); 4 in docs (docs/evidence/simplification_census_2026-10/content.proof.md x2, docs/evidence/simplification_census_2026-10/rescreen-delta.md, docs/landings/2026-07-13-graph-tooling.md)
  - CORPUS `CONVERSION_DEBT_LEDGER` -> 0 project files (loading check only)
  - GIT `CONVERSION_DEBT_LEDGER` -> 4+ commits, newest: bba59955a 2026-10-08 P1 review fixes: tests whose subject is a real node or preset are restored with their original nodes and live renderer-side (tone
- The ledger is a deliberate gate from GRAPH_TOOLING_DESIGN.md D5 (the meta-test freezes conversion debt behind an explicit ledger): `ConversionDebt` is legal only for ids in a reviewed list. With one entry that the entry's own comment says is mis-filed, the gate guards nothing. This needs Peter because D5 is a recorded design call.
- classify.rs:418-436: the comment on the single entry says it is not a mechanical conversion candidate and matches `FusedBundle`'s `awaiting decomposition into atoms`; docs/PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md tracks the real fix.
- Other mentions to reword: freeze/region/census.rs:21 (doc), manifold-nodes/tests/contracts/freeze/region.rs:697 (doc).
- Lines: 53 (estimate). Ledger span 24 (413-436), variant and its doc 7, match arm 1, ledger half of the meta-test about 21.

### rescreen-011

Claim: `scheduled_ticks` is read by 2 asserts in manifold-app/tests/contracts/node_graph/catalog_tests/physics_impulses.rs (:288, :527) and named in a doc at physics_impulses.rs:382; it is the second test-only getter next to `is_scheduled` in water-009 (capture_scene_impulse wrapper and getter); 0 corpus

- `scheduled_ticks` (TEST-ONLY fn, manifold-nodes-water/src/runtime/physics_impulses.rs:92)
  - RG `scheduled_ticks` -> 5 hits: 3 in other files (crates/manifold-nodes-water/src/runtime/physics_impulses.rs, crates/manifold-app/tests/contracts/node_graph/catalog_tests/physics_impulses.rs x2); 1 in comments (crates/manifold-nodes-water/src/runtime/physics_impulses.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `scheduled_ticks` -> 0 project files (loading check only)
  - GIT `scheduled_ticks` -> 3 commits, newest: 66fba804e 2026-10-10 Remove the CPU FLIP node and its runtime
- Same file and same callers as water-009. Extends that row; one lane should take both.
- Lines: 7 (estimate). Span 6 + blank. The doc at :382 needs a reword.

### rescreen-012

Claim: `CompletionLedger` and `CompletionReceipt` have no use outside stepping.rs: the other hits are the impl block and 3 tests (`live_interval_completion_receipts`, `live_interval_completion_rejects_public_nonfinite_receipt`, `live_interval_coupled_endpoints`); PHYSICS_ENGINE_BOUNDARY_DESIGN.md:36 lists it as a type to reuse across the boundary; landed in an unreviewed checkpoint (f2f561574); 0 corpus

- `CompletionLedger` (TEST-ONLY struct, manifold-physics/src/stepping.rs:149)
  - RG `CompletionLedger` -> 8 hits: 6 in other files (crates/manifold-physics/src/stepping.rs x6); 2 in docs (docs/PHYSICS_ENGINE_BOUNDARY_DESIGN.md, docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `CompletionLedger` -> 0 project files (loading check only)
  - GIT `CompletionLedger` -> 1 commits, newest: f2f561574 2026-10-03 Live sim clock: work in progress checkpoint (P1 reference, partial P2, HUD plumbing; unreviewed)
- Same file and same decision as water-014 (StepInterval::visit_hits, StepAction and pressure_impulse_delta_velocity). Ask both together.
- `module_check` on stepping.rs lists `CompletionReceipt`, `CompletionLedger`, `ScheduledStep`, `cfl_step_duration`, `visit_hits`, `StepAction`, `pressure_impulse_delta_velocity` and the two LIVE_DEFAULT consts as having no use outside the file.
- Lines: 165 (estimate). Receipt struct with doc about 11, ledger struct 6, impl 97, 3 tests 1068-1117 (50 lines).

### rescreen-013

Claim: `CflPolicy` and the 99-line `cfl_step_duration` it calls have no product caller: the hits are 2 reference checks (clock.rs:746 and the GPU test `one_step_bound_requires_a_whole_reference_interval`, gpu_flip_step.rs:2873-2881) plus 2 in-file tests; `gpu_flip_clock.rs` takes `CflRestrictions` and its own marker speed rule; 0 corpus

- `CflPolicy` (TEST-ONLY struct, manifold-physics/src/stepping.rs:480)
  - RG `CflPolicy` -> 8 hits: 7 in other files (crates/manifold-physics/src/stepping.rs x3, crates/manifold-physics/src/clock.rs x2, crates/manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs x2); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `CflPolicy` -> 0 project files (loading check only)
  - GIT `CflPolicy` -> 2 commits, newest: 8fe15aeb5 2026-10-04 Skip provably inactive GPU FLIP slots using exact retired state
- The type is the `live reference CFL rule` (its own doc); the defaults match the vendored FLIP Fluids engine. Cutting it removes the oracle for the one-step CFL bound test.
- Struct 476-485, `Default` 487-496, impl 498-521 (`duration`, `schedule`), `cfl_step_duration` 523-624, in-file tests `live_cfl_epsilon_and_ceil_match_reference` and `live_nonfinite_speed_reports_diagnostic_and_keeps_time` (about 35 lines).
- Lines: 181 (estimate). 10 + 10 + 24 + 102 + about 35.

### rescreen-014

Claim: `stiffness_fitting_cap` is read only by its own test (`matter_dials_limited_to_substep_cap`, matter.rs:434); matter_domain.rs:891 sets `let fitted = stiffness;` and :892 `let limited = fitted < stiffness;`, which is always false, so the warning, the `limited` field and the `limited_by_substeps` output (always 0) never fire; GPU_MPM_SOLVER_DESIGN.md:228 still promises the fit and a node warning; 0 corpus

- `stiffness_fitting_cap` (TEST-ONLY fn, manifold-water-gpu-mpm/src/matter.rs:296)
  - RG `stiffness_fitting_cap` -> 4 hits: 2 in other files (crates/manifold-water-gpu-mpm/src/matter.rs x2); 1 in comments (crates/manifold-water-gpu-mpm/src/matter.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `stiffness_fitting_cap` -> 0 project files (loading check only)
  - GIT `stiffness_fitting_cap` -> 2 commits, newest: f0abf1ac3 2026-09-30 MPM P1 (3/n): the Live Matter graph runs end to end
- Today a Stiffness that needs more than `MAX_SUBSTEPS` runs capped at 128 substeps and raises `step_cap_hit` (matter_domain.rs:970) instead of dialing stiffness down. Wiring the fit changes the look of a stiff scene at high resolution; deleting keeps today's look.
- `limited_by_substeps` is an output port of the Matter node (matter_domain.rs:286, :486, docs/node_catalog.json:21938). 0 hits anywhere else, 0 corpus files. Deleting it regenerates the catalog and removes a port from the node.
- git log -S finds only the two P1 commits (dc98642b4, f0abf1ac3): the fit was never wired to the node after the function landed.
- Lines: 43 (estimate). Fn span 9 + blank, test about 19, dead block about 10, field 1, output 3.

### kept-after-check

Each item below got the standard trio and a read of its callers. The reason is the last bullet.

- `GuardFixture` (TEST-ONLY struct, manifold-node-engine/src/load/expand/testkit.rs:25)
  - RG `GuardFixture` -> 4 hits: 3 in other files (crates/manifold-node-engine/src/load/expand/testkit.rs x2, crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/expand_parameter_guards.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `GuardFixture` -> 0 project files (loading check only)
  - GIT `GuardFixture` -> 1 commits, newest: 91000f187 2026-10-07 P1 tests review: smallest test surface. Production internals stay private; tests reach engine state through 91 narrow testkit pro
  - Kept because: Test infrastructure by design. It lives in `load/expand/testkit.rs` (census section 3.2 leaves testkit modules out); its one user is manifold-nodes/tests/contracts/node_graph/catalog_tests/expand_parameter_guards.rs:1. The screen lists it because the file is named `testkit.rs`, not `testkit/`.
- `write_synthetic_multimaterial_glb` (TEST-ONLY fn, manifold-nodes-scene/src/node_graph/gltf_import/testkit.rs:102)
  - RG `write_synthetic_multimaterial_glb` -> 11 hits: 6 in other files (crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/gltf_upgrade.rs x3, crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/gltf_import.rs, crates/manifold-nodes-scene/src/node_graph/gltf_import/tests.rs, crates/manifold-nodes-scene/src/node_graph/gltf_import/testkit.rs); 4 in comments (crates/manifold-nodes-scene/src/node_graph/gltf_load.rs, crates/manifold-nodes-scene/src/node_graph/gltf_import/synthetic_glbs.rs x2, crates/manifold-nodes-scene/src/node_graph/gltf_import/testkit.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `write_synthetic_multimaterial_glb` -> 0 project files (loading check only)
  - GIT `write_synthetic_multimaterial_glb` -> 4+ commits, newest: bbd1e34f2 2026-10-08 P2 prep: mixed scene tests that need image nodes move unchanged into renderer catalog homes
  - Kept because: Test infrastructure by design: `gltf_import/testkit.rs`, used by catalog_tests/gltf_upgrade.rs (2 sites), gltf_import.rs and the in-crate gltf_import/tests.rs:25.
- `seed_segment_cache_for_test` (TEST-ONLY fn, manifold-node-engine/src/freeze/install.rs:1215)
  - RG `seed_segment_cache_for_test` -> 14 hits: 10 in other files (crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/runtime_chain_fusion.rs x9, crates/manifold-node-engine/src/freeze/install.rs); 1 in comments (crates/manifold-node-engine/src/freeze/install.rs); 3 in docs (docs/FREEZE_COMPILER_MAP.md, GRAPH_COMPILER_HARVEST_PLAN.md (deleted), docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `seed_segment_cache_for_test` -> 0 project files (loading check only)
  - GIT `seed_segment_cache_for_test` -> 4+ commits, newest: e09e078ba 2026-07-22 refactor(preset_runtime): P3-R — move 11 test mods out to tests/*.rs (W3-D1)
  - Kept because: Feature-gated test seam: `#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]`, documented at freeze/install.rs:1050 as the way fused paths are exercised without the worker thread. 9 users in catalog_tests/runtime_chain_fusion.rs.
- `NodeErrorTap` (TEST-ONLY struct, manifold-node-engine/src/exec/effect_node.rs:285)
  - RG `NodeErrorTap` -> 10 hits: 7 in other files (crates/manifold-node-engine/src/exec/effect_node.rs x3, crates/manifold-app/tests/renderer_contracts/gpu_proofs/liquid_conformance.rs x4); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md); 2 in beads
  - CORPUS `NodeErrorTap` -> 0 project files (loading check only)
  - GIT `NodeErrorTap` -> 3 commits, newest: 86d5a8c13 2026-10-08 P1 residual: engine crate compiles and passes alone
  - Kept because: Gated on `gpu-proofs` and `doc(hidden)`; 3 users in manifold-app/tests/renderer_contracts/gpu_proofs/liquid_conformance.rs. Open bead BUG-c4ed (Run-time node errors reach only stderr, never the performer) names it and says to delete the tap once a real error channel exists. Not a second finding.
- `standalone_2d_slots` (TEST-ONLY fn, manifold-node-engine/src/primitives/standalone_pipeline.rs:58)
  - RG `standalone_2d_slots` -> 6 hits: 3 in other files (crates/manifold-node-engine/src/primitives/standalone_pipeline.rs, crates/manifold-nodes/tests/contracts/freeze/codegen.rs x2); 1 in comments (crates/manifold-node-engine/src/primitives/standalone_pipeline.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md); 1 in beads
  - CORPUS `standalone_2d_slots` -> 0 project files (loading check only)
  - GIT `standalone_2d_slots` -> 2 commits, newest: b7d188564 2026-09-10 Fix audit regressions in tempo persistence, allocation reuse, and layout proofs
  - Kept because: Executable spec for the canonical binding order, read by the contract test `canonical_kernels_match_dispatch_helper_slot_order` (manifold-nodes/tests/contracts/freeze/codegen.rs:216-250). Observation for the lane that owns it: `dispatch_standalone_2d` (standalone_pipeline.rs:81) builds its bindings inline and never calls `standalone_2d_slots`, so the test checks a model of the order, not the helper. Making the helper call the spec closes that.
- `from_json_str` (TEST-ONLY fn, manifold-node-engine/src/runtime/build.rs:255)
  - RG `from_json_str` -> 53 hits: 43 in other files (crates/manifold-nodes-water/src/runtime/physics_sampling/input_tests.rs, physics_scene.rs x2, crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/physics_sampling.rs x7, crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/gpu_flip_preset.rs, ...); 4 in comments (crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/runtime_generator_runtime.rs x2, crates/manifold-node-engine/src/load/graph_loader.rs, crates/manifold-node-engine/src/runtime/build.rs); 6 in docs (docs/RENDERER_RUNTIME_DECOMPOSITION_DESIGN.md x2, BUG_BACKLOG_CLOSED.md (deleted), docs/evidence/simplification_census_2026-10/rescreen-delta.md, docs/NODE_ID_TARGETING.md x2)
  - CORPUS `from_json_str` -> 0 project files (loading check only)
  - GIT `from_json_str` -> 4+ commits, newest: 6b2c54990 2026-10-10 Water test cut ((water test cut)): 45 probes, references and measurements deleted, slow proofs shortened, the live
  - Kept because: Documented mock-backend test constructor (`fine for unit tests; production uses from_json_str_with_device`, runtime/build.rs:253). About 40 test sites in manifold-nodes and manifold-nodes-water. A move to a testkit is possible but frees no lines.
- `clear_project_presets` (TEST-ONLY fn, manifold-node-engine/src/load/preset_loader.rs:298)
  - RG `clear_project_presets` -> 26 hits: 19 in other files (crates/manifold-nodes/src/bundled_presets.rs, crates/manifold-nodes/tests/scene_modifier_file_authoring.rs x2, crates/manifold-nodes/tests/project_preset_overlay.rs x6, crates/manifold-app/tests/project_local_preset_reload.rs x5, ...); 3 in comments (crates/manifold-nodes/tests/project_preset_overlay.rs, crates/manifold-app/tests/stock_preset_round_trip.rs, crates/manifold-node-engine/src/load/preset_loader.rs); 3 in docs (docs/BUG_BACKLOG.md, docs/evidence/simplification_census_2026-10/rescreen-delta.md, PRESET_INSTANCE_COLLAPSE_PLAN.md (deleted)); 1 in beads
  - CORPUS `clear_project_presets` -> 0 project files (loading check only)
  - GIT `clear_project_presets` -> 4+ commits, newest: 098daabd8 2026-10-07 P1 seams: preset metadata reloads go through named core providers in name order; duplicate preset ids fail
  - Kept because: A 3-line alias for `set_project_presets` with empty lists, used by about 20 test sites (nodes tests, manifold-app project_local_preset_reload.rs). Product code replaces the overlay wholesale on each project load through `install_embedded_presets` (manifold-app/src/project_io.rs:44-60). The doc says project close or switch; not checked whether File > New clears the previous project's local presets.
- `set_dump_arrays` (TEST-ONLY fn, manifold-node-engine/src/runtime/dump_sets.rs:47)
  - RG `set_dump_arrays` -> 5 hits: 2 in other files (crates/manifold-nodes-water/src/testkit/whitewater_scene.rs, crates/manifold-node-engine/src/runtime/dump_sets.rs); 1 in comments (crates/manifold-node-engine/src/runtime/dump_sets.rs); 2 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md, docs/evidence/simplification_census_2026-10/graph.proof.md)
  - CORPUS `set_dump_arrays` -> 0 project files (loading check only)
  - GIT `set_dump_arrays` -> 4+ commits, newest: 6b2c54990 2026-10-10 Water test cut ((water test cut)): 45 probes, references and measurements deleted, slow proofs shortened, the live
  - Kept because: Probe API. Its only caller is manifold-nodes-water/src/testkit/whitewater_scene.rs:531; the reader it feeds, `dump_arrays_all`, is used by manifold-nodes/examples/fluid_capture.rs (4 sites) and 5 test files. The product (generator_renderer.rs:450) uses `set_dump_visible`.
- `render_target_2d` (TEST-ONLY fn, manifold-node-engine/src/exec/metal_backend.rs:425)
  - RG `render_target_2d` -> 6 hits: 4 in other files (crates/manifold-nodes/tests/contracts/runtime/transient_slot_tests.rs x3, crates/manifold-node-engine/src/exec/metal_backend.rs); 1 in comments (crates/manifold-node-engine/src/exec/metal_backend.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `render_target_2d` -> 0 project files (loading check only)
  - GIT `render_target_2d` -> 4+ commits, newest: bba59955a 2026-10-08 P1 review fixes: tests whose subject is a real node or preset are restored with their original nodes and live renderer-side (tone
  - Kept because: A 3-line observer used by `transient_slot_tests.rs:75-80`, which pins which slots own a render target. `texture_2d` cannot answer that, because a borrowed texture shadows the owned one.
- `render_viewport_frame` (TEST-ONLY fn, manifold-nodes-scene/src/node_graph/viewport_render.rs:148)
  - RG `render_viewport_frame` -> 20 hits: 9 in other files (crates/manifold-nodes/tests/scene_loop_wrap_parity.rs x3, crates/manifold-nodes/tests/scene_loop_probe.rs x3, crates/manifold-nodes-scene/src/node_graph/viewport_render.rs, crates/manifold-nodes-scene/tests/gpu_proofs/scene_viewport_navigate.rs x2); 6 in comments (crates/manifold-nodes/tests/scene_loop_wrap_parity.rs, crates/manifold-nodes/tests/gpu_proofs/scene_viewport_session.rs, crates/manifold-nodes-scene/src/node_graph/viewport_render.rs, crates/manifold-nodes-scene/src/node_graph/viewport_session.rs x3); 3 in docs (docs/SCENE_LOOP_DESIGN.md x2, docs/evidence/simplification_census_2026-10/rescreen-delta.md); 2 in beads
  - CORPUS `render_viewport_frame` -> 0 project files (loading check only)
  - GIT `render_viewport_frame` -> 4+ commits, newest: 891be1cce 2026-10-09 P2 residual: manifold-nodes-scene compiles and passes alone; compiler-demanded wiring, testkit-only widenings and gate ownership
  - Kept because: Headless one-shot render used by three GPU test files: the P5 viewport gate proof (manifold-nodes-scene/tests/gpu_proofs/scene_viewport_navigate.rs:155) and two probes (manifold-nodes/tests/scene_loop_wrap_parity.rs:473, scene_loop_probe.rs:76). The live viewport uses `viewport_session`. Fits the headless-render rule in CLAUDE.md; a move to a testkit is possible but frees no lines.
- `VolumeDrift` (TEST-ONLY struct, manifold-water-gpu-flip/src/primitives/gpu_flip_volume.rs:39)
  - RG `VolumeDrift` -> 5 hits: 4 in other files (crates/manifold-water-gpu-flip/src/primitives/gpu_flip_volume.rs x2, crates/manifold-nodes/tests/contracts/water/primitives/gpu_flip_scene_tests.rs x2); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `VolumeDrift` -> 0 project files (loading check only)
  - GIT `VolumeDrift` -> 3 commits, newest: 6b2c54990 2026-10-10 Water test cut ((water test cut)): 45 probes, references and measurements deleted, slow proofs shortened, the live
  - Kept because: Same file as `volume_and_area`, which water.md keeps as test infrastructure (callers in another crate's tests; widening owned by BUG-hkbdp.6.9 (Consolidate water test-only compilation)). Its one user is gpu_flip_scene_tests.rs:1107.
- `set_all_tiles` (TEST-ONLY fn, manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs:207)
  - RG `set_all_tiles` -> 18 hits: 16 in other files (crates/manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs, crates/manifold-water-gpu-flip/src/primitives/gpu_flip_pressure_tests.rs x3, crates/manifold-nodes/tests/contracts/water/primitives/gpu_flip_tile_tests.rs x7, crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/gpu_flip_body.rs x5); 1 in comments (crates/manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs); 1 in docs (docs/evidence/simplification_census_2026-10/rescreen-delta.md)
  - CORPUS `set_all_tiles` -> 0 project files (loading check only)
  - GIT `set_all_tiles` -> 4+ commits, newest: bba59955a 2026-10-08 P1 review fixes: tests whose subject is a real node or preset are restored with their original nodes and live renderer-side (tone
  - Kept because: Feature-gated test lever (`cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))`), documented as the test-only oracle that makes the sparse passes run dense (gpu_flip_step.rs:199-205, design D-6). Users: gpu_flip_pressure_tests.rs:1688 and gpu_flip_tile_tests.rs:658-698. Same class as the water-gpu-flip `set_*` hooks water.md keeps under BUG-hkbdp.6.9.

### module-follow-up

- `module_check.py` (the engine-model sweep's scratchpad script: every `pub` item of a file, counted outside the file with comment- and impl-aware rules) over the 9 files that hold a candidate row: bindings.rs, exec/metal_backend.rs, graph.rs, state_store.rs, palette.rs (node-engine), sheeter.rs (fluids), video_renderer.rs (media), matter.rs (water-gpu-mpm), analysis.rs (audio).
- -> No file is a dead module. Items with no use outside their file that are not in the 36 (the screen files them CRATE-ONLY because the file uses them): `storage_revision_of` (bindings.rs:388), `drop_all_resources` (metal_backend.rs:518, 19 lines), `PaletteAtom` (palette.rs:75), `SheetTrace` (sheeter.rs:245; used by `trace_sheet_particles`, which the GPU tests call), `AnalyzedHop`, `get_linear` and `queue_snare_fires` (analysis.rs), 5 `matter.rs` consts. None is judged here.
- stepping.rs (physics) was checked the same way for rescreen-012 and rescreen-013; its result is in those rows.

