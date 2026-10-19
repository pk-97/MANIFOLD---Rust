# Simplification census: graph sweep, proof

Main commit 37a9c180a. Candidate table: `graph.md`. This file holds the commands behind each row and a one-line summary of each output.

## Shorthand

```
REPO="/Users/peterkiemann/MANIFOLD - Rust"
CORPUS=/tmp/simplification_census/corpus/projects
S1: rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F NAME "$REPO"
S2: rg -l -w -F NAME $CORPUS
```

S1 and S2 are the standard pair of the census doc's section 3.1 (Dead pub items). Every row below ran both for every name it lists; the summary says what S1 printed (definition line, other hits). S2 printed nothing unless stated. The corpus is 182 projects; the 67 online-only Dropbox files were not opened.

Scope-wide checks, run once:

- `rg -c 'extern "C"|#\[no_mangle\]' crates/manifold-{node-engine,nodes,nodes-image,nodes-scene}`: no output. No `.m`, `.swift` or `.h` file names any item below (`rg -l --glob '*.{m,mm,swift,h}' -w -e LinePipeline -e MeshPipeline -e UniformArena crates`: no output).
- `rg -n 'paste::|paste!|concat_idents' crates/manifold-{node-engine,nodes-scene,nodes-image,nodes}/src`: no output, and none of the four Cargo.toml files has a `paste` dependency. `stringify!` appears only inside the `primitive!` macro (primitive.rs:1186, :1201) and builds node type ids, not Rust item names.
- Only two build.rs files exist in scope (node-engine, nodes-scene); `include!(` appears in no scope `.rs` file outside them.
- Denied commands: one `cd` attempt at the start (persistent_cd_guard); rerun with absolute paths. No other command was denied.
- Existing tooling reused: `raw/pub_refs/*.tsv` (census_refs output) for every DEAD and TEST-ONLY row; `bd show BUG-axzlz` and `bd show BUG-hkbdp` children for the bead checks. A scratchpad shell loop (`single_file_items.sh`, not kept) re-ran `rg -l -w NAME` per non-USED row to find names that occur in one file only; nothing in `scripts/TOOLS.md` does that.

## Candidates

### graph-001
Module `line.rs` plus `shaders/generator_lines.wgsl`.
- Started from DEAD row `resize_vertices` (line.rs:227). S1 on `resize_vertices`: definition only.
- S1 on `LineGeneratorHelper`: line.rs, one `.claude/skills/generator-development/SKILL.md` example, two comments in nodes-scene render_lines.rs and render_lines.wgsl. S1 on `LinePipeline`: line.rs, two skill and doc lines, a comment in mesh/pipeline.rs. S1 on `EdgeInstance`: line.rs, generator_lines.wgsl, an archived audit doc, and a string `"EdgeInstance"` in nodes/tests/uniform_layout_extended.rs:373 that names the render_lines.rs struct (a different type) as an exclusion. S1 on `DEFAULT_DOT_RADIUS`: line.rs:2 and render_lines.rs:45 (its own private const). S1 on `prepare_instances`: line.rs and the skill example.
- `rg -n 'node_engine::line|crate::line|mod line' crates --glob '*.rs'`: lib.rs:7 only (plus an unrelated `mod linear_gradient;`). No code imports from the module.
- `rg -n -F generator_lines "$REPO"` (hidden dirs included): line.rs:62 (`include_str!`), comments in render_lines.rs and render_lines.wgsl, two doc rows, one gate fixture JSON (`scripts/fixtures/gate-p1.json`, a census snapshot).
- S2 on the three type names: one file, `108_feedbackTests_.json` (index: `~/Downloads/feedbackTests.manifold`, 2026-05-21), matching `LineGeneratorHelper` inside a node `purpose` string ("carries the animation ... from the legacy LineGeneratorHelper"). Description text, not a use.
- `git log -S'LinePipeline::new'`: last change a8fbc12f4 (2026-05-25, generators: remove OscilloscopeXY). No tests in line.rs (`rg '#\[cfg\(test\)\]' line.rs`: none). 392 + 149 + 1 = 542 lines.

### graph-002
Directory `runtime/effects/`.
- Started from DEAD rows `dispatch_a_only`, `dispatch_a_only_with`, `HDR_BUFFER_DIVISOR`. S1 on each: definition only; `HDR_BUFFER_DIVISOR` also in `.claude/hooks/context-nudges/gpu-shader-gotchas.md:18` (prose).
- S1 on `ComputeBlitHelper`: its file, skills (effect-development, gpu-metal-patterns), MANIFOLD_GPU_ARCHITECTURE.md:245, DEVELOPMENT_REFERENCE.md:90, one comment in the dual helper. S1 on `ComputeDualBlitHelper`: its own file only.
- `rg -n 'runtime::effects|effects::compute' crates --glob '*.rs'` filtered to node-engine paths: no use outside `runtime/mod.rs:132` (`pub mod effects;`).
- S2 on both names: no output. `git log -S'ComputeBlitHelper::new'`: last change 75a7391ea (2026-05-30, delete 14 legacy effect monoliths). Lines: mod.rs 6 + 102 + 169 + 1 = 278.

### graph-003
`stateful.rs`.
- DEAD rows `read_texture`, `write_texture`: S1 definition only. `StatefulState` was CRATE-ONLY with own_src 1.
- S1 on `StatefulState`: the definition and `impl StatefulState` (stateful.rs:6, :13), nothing else in the repo. `rg -n 'stateful::|StatefulTextures|mod stateful' crates`: lib.rs:9 only.
- S2: no output. `git log -S'StatefulState::new'`: last change d99234582 (2026-05-23, oily_fluid cutover). 75 + 1 = 76 lines.

### graph-004
`mesh/pipeline.rs` lines 1-166 and `shaders/mesh_pipeline.wgsl`.
- S1 on `MeshPipeline`: pipeline.rs, the shader's header comment, one skill line. S1 on `MeshInstance`: pipeline.rs and a doc comment at mesh.rs:100. S1 on `MeshUniforms`: pipeline.rs, plus an unrelated private struct of the same name in manifold-water-surface volume_surface_mesh.rs:44.
- `rg -n 'mesh_pipeline' crates --glob '*.rs'`: pipeline.rs:60 (`include_str!`) and two comments.
- Kept: `perspective_rh`, `look_at_rh`, `mat4_mul`, `ortho_rh`, `sub3`, `cross3`, `dot3`, `normalize3` (lines 168-260). Imported at scene/camera.rs:21, scene/light.rs:35, nodes-scene nested_cubes_geometry.rs:20, digital_plants_render.rs:30 and render_scene.rs (comment).
- S2 on `MeshPipeline`: no output. `git log -S'MeshPipeline'`: only commit 98c55d683 (2026-04-03, Galactic Rock) touches the string. 166 + 186 = 352 lines.

### graph-005
- S1 on `PARTICLE_COMMON_WGSL`: definition at particles.rs:63 only. `rg --hidden -l -F particle_common "$REPO"`: particles.rs, three WGSL files (comments saying "identical to particle_common.wgsl"), nodes/tests/wgsl_validation.rs:32 (a PARTIAL_SHADERS skip entry), and the gate fixture JSON.
- The shader `shaders/particle_common.wgsl` is 217 lines, included only by the const. A second shader, `fluid_blur_3d.wgsl`, is a parity oracle read by `include_str!` in nodes-image image_tests/codegen.rs:1442 and stays.
- S2: no output. `git log -S'PARTICLE_COMMON_WGSL'`: c50031653 (2026-03-16). 217 + 4 (doc and const) = 221 lines.

### graph-006
- S1 on `outer_routings_from_bindings`: definition param_binding.rs:792; prose in docs/archive/BINDINGS_UNIFICATION_PLAN.md (3 lines) and docs/EFFECT_RUNTIME_UNIFICATION.md:691. No code.
- S1 on `inner_routing_for`: composite_handle.rs:74, the call at param_binding.rs:811, doc comments at param_binding.rs:786 and validate.rs:428.
- The live path: `rg -n outer_routings_from_view crates`: app graph_snapshot.rs:130-141, loaded_preset_view.rs:203.
- `bd show BUG-axzlz`: names the compositor's `graph_snapshot_for` and `outer_routings_for` only; not this fn. Kept as a sibling, not a duplicate. `OuterParamRouting` stays alive through loaded_preset_view.rs and the app.
- S2: no output. `git log -S'outer_routings_from_bindings('`: last change 5dc1c672a (2026-05-15). Lines 778-837 = 60, plus `inner_routing_for` 7 + 4 doc = ~70.

### graph-007
- S1 on `new_memoryless`, `resize_pooled`: definitions only (render_target.rs:254, :289). S1 on `create_texture_memoryless`: manifold-gpu metal/device.rs:2080 and the call at render_target.rs:261 only, so it loses its caller (app-ui sweep). S1 on `new_pooled`: other callers (render_target_pool.rs:74, runtime/core.rs:1298, compositor), so it stays; `release_to_pool` is called at render_target_pool.rs:88, stays.
- S2: no output. `git log -S'new_memoryless'`: d3002a874 (2026-03-26). 26 + 4 doc + 9 + 2 = ~41.

### graph-008
- S1 on `invalidate_memoized_dataflow`: definition exec/execution.rs:661; prose in docs/BUG_BACKLOG.md:65, :1544.
- `git log -S'invalidate_memoized_dataflow'`: added in cf79f628d (BUG-318 (plan-swap stale executor state)); the in-place recompile it served was retracted by c901fcc9b (BUG-319 (retract in-place plan recompile)). `rg -n memo_steps_len crates`: the live resets are execution.rs:670 (`reset_after_resource_replacement`, called at runtime/resize.rs:94) and the length check at :1634.
- S2: no output. Lines 646-663 = 18.

### graph-009
- S1 on `set_executor`: definition runtime/core.rs:1753; one table row in docs/RENDERER_RUNTIME_DECOMPOSITION_DESIGN.md:77 listing core.rs contents. S2: no output. 6 lines.

### graph-010
- S1 on `prepared_modifier_buffer_usage_with_snapshot`: definition modifier_runtime.rs:221, nothing else. S1 on `prepared_modifier_buffer_usage`: callers in app scene_modifier_edit.rs:860 and scene_modifier_performance.rs:320, so the sibling stays. S1 on `admit_candidate_bytes`: callers in water-gpu-flip and the app, stays. `GpuMemorySnapshot` still used by chain_dispatch.rs. S2: no output. `git log -S`: b20e360c1, c1a2db88b (2026-09). 18 lines.

### graph-011
- S1 on `MAX_GEN_PARAMS`: definition preset_context.rs:39; six prose hits in archived docs and `.claude/skills/generator-development/SKILL.md:91`. S2: no output. 4 lines.

### graph-012
- S1 on `PLATONIC_SHAPE_COUNT`: definition mesh.rs:264 only. S2: no output. 3 lines (doc, const, blank).

### graph-013
- S1 on `tap_count`: definition scene/light.rs:108 only (common word, but the whole-word hit list is one line). S2: no output. `git log -S'tap_count'`: 712f18f49 (2026-09-29, renderer test prune). 14 lines.

### graph-014
- S1 on `set_reference_fusion`: definition runtime/testkit.rs:47 only; the function is behind `#[cfg(feature = "gpu-proofs")]`. S2: no output. 5 lines.

### graph-015
- S1 on `ENGINE_SOURCE_IDENTITY`: definition scene/source_asset.rs:8. `rg -n 'MANIFOLD_PHYSICS_INTEGRATION_IDENTITY'`: source_asset.rs:8 and node-engine build.rs:16 only.
- `git show 66fba804e` removed the reader (`compose_solver_identity` and the `ENGINE_SOURCE_IDENTITY` use) with the CPU FLIP node. Its message also says the sha2 build dependency went; node-engine's Cargo.toml still has `[build-dependencies] sha2`, which build.rs needs only through `scripts/native_source_identity.rs`.
- `bd show BUG-hkbdp` children: the water Cargo hygiene bead names the water crate's build.rs and `MANIFOLD_WATER_SOURCE_IDENTITY`, not node-engine's. The helper `scripts/native_source_identity.rs` stays while core, physics and fluids build.rs files still call it (they are in "seen, not judged").
- build.rs has an `#[expect(dead_code, ...)]` (blame 2026-10-08, 86d5a8c133) that disappears with the file. Lines: 3 (const) + 19 (build.rs) + 3 (Cargo) = 25.

### graph-016
- S1 on `scale_drag_delta`: definition only. `ROTATE_SENSITIVITY`: definition only (doc says it serves `rotate_drag_delta`, which computes angles and does not read it). `SCALE_SENSITIVITY`: definition and the multiply inside `scale_drag_delta`.
- S1 on `move_drag_delta`: viewport_gizmo.rs:431, the call in `scale_drag_delta`, and two tests at :824-837; no caller outside the file. App window_input.rs (Move and Scale arms near :1040) calls `target.projected_drag_delta`, defined at viewport_gizmo.rs:171, and the Rotate arm calls `rotate_drag_delta`.
- S2: no output. `git log -S'scale_drag_delta'`: d2bc03f50, 891be1cce. Lines: 5 + 10 + 3 for the three items, 19 for `move_drag_delta` with its doc, ~17 for the two tests = ~54. The compiler will report `GizmoAxis::unit` if it becomes unused.

### graph-017
- S1 on `relight_field_name`: definition nodes-scene relight.rs:98 only. S2: no output. 11 lines. Independent of the relight decision: no caller exists whether or not the feature returns.

### graph-018
- TEST-ONLY row `remove_node`. S1: definition graph.rs:360; calls at node_pairs.rs:467, graph.rs:843, :845, :881, :906. All in `#[cfg(test)]` modules.
- Tests: graph.rs:824 `removing_pairs_disables_a_survivor_only_after_its_last_pair` (also asserts duplicate-pair rejection, 1 line of other coverage), :887 `add_then_remove_node_clears_wires_touching_it` (only remove_node), :851 `external_output_registration_validates_and_clears_with_node` (ends with a 3-line remove check, the rest tests other things and stays), node_pairs.rs:454 `removing_coupled_participant_clears_compile_metadata` (only remove_node).
- S2: no output. Lines: fn 26 + tests ~21 + ~23 + 17 = ~86 (the first test keeps its duplicate-pair assertion if the lane wants it).

### graph-019
- TEST-ONLY row `binding_value`. S1: definition param_binding.rs:769; calls at :1547, :1549, :1552 inside `binding_value_finds_id_in_either_tier` (:1519-1553), which asserts nothing else. Its doc names `should_skip` predicates; `rg -n should_skip crates` finds none that use it. S2: no output. 17 + 34 = ~50 lines.

### graph-020
- DEAD row `uniform_arena_mut` (gpu_encoder.rs:121). S1: definition only.
- `rg -n 'uniform_arena|UniformArena|push_bytes' crates tools --glob '*.rs'` outside the compositor: gpu_encoder.rs:17 (field), :55 and :80 (init None), :121-122 (the dead accessor), scene_viewport.rs:81 (copies the field), generator_renderer.rs/layer_compositor.rs (set, reset, flush), one app test calling `flush`. No place reads through the pointer except the dead accessor.
- `rg -n 'arena\.(push|push_bytes|buffer)\(' crates`: one hit, layer_compositor.rs:120 `let _offset = arena.push(uniforms);`. The following dispatch uses `GpuBinding::Bytes` inline. Comments at layer_compositor.rs:478 and :3632 say the buffer is not read on the native path.
- Bead check: `bd show BUG-bfktg` (Rendering-side decomposition epic): its crate-split table lists `UniformArena` as an engine item used only by the compositor, meaning a move; no bead says it is dead. `git log -S'uniform_arena.push'`: 26eeb4a18 (2026-03-25, "global uniform arena", a perf change).
- Count: uniform_arena.rs 72; gpu_encoder.rs ~15; layer_compositor.rs 29 mentions (~35 lines with fn parameters); generator_renderer.rs 12 mentions plus doc and cfg lines (~16); scene_viewport.rs 1; app test 1: ~140. Hot-path note: removing it also removes one memcpy per blend pass per frame. S2: no output.

### graph-021
- TEST-ONLY row `prewarm_ids`. `rg -w PluginPrewarm crates`: plugin_prewarm.rs (definition, `collect!`, the two iterators, the test's message), a doc line in effect.rs; no `submit!` anywhere. `git log -S'PluginPrewarm {'`: last submissions removed in fb99e0924 (2026-06-12) and 60d4a42de.
- `ast-grep run -l rust -p 'impl PostProcessEffect for $T { $$$ }' crates` and the `$P::PostProcessEffect` variant, plus `rg -n 'impl .*PostProcessEffect' crates plugins`: no output. No `macro_rules!` in runtime/ generates impls. Last implementor removed in fb99e0924.
- Users that remain: `prewarm_all` at compositor layer_compositor.rs:748 and node-engine testkit/gpu_harness.rs:81 (`mem::forget` of an empty Vec); `plugin_warmups` read at layer_compositor.rs:3655 (resize refusal, never true) and :3746 (flush loop over nothing).
- The test `plugin_using_effects_all_have_prewarm_submissions` iterates `expected: [PresetTypeId; 0]`. Its doc says so.
- Overlaps the 3.10 screen (no implementors of PostProcessEffect, empty PluginPrewarm); the patterns sweep should drop its row. Lines: struct and collect ~27, `prewarm_all` 12, `prewarm_ids` 7, test ~35 (plugin_prewarm.rs ~80 of 119; `PipelinePrewarm` stays), effect.rs 45 + 1, harness 8, compositor ~12: ~160. S2 on both names: no output.

### graph-022
- `rg -n 'SourceImplementationIdentity' crates`: source_asset.rs:12 (struct), :18 (`collect!`), gltf_anim_identity.rs:9 (the one `submit!`) and its test at :92 (`inventory::iter` then `assert_eq!` on the entry). The reader `compose_solver_identity` was deleted in 66fba804e.
- `MANIFOLD_PHYSICS_FAMILY_IDENTITY` is read at gltf_anim_identity.rs:11, :95 and fed by nodes-scene build.rs; nothing else. All of it is `#[cfg(feature = "gpu-proofs")]`.
- Not A because the registry shape (a lower crate declaring a hook a higher crate fills) is the 3.10 inversion pattern; here no consumer remains, but the lead should confirm with `docs/RENDERER_CRATE_SPLIT_DESIGN.md` D4 (no-facade rule) that no planned reader exists. Lines: struct and collect ~9, build.rs 15, submit 8, test 13, Cargo 3 = ~45.

### graph-023
- `cargo machete` rows from `raw/machete.txt`: node-engine `manifold-playback`, `png`; nodes-scene `png`.
- `rg -n '\bmanifold_playback::|use manifold_playback\b|extern crate manifold_playback' crates/manifold-node-engine`: one hit, a doc-comment word at runtime/core.rs:2010. `rg -n 'dep:manifold-playback|"manifold-playback/' Cargo.toml`: none.
- `rg -n '\bpng::|use png\b|extern crate png'` in node-engine: two hits, both `image::codecs::png::PngEncoder` (testkit/gpu.rs:106, gpu/headless_readback.rs:213). nodes-scene: one hit, `image::codecs::png` at gltf_import/synthetic_glbs.rs:316. `dep:png`: none. Both crates already depend on `image` with the png feature.
- `crates/manifold-app/tests/crate_layering.rs:21` lists playback among allowed node-engine dependencies; it is an allow-list. No Cargo feature, derive or macro in these crates expands to `png` or `manifold_playback` names. Final proof is the lane's `cargo check -p manifold-node-engine -p manifold-nodes-scene --all-targets --all-features`. 3 lines.

### graph-024
- `raw/suppressions.txt` entry primitive.rs:1294 and :1313, `rg -n '(allow|expect)\((dead_code|unused)' crates/manifold-{node-engine,nodes,nodes-image,nodes-scene}` re-run directly: also lists install.rs:1790 (not in the raw file), build.rs x2, one_euro_filter.rs:273, rt_bugmajv_kernel_toggle.rs:169.
- `git blame -L 1294,1294` and `-L 1313,1313`: 8cb368605f, 2026-05-11. The reason comment is "Structural, not deferred". Condition: "removing it would require auditing each expansion site". The condition is a compiler run, not a code change: remove both and `cargo check --all-targets` per primitive crate. Not graded A because this sweep may not build.

### graph-025
- TEST-ONLY row `default_sun`. S1: definition light.rs:167; calls at :500 (its own test `default_sun_populates_all_fields_with_shadows_off`), :746 (`sun_shadow_proj_is_orthographic`), :798 (`sun_shadow_depth_increases_away_from_the_light`). The last two assert shadow math and need a replacement light. S2: no output. Lines 25 + 13 + fixture edits.

### graph-026
- Flag: `RELIGHT_FEATURE_ENABLED = false` at foundation/feature_flags.rs:16. Readers: core effects/instance.rs:270, ui param_card (5 sites), nodes tests (runtime_chain_fusion.rs 4 sites, topology_hash_tests.rs:66), node-engine runtime/topology_hash_tests.rs:100.
- `rg -il relight crates/manifold-nodes/assets`: no output. `rg -il relight tests/fixtures`: no output. `rg -l -i relight $CORPUS`: no output, and `rg -l '"relight":\s*(true|false)' $CORPUS`: no output (0 of 182).
- Size in scope: `fd -i relight` gives relight.rs (483) and the nodes test relight.rs (283); `rg -c -i relight` per file gives node-engine runtime/core.rs 25, runtime/bindings.rs 20, topology_hash_tests.rs 16 (+18 in nodes), load/augmentation.rs 12, load/chain_spec.rs 9, freeze/install.rs 8, others under 7. Estimate ~900 lines including tests; the rest of the 79 files are in other scopes.
- Decision belongs to Peter (census doc section 10 lists it). Grade C.

### graph-027
- `rg -n 'env::var(_os)?\(' crates/manifold-{node-engine,nodes,nodes-image,nodes-scene}` minus tests: the 16 variables in graph.md. `rg -o 'env::var(_os)?\("[A-Z_0-9]+"\)' src -N --no-filename | sort | uniq -c`: counts match the table. Doc check per variable: `rg -l -w NAME "$REPO" --glob '!crates/**' --glob '!.beads/**'`; scripts, hooks and `.config` name none of them. No judgment made.

## Rows examined and rejected

TEST-ONLY rows in `raw/pub_refs/manifold-node-engine.tsv` and `manifold-nodes-scene.tsv` that are not candidates. For each, S1 gave callers only in tests or test infrastructure; the reason is the one column.

| row | location | callers | reason |
|---|---|---|---|
| `with_preview_budget` | node-engine exec/effect_node.rs:108 | 9 test sites (water-rigid, nodes-water, nodes) | one-line builder; the tests cover step budgets and physics carry |
| `host_array_buffer` | exec/execution.rs:903 | 13 proof harness sites incl. node-engine testkit/atom.rs:193 | documented host-read contract (MANIFOLD_GPU_ARCHITECTURE.md:98); proofs are its readers |
| `malformed_frame`, `inverse_fft_rebind_bytes` | exec/extent/testkit.rs (`#[cfg(any(test, feature = "testkit"))] pub mod testkit;`) | liquid_extent tests | test infrastructure |
| `assert_shared_roots_never_overlap` | exec/resource_allocation.rs:651 | 2 own tests, 1 nodes test | a panicking oracle for the allocator tests |
| `test_canvas_capacity_lineage` | exec/resource_allocation/testkit.rs | 1 nodes test | testkit |
| `fused_generator_view_by_id` | freeze/install.rs:1245 | 4 nodes tests | thin wrapper over the content-keyed path, 5 lines; tests cover generator fusion |
| `fused_effect_cache_len_for_test` | freeze/install.rs:391 | 3 | `cfg(all(any(test, testkit), gpu-proofs))` |
| `fail_allocation_after` | gpu/render_target.rs:29 | 1 | same cfg |
| `prepare_impulses`, `resource_node_id`, `sources_empty`, `imported_view` | load/expand/testkit.rs, load/loaded_preset_view/testkit.rs | tests | testkit |
| `bundle_subdir_for_test`, `dev_subdir_for_test` | load/preset_loader.rs:950 | tests | `#[cfg(any(test, feature = "testkit"))] impl KindDirs` |
| `std430_stride` | ports.rs:467 | about 20 test sites in 3 crates (node-engine, water-gpu-mpm, water-liquid) | drift guard that checks every `*_SPECS` against `size_of` |
| `MIX_TYPE_ID` | primitives/mix.rs:82 | nodes persistence test | one const, one line |
| `handles_for_test`, `bound_for_test`, `bound_mut_for_test`, `generator_input_node_for_test`, `width_for_test`, `height_for_test`, `set_dimensions_for_test` | runtime/core.rs:2032-2042 | tests | `#[cfg(any(test, feature = "testkit"))]` impl blocks |
| `backend_for_test` | runtime/instrumentation.rs:284 | 7 | cfg-gated |
| `modifier_node_copies` | runtime/modifier_runtime.rs:367 | 3 nodes tests | 10-line view method; tests cover the expand compiler |
| `group_preview`, `insert_state`, `has_state`, `set_modifier_events` | runtime/testkit.rs | tests | testkit |
| `find_trigger_driven_switch_value_node_ids` | trigger_shadow_lint.rs:277 | nodes `trigger_shadow_class_guard.rs` | the lint helper exists for that guard test; its sibling is used by runtime/build.rs:389 |
| `azalea_fixture_path`, `merge_summary`, `render_scene_objects`, `rosetta_stone_fixture_path`, `scene_def_with_bbox_half_extent`, `write_synthetic_default_material_glb` | nodes-scene gltf_import/testkit.rs (`cfg(any(test, feature = "testkit"))`) | tests | testkit |
| `anim_set_one_clip` | primitives/gltf_animation_source.rs:981 | tests | inside `cfg(any(test, testkit)) pub mod testkit` |
| `clip_triangle_to_planes` | primitives/mesh_cut_map/oracle.rs:44 | 6 own tests | `#[cfg(test)] mod oracle;`, a test oracle |
| `disarm_rt_capture` | primitives/render_scene.rs:269 | 4 | RT capture pair with `arm_rt_capture`, used by gpu proofs |
| `with_legacy_snapshots`, `inject_history_sentinel` | render_scene.rs:84, render_scene/rt_proof.rs:50 | proofs | `gpu-proofs` only; legacy schedule kept for one byte-exact proof (seen, not judged in graph.md) |
| `is_relight_node_id` | relight.rs:112 | 1 nodes test | part of graph-026 |
| `migrate_fluid_objects` | scene_exposure.rs:500 | 1 | inside the `testkit` module |
| `is_scene_setup_node` | scene_exposure.rs:73 | 2 nodes tests | 3-line predicate over `SCENE_VOCABULARY`, which is product data |

Other items checked and kept:

- `raw/const_bools.txt` for these crates: `Primitive::PURE` (primitive.rs:124) and `Primitive::STENCIL_FETCH` (:188, fixture :125). `rg -c '^\s*pure:\s*true'`: 21 sites in 16 files; `rg -c '^\s*stencil_fetch:\s*true'`: 7 sites in 7 files. Both take both values.
- `raw/features.tsv` rows for node-engine, nodes, nodes-image, nodes-scene: `testkit` 1881 places, `gpu-proofs` 1033, `fluid-perf-proofs` 17, `rt-perf-proofs` 26, `whitewater-oracle` 23. No row of 3 or less.
- Suppressions kept: `fuse_canonical_def` (install.rs:1790, accurate: test-visible wrapper), both `#[expect(dead_code)]` on the shared helper in build.rs (node-engine goes with graph-015, nodes-scene with graph-022; blame 2026-10-08), `one_euro_filter.rs:273` and `rt_bugmajv_kernel_toggle.rs:169` (`unused_assignments` in tests; not dead code, per census doc section 3.3).
- CRATE-ONLY rows with a single in-crate hit (about 40 node-engine rows, 9 nodes-scene, 30 nodes-image) were spot-checked for a dead caller: `install_prepared_routes`, `unretarget_shadow`, `mark_persistent_initialized`, `take_render_target`, `array_scratch`, `try_read_packed_into`, `walk_wires_into`, `visit_presets`, `harvest_from`, `reload_generator_catalog`, `reload_scene_modifier_catalog`, `JOINT_MATRIX_SPECS`, `format_to_str`, `init_cached_type_id`, `awaiting_fused_swap`, `set_dump_arrays`, `RuntimeFactory`, `RuntimePreparation`, `CreateGenerator`, `load_mesh`, `GraphInstaller`, `NodeConstructor`, `prewarm_font`, `composite_overlays`, `group_fluid_role_nodes`, `all_params_for_node_type`, `generated_block`, `camera_frustum_lines`, `OrbitParams`, `SceneParamFacts`, `CutMapReadback`: each has a live use in its own crate (type aliases are used by their own struct fields). Low-use types `ChainDispatchStats` (app content_thread.rs:929), `PaletteAtom`, `NodePair`, `StepDebugInfo`, `PixelProjection`, `ParamDoc` (macro submissions), `TextureChannelMismatchInfo`, `ChannelMismatchInfo`, `VirtualChain`, `RegionSummary`: live.
