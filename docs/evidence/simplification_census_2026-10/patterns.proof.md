# patterns: proof

Commands and one-line results for every candidate in `patterns.md`. `$R` is the repo root, `$C` is `$R/crates`, `$S` is `/tmp/simplification_census`. All searches ran at 37a9c180a; HEAD 30aaa08fa has no change under `crates/` (`git -C "$R" diff --stat 37a9c180a HEAD -- crates` is empty). "0 impls" always means three searches came back empty: `impl NAME for`, `dyn NAME`, and `: NAME` or `NAME +` as a bound.

### patterns-001

Claim: `LayerLifecycleCallbacks` has no implementor and no user.

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F LayerLifecycleCallbacks "$R"` → command.rs:140 (the definition) and census docs only.
- `rg -n 'impl\s+LayerLifecycleCallbacks|dyn\s+LayerLifecycleCallbacks' "$C"` → none.
- `sed -n 136,150p "$C/manifold-editing/src/command.rs"` → doc "Port of C# ILayerLifecycleCallbacks", 8 lines.
- engine-model.proof.md line 993 lists it as DEAD in its "seen, not judged" list; no row there.

### patterns-002

Claim: `SelectionRegionTarget` has no implementor.

- `rg -n -w SelectionRegionTarget "$C"` → selection.rs:129 (definition) and lib.rs:121 (re-export) only.
- `rg -n 'impl\s+SelectionRegionTarget|dyn\s+SelectionRegionTarget|SelectionRegionTarget\s*[+>]' "$C"` → none.

### patterns-003

Claim: `EditingHost` has no implementor; its only user is the dead `get_current_grid_step`.

- `rg -n -w EditingHost "$C"` → service.rs:17 (definition) and service.rs:1484 (`pub fn get_current_grid_step(host: &dyn EditingHost)`).
- `rg -n 'impl\s+EditingHost' "$C"` → none.
- engine-model.proof.md line 565 records `get_current_grid_step` as dead (engine-026), with this trait as its cascade.

### patterns-004

Claim: the two alias sidecar registries have no submissions and can only ever attach empty tables.

- `rg -n 'EffectAliasMetadata|EffectValueAliasMetadata' "$C" -g '*.rs' | rg -v 'effect_registration.rs|preset_definition_registry.rs'` → one doc mention (generator_registration.rs:221), 0 submissions.
- `sed -n 140,170p "$C/manifold-core/src/preset_definition_registry.rs"` → `build_effect_kind_map` attaches alias tables to inventory `EffectMetadata` entries only; the comment says "the inventory loop above only fires for tests". JSON-loaded defs replace the inventory entry for the same id and carry their own table (`leak_alias_table(&meta.param_aliases)` at :637).
- `rg -n 'fn all_default_effect_defs_have_empty_alias_table' "$C"` → preset_definition_registry.rs:887; it asserts every table is empty.
- `rg -n 'EffectAliasMetadata' "$R/scripts"` → `audit_rename.py` writes such submissions. This is the named gap.
- `rg -c 'GeneratorAliasMetadata' "$C" -g '*.rs'` → 10 submissions, so the generator side is live and stays.

### patterns-005

Claim: every `EffectMetadata` inventory submission is test code.

- `rg -n 'EffectMetadata\s*\{' "$C" -g '*.rs'` → 16 hits:
  - the struct (effect_registration.rs:78) and its impl (:158);
  - 13 literals inside `inventory::submit!`, at test_support.rs:9 and :32, editing tests/command_roundtrips.rs:35, editing commands/automation.rs:761, core preset_definition_registry.rs:682, 698, 711, 725, 739, playback automation.rs:935, and playback modulation.rs:1551, 1563, 3225;
  - one test-local `let` (preset_definition_registry.rs:1000).
- Each submit site sits under `#[cfg(test)]`, checked with `rg -n -B30 'EffectMetadata \{' FILE | rg 'cfg\(test\)|mod tests'`. `effects/mod.rs` declares `#[cfg(test)] mod test_support`.

### patterns-006

Claim: `Compositor` has one impl that overrides every method, so the trait and its default bodies are dead weight.

- `rg -n 'impl .*Compositor for' "$C" -g '*.rs'` → only layer_compositor.rs:3043 (`impl Compositor for LayerCompositor`).
- The two method lists match: `rg -o '^    fn (\w+)' -r '$1'` on compositor.rs:148-446 and on layer_compositor.rs:3043-3842 gives 41 names each. `comm -23` of the sorted lists is empty, so every method is overridden.
- `awk` over the trait range, classifying each `fn` as ending in `;` or in a body → 10 required, 31 defaulted. No default body can run.
- `rg -n 'dyn Compositor' "$C"` → app content_pipeline.rs:747 and 1145 (the field and its construction).
- The layering table (RENDERER_CRATE_SPLIT_DESIGN D12) has app depending on compositor directly, so the trait is not a seam.

### patterns-007

Claim: `ExternalProcessRunner` has one impl and callers name it.

- `sed -n 1,20p "$C/manifold-playback/src/process_runner.rs"` → one assoc fn `run_async(...) where Self: Sized`.
- `rg -n 'ExternalProcessRunner|ProcessRunnerImpl' "$C"` → the one impl (process_runner.rs:128); the one product caller (percussion_orchestrator.rs:1201) and two tests write `ProcessRunnerImpl::run_async`. No `dyn` is possible (`Self: Sized`).

### patterns-008

Claim: `PercussionAnalysisParser` has one unit-struct impl held concretely.

- `sed -n 1,30p "$C/manifold-playback/src/percussion_parser.rs"` → "Port of Unity IPercussionAnalysisParser interface"; `JsonPercussionAnalysisParser` is the only impl.
- `sed -n 158,170p "$C/manifold-playback/src/percussion_orchestrator.rs"` → field `percussion_analysis_parser: JsonPercussionAnalysisParser` (:142, set at :166), a concrete type.

### patterns-009

Claim: `ParamSource` has one impl, and its doc claims a second that does not exist.

- `rg -n 'impl.*ParamSource for' "$C"` → instance.rs:1597 (`PresetInstance`) only.
- `rg -n 'dyn ParamSource|: ParamSource|ParamSource \+|impl ParamSource' "$C"` → none besides the impl.
- `sed -n 66,80p "$C/manifold-core/src/effects/mod.rs"` → "Port of Unity IParamSource.cs"; "Both PresetInstance and generator params implement this".

### patterns-010

Claim: the two percussion planner traits have one real impl each plus test mocks.

- `rg -n 'impl .*(PercussionBindingResolver|BeatTimeConverter) for' "$C"` → one product impl each, plus `PanicConverter` and `KickResolver` in percussion_planner.rs:195-213 (tests).
- `sed -n 15,33p "$C/manifold-playback/src/percussion_planner.rs"` → both are held as `Box<dyn …>`.

### patterns-011

Claim: the three native detector traits each have one FFI impl, and callers name the FFI type.

- `rg -n 'impl .*(RegionDetector|BlobDetector|DepthEstimator) for' "$C"` → one `Ffi*` impl each in manifold-native.
- `rg -n 'Ffi(Region|Blob|Depth)\w*' "$C/manifold-nodes-image" -g '*.rs'` → blob_detect_ffi.rs:188, detect_regions.rs:305, depth_estimate_midas.rs:133, optical_flow_estimate.rs:194, person_segment.rs:148, all concrete.
- `rg -n 'libloading' "$C/manifold-native"` → plugins load dynamically, so another platform ships another library, not another impl.

### patterns-012

Claim: `FrameSource` has one impl.

- `rg -n 'FrameSource' "$C/manifold-audio" -g '*.rs'` → trait at trained/frames/mod.rs:26, `impl FrameSource for FrameBank` at :61, `&dyn FrameSource` at trained/net/mod.rs:137 and 150, an import in trained/tests.rs:7.

### patterns-013

Claim: `GpuDenoiser` presents itself as portable but is Metal-typed, with one legacy impl, tracked by BUG-ns5k (Benchmark legacy-path MetalFX denoised scaler).

- `sed -n 70,100p "$C/manifold-gpu/src/metal/denoiser.rs"` → the doc names NRD and ReLAX; the signature takes `ProtocolObject<dyn MTLCommandBuffer>`.
- `rg -n 'impl GpuDenoiser for|dyn GpuDenoiser' "$C"` → one impl (`MetalFxDenoisedScaler`), one user (nodes-scene denoiser.rs:21).
- `jq -r --arg ID "$BEAD" 'select(.id==$ID) | .status' "$R/.beads/issues.jsonl"` with the bead above → open.

### patterns-014

Claim: the "at most one assets root" guard protects a state one line controls.

- `sed -n 405,430p "$C/manifold-node-engine/src/load/preset_loader.rs"` → `select_assets_root` asserts at most one submission and is `testkit_visible!`.
- `rg -n 'should_panic' "$C/manifold-node-engine/src/load/preset_loader.rs"` → :1006, :1012, both feeding it two fake roots.
- `rg -n 'PresetAssetsRoot' "$C" -g '*.rs' | rg submit` → one submission, nodes lib.rs:11.

### patterns-015

Claim: the single-provider lookup is hand-written seven times.

- `rg -n 'inventory::iter::<' "$C" -g '*.rs' -A3 | rg 'next\(\)|more than one|exactly'` → node-engine mesh_asset_source.rs:13, scene/exposure_source.rs:11, load/augmentation.rs:20, resource_allocation.rs:31, generator_provider.rs:26 (`exactly_one`), runtime/plugin_prewarm.rs:65, core param_lookup.rs:51. Each is the same take-first plus duplicate check.

### patterns-016

Claim: three registries and one trait answer one question, and only core's registry is needed.

- `sed -n 30,60p "$C/manifold-core/src/param_lookup.rs"` → `NodeParamSource` registry; `node_source()` at :50 reads it.
- `sed -n 85,160p "$C/manifold-nodes-scene/src/node_graph/scene_exposure.rs"` → `node_params(type_id, with_hidden)` at 98-157 is the one source. `NodeParamSource` is submitted at 91-95 with hidden params. The hidden filter is `role == SurfaceRole::Hidden`, which follows from `NodeParamMetadata.scene = facts.map(|f| f.desc)`.
- `cat "$C/manifold-node-engine/src/scene/exposure_source.rs" | wc -l` → 18 lines for `SceneExposureSource`.
- `sed -n 460,495p` of the same nodes-scene file → its submission (478-482, hidden filtered), the ZST `PrimitiveRegistrySceneExposureProvider` (465-476) and the parity test `scene_exposure_registration_preserves_metadata` (486-494).
- `rg -n 'SceneExposureSource|scene_exposure_source' "$C" -g '*.rs'` → readers only at nodes-water presets/gpu_flip.rs:827 and 829, which filter by param name ("radius", "visible").
- `rg -n 'NodeParamProvider' "$C" -g '*.rs'` → trait at core scene_exposure.rs:50, product impl at nodes-scene :465, passed as `&dyn` into core `migrate_scene_exposures` (:378) from nodes-scene :169; test providers at core :723, 821, 932, 1008, 1268.
- `rg -n 'Box<dyn NodeParamProvider' "$C"` → none, so the trait doc's claim that editing commands store it is false.
- `rg -n 'with_world_metadata\(|metadata_for_node_type\(' "$C/manifold-app" | wc -l` → about 50 call sites. Editing commands get metadata by value from the app, a fourth channel; noted, not rowed.
- The layering check: core param_lookup and core scene_exposure both need the answer, and core is below node-engine and nodes-scene, so `NodeParamSource` is the one D12 allows.

### patterns-017

Claim: five catalog registries are one concept.

- `rg -n 'inventory::collect!\((PresetCatalogSource|PresetSource|LoadedSceneModifierPresetSource|TemplateGraphSource)' "$C"` → declarations in node-engine (`PresetCatalogSource`) and core (the other four).
- `rg -n 'inventory::submit!' "$C/manifold-nodes/src" -A4 | rg 'PresetCatalogSource|PresetSource|LoadedSceneModifierPresetSource|TemplateGraphSource'` → all five submitted from manifold-nodes; bundled_presets.rs:42 and :968 among them.
- `sed -n 35,60p "$C/manifold-nodes/src/bundled_presets.rs"` → `def` and `graph_for` both resolve to `bundled_preset_def`.
- preset_definition_registry.rs:561 reads `LoadedSceneModifierPresetSource`.

### patterns-018

Claim: one nested max-id walk, seven product copies.

- `rg -n 'fn max_node_id' "$C" -g '*.rs'` → 8 hits: the seven product sites in the row plus app tests/contracts/node_graph/catalog_tests/fluid_objects.rs:234 (test). The bodies are the same recursion over `group.nodes` (read side by side with `sed -n`).

### patterns-019

Claim: core's `find_node` does not search groups, so crates wrote nested copies.

- `sed -n 305,330p "$C/manifold-core/src/effect_graph_def.rs"` → `find_node` and `find_node_mut` search the top level only.
- `rg -n 'fn (find_node|node_by_id|find_node_mut|find_node_by_stable_id_mut)\b' "$C" -g '*.rs' -g '!**/tests/**'` → the five product copies in the row, each recursing into groups. Some match by stable id (the named gap).

### patterns-020

Claim: one `ExecutionPlan` lookup is written 30 times.

- `rg -c 'fn output_resource\(' "$C" -g '*.rs' | awk -F: '{s+=$2} END{print s}'` → 30.
- `rg -n 'fn output_resource\(' "$C" -g '*.rs' | rg -v test` → compositor preset_thumbnail.rs:439 and node-engine runtime/core.rs:17. The rest sit in per-primitive `#[cfg(test)]` modules; nodes-scene mesh_snapshot.rs is `cfg(all(test, gpu-proofs))`.

### patterns-021

Claim: `FrameTime` has no constructor, so 73 fixtures build it.

- `rg -c 'fn frame_time\(' "$C" -g '*.rs' | awk -F: '{s+=$2} END{print s}'` → 73.
- `sed -n 145,175p "$C/manifold-node-engine/src/exec/effect_node.rs"` → `FrameTime` is a plain struct with no `new` or `Default`.

### patterns-022

Claim: core has no nested-node walker, so graph passes recurse by hand.

- `rg -n '\w+\(\s*&\s*(mut\s+)?group\.nodes' "$C" -g '*.rs' -g '!**/tests/**'`, counted per crate → 131 sites in product source (app 29, nodes-scene 30, core 21, editing 21, node-engine 11, nodes-water 8, io 5, nodes 4, ui 2). Saved as `$S/patterns_node_walkers.txt`.
- `rg -n 'fn (walk|walk_mut|visit_nodes|for_each_node)' "$C/manifold-core/src/effect_graph_def.rs"` → none.
- `rg -n 'fn nodes_contain' "$C"` → fragment_cuts.rs:157, phong_migration.rs:69, gltf upgrade/project.rs:162.
- The line figure is an estimate (about 2 to 3 lines saved per site), hence C.

### patterns-023

Claim: vector helpers repeat inside manifold-physics.

- `rg -n '^\s*(pub(\(crate\))? )?fn (dot|cross|add|scale|length|normalize|lerp)\b' "$C/manifold-physics/src"` → coupled_motion.rs (dot, cross) and interaction.rs (add, scale, dot, cross, length, normalize, lerp). sdf.rs is f64, so it is a different type and stays.

### patterns-024

Claim: the relight switch keeps about 1,800 lines alive; graph-026 owns about 900.

- `git -C "$R" log -S RELIGHT_FEATURE_ENABLED --reverse --format='%ad %h %s' --date=short | head -1` → 2026-07-18 50a9da35d, "renders poorly and is not ready for the stage … kept intact for a future fix".
- `rg -n 'RELIGHT_FEATURE_ENABLED' "$C"` → core instance.rs:270; ui param_card mod.rs:3380 and render.rs 321, 468-469, 694, 1532; tests in node-engine topology_hash_tests.rs:100 and nodes runtime_chain_fusion.rs (4 sites), topology_hash_tests.rs:66.
- `rg -il relight "$C" | wc -l` → 84 files.
- `wc -l` on the dedicated files: core effects/relight.rs 152, nodes-scene relight.rs 483, nodes catalog_tests/relight.rs 283.
- `rg -ic relight` per file for the rest: editing effects.rs 38, ui param_card render 57, mod 40, state 37, routing 28, compositor generator_renderer 35, app ui_translate 35, scrub 17.
- `rg -l -i relight $S/corpus/projects | wc -l` → 0 of 182.
- The 900 here is the total minus graph-026's figure, so it is an estimate.

### patterns-025

Claim: the diagnostics for BUG-060 (inspector-footer-overpaint) outlived the bug.

- `rg -n 'BUG060|bug060' "$C" -g '*.rs'` → app frame/present.rs:9-52 and 812-840; ui-paint ui_renderer.rs:512, 686, 1163-1194 (`bug060_log_batch`), called at 1782 and 1795, and 2242-2252 (`bug060_trace_band`); ui_cache_manager.rs:241. The code comment says to remove them with the bug.
- `rg -n 'inspector-footer-overpaint' "$R/docs/BUG_BACKLOG.md"` → :197 marks it FIXED, rig-verified on 2026-07-10 and re-confirmed solved on 2026-07-12.

### patterns-026

Claim: the denoise probe outlived BUG-iadf (RT denoise 1:1 mode).

- `rg -n 'MANIFOLD_DENOISE_PROBE|probe_t0' "$C/manifold-nodes-scene"` → render_scene.rs:5986-5990, 9656-9684. The read sits in the per-frame encode path.
- `git -C "$R" log -S MANIFOLD_DENOISE_PROBE --reverse --format='%ad %h' --date=short | head -1` → 2026-08-08.
- `jq` on `.beads/issues.jsonl` for that bead's status and close time → closed 2026-08-08.

### patterns-027

Claim: the denoise-skip ablation branch is a closed experiment read on every frame.

- `sed -n 5635,5682p "$C/manifold-nodes-scene/src/node_graph/primitives/render_scene.rs"` → `std::env::var_os("MANIFOLD_DENOISE_SKIP")` inside the frame path, with no OnceLock.
- `rg -n '// PROBE:' render_scene.rs` → :5545, :5726.
- `rg -l MANIFOLD_DENOISE_SKIP "$R/docs"` → `docs/rt_dn_i_sweep_results.md` (a results doc).

### patterns-028

- `sed -n 6405,6420p render_scene.rs` → `env::var("MANIFOLD_PROBE")` per frame, prints the camera.
- `git -C "$R" log -S '"MANIFOLD_PROBE"' --reverse --format='%ad %h' --date=short | head -1` → 2026-08-01. `rg -l '"MANIFOLD_PROBE"' "$R/docs" "$R/scripts"` → none.

### patterns-029

- `rg -n MANIFOLD_PROBE_RT_ACCEL "$R"` → gpu accel.rs:477, 558 only. Each prints once per acceleration-structure build. No doc, no script.

### patterns-030

- `sed -n 3600,3612p render_scene.rs` → `MANIFOLD_DISABLE_EMISSIVE_SAMPLER`, commented "RS-C test-only gate", read per frame.
- `rg -n MANIFOLD_DISABLE_EMISSIVE_SAMPLER "$C"` → the read plus gpu proof rt_emissive_direct.rs (leg 2 sets it).

### patterns-031

- `sed -n 1188,1200p "$C/manifold-app/src/content_pipeline.rs"` → `MANIFOLD_OCCLUSION_RENDER_SKIP`, default on; the env var only turns the skip off.
- `git -C "$R" log -S MANIFOLD_OCCLUSION_RENDER_SKIP --reverse --format='%ad' --date=short | head -1` → 2026-07-18. `rg -l MANIFOLD_OCCLUSION_RENDER_SKIP "$R/docs" "$R/scripts"` → none.

### patterns-032

Claim: `PercussionPipelineSettings` is never constructed, so every `Some` branch is unreachable.

- `rg -n 'PercussionPipelineSettings' "$C" -g '*.rs'` → its definition, impls and `Option<&PercussionPipelineSettings>` parameters only. There is no struct literal, no `::default()` and no deserialize site outside percussion_settings.rs.
- `rg -n 'PercussionImportOrchestrator::new\(' "$C" -A2` → app.rs:1893-1894 passes `None`, headless_harness.rs:80-81 passes `None`, and the only test (percussion_orchestrator.rs:1380) passes `None`.
- `rg -n 'pipeline_settings' "$C"` → set only in the constructor (percussion_orchestrator.rs:164); no setter.
- `rg -n 'build_invocation(_with_settings)?\(' "$C"` → the `Some` arm at percussion_orchestrator.rs:307 is the only caller of `build_invocation_with_settings`, which is the only caller of `build_settings_arguments` (backend 284-385), which is the only caller of `serialize_to_detection_config_json` (:374) and `stem_mode_str` (450-456).
- `rg -n 'mod tests|cfg\(test' percussion_settings.rs percussion_backend.rs` → none, so no test covers the settings path either.
- `git -C "$R" log --format='%ad %h %s' --date=short -- crates/manifold-core/src/percussion_settings.rs | tail -1` → 2026-03-19 004688e6e, the Unity port.
- Lines: percussion_settings.rs 15-666 (652) plus 762-774, and backend 73-110, 284-385, 450-456. Add the `Some` arms in import and orchestrator, and remove engine-016's 4 lines: about 840.

### patterns-033

Claim: the rate-of-change mode has no way in, so the field and everything around it is dead.

- `rg -n 'AudioModSetRateOfChange' "$C"` → the variant (ui actions.rs:747) and its dispatch arm (app modulation.rs:228). It is never constructed.
- `rg -n '\.rate\b' "$C/manifold-ui/src"` → none, so the view-model `rate` field (state.rs:293) is never read. It is filled at app projection/inspector.rs:1460.
- `sed -n 1508,1520p "$C/manifold-ui/src/panels/param_slider_shared/builders.rs"` → the Delta button was removed from the UI (section 7.2 item 2), and the runtime field and its `condition()` arm "stay compiled for a possible future re-wire".
- `rg -n 'deny_unknown_fields' "$C/manifold-core/src/audio_mod.rs"` → none, so serde ignores a stale `rateOfChange` key once the field is gone.
- `rg -n prev_raw "$C" | wc -l` → 41 sites that only feed the rate arm.
- fallbacks.md row fallbacks-012 holds the load migration (118 lines) and hands this field to this sweep (fallbacks.md "Seen, not judged").

### patterns-034

Claim: `ObjectKnownRow.transform` is built and never read.

- `rg -n '\.transform\b' "$C/manifold-ui/src" "$C/manifold-app/src" | rg -i 'known|row|object'` → the builder only. The `project.rs` hits read `SceneVm` (nodes-scene) inside `#[cfg(test)]`, a different type.
- `sed -n 290,362p "$C/manifold-app/src/ui_bridge/projection/inspector.rs"` → the `transform_row` closure, about 70 lines, with hard-coded ranges.

### patterns-035

- `rg -n '\.material\b' "$C/manifold-ui/src/panels"` → no read of `ObjectKnownRow.material`. `material_inspector` is a different field and is read.
- `sed -n 362,442p inspector.rs` → `material_row` with 0.8, 0.0, 0.5 placeholders and the comment "Values/driven-state are P3 manifest reads … the struct only carries identity now".

### patterns-036

- `sed -n 1835,1860p "$C/manifold-ui/src/panels/scene_setup_panel.rs"` → environment: a match on the variant and `Importer.hdri_file`. Atmosphere: `matches!(vm.atmosphere, AtmosphereRowVm::None)`.
- `rg -n 'mode_is_hdri|\.intensity|\.fill\b|density|height_falloff' "$C/manifold-ui/src/panels/scene_setup_panel.rs" "$C/manifold-ui/src/panels/scene_setup_panel/"` → struct fields and tests only.
- Builder: inspector.rs:780-853 (environment) and 854-891 (atmosphere).

### patterns-037

- `rg -n 'RowModulation|ModulatedRow' "$C" -g '*.rs' -g '!**/tests*'` → definitions (scene_setup_panel.rs:217-267) and builders in inspector.rs (`mrow` 281-290, `scene_row_modulation` 1555-1578, `row_modulation_for_id` 1580-1629). No reader.
- `rg -n '\.modulation\b' "$C/manifold-ui/src"` → scene_card.rs:577, cards.rs:527, inspector.rs:33 and param_card, all on `ParamRow`/`RowMod`, not on these rows.

### patterns-038

- `rg -n 'graph_source' "$C/manifold-ui/src" "$C/manifold-app/src"` → field (scene_setup_panel.rs:323) and builder (projection/material.rs:206) only.

### patterns-039

Claim: `PlaybackEngine::get_seconds_per_beat` is masked by a trait method of the same name.

- `$SCRATCH/masked_scan.py $S/patterns_masked_candidates.tsv > $S/patterns_masked_scan.tsv` → for `get_seconds_per_beat` on `PlaybackEngine`: 0 `PlaybackEngine::`/`Self::` calls, and every `.get_seconds_per_beat(` call is on a `TimelineInputHost` receiver.
- `rg -n 'fn get_seconds_per_beat\b' "$C"` → engine.rs:2690 (inherent), ui timeline_input_host.rs:45 (trait), app input_host.rs:159 and input_handler.rs:598 (trait impls). The impls compute from the project tempo map themselves; neither calls the engine fn (read with `sed -n`).

### patterns-040

Claim: `AudioSetupPanel::claims_drag` is only called by its own tests.

- `rg -n 'claims_drag' "$C" -g '*.rs'` → audio_setup_panel.rs:2072 (inherent) and tests :2282, :2305. drag.rs:52 calls `ov.claims_drag` on `&dyn Overlay`. The other `claims_drag` definitions are `Overlay` impls.
- `rg -n 'impl Overlay for AudioSetupPanel' "$C"` → none.
- `sed -n 185,200p "$C/manifold-app/src/ui_root/events.rs"` → the dock routes through `handle_event`; `wants_immediate_drag` is live, `claims_drag` is never asked.

### patterns-041

Claim: per-knob rulings for the env knobs other sweeps listed.

- `rg -n 'env::var(_os)?\("([A-Z_0-9]+)"' "$C" -g '*.rs' -o`, classified by path → `$S/patterns_env_reads.tsv`: 174 reads, 68 distinct variables in product source.
- Dates: `git -C "$R" log -S NAME --reverse --format='%ad %h' --date=short -- crates | head -1` per name. RT_SOURCE_TRACE 2026-09-09, MODIFIER_MEMORY_MIB 2026-09-12, SSS_HISTORY_TRACE 2026-10-09, PRESET_PREWARM 2026-08-14, WARMUP_SCALE 2026-10-13.
- Docs and scripts: `rg -l NAME "$R/docs" "$R/scripts" "$R/.claude" "$R/tools"` per name.
- Bead status: `jq -r --arg ID "$BEAD" 'select(.id==$ID) | .status' "$R/.beads/issues.jsonl"` per bead. woji, rinn, l7t4 and q7na are open; iadf, olp9 and k1n2w are closed (names in patterns.md).
- Demucs: `rg -n 'MANIFOLD_DEMUCS_\w+' -o "$C"` → nine names, all in percussion_backend.rs. `build_settings_arguments` never runs (patterns-032), so `append_demucs_defaults` (386-409) is the live path, and env overrides the constants there.

## Rulings and kept items

- app-ui-061, the shader branch: `sed -n 80,105p "$C/manifold-ui-paint/src/ui_renderer.rs"` → `if border_w < 0.0` selects the shadow path inside the fragment shader for every UI quad. `rg -n 'fn draw_shadow' "$C/manifold-ui-paint"` → :1000, reached only from the two gated sites. `git -C "$R" log -S SHADOWS_ENABLED --reverse --format='%ad %h' --date=short | head -1` → 2026-07-14 7bac599544.
- engine-049: `rg -n 'bench-timing' "$R/scripts" "$R/.config" "$R/.github" 2>/dev/null` → only the feature matrix, which builds without running.
- Export flush (Bugs, not slop): `rg -n 'flush_all_background_work' "$C"` → compositor.rs:277 (trait), layer_compositor.rs:3745 (loops `plugin_warmups`), content_pipeline.rs:4152, content_export.rs:1127. `rg -n 'plugin_warmups' "$C"` → filled only from `prewarm_all`, which returns an empty Vec (graph-021).
- Top-level next-id helpers: `rg -n 'fn next_node_id|\.iter\(\)\.map\(\|n\| n\.id' "$C" -g '!**/tests/**'` → fragment_cuts.rs:117, liquid.rs:204, blob_bounds.rs:184, cards.rs:1725, group_edit.rs:174 and :307 scan `graph.nodes` without recursion. Collision not reproduced.
- Seams: each kept trait was read at its definition for a doc or design reference (D10 and D12 of the crate split design, RAYTRACING_DESIGN D16 for `ShadowRayTracer`). Each kept registry was checked to be declared in a lower crate than its submitter, using the layering table.
- False zeros: `rg -n 'macro_rules! param_tooltips' -A12 "$C/manifold-node-engine/src/param_doc.rs"` → it expands to `inventory::submit!`. `rg -c 'param_tooltips!' "$C" -g '*.rs'` → 150.
- Masking scan: `$S/patterns_host_methods.tsv` (host-trait method names) joined against `$S/patterns_impl_fns.txt` (inherent pub fns) gives `patterns_masked_candidates.tsv`; `masked_scan.py` scores each. Rows with an assoc call or a concrete receiver were kept. Rows with no call were read by hand: patterns-039 and patterns-040, plus the three in engine-026.
