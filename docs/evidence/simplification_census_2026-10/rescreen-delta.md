# Rescreen delta: pub screen v2 against v1

Rows that are DEAD or TEST-ONLY in `raw/pub_refs_v2/` but not in `raw/pub_refs/`, minus rows the engine-model, engine-runtime, graph and water records already hold. No proof sections: each row needs the standard proof pair before it becomes a candidate.

- **Why rows moved:** v2 skips `//` comment hits and a struct or enum's own `impl X` header lines (`comment` and `impl` columns below count the hits skipped). The tool and `TOOLS.md` carry the change; v1 files are untouched.
- **Main commit:** 37a9c180a screens; `crates/` is identical at d4d34a0c2 (`git diff --stat 37a9c180a HEAD -- crates` is empty).
- **Subtracted:** by name wherever a record names the item; by file or group row where a record covers the whole set (see the end). The app-ui, content and fallbacks records are not subtracted (the brief named four records). Rows that app-ui.md already names are marked in the note column; content.md names none.
- **Edge:** a name that follows a `//` inside a string literal reads as a comment hit. Rare, and it can only add a row to this list, never hide one from it.

Total: 56 rows to look at. 19 of them are named in app-ui.md. Of 106 rows new in v2, 26 are named in a record and 24 sit under a group or file row.

## manifold-app (1)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-app/src/texture_pane.rs:70` | `bridged` | fn | TEST-ONLY | 9 | 4 | 0 | named in app-ui.md |

## manifold-audio (1)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-audio/src/analysis.rs:422` | `is_identity` | fn | TEST-ONLY | 3 | 1 | 0 | doc at analysis.rs:401 says the runtime skips on it; no code does |

## manifold-fluids (1)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-fluids/src/sheeter.rs:233` | `generate_sheet_particles` | fn | DEAD | 9 | 1 | 0 | sheeter.rs; the oracle files are water-019 |

## manifold-gpu (6)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-gpu/src/metal/defer_drop.rs:159` | `pump_deferred_drops` | fn | DEAD | 34 | 1 | 0 | named in app-ui.md |
| `crates/manifold-gpu/src/metal/device.rs:878` | `create_specialized_render_pipeline` | fn | DEAD | 15 | 1 | 0 | named in app-ui.md |
| `crates/manifold-gpu/src/metal/encoder.rs:1360` | `draw_instanced_depth_msaa_batch` | fn | DEAD | 20 | 6 | 0 | named in app-ui.md |
| `crates/manifold-gpu/src/metal/encoder.rs:2626` | `pipeline_barrier` | fn | DEAD | 3 | 1 | 0 | named in app-ui.md |
| `crates/manifold-gpu/src/metal/encoder.rs:2902` | `commit_and_wait_scheduled` | fn | DEAD | 6 | 2 | 0 | named in app-ui.md |
| `crates/manifold-gpu/src/queue.rs:81` | `configure_child` | fn | TEST-ONLY | 5 | 1 | 0 | named in app-ui.md |

## manifold-media (1)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-media/src/video_renderer.rs:403` | `pre_warm_clips` | fn | DEAD | 19 | 1 | 0 | next to engine-runtime-041 |

## manifold-node-engine (16)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-node-engine/src/bindings.rs:370` | `material_slot` | fn | DEAD | 3 | 2 | 0 |  |
| `crates/manifold-node-engine/src/bindings.rs:376` | `transform_slot` | fn | TEST-ONLY | 3 | 2 | 0 |  |
| `crates/manifold-node-engine/src/exec/effect_node.rs:285` | `NodeErrorTap` | struct | TEST-ONLY | 1 | 0 | 2 |  |
| `crates/manifold-node-engine/src/exec/metal_backend.rs:309` | `install_texture_2d` | fn | DEAD | 5 | 3 | 0 |  |
| `crates/manifold-node-engine/src/exec/metal_backend.rs:425` | `render_target_2d` | fn | TEST-ONLY | 3 | 1 | 0 |  |
| `crates/manifold-node-engine/src/freeze/classify.rs:418` | `CONVERSION_DEBT_LEDGER` | const | TEST-ONLY | 19 | 2 | 0 |  |
| `crates/manifold-node-engine/src/freeze/install.rs:1215` | `seed_segment_cache_for_test` | fn | TEST-ONLY | 11 | 1 | 0 |  |
| `crates/manifold-node-engine/src/graph.rs:278` | `node_id_by_handle` | fn | TEST-ONLY | 3 | 3 | 0 | same file as graph-018 |
| `crates/manifold-node-engine/src/graph.rs:694` | `walk_wires_into` | fn | DEAD | 9 | 1 | 0 | same file as graph-018 |
| `crates/manifold-node-engine/src/load/expand/testkit.rs:25` | `GuardFixture` | struct | TEST-ONLY | 1 | 0 | 1 |  |
| `crates/manifold-node-engine/src/load/preset_loader.rs:298` | `clear_project_presets` | fn | TEST-ONLY | 3 | 3 | 0 |  |
| `crates/manifold-node-engine/src/palette.rs:146` | `catalog_graph_def_for` | fn | TEST-ONLY | 3 | 1 | 0 |  |
| `crates/manifold-node-engine/src/primitives/standalone_pipeline.rs:58` | `standalone_2d_slots` | fn | TEST-ONLY | 10 | 1 | 0 |  |
| `crates/manifold-node-engine/src/runtime/build.rs:255` | `from_json_str` | fn | TEST-ONLY | 9 | 4 | 0 |  |
| `crates/manifold-node-engine/src/runtime/dump_sets.rs:47` | `set_dump_arrays` | fn | TEST-ONLY | 4 | 1 | 0 |  |
| `crates/manifold-node-engine/src/state_store.rs:156` | `cleanup_owner` | fn | TEST-ONLY | 10 | 2 | 0 |  |

## manifold-nodes-scene (2)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `manifold-nodes-scene/src/node_graph/gltf_import/testkit.rs:102` (deleted) | `write_synthetic_multimaterial_glb` | fn | TEST-ONLY | 90 | 4 | 0 |  |
| `crates/manifold-nodes-scene/src/node_graph/viewport_render.rs:148` | `render_viewport_frame` | fn | TEST-ONLY | 39 | 6 | 0 |  |

## manifold-nodes-water (1)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-nodes-water/src/runtime/physics_impulses.rs:92` | `scheduled_ticks` | fn | TEST-ONLY | 6 | 1 | 0 | sibling getter of `is_scheduled` (water-009) |

## manifold-physics (2)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-physics/src/stepping.rs:149` | `CompletionLedger` | struct | TEST-ONLY | 5 | 0 | 1 | same file as water-014 |
| `crates/manifold-physics/src/stepping.rs:480` | `CflPolicy` | struct | TEST-ONLY | 6 | 0 | 2 | same file as water-014 |

## manifold-playback (9)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-playback/src/audio_layer_playback.rs:612` | `drain_layer_tap` | fn | TEST-ONLY | 11 | 1 | 0 |  |
| `crates/manifold-playback/src/audio_mixdown.rs:303` | `render_export_mix` | fn | TEST-ONLY | 11 | 4 | 0 |  |
| `crates/manifold-playback/src/automation.rs:440` | `evaluate_all_automation` | fn | TEST-ONLY | 16 | 4 | 0 |  |
| `crates/manifold-playback/src/live_clip_manager.rs:245` | `clear_on_seek` | fn | TEST-ONLY | 12 | 1 | 0 | same file as engine-runtime-007 |
| `crates/manifold-playback/src/modulation.rs:1197` | `evaluate_all_audio_mods` | fn | TEST-ONLY | 17 | 6 | 0 |  |
| `crates/manifold-playback/src/modulation.rs:299` | `evaluate_all_drivers` | fn | TEST-ONLY | 38 | 3 | 0 |  |
| `crates/manifold-playback/src/modulation.rs:373` | `evaluate_all_envelopes` | fn | TEST-ONLY | 32 | 1 | 0 |  |
| `crates/manifold-playback/src/modulation/composition.rs:67` | `compose_controls` | fn | TEST-ONLY | 13 | 1 | 0 |  |
| `crates/manifold-playback/src/session_state.rs:333` | `resolve_refs` | fn | TEST-ONLY | 10 | 7 | 0 |  |

## manifold-ui-paint (1)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-ui-paint/src/native_text.rs:1233` | `clear_commands` | fn | DEAD | 5 | 1 | 0 | named in app-ui.md |

## manifold-ui (12)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-ui/src/anim.rs:303` | `FlipList` | struct | TEST-ONLY | 4 | 6 | 1 | named in app-ui.md |
| `crates/manifold-ui/src/automation_hit_tester.rs:212` | `dots_in_rect` | fn | TEST-ONLY | 14 | 1 | 0 | named in app-ui.md |
| `crates/manifold-ui/src/chrome/components.rs:209` | `state_button` | fn | DEAD | 3 | 1 | 0 | named in app-ui.md |
| `crates/manifold-ui/src/chrome/components.rs:630` | `panel_state` | fn | TEST-ONLY | 3 | 2 | 0 | named in app-ui.md |
| `crates/manifold-ui/src/color.rs:432` | `DRIVER_ACTIVE` | const | DEAD | 1 | 1 | 0 |  |
| `crates/manifold-ui/src/panels/param_card/mod.rs:1226` | `label_hit` | fn | DEAD | 27 | 1 | 0 | named in app-ui.md |
| `crates/manifold-ui/src/panels/param_card/state.rs:281` | `is_spawning` | fn | TEST-ONLY | 3 | 1 | 0 | named in app-ui.md |
| `crates/manifold-ui/src/panels/param_card/state.rs:398` | `begin_value_snapback` | fn | TEST-ONLY | 14 | 7 | 0 | named in app-ui.md |
| `crates/manifold-ui/src/panels/scene_setup_panel.rs:454` | `EnumRowValue` | struct | DEAD | 4 | 2 | 0 | named in app-ui.md |
| `crates/manifold-ui/src/slider.rs:117` | `register_intents_with_mapping` | fn | DEAD | 4 | 1 | 0 | named in app-ui.md |
| `crates/manifold-ui/src/snap.rs:15` | `MAX_SNAP_BEATS` | const | DEAD | 1 | 1 | 0 | named in app-ui.md |
| `crates/manifold-ui/src/tree.rs:763` | `all_roots_are_regions` | fn | TEST-ONLY | 5 | 2 | 0 | named in app-ui.md |

## manifold-water-gpu-flip (2)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs:207` | `set_all_tiles` | fn | TEST-ONLY | 3 | 1 | 0 | same file as water-007 |
| `crates/manifold-water-gpu-flip/src/primitives/gpu_flip_volume.rs:39` | `VolumeDrift` | struct | TEST-ONLY | 4 | 0 | 1 |  |

## manifold-water-gpu-mpm (1)

| def | name | kind | verdict | lines | comment | impl | note |
|---|---|---|---|---|---|---|---|
| `crates/manifold-water-gpu-mpm/src/matter.rs:296` | `stiffness_fitting_cap` | fn | TEST-ONLY | 6 | 1 | 0 |  |

## Covered by a record, not listed

- `manifold-core/src/mri.rs` `MriVolHeader`: engine-model.md engine-003 (the orphan file).
- 3 `effect_groups.rs` commands: engine-031. 2 `effects.rs` expose commands: engine-033. 9 `stage.rs` display commands: engine-037. 7 `session_commands.rs` commands: engine-038. All in engine-model.md.
- `manifold-fluids/src/whitewater_oracle.rs` `emit_engine`, `set_emission_rates`: water.md water-019 (the whole oracle file).
- 26 rows whose name a record already carries (core 6, editing 11, io 1, media 1, node-engine 3, playback 4).

Nothing left after subtraction: compositor, core, editing, foundation, io, led, native, nodes, nodes-image, profiler, recording, spectral, water-liquid, water-rigid, water-surface, water-whitewater.
