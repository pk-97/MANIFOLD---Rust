# patterns sweep

- **Sweep:** `patterns` (census sections 3.9 to 3.11: switches and knobs, traits and registries, duplicates), brief 11.9, plus two extra steps: write-only view models, and trait-method masking across the UI host traits.
- **Model:** Opus (claude-opus-5-5), no helpers.
- **Main commit:** 37a9c180a (screens). HEAD 30aaa08fa has no change under `crates/` (`git diff --stat 37a9c180a HEAD -- crates` is empty).
- **Raw files used:** `raw/traits.tsv`, `raw/registries.tsv`, `raw/free_fn_dupes.tsv`, `raw/features.tsv`, `raw/env_knobs.txt`, `raw/pub_refs_v2/`, the corpus index (loading check only).
- **Scratch outputs** (in `/tmp/simplification_census/`): `patterns_impl_fns.txt`, `patterns_trait_fns.txt`, `patterns_prod_dupes.tsv`, `patterns_node_walkers.txt`, `patterns_env_reads.tsv`, `patterns_host_methods.tsv`, `patterns_masked_candidates.tsv`, `patterns_masked_scan.tsv`. One scratchpad script, `masked_scan.py`, counts calls to an inherent fn that shares its name with a host-trait method. It skips trait impls, comments and the definition line.
- **Search before build:** the census tools (`census_refs.py`, the `raw/` screens) were reused. The masking scan is new, because nothing in `scripts/` counts calls by receiver type. The engine-model sweep did the same check by hand for `service.rs` only.
- **Denied commands:** none.
- **Layering rule used:** RENDERER_CRATE_SPLIT_DESIGN D4 (no facades or shims) and D12 (layering is the cargo metadata test). A registry declared in a lower crate and filled by a higher one is a required seam. A trait with one implementor is a seam only when that table or a named backend design needs it.

## In one paragraph

The code carries three kinds of weight that no feature needs. First, interfaces ported from Unity that were never needed. Thirteen C# interfaces became traits; four have no implementor, two have one, and two have one plus a test mock. Second, one question answered through several channels. "What params does node X have" goes four ways, and two of them are pinned equal by a test. The bundled-preset catalog goes through five registries that all point at one function. Third, things switched off or left behind instead of deleted. Relight is off behind a const, about 1,800 lines, since 2026-07-18. Diagnostics stayed in after their bugs closed. A Unity settings object (about 840 lines) has never been built, because the app passes `None`. Scene-panel view models are filled on every projection and never read. Helpers keep being rewritten per file because the type they serve has no such method: seven copies of `max_node_id`, thirty of `output_resource`, 73 `frame_time` fixtures, and 131 hand-written recursions into `group.nodes`. Removable: A about 220 lines, B about 2,900, C about 1,300 plus the knob list.

## Candidates

| id | location | category | proof | risk | grade | action | lines |
|---|---|---|---|---|---|---|---|
| patterns-001 | `crates/manifold-editing/src/command.rs:140` | 3.10 | trait `LayerLifecycleCallbacks` (port of C# `ILayerLifecycleCallbacks`): 0 impls, 0 `dyn`, 0 bounds, 0 uses outside its definition | none | A | delete | 8 |
| patterns-002 | `crates/manifold-core/src/selection.rs:129`, `lib.rs:121` | 3.10 | trait `SelectionRegionTarget` (port of Unity `ISelectionRegionTarget`): 0 impls, 0 `dyn`, 0 bounds; the re-export is its only other hit | none | A | delete | 14 |
| patterns-003 | `crates/manifold-editing/src/service.rs:17` | 3.10 | trait `EditingHost`, 7 methods, 0 impls; its one user is `get_current_grid_step` (service.rs:1484), already dead in engine-026 | none | A | delete with engine-026 | 10 |
| patterns-004 | `crates/manifold-core/src/effect_registration.rs:89-156`, `preset_definition_registry.rs:147-163, 887-…` | 3.10 | `EffectAliasMetadata` and `EffectValueAliasMetadata`: 0 submissions in crates. Their loop only attaches tables to inventory `EffectMetadata` entries, which only tests submit, and the JSON load overwrites those entries for the same id. Test `all_default_effect_defs_have_empty_alias_table` pins the always-empty state. Gap: `scripts/audit_rename.py` still writes these submissions, so the tool must switch to JSON `param_aliases` in the same landing | prep | B | delete (both registries, the loop, the test; fix the rename tool) | 90 |
| patterns-005 | `crates/manifold-core/src/effect_registration.rs` (`EffectMetadata` collect), 13 submissions | 3.10 | every `inventory::submit!` of `EffectMetadata` sits in a `#[cfg(test)]` module or a test file (editing automation.rs:760, playback modulation.rs:1550/1562/3224, automation.rs:934, core preset_definition_registry.rs:681-738, `effects/test_support.rs`, editing `tests/command_roundtrips.rs`); the code says "the inventory loop above only fires for tests". Production effects come from JSON | dev | C | move-to-fixtures: a test-only registry in product code; Peter decides whether the test path should use JSON fixtures | ~30 |
| patterns-006 | `crates/manifold-compositor/src/compositor.rs:148-446` | 3.10 | trait `Compositor`: 299 lines, 41 methods, 31 of them with default bodies. Its one impl `LayerCompositor` (layer_compositor.rs:3043-3842) overrides all 41, so no default body ever runs. The app holds `Box<dyn Compositor>` (content_pipeline.rs:747, 1145); app→compositor is a plain dependency edge, not a seam in the layering table | stage | B | inline (inherent methods on `LayerCompositor`, concrete field in the app) | ~200 |
| patterns-007 | `crates/manifold-playback/src/process_runner.rs:6` | 3.10 | trait `ExternalProcessRunner`: one `Self: Sized` assoc fn, one impl `ProcessRunnerImpl`; every caller names `ProcessRunnerImpl::run_async` | prep | A | inline | 8 |
| patterns-008 | `crates/manifold-playback/src/percussion_parser.rs:14` | 3.10 | trait `PercussionAnalysisParser` (port of Unity `IPercussionAnalysisParser`): one unit-struct impl, held as a concrete field (`percussion_orchestrator.rs:142`) | prep | B | inline | 6 |
| patterns-009 | `crates/manifold-core/src/effects/mod.rs:73` | 3.10 | trait `ParamSource` (port of Unity `IParamSource`); its doc says "Both PresetInstance and generator params implement this", but only `PresetInstance` does (instance.rs:1597); 0 `dyn`, 0 bounds. Gap: compile not run, method names may collide with inherent fns | prep | B | inline | 20 |
| patterns-010 | `crates/manifold-core/src/percussion_binding.rs:14`, `percussion_analysis.rs:761`, `crates/manifold-playback/src/percussion_planner.rs:17-31, 195-213` | 3.10 | `PercussionBindingResolver` and `BeatTimeConverter` are held as `Box<dyn>` in `PercussionTimelinePlanner`; the only second implementors are the test mocks `PanicConverter` and `KickResolver` | prep | B | simplify (concrete types; tests build real ones) | ~30 |
| patterns-011 | `crates/manifold-native/src/region_detector.rs:45`, `blob_detector.rs:8`, `depth_estimator.rs:21` | 3.10 | `RegionDetector`, `BlobDetector`, `DepthEstimator`: one FFI impl each; every caller names the `Ffi*` type (nodes-image blob_detect_ffi.rs:188, detect_regions.rs:305, depth_estimate_midas.rs:133, optical_flow_estimate.rs:194, person_segment.rs:148). The plugins load through `libloading`, so a second platform needs no second impl | stage | B | inline | ~60 |
| patterns-012 | `crates/manifold-audio/src/trained/frames/mod.rs:26` | 3.10 | trait `FrameSource`: one impl `FrameBank` (:61), used as `&dyn FrameSource` at trained/net/mod.rs:137, 150 | prep | B | inline | ~10 |
| patterns-013 | `crates/manifold-gpu/src/metal/denoiser.rs:81` | 3.10 | trait `GpuDenoiser` says it is portable (NRD, ReLAX) but its signature takes `ProtocolObject<dyn MTLCommandBuffer>`; one impl `MetalFxDenoisedScaler`, the legacy path of BUG-ns5k (Benchmark legacy-path MetalFX denoised scaler); one user, nodes-scene denoiser.rs:21 | stage | C | ask: keep as the Vulkan seam (then make the signature portable) or inline | ~20 |
| patterns-014 | `crates/manifold-node-engine/src/load/preset_loader.rs:414-424`, tests :1006, :1012 | 3.10 | `select_assets_root` asserts "at most one" `PresetAssetsRoot` submission and is widened through `testkit_visible!` for two `#[should_panic]` tests. The one submission is a single line in nodes `lib.rs:11`, so the guarded state cannot happen | prep | B | simplify (read the one submission; drop the assert, the widening and both tests) | 25 |
| patterns-015 | seven hand-rolled single-provider lookups: node-engine `mesh_asset_source.rs:13`, `scene/exposure_source.rs:11`, `load/augmentation.rs:20`, `resource_allocation.rs:31`, `generator_provider.rs:26` (`exactly_one`), `runtime/plugin_prewarm.rs:65`, core `param_lookup.rs:51` | 3.11 | the same `inventory::iter().next()` plus "more than one" check written seven times | prep | B | merge (one generic helper in core, the lowest crate with such a registry) | 15 |
| patterns-016 | core `scene_exposure.rs:50` (`NodeParamProvider`), core `param_lookup.rs:37` (`NodeParamSource`), node-engine `scene/exposure_source.rs:16` (`SceneExposureSource`), nodes-scene `scene_exposure.rs:91-95, 465-494` | 3.11 | one concept, "what params does node X have". All three are filled by nodes-scene from `node_params(type_id, with_hidden)` (scene_exposure.rs:98-157); they differ only in whether hidden params are filtered. `SceneExposureSource` has two readers (nodes-water presets/gpu_flip.rs:827, 829), which pick params by name. A parity test (scene_exposure.rs:486-494) exists only to prove two channels agree. `NodeParamProvider` has one product impl, a zero-sized type that nodes-scene passes to core `migrate_scene_exposures` (core scene_exposure.rs:378); its doc says editing commands store it, which no code does. The layering allows core `NodeParamSource` to stay (core is the lowest crate that asks) | prep | C | merge: keep `NodeParamSource`, filter hidden params at the caller; delete `SceneExposureSource`, its submission and parity test, and the `NodeParamProvider` trait and impl (core takes the registry or a fn; its five test providers become fns) | ~50 |
| patterns-017 | node-engine `PresetCatalogSource` (bundled_presets.rs:42), core effect `PresetSource`, generator `PresetSource`, `LoadedSceneModifierPresetSource` (read at preset_definition_registry.rs:561), `TemplateGraphSource` (bundled_presets.rs:968) | 3.11 | five registries, all filled by manifold-nodes from the bundled preset tables; `def` and `graph_for` both resolve to the same `bundled_preset_def`. All the types they carry are core types, so one core registry serves every reader. Gap: compile not run; the node-engine readers of `PresetCatalogSource` were not traced one by one | stage | C | merge (one core registry) | ~60 |
| patterns-018 | `max_node_id_recursive` ×7: core scene_object_migration.rs:148, phong_migration.rs:307; nodes-scene gltf_import/merge/scans.rs:12, gltf_import/migration.rs:325, gltf_import/upgrade.rs:381; editing commands/graph/modifiers.rs:87, commands/graph/scene.rs:1949 | 3.11 | the same nested max-id walk in three crates under four names; the owner the layering allows is core, on `EffectGraphDef` | prep | B | merge (one `EffectGraphDef` method) | ~65 |
| patterns-019 | `find_node` copies: nodes-scene scene_modifier_legacy_migration/fragment_masks.rs:152, nodes-scene scene_items.rs:206, editing scene/clone_values.rs:158, node-engine load/expand/bindings.rs:445, app fluid_domain_edit.rs:150 | 3.11 | core `EffectGraphDef::find_node`/`find_node_mut` (effect_graph_def.rs:311) exist, but they do not search inside groups, so each crate wrote a nested copy. Gap: some copies match by stable id, not node id; the merged method must take both | prep | B | merge (nested search on `EffectGraphDef`) | ~60 |
| patterns-020 | `fn output_resource(` ×30: compositor preset_thumbnail.rs:439, node-engine runtime/core.rs:17, and about 27 per-primitive test modules | 3.11 | one lookup on `ExecutionPlan` written 30 times, because the plan has no such method | dev | B | merge (`ExecutionPlan::output_resource` in node-engine) | ~300 |
| patterns-021 | `fn frame_time(` ×73, mostly test fixtures | 3.11 | `FrameTime` (node-engine exec/effect_node.rs:151) has no constructor, so every test module writes its own. Each copy is cheap (census trap 3.11), but 73 copies is the scale signal | dev | B | merge (one testkit constructor) | ~500 |
| patterns-022 | 131 hand-written recursions `name(&group.nodes…)` in product source (app 29, nodes-scene 30, core 21, editing 21, node-engine 11, nodes-water 8, io 5, nodes 4, ui 2); `nodes_contain` ×3 (fragment_cuts.rs:157, phong_migration.rs:69, gltf upgrade/project.rs:162) | 3.11 | core has no nested-node walker, so every graph pass rolls its own recursion | prep | C | simplify (`walk`, `walk_mut`, `any` on `EffectGraphDef`, then fold call sites a crate at a time); estimate only | ~300 (est.) |
| patterns-023 | `crates/manifold-physics/src/coupled_motion.rs`, `interaction.rs` | 3.11 | `dot`, `cross`, `add`, `scale`, `length`, `normalize`, `lerp` written twice inside one crate (fluids, node-engine and sdf.rs keep their own on purpose: separate solvers, f64 in sdf) | none | B | merge (within physics only) | ~10 |
| patterns-024 | `crates/manifold-foundation/src/feature_flags.rs:16` and its readers outside graph scope | 3.9 | `RELIGHT_FEATURE_ENABLED = false` since 2026-07-18 (50a9da35d, "renders poorly and is not ready for the stage"). It keeps alive about 1,800 lines across 84 files; graph-026 counts about 900 of them. This row is the rest: core effects/relight.rs (152), `SetRelightParamCommand` in editing, the ui param_card relight branches (render, mod, state, routing), compositor generator_renderer, app ui_translate and scrub. 0 of 182 projects contain "relight". engine-046 is the switch itself | stage | C | ask: delete with graph-026, or turn it back on | ~900 (est.) |
| patterns-025 | `crates/manifold-app/src/frame/present.rs:9-52, 812-840`; `crates/manifold-ui-paint/src/ui_renderer.rs:512, 686, 1163-1194, 1782, 1795, 2242-2252`; `ui_cache_manager.rs:241` | 3.9 | `MANIFOLD_BUG060_DUMP` and `MANIFOLD_BUG060_TRACE`, from graph-027 and app-ui-064. The code says "Remove with BUG-060". BUG-060 (inspector-footer-overpaint) was fixed on 2026-07-10 and re-confirmed on 2026-07-12. The trace adds a field and a per-batch call in the UI renderer | none | A | delete | ~130 |
| patterns-026 | `crates/manifold-nodes-scene/src/node_graph/primitives/render_scene.rs:5986-5990, 9656-9684` (probe_t0 field) | 3.9 | `MANIFOLD_DENOISE_PROBE`, added for BUG-iadf (RT denoise 1:1 mode) on 2026-08-08; BUG-iadf closed the same day. It is read every frame | none | A | delete | ~30 |
| patterns-027 | `render_scene.rs:5637-5680` | 3.9 | `MANIFOLD_DENOISE_SKIP`: an ablation branch for BUG-iadf (closed), `env::var_os` on every frame. Gap: `docs/rt_dn_i_sweep_results.md` names it; that doc is a closed result | none | B | delete (and the stale `// PROBE:` comments at :5545, :5726) | ~30 |
| patterns-028 | `render_scene.rs:6412` | 3.9 | `MANIFOLD_PROBE`: a camera print added 2026-08-01, no doc, read every frame | none | A | delete | 6 |
| patterns-029 | `crates/manifold-gpu/src/metal/raytrace/accel.rs:477, 558` | 3.9 | `MANIFOLD_PROBE_RT_ACCEL`: a print on every acceleration-structure build; no doc, set by nothing | none | A | delete | 8 |
| patterns-030 | `render_scene.rs:3605` | 3.9 | `MANIFOLD_DISABLE_EMISSIVE_SAMPLER`, labelled "RS-C test-only gate", is read every frame in product code. Its one user is leg 2 of the gpu proof `rt_emissive_direct.rs` | dev | B | move-to-fixtures (the proof sets a field or param, not an env var) | ~10 |
| patterns-031 | `crates/manifold-app/src/content_pipeline.rs:1193` | 3.9 | `MANIFOLD_OCCLUSION_RENDER_SKIP`, added 2026-07-18: an off-switch for a skip that is on by default; no doc, set by nothing | prep | B | simplify (keep the skip, drop the knob) | ~10 |
| patterns-032 | `crates/manifold-core/src/percussion_settings.rs:15-666, 762-774`; `crates/manifold-playback/src/percussion_backend.rs:73-110, 284-385, 450-456`; `percussion_import.rs:67-75`; `percussion_orchestrator.rs:138, 306-320` | 3.9 | `PercussionPipelineSettings`, a port of the Unity ScriptableObject with 12 nested settings structs, is never constructed. Both app call sites pass `None` (app.rs:1894, headless_harness.rs:81), and so does the only test (percussion_orchestrator.rs:1380). Every `Some` branch is unreachable, including `build_invocation_with_settings`, `build_settings_arguments`, `serialize_to_detection_config_json` and `build_import_options`. No test touches any of it. engine-016 (`reset_to_defaults`) is a 4-line part of this. Gap: whether Peter relied on tuned values in the Unity asset; the Rust app has passed `None` since the port on 2026-03-19 | prep | B | delete (the settings object and every `Some` branch; `Option` params become plain calls) | ~840 |
| patterns-033 | `crates/manifold-core/src/audio_mod.rs:335, 380-385`; `crates/manifold-ui/src/panels/actions.rs:735-747`; `crates/manifold-app/src/ui_bridge/dispatch/modulation.rs:228-262`; `crates/manifold-ui/src/panels/param_slider_shared/state.rs:293-294`; app projection/inspector.rs:1460 | 3.9 | `AudioModShape::rate_of_change`. The UI button went on 2026-07-11 (Peter: "not very useful and adds a lot of clutter"). The field, its `condition()` arm, the action variant `AudioModSetRateOfChange` (never constructed) and its dispatch arm stayed "for a possible future re-wire". The view-model field `rate` is never read. Deleting the field also does fallbacks-012's job: serde drops an unknown `rateOfChange` key on load (no `deny_unknown_fields`). Cascade: the `prev_raw` state (41 sites) only feeds the rate arm | prep | B | delete (with fallbacks-012) | ~90 |
| patterns-034 | `crates/manifold-ui/src/panels/scene_setup_panel.rs:296-300, 387`; `crates/manifold-app/src/ui_bridge/projection/inspector.rs:292-361` | 3.1 | `ObjectKnownRow.transform: Option<Box<TransformRowVm>>` has no production reader. The `project.rs` hits read the nodes-scene `SceneVm`, a different type, and only in tests. The `transform_row` closure builds it on every projection, with hard-coded ranges | prep | B | delete | ~85 |
| patterns-035 | `scene_setup_panel.rs:307-312, 388`; `inspector.rs:362-441` | 3.1 | `ObjectKnownRow.material: ObjectMaterialVm` has no production reader. `material_row` builds it on every projection, with placeholder values 0.8, 0.0 and 0.5 ("the struct only carries identity now") | prep | B | delete | ~90 |
| patterns-036 | `scene_setup_panel.rs:270-281` (`EnvironmentRowVm`), `AtmosphereRowVm::Wired`; `inspector.rs:780-891` | 3.1 | the panel reads only the environment variant and `Importer.hdri_file` (scene_setup_panel.rs:1837-1855), and only `matches!(…, AtmosphereRowVm::None)` for atmosphere (:1857). `mode_is_hdri`, `intensity`, `fill`, `density` and `height_falloff` are built and never read | prep | B | simplify (variants keep only what is read) | ~80 |
| patterns-037 | `scene_setup_panel.rs:217-267` (`RowModulation`, `ModulatedRow`); `inspector.rs:281-290` (`mrow`), :1555-1578 (`scene_row_modulation`), :1580-1629 (`row_modulation_for_id`) | 3.1 | once app-ui-065, app-ui-066, patterns-034 and patterns-036 land, nothing builds these. No production code reads them today: the `.modulation` hits in scene_card.rs:577, cards.rs:527, inspector.rs:33 and param_card belong to the different `ParamRow`/`RowMod` type. Today every scene projection computes a boxed modulation lookup per row that nobody reads | prep | B | delete | ~150 |
| patterns-038 | `scene_setup_panel.rs:323`; app projection/material.rs:206 | 3.1 | `MaterialTextureInfo.graph_source` is built and never read | none | B | delete | 5 |
| patterns-039 | `crates/manifold-playback/src/engine.rs:2690` | 3.1 | `PlaybackEngine::get_seconds_per_beat`: 0 callers. Every text hit is a `TimelineInputHost` method of the same name (ui timeline_input_host.rs:45, app input_host.rs:159, input_handler.rs:598) | none | A | delete | 4 |
| patterns-040 | `crates/manifold-ui/src/panels/audio_setup_panel.rs:2072`, tests :2282, :2305 | 3.1 | `AudioSetupPanel::claims_drag`: only its own tests call it. The doc says "Read by UIRoot::resolve_drag_owner", but drag.rs:52 calls `claims_drag` on `&dyn Overlay`, and `AudioSetupPanel` is not an `Overlay`. The tests assert that a missed grab inside the panel is claimed; production never claims it. This is a missing wire or a dead fn | prep | B | ask: wire it into `resolve_drag_owner`, or delete it and its tests | ~40 |
| patterns-041 | environment knobs from graph-027, app-ui-064, engine-047, engine-048 and engine-runtime-036 to 038 not settled above (table below) | 3.9 | rulings per knob; none is set by a repo script except where noted | none | C | ask (keep list and ask list below) | 0 |

Totals, not counting rows owned elsewhere:
- A: 9 rows (001, 002, 003, 007, 025, 026, 028, 029, 039), about 220 lines.
- B: 26 rows, about 2,900 lines. Of those, 840 is the never-built settings object (032), about 800 are test and fixture copies (020, 021), and about 410 are scene-panel view models (034 to 038).
- C: 6 rows (005, 013, 022, 024, 040, 041), about 1,300 lines (estimate) plus the knob list.

## Rulings on rows other sweeps handed here

- **engine-046 and graph-026 (relight):** C, ask. The switch, the graph-scope files and patterns-024 come as one decision: delete all of it, or turn it back on. Half-deleting leaves a switch guarding nothing.
- **app-ui-061 (`SHADOWS_ENABLED = false`, ui color.rs:148, since 2026-07-14):** C, ask, lines stay with app-ui-061. It keeps alive more than its two gated sites (ui-paint clip_draw.rs:65-85, app tree_passes.rs:102-120). It also keeps `draw_shadow` (ui_renderer.rs:1000-1028), six shadow constants, and a fragment-shader branch, `if border_w < 0.0` (ui_renderer.rs:86-102). That branch is evaluated for every UI pixel on every frame, even though no shadow quad is ever drawn.
- **engine-049 (`bench-timing`):** B, delete the two wall-clock benches (core params.rs:647, audio_trigger.rs:378), or run them in the nightly. No gate passes the feature; the feature matrix only builds it. That makes it an ignored test under another name. Lines stay with engine-049.
- **engine-047 (`MANIFOLD_DEVICE_LOG`), engine-048 (`MANIFOLD_WORK_TRACE`):** keep. Both are general diagnostics, read once, and documented. WORK_TRACE is a live-run gate in UI_INVALIDATION_DESIGN.md.
- **engine-runtime-036 (nine `MANIFOLD_DEMUCS_*`):** the "five overlap settings" concern is moot. The settings never exist (patterns-032), so the env knobs are the only Demucs configuration. Ask: keep them if Peter tunes Demucs this way (`tools/audio_analysis/manifold_audio/external_tools.py` also reads them), or fix the values as constants.
- **engine-runtime-037 (`MANIFOLD_LOG_REBUILD_REASON`, `MANIFOLD_ODF_DEBUG`):** keep. Seven reads across four crates, named in two design docs. ODF_DEBUG is read by a tool (`odf_attribution.py`).
- **engine-runtime-038 (tool paths):** keep. They are paths, not knobs.
- **graph-021 (PluginPrewarm, PostProcessEffect):** confirmed zeros; lines stay there. See "Bugs, not slop" for what the empty export flush means.
- **graph-022 (`SourceImplementationIdentity`):** confirmed: no consumer.
- **engine-runtime-003 (`PlaybackNotifier`):** confirmed: no implementor.
- **fallbacks-012 (rate-of-change clear):** decided by patterns-033. Delete the field and the migration goes with it.
- **BUG-bjk6 (Dead LensRowVm):** widens again. patterns-034 to 038 are the same class as app-ui-065 and app-ui-066.

### Environment knobs (patterns-041)

Of the 68 variables read in product source (`patterns_env_reads.tsv`):
- **Already rowed for deletion above:** BUG060_DUMP, BUG060_TRACE, DENOISE_PROBE, DENOISE_SKIP, PROBE, PROBE_RT_ACCEL, DISABLE_EMISSIVE_SAMPLER, OCCLUSION_RENDER_SKIP.
- **Keep:**
  - Paths: FFMPEG_PATH, FFPROBE_PATH, PYTHON_PATH, HOME, XDG_CONFIG_HOME, APPDATA, UI_SOCKET, GPU_QUEUE_DIR.
  - Documented diagnostics: RENDER_TRACE, LOG_HARVEST, LOG_REBUILD_REASON, UI_FRAME_PROFILE, GPU_DIAGNOSTICS, ENCODE_REPLAY, INPUT_TRACE, AUDIO_TRACE, LOG_CHAIN_STATS, POOL_STATS, WORK_TRACE, DEVICE_LOG, ODF_DEBUG.
  - Fusion and freeze off-switches named in FREEZE_COMPILER_MAP: FREEZE, CHAIN_FUSION, FREEZE_Q16, FEEDBACK_PINGPONG.
  - Knobs for open bugs: MTL4FX_DENOISER and MTL4FX_TEMPORAL, for BUG-woji (MTL4FX availability probe) and BUG-rinn (re-test MTL4 temporal scaler).
  - The budget override MODIFIER_MEMORY_MIB (named in SCENE_MODIFIER_VALIDATION_PLAN).
  - The defer-drop set with app-ui-013, while BUG-l7t4 (audition-grid page fault) is open; the same goes for RETIRE_MIN_DRAINS.
  - Dev-probe binaries' own variables (CORRIDOR_*, RT_HELD_OUT_*, RT_CAPTURE*, PROBE_TORN, GPU_FLIP_DEMO_DIR), which go with their modules in app-ui-064.
- **Ask, each a one-off trace with no doc or a closed bug:**

  | knob | added | note |
  |---|---|---|
  | SSS_HISTORY_TRACE | 2026-10-09 | no doc |
  | WGSL_COMPUTE_TRACE | 2026-05-27 | no doc |
  | REGION_DEBUG | 2026-09-17 | BUG-q7na (relax_surface_mesh cannot head a fused region) is open, so keep while it is |
  | RT_SOURCE_TRACE | 2026-09-09 | read through a OnceLock, named in SCENE_MODIFIER_RT_DESIGN |
  | GROUP_CANVAS_LOG | | documented |
  | GLTFEDITOR_DEBUG | 2026-07-10 | |
  | GPU_ALLOC_LOG and GPU_ALLOC_BT | | BUG-olp9 (30GB process leak) closed 2026-08-25 |
  | PRESET_PREWARM | 2026-08-14 | no doc |
  | WARMUP_SCALE | 2026-10-13 | BUG-k1n2w (CDM Kill timeout left exports hanging) closed 2026-10-13 |

  Cheapest rule: a knob named for a closed bug goes; a knob with no doc goes unless Peter uses it.

## Bugs, not slop

- **Export's frame-perfect flush does nothing.**
  - content_export.rs:1123-1127 calls `flush_all_background_work` "so export is frame-perfect" for "blob tracking, wireframe depth, depth-of-field" workers.
  - The flush loops `plugin_warmups`, which is always empty (graph-021).
  - Whether the decomposed atoms that replaced those workers (`node.blob_tracker`, `depth_estimate_midas`) are deterministic in export is unverified.
  - Oracle: export one clip that uses blob tracking twice and diff the frames.
- **The audio dock's drag claim is not wired** (patterns-040). A test asserts behaviour that production does not have.
- **Wasted work on every scene projection** (patterns-034 to 037). Each projection builds transform, material, environment and modulation view models nobody reads, including one boxed modulation lookup per row.
- **Env reads on the render hot path.**
  - `render_scene` calls `env::var_os` or `env::var` on every frame for DENOISE_SKIP, DENOISE_PROBE, PROBE and DISABLE_EMISSIVE_SAMPLER.
  - Each read takes the process environment lock. A set value also allocates.
  - patterns-026 to 028 and 030 remove them.
- **Possible id collision in graph edits.**
  - Several next-id helpers scan only the top level of a graph: `fragment_cuts.rs:117` `next_node_id`, `liquid.rs:204`, `blob_bounds.rs:184`, `cards.rs:1725`, `group_edit.rs:174` and `:307`.
  - Seven other places scan nested groups (patterns-018).
  - If a graph has a group, a top-level-only scan can hand out an id a nested node already holds.
  - Unverified. Oracle: a CPU test that adds a node to a graph whose highest id sits inside a group.
- **Docs that describe code that is not there.**
  - `NodeParamProvider`'s doc says editing commands store it.
  - `ParamSource`'s doc says generator params implement it.
  - `GpuDenoiser` says it is portable.
  - `claims_drag` says `resolve_drag_owner` reads it.
  - Each goes with its row.
- **Settings that were never wired** (patterns-032). The import never uses the Unity percussion settings. If any of their defaults differ from `create_default`, imports have used the other values since the port. Which values are right is Peter's call only if he notices a difference.

## Kept seams (read and kept)

- **One-implementor traits that are seams:**
  - `AsAny` (blanket impl, D10).
  - `AnyState`, `ErasedValues`, `ErasedWrites` (private type erasure).
  - `EffectGraphDefExt`, `WaterRuntimeExt` (extension traits).
  - `RuntimeExtension` (hub to water).
  - `ViewportPass` (D10).
  - `NodePairBehavior` (water).
  - `TimelineInputHost`, `TimelineEditingHost` (the ui→app seam the crate rule requires).
  - `ShadowRayTracer` (the Vulkan backend seam named in RAYTRACING_DESIGN D16).
  - `GeneratorRendererTestkit`, `SlotStorage` (test infrastructure for GPU code).
  - `Timestamped` (the physics engine API).
  - `SourceScanner` (private hardware boundary over midir, with a test mock).
- **Traits with two or more real impls:** `EffectContainer`, `TriggerPulseSink`, `AudioDeviceDirectory`, `CaptureBackend`, `EventDetector`, `SyncSource`, `ClipRenderer`, `SyncTarget`, `SyncArbiterTarget`, `LiveClipHost`.
- **One-submission registries, all lower-crate declarations filled by a higher crate (required by D12):**
  - `GeneratorProvider`
  - `GraphInstantiationHook`
  - `MeshAssetSource`
  - `PipelinePrewarm`
  - `PresetAssetsRoot`
  - `RuntimeRegistration`
  - `ArrayScratch`
  - `NodeParamSource`
  - `RelightAugmentation` (a seam, but it only serves relight: patterns-024)
  - the five catalog registries are merged by patterns-017, not deleted.
- **False zeros in `raw/registries.tsv`:**
  - `ParamDoc` is fed by `param_tooltips!` (150 uses; read by catalog_gen.rs:35 and app ui_translate.rs:709).
  - `NodeDescriptor` and `PrimitiveFactory` are fed by `primitive!` and `hand_descriptor!`.
  - `GeneratorAliasMetadata` has 10 submissions and is live.
- **Masking scan, read and kept:**
  - Host traits scanned: `TimelineInputHost` (93 methods), `TimelineEditingHost` (48), `LiveClipHost` (14), `SyncTarget`, `SyncArbiterTarget`, `HitTargets`, `TextMeasure`, `Panel`, `Overlay`.
  - Every other inherent same-name fn has a call by type or by a concrete receiver: `duplicate_clip_to`, `layer_id_at_index`, `zoom_to_fit`, `selection_count`, `toggle_automation_mode_visible`, `has_region`, `wants_immediate_drag`.
  - The three dead `EditingService` fns are engine-026.

## The over-engineering patterns behind these

Peter's framing: "Everything is over engineered... fallbacks and weird states that then need pointless tests or logic and overly complex solutions." These are the patterns this sweep saw; the fallbacks sweep's seven (migrations, no-op instead of reject, defaults for impossible states, tests that pin fallbacks) are not repeated.

- **Port the interface, not the need.**
  - Example: thirteen C# interfaces became Rust traits. Four have no implementor (`LayerLifecycleCallbacks`, `SelectionRegionTarget`, `PlaybackNotifier`, `EditingHost`). A Unity ScriptableObject became 650 lines of settings that nothing builds (patterns-032).
  - Rule: a trait lands with its second implementor or a named layering seam; a ported type lands with its first production caller.
- **Default bodies for an implementor that does not exist.**
  - Example: `Compositor` has 31 default bodies; its one impl overrides all 41 methods.
  - Rule: one implementor means plain inherent methods.
- **A guard over an empty list.**
  - Example: the PluginPrewarm "forward guard" test loops over `[PresetTypeId; 0]`; `all_default_effect_defs_have_empty_alias_table` asserts a registry stays empty.
  - Rule: when a registry's last submission goes, the registry and its tests go in the same landing.
- **Switch it off instead of deciding.**
  - Example: relight has been off behind a const since 2026-07-18, about 1,800 lines. The shadow switch keeps a per-pixel shader branch. `rate_of_change` stayed "for a possible future re-wire" after its button went.
  - Rule: turning a feature off and deleting it are the same landing, unless Peter names a date to turn it back on.
- **One question, many channels.**
  - Example: node params reach readers four ways, with a test to prove two of them agree. The bundled catalog goes through five registries that resolve to one function.
  - Rule: one registry per question, in the lowest crate that asks it. A parity test between two channels means one of them should go.
- **The helper lives in every file except on the type.**
  - Example: `max_node_id` ×7, nested `find_node` ×5, `output_resource` ×30, `frame_time` ×73, and 131 recursions into `group.nodes`.
  - Rule: a helper over a type goes on the type. Before writing one, `rg` the concept in the type's own module.
- **Probes left in the instrument.**
  - Example: the BUG-060 trace (fixed in July), the BUG-iadf probes (closed in August), and env reads on every render frame.
  - Rule: a probe named for a bug is deleted in the landing that closes the bug.
- **A new path lands and the old view model keeps being filled.**
  - Example: the scene panel moved to manifest-backed rows. `TransformRowVm`, `ObjectMaterialVm` and `RowModulation` are still built on every projection, with placeholder values.
  - Rule: when a surface moves to a new source, the old fields go in the same landing.
- **A test whose subject production never calls.**
  - Example: `AudioSetupPanel::claims_drag` is tested and never called. `bench-timing` gates two tests that no gate runs.
  - Rule: a test needs a production caller for its subject and a gate that runs it.
- **Guards for a state one line controls.**
  - Example: `select_assets_root` asserts "at most one" submission. Two `#[should_panic]` tests cover a registry with one hard-coded submission.
  - Rule: do not test for a state the code cannot reach. Delete the guard, not the line.
- **An option every caller sets the same way.**
  - Example: `pipeline_settings: Option<…>` is `None` at every call site since 2026-03-19, so every `Some` branch is dead.
  - Rule: when every production caller passes the same value, the parameter is a constant; delete the other branch.

## Seen, not judged

- `.claude/hooks/ignored_tests_baseline.txt` holds 16 baselined `#[ignore]` tests. One of them (app blender_import.rs:395) is gated twice, by `#[ignore]` and by `MANIFOLD_RUN_BLENDER_TESTS`. That belongs to a tests sweep.
- `scripts/audit_rename.py` writes `EffectAliasMetadata` submissions (see patterns-004).
- nodes-scene scene_vm.rs `TransformVm` value fields (`pos_value`, `rot_value`, `scale_value`, `pos_addr`) may become test-only after patterns-034. `pos_driven` and its siblings are read at scene_vm.rs:1387.
- `AudioHopSample` is a generic whose only second type is the test `GenericHop`.
- `cached_type_id` is a OnceLock idiom copied into each primitive; small, left alone.

## Not covered

- Masked inherent methods whose `.name(` call count was above 4. The scan only checks low-count names by hand, so a dead fn with a popular name can still hide.
- `ClipRenderer`: its generic trait header was skipped by the masking scan.
- Generics with one instantiation, apart from `AudioHopSample`.
- Export determinism of the decomposed async atoms. This needs a run, which the brief forbids.
- Online-only Dropbox projects (never opened).
- No compile check was run, so cascades (unused imports, newly dead helpers) are for the deleting lane to find with `cargo check`.
