# fallbacks: proof

Commands and one-line results for every candidate in `fallbacks.md`. Paths are relative to the repo root unless absolute. `$P` is `/tmp/simplification_census/corpus/projects` (182 dumped projects). `$S` is the sweep's scratchpad, which holds these helpers:

- `versions.txt`: `rg -o -m1 '"projectVersion"\s*:\s*"[^"]*"' $P`, one line per project (182 of 182 carry a version string).
- `below.sh T [PATTERN]`: corpus files whose version is below `T`, optionally only those that contain the fixed string `PATTERN`.
- `skipped_dates.tsv`: `stat -f '%Sm' -t '%Y-%m-%d'` on each online-only path in `skipped.tsv`. Metadata only; no file was opened. 67 dates, 2026-02-13 to 2026-09-28.
- `formats.sh`: first two bytes of each non-Dropbox source file (ZIP or plain JSON) plus its layer `durationMode` values.
- jq filters `legacy_scene_wires.jq`, `overlaps.jq`, `ww_level_set.jq` (each reproduced where used).

**The window test.** A legacy shape can only sit in a file last saved between the day the shape was first written and the day its migration landed. For each zero-corpus shape the proof gives that window (from `git log -S ... --reverse`, first commit) and counts the online-only files whose modification date falls in it. Gap: a modification date is the last save as Dropbox reports it; a file copied without its date would escape the window.

Rung dates (first commit that added each gate to `crates/manifold-io/src/migrate.rs`, `git -C "$R" log -S 'is_version_less_than(&version, "X")' --format='%ad %h' --date=short -- crates/manifold-io/src/migrate.rs | tail -1`): 1.1 2026-03-13, 1.2 05-07, 1.3 05-08, 1.4 06-05, 1.5 06-06, 1.6 06-06, 1.7 06-12, 1.8 06-17, 1.9 06-18, 1.10 06-19, 1.11 07-05, 1.12 07-10, 1.13 08-26, 1.14 09-01, 1.15 09-12, 1.16 09-17, 1.17 10-03, 1.18 (no hit for the literal), 1.19 10-06, 1.20 10-06, 1.21 10-14.

Corpus versions (`raw/project_versions.txt`, re-derived into `versions.txt`): 68 at 1.0.0, 29 at 1.1.0, 3 at 1.3.0, 4 at 1.6.0, 3 at 1.10.0, 3 at 1.11.0, 35 at 1.12.0, 4 at 1.13.0, 5 at 1.14.0, 1 at 1.15.0, 11 at 1.16.0, 3 at 1.18.0, 2 at 1.19.0, 3 at 1.20.0, 8 at 1.21.0. Projects below each rung (`below.sh T | wc -l`): 1.1 68, 1.4 100, 1.7 104, 1.11 107, 1.12 110, 1.13 145, 1.14 149, 1.16 155, 1.17 166, 1.18 166, 1.19 169, 1.20 171, 1.21 174.

## Migration cost table: per-row evidence

Corpus probes, each `below.sh <rung> '<string>' | wc -l` unless shown:

- 1.0→1.1: `"genParamValues"` 46, `"importedPercussion` 40, `"genDrivers"` 45 of the 68 files at 1.0.0. Online-only saved before 2026-03-13: 20.
- 1.3→1.4: `"userParamBindings"` 0 below 1.4, 0 in the whole corpus.
- 1.4→1.5: `jq '[.timeline.layers[]? | select(has("generatorGraph"))] | length'` → 108_feedbackTests 1, 116_graphTests 2 (both Downloads test projects at 1.3.0).
- 1.5→1.6: `"targetEffectType"` in 7 files; container envelopes non-empty (`jq` over layer and clip `envelopes`) in 4: Liveschool V6 LEDS (both copies, 15 each), Liveschool V4 GROUPS 17, FX Testing 1.
- 1.6→1.7: `"WireframeDepthGraph"` 0 below 1.7, 0 anywhere.
- 1.7→1.8: `jq '[.timeline.layers[]?.genParams | select(type=="object") | select(has("generatorType")|not) | select(has("effectType"))] | length'` over the 104 files below 1.8 → 0 files.
- 1.8→1.9: `"brightness"` 0, `"liveliness"` 0 below 1.9.
- 1.9→1.10: `jq '[.audioSetup.sends[]? | .source | select(. != null)] | length'` → 0 files below 1.10.
- 1.10→1.11: `"paramValues"` in 82 of 107 files below 1.11, `"baseParamValues"` in 41.
- 1.11→1.12: `"node.render_scene"` below 1.12: 1 file (123_meshImportTests), which carries `"pos_x_0"`.
- 1.12→1.13: 35 files below 1.13 hold `node.render_scene`; 34 have no `node.coc_from_depth`.
- 1.13→1.14: 3 files below 1.14 hold `node.coc_from_depth`; 1 (119_helmetGlitches) carries `sceneBounds`; 0 already carry `world_to_mm`.
- 1.15→1.16 (audit only): 42 files below 1.16 hold `node.render_scene`.
- 1.16→1.17: 7 files below 1.17 hold `node.rigid_body`; `jq '[.. | objects | select(.typeId? == "node.rigid_body") | select((.params // {}) | has("mass"))] | length'` → 4, 7, 7, 4, 5, 18, 6 bodies with mass.
- 1.17→1.18: 3 files with `node.gpu_flip_domain` below 1.18, 0 with `solve_level`.
- 1.18→1.19: 6 below 1.19, 0 with `max_iterations`.
- 1.19→1.20: 8 files with `node.gpu_flip_step` below 1.20, 0 with `contacts`.

Serde-level and post-load passes:

- integer preset ids: `rg -l '"effectType"\s*:\s*[0-9]' $P | wc -l` → 46, generatorType → 54; all at 1.0.0 (50) or 1.1.0 (4).
- `"BasicShapesSnap"` → 9 files.
- `"paramIndex"` → 57 files, all below 1.2.
- `"param0"` … `"param3"` → 40 files each, all 1.0.0.
- macro targets: `jq -r '[.settings.macroBank.slots[]?.mappings[]?.target | "\(.type)|idx=\(has("param_index"))"] | .[]'` → layerEffect 8 (6 with param_index), genParam 15 (9 with param_index), effect 2.
- `"handleNode"` → 1 file. `"layerType": "Led"` → 0. Layer type strings: 67 "Generator", 7 "Video", all in 1.0.0 and 1.1.0 files.
- `"audioTrigger"` → 0.
- audioSetup (32 files): `deviceName` 0, single-channel sends 1, non-empty send `triggers` 2. `rg -l '"rateOfChange"\s*:\s*true'` → 0. `"triggerMode"`: both 2, clipEdge 3, transient 3; `jq` shows `action` absent (Continuous) on all five clip-edge mods; no `"action"` value anywhere in the corpus.
- nodes with an empty or missing `nodeId` and a `handle`: 108_feedbackTests 10, 124_nodeTests 4 (both 1.3.0).
- embedded preset ids containing `#`: 53, all with a non-empty `displayName`.
- `"node.phong_material"` → 1 file (111_fluidTesting, Downloads, 1.14.0, 2026-09-11).
- retired CPU FLIP: `"node.fluid_surface"` 7 files, `"WaterDamBreak"` 6, `"WaterDamBreakGpu"` 1.
- overlapping clips (`overlaps.jq`: per layer, sorted by `startBeat`, count adjacent pairs whose end passes the next start): 38 files, every one at 1.0.0 or 1.1.0, including WAYPOINTS (297), PATTERN (573), STAGNATE (983), STAGNATE V3 (3232), WAYPOINTS VERT (229). Sample, WAYPOINTS layer Bass: clips at 99.998 and 100.248, both 4 beats long.
- 3D projects with `node.render_scene` and no `"sceneModifiers"` key: 47 (1 at 1.11, 34 at 1.12, 2 at 1.13, 4 at 1.14, 5 at 1.16, 1 at 1.18).
- `"node.ssao_gtao"` 48 files, 13 of them without `"node.masked_mix"` (upper bound for the AO mask migration).
- `"node.gltf_mesh_source"` 54, skinned 7. `"node.liquid_frame"` 11, `"node.gpu_flip_domain"` 11, `"node.blob_bounds"` 5.
- whitewater `level_set` (`ww_level_set.jq`: wires into a `node.whitewater_step` node's `level_set` port) → 138_waterFunv3 1, 110_fluidTestGpuFlip 1; the Hung Up and Murmur shows 0.
- old graph ids (`TYPE_ID_MIGRATIONS` minus the two `RETIRED_PARAMS` rows, 114 ids): 40 files hold one or more, by version 1.0.0 5, 1.1.0 17, 1.3.0 2, 1.6.0 3, 1.10.0 3, 1.12.0 7, 1.13.0 1, 1.14.0 2. No file at 1.15 or later.

Line counts: `wc -l` per file and `rg -n '#\[cfg\(test\)\]'` for the product and test split (`crates/manifold-io/src/migrations/`: contacts_wire_v1200 82/78, param_storage_v14 491/393, pressure_cap_card_v1190 216/277, reversed_depth_v1160 56/48, rigid_body_density_v1170 100/43, scene_cinematic_tail_v1130 461/285, scene_scale_coc_v1140 193/144, scene_transform_v1120 553/352, solve_level_card_v1180 139/85; phong_migration 710/345; scene_object_migration 333/411; gltf_import/migration.rs 415/293; gltf upgrade.rs 393 plus upgrade/ 792; scene_modifier_legacy_migration 65 plus 2,649 in its folder). Test files: `crates/manifold-app/tests/legacy_param_id_resolution.rs` 705, `crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/gltf_upgrade.rs` 604, `gltf_upgrade_project.rs` 199, `gpu_proofs/catalog/render_scene_material_upgrade.rs` 276, `gpu_proofs/scene_object_migration_round_trip.rs` 165, `scene_modifier_legacy_migration.rs` 177, `scene_modifier_photoscan_migration.rs` 307, `scene_modifier_fragment_masks.rs` 233, `gpu_proofs/scene_modifier_legacy.rs` 157, `catalog_tests/loop_upgrade.rs` 328, `catalog_tests/binding_migration.rs` 147.

Note on `raw/type_id_migrations.tsv`: its regex also caught the two `RETIRED_PARAMS` rows (`node.gpu_flip_step`, `node.liquid_frame`, current ids with 11 hits each). The real rename table has 114 entries plus one fixture; 42 of the 114 old ids appear in projects, not 44 of 116 as census section 2 says.

### fallbacks-001

Claim: every rung gate reads the version captured once at the top, so the 21 intermediate `projectVersion` stamps are dead writes, and the "Literal 1.11.0, NOT CURRENT_PROJECT_VERSION" comment describes a hazard that cannot happen.

- `sed -n 5,188p crates/manifold-io/src/migrate.rs` → `let version = root.get("projectVersion")…to_string()` at line 17; every gate is `is_version_less_than(&version, …)`.
- `rg -n 'projectVersion' crates/manifold-io/src/migrations/ crates/manifold-io/src/migrate.rs` → outside the stamps and the line-17 read, only test fixtures. No migration reads the stamped value.
- `rg -n 'project_version' crates -g '*.rs'` → `Project::project_version` comes from the migrated JSON and is written back verbatim, so one final stamp is needed and enough.
- Stamp-only rungs: 1.1→1.2 and 1.2→1.3 call empty functions (`migrate_v110_to_v120`, `migrate_v120_to_v130`, lines 706-742); 1.15 and 1.21 stamp only.

Lines: about 21 stamps, 30 comment lines repeating the wrong rationale, 36 lines of empty functions and docs, 8 lines of stamp-only rungs. Tests: `test_v100_chains_through_to_v140` keeps its assert on the final version.

### fallbacks-002

- `rg -l -F '"WireframeDepthGraph"' $P | wc -l` → 0. Bundled presets: `rg -l -F '"WireframeDepthGraph"' crates/manifold-nodes/assets` → 0.
- `git log -S WireframeDepthGraph --reverse` → first 2026-05-28 (a0ab270eb); rung 1.7 landed 2026-06-12.
- Window [05-28, 06-12): online-only 0, local 5 (none carries it).
- Code: `migrate_v160_to_v170` (`crates/manifold-io/src/migrate.rs:309-328`), test `test_v170_renames_wireframe_depth_graph_type` (1196-1244).

### fallbacks-003

- 1.7→1.8 probe above → 0 files with a generator `genParams` carrying `effectType` and no `generatorType`.
- Generator identity probe: `jq '[.timeline.layers[]?.genParams? | select(type=="object") | select(((.generatorType // "None") == "None") and ((.graph.presetMetadata.id // "") != ""))] | length'` over every file with `genParams` → 0 files.
- Root bug window: `PresetInstance` first appears 2026-06-06 (79e6c8f5e); the fix c8bafe641 ("generator layers no longer save as a cleared (NONE) generator") is 2026-06-17. Online-only in [06-06, 06-17): 0. Local: 5, none carries it.
- `rg -n --hidden -g '!.git/' -w reconcile_generator_identity` → `crates/manifold-io/src/loader.rs:466` (load step 9.5), `crates/manifold-core/src/layer.rs:912`, its four tests (1355-1388), and a doc mention in `migrate.rs:279`. Added 2026-06-09 (b7fa15d04, "Reconcile desynced generator identity on load"), eight days before the root fix.
- Both passes fix the same files: once 1.7→1.8 renames `effectType` to `generatorType`, the type is no longer NONE and the reconcile never fires.
- Lines: `migrate_v170_to_v180` 269-307 (39) and its two tests 1245-1316 (72); `reconcile_generator_identity` with doc 900-934 (35); loader step 458-474 (17); layer tests with the `desynced_generator` helper 1322-1388 (66).

### fallbacks-004

- `below.sh 1.9.0 '"brightness"'` 0, `'"liveliness"'` 0.
- `git log -S Liveliness --reverse -- crates` → 2026-06-17 (14e067a2e); rename rung 1.9 landed 2026-06-18. Window: online-only 0, local 0.
- Code: `migrate_v180_to_v190` (235-267), test `test_v190_renames_audio_mod_feature_kinds` (1317-1379).

### fallbacks-005

- Probe above → 0 files with any `audioSetup.sends[].source`.
- `struct AudioSend` first 2026-06-15 (b186134c8), `enum AudioSendSource` 2026-06-18 (d81bbf213), rung 1.10 2026-06-19. Window [06-15, 06-19): online-only 0, local 0.
- Code: `migrate_v190_to_v1100` with doc (190-233), test `test_v1100_send_source_becomes_layer_set` (1380-1412).

### fallbacks-006

- Old table keys (`joint_parent_table`, `joint_root_world_table`, `inverse_bind_table`, `translation_tracks`, `rotation_tracks`, `scale_tracks`, `translation_track`, `rotation_track`, `scale_track`, `weight_tracks`): `rg -l -F` over `$P` → 0 each; over `crates/manifold-nodes/assets` → 0 each. Sampler nodes do exist (gltf_skeleton_pose 7 files, gltf_animation_source 4), all in the new shape.
- `node.gltf_animation_source` first 2026-07-16 (8d48d7b80); `migrate_gltf_anim_v2` first 2026-07-18 (9f153abe4). Window: online-only 0, local 2 (neither carries it).
- Code: `crates/manifold-nodes-scene/src/node_graph/gltf_import/migration.rs:5-153` plus its `GraphMigration` registration (~400); tests `migrate_gltf_anim_v2_strips_tables_and_stamps_sibling_path` (469) and `migrate_gltf_anim_v2_leaves_unresolvable_node_inert` (537) with their helpers, about 170 lines. `crates/manifold-nodes/tests/contracts/load/migration.rs` names the registration order and needs a one-line edit.
- It runs at every graph build (`MigrationStage::BeforeFlatten` in `graph_loader.rs:528`), not once.

### fallbacks-007

Claim: 41 V1.0 flat fields deserialize, round-trip, and are read by nothing except the change-audit fingerprint.

- `rg -n 'pub legacy_' crates/manifold-core/src/{settings,layer,clip}.rs` → settings 18 (`bloomAmount` … `masterEffectOrder`, lines 158-266), layer 14 (`generatorType`, `genParamValues` … `genWindowSize`, 170-254), clip 5 (147-177). `crates/manifold-core/src/effects/instance.rs:128-132` and `instance_serde.rs:613-626, 685-697, 792-799, 846-849` hold `param0`..`param3`.
- Readers, per name (`rg -n -w -F NAME crates`): `legacy_bloom_amount`, `legacy_strobe_mode`, `legacy_master_effect_order` → definition, Default, `change/audit.rs` only. `legacy_gen_rot_speed_xy`, `legacy_gen_proj_distance`, `legacy_gen_param_values`, `legacy_gen_drivers`, `legacy_gen_param_version`, `legacy_gen_anim_speed`, `legacy_gen_show_vertices`, `legacy_gen_window_size` → same. `legacy_param0` → serde, Default, audit, three tests in `instance_serde_tests.rs` (109, 149, 362). `legacy_generator_type` → also read by `Layer::generator_type` (`layer.rs:444-452`) when `gen_params` is None, but the 1.0→1.1 rung moves the layer's `generatorType` into `genParams` (`move_field`, `migrate.rs:792`), so after load that branch always returns NONE.
- `rg -n 'param0' crates/manifold-io/src` → 0: the param-storage migration does not use them.
- `rg -n 'deny_unknown_fields' crates/manifold-core/src` → 0, so removing the fields leaves loading unchanged.
- Corpus: `bloomAmount` 45, `feedbackAmount` 53, `strobeMode` 43, `masterEffectOrder` 44, `genRotSpeedXY` 49, `param0` 40 files, every one 1.0.0. Files at 1.1.0 or later carrying any of the settings, layer or clip flat fields: 0. WAYPOINTS carries `bloomAmount` 0.037 beside a six-entry `masterEffects` array, so the flat fields were already superseded in the Unity format.
- Lines: settings ~127, layer ~103, clip ~36, instance ~45, audit ~55, tests ~30.

### fallbacks-008

- `sed -n 883,917p crates/manifold-core/src/types.rs` → `enum ClipDurationMode { NoteOff = 2 }`; its `Deserialize` reads any JSON value and returns NoteOff.
- `rg -n 'ClipDurationMode|duration_mode' crates -g '*.rs'` (non-test) → fields on `Layer` (`layer.rs:158`) and `MidiNoteMapping` (`midi.rs:18`), the audit fingerprint (`change/audit.rs:281, 317`), `migrate_duration_modes` (`load_migration.rs:826-834`) called from `loader.rs:68-76` for plain-JSON files only, and two reads in `manifold-playback/src/midi_input.rs` (a `log::debug!` argument at 722 and a `None` constructor at 966).
- `formats.sh` → 63 plain-JSON sources (61 at 1.0.0, 2 at 1.16.0) carry `durationMode` 1 or 2; all load as NoteOff whatever they say.

### fallbacks-009

- `rg -l -F '"userParamBindings"' $P` → 0.
- `user_param_bindings` first 2026-05-08 (8fc0ca763); rung 1.4 2026-06-05. Window: online-only 1, `Liveschool Live Show/Liveschool Live Show V6 VERTICAL.manifold` dated 2026-05-13; local 2, neither carries it.
- Code: `migrate_v130_to_v140`, `fold_user_param_bindings`, `default_preset_metadata` (`migrate.rs:465-496, 544-704`); tests 1048-1195. `crates/manifold-node-engine/src/load/binding_migration.rs` (113) completes the stub graphs the fold leaves and is called once from `crates/manifold-app/src/project_io.rs:775`; its contract test `crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/binding_migration.rs` (147).
- A fallback inside the fold (`migrate.rs:655-659`): a binding with neither `nodeId` nor `nodeHandle` gets `nodeId = innerParam`, a node id that does not exist, "so nothing is silently dropped". The binding then targets nothing.

### fallbacks-010

- `rg -l -F '"math_view_mode"' $P` → 7 files; 6 are 1.21.0 shows that already carry the standalone `MathView` recipe. `"VortexFragments"` (the only carrier recipe `legacy_eligible` accepts) → 1 file, 122_mathTesting (Downloads, 1.15.0, 2026-09-14).
- Embedded controls first 2026-09-13 (ce78a59d0, `math_view_mode`); standalone recipe 2026-09-17 (8f8162316). Window online-only: Corrosion Music Video V6 and Corrosion No Thistles (09-13), Head Split (09-15).
- Code: legacy helpers in `crates/manifold-core/src/scene_modifier_math_view.rs:76-552` (shared vocabulary is lines 1-75); `migrate_legacy_math_views` and `append_legacy_math_view` in `crates/manifold-app/src/project_io.rs:237-506`; core tests 553-1047; app journey tests in `crates/manifold-app/src/scene_modifier_journey.rs`.
- `bd show BUG-cavux` → six legacy Math View migration journeys are red on main (open P2).

### fallbacks-011

- `rg -o '"action"\s*:\s*"[a-zA-Z]+"' $P` → none. The five clip-edge or both mods in the corpus have no `action` (Continuous), which the pass leaves alone.
- `enum TriggerAction` first 2026-07-08 (c235c48c1); the pass landed 2026-08-19 (af693fd36). Window: online-only 5 (Surveillance 08-13, Flower Renders and Flower Renders v2 08-15, Corrosion Album Art and Right Where I Need You - Artwork 08-17); local 32, none carries it.
- Code: `load_migration.rs:249-312`; tests 1370-1476.

### fallbacks-012

- `rg -l '"rateOfChange"\s*:\s*true' $P` → 0.
- `rate_of_change` first 2026-06-17 (ec7b073d6); the clear landed 2026-07-11 (53195f584). Window: online-only 3 (ImageTesting 06-24, Album Art Animation 06-27, backgroundImages 06-28); local 6, none carries it.
- Code: `load_migration.rs:208-247`; tests 1292-1369. Its doc says the runtime field "stays compiled for a possible future re-wire" (patterns sweep: a field no UI can set).

### fallbacks-013

- `jq '(.audioSetup.deviceName // null) != null'` over the 32 audioSetup files → 0.
- `deviceName` first 2026-05-07 (13fc6159b); fold landed 2026-06-17 (177f5459a). Window: online-only 1 (the same V6 VERTICAL file, 05-13).
- Code: `crates/manifold-core/src/audio_setup.rs:337-342` field, 363 `is_empty` term, 388-403 `migrate_legacy_device`, call in `load_migration.rs:45`; tests `legacy_device_name_migrates_to_uidless_ref` (533), `new_uid_ref_takes_precedence_over_legacy` (~719).

### fallbacks-014

- `jq -r '.embeddedPresets[]? | .def.presetMetadata?.id'` over 64 files → 53 ids contain `#`; a second probe for `#` ids with an empty `displayName` → 0.
- Embedded presets first 2026-06-06 (d918c1613); backfill landed 2026-07-04 (5e96601f3). Window: online-only 3 (the late-June album-art files).
- Code: `load_migration.rs:800-824`, `loader.rs:476-487`, test `backfill_legacy_fork_display_names_derives_variant_label_from_id` (845-886).

### fallbacks-015

- `rg -l -F '"node.phong_material"' $P` → 1 (111_fluidTesting).
- `node.phong_material` first 2026-05-27 (bf2bcac92); `phong_migration.rs` added 2026-09-27 (cb249a8a6). Online-only saved in that window: 48 of 67, including Corrosion Music Video V2 to V10, Right Where I Need You V3 and V4, Earth, Stone Effects v2 and v3.
- Code: `crates/manifold-core/src/phong_migration.rs` (710 product, 345 tests), `migrate_phong_graphs` in `loader.rs:369-411`, loader test module `phong_graph_migration_tests` (912-968).

### fallbacks-016

- `legacy_scene_wires.jq` (wires into `node.render_scene` on a `mesh_k`, `material_k`, `*_map_k`, `transform_k` or `instances_k` port) over 67 files with render_scene → 123_meshImportTests (1.11.0) 42 wires, 072_SceneLadders (1.12.0) 176, 114_glbTestingV2 (1.12.0) 28. All Downloads test projects.
- `scene_object_migration.rs` added 2026-07-17 (a4f101802). Online-only saved before then: 37.
- It runs twice: at every graph build (`graph_loader.rs:519-526`) and at app load for layer graphs (`project_io.rs:529`, with a comment explaining why both).
- Code: `crates/manifold-core/src/scene_object_migration.rs` 744 (about 65 lines of `loose_scene_object_owned_ids` are not migration and stay), `crates/manifold-nodes/tests/gpu_proofs/scene_object_migration_round_trip.rs` 165.

### fallbacks-017

Claim: graph-level migrations run on a copy at every build and never reach the saved file, so their tables can never shrink.

- `crates/manifold-node-engine/src/load/graph_loader.rs:496-546` → `migrate_def_type_ids`, `migrate_scene_object_wires` and the `BeforeFlatten`/`AfterFlatten` stages all work on a clone; `migration.rs:44-51` `run_stage` returns `Cow`, "The caller retains the authored document separately".
- `crates/manifold-app/src/project_io.rs:510-570` bakes only layer generator graphs (scene object wires, camera, exposure, legacy modifiers, Math View, periodicity). Embedded presets and effect graphs are never rewritten.
- `jq -r 'paths(objects and .typeId? == "node.channel_mix")'` on 166_Corrosion_Music_Video_V5 (saved 2026-09-11) → `embeddedPresets.24.def.nodes.12`. `node.channel_mix` was renamed 2026-07-03 (2b21a4fb0). The same id sits in 125_offlineRTDemo (09-01), 182_Right_Where_I_Need_You V5 (09-02) and 040_MeshAudio (08-21).
- 72 of the 114 renamed ids appear in no corpus file (`raw/type_id_migrations.tsv` rows with 0).
- `crates/manifold-core/src/type_id_migration.rs:39-46` says the table is "Empty in every shipped build". It holds 114 entries.
- `GraphMigration` submissions (`rg -n -A4 'GraphMigration \{'`): `migrate_gltf_anim_v2`, `migrate_gltf_ao_mask` (BeforeFlatten); `wire_liquid_intervals`, `wire_gpu_flip_grid`, `wire_liquid_frame_cursor`, `wire_retained_whitewater`, `wire_blob_bounds` (AfterFlatten); `wire_gpu_flip_grid` (BeforeBindingCapture); `prepare_gpu_flip_surface` (BeforeSceneModifiers).

### fallbacks-018

- `rg -l -F '"audioTrigger"' $P` → 0.
- `struct AudioTriggerMod` first 2026-07-07 (f7cdbc11b) and `migrate_legacy_audio_trigger` first the same day (e7517ff75); the doc says the field "existed in roughly one project for one day". Online-only files dated in July: 0.
- Code: `crates/manifold-core/src/effects/instance_serde.rs:713-751` plus the two `Raw` fields and calls (783-785, 824-826, 880-883, 915-917); `crates/manifold-core/src/audio_trigger.rs:134-154`; tests `legacy_audio_trigger_mod_deserializes_the_pre_unification_wire_shape` (`audio_trigger.rs:509-532`) and the `audioTrigger` cases in `instance_serde_tests.rs:591-700`.

## Silent fallbacks (3.8a)

Rows 020 to 050 come from two read-only helper passes (core, io and editing; node-engine load and graph build plus the app's load and command glue). The lead re-read the cited lines for 020, 021, 022, 023, 027, 028, 029, 035 (count), 043 and 045 (`install.rs`) before taking them; each re-read is marked "lead re-read". The rest carry the helper's commands as reported.

Starting idioms per crate (lead): `rg -c 'unwrap_or_default\(\)|unwrap_or_else\(\|_\||\.ok\(\)\?|let _ = '` → manifold-core 13/0/0/2, manifold-io 7/4/10/40, manifold-editing 9/0/9/40, manifold-node-engine 44/0/26/64, manifold-app 118/16/19/230 (unwrap_or_default / unwrap_or_else(|_|) / .ok()? / let _ =).

### fallbacks-020

- Lead re-read `crates/manifold-app/src/content_export.rs:436-447`: `while let Ok(cmd) = cmd_rx.try_recv() { if matches!(cmd, ContentCommand::CancelExport) { return true; } }`. Every other command is consumed and dropped.
- `rg -n poll_export_cancel crates/manifold-app/src` → 3 call sites (911, 965, 970), all in the export frame loop.
- `git log -S 'The modal prevents authoring commands' -- crates/manifold-app/src/content_export.rs` → a5129aff9 (2026-09-26); the same drop loop existed inline before.
- Gap: which commands the UI sends while the export modal is up was not observed at runtime.

### fallbacks-021

- Lead re-read `crates/manifold-io/src/archive.rs:82-150` and `224-238`: `manifest = read_manifest(path)` where every step is `.ok()?`; then `manifest.unwrap_or_default()`; `history.insert(0, current)`; `live_hashes` is built from that history and the copy keeps only live hashes.
- `rg -n 'pub struct ProjectManifest' -A25 crates/manifold-io/src/manifest.rs` → every field has a serde default, so only corruption or an I/O error reaches the fallback.
- `rg -n 'read_manifest\(' crates` → archive.rs (3), `loader.rs:45` (the archive too-new guard, which the same None silently skips), `app_lifecycle.rs:1057`.
- No test covers a corrupt manifest.

### fallbacks-022

- Lead re-read `crates/manifold-editing/src/service.rs:355-360` (`layer_index_range(..).unwrap_or((0, 0))`), `crates/manifold-core/src/selection.rs:110-125` (one missing id collapses to the other end), `crates/manifold-ui/src/ui_state.rs:1200-1216` (prune touches only `selected_layer_ids`).
- `rg -n 'start_layer_id|end_layer_id' crates` → set only in `ui_state.rs:927-928`, copied to core in `manifold-app/src/ui_translate.rs:622-623`.
- `rg -n 'EditingService::delete_clips\(' crates/manifold-app/src` → `input_host.rs:562` and `:662` pass the region.
- Same `unwrap_or((0, 0))` in `ui_bridge/state_sync.rs:622` and `input_host.rs:637`.
- Gap: reachability reasoned from the prune code, not reproduced.

### fallbacks-023

- Lead re-read `crates/manifold-app/src/content_commands.rs:330-355`: `Ok(cmd) => { let _ = cmd_tx.send(cmd); }` inside the per-layer warmup loop.
- `app_lifecycle.rs:950-971` → SeekTo, ResizeContent and SetProject are queued right after LoadProject.
- `content_thread.rs:330-345` → the normal drain keeps order on purpose.
- `git log -S 'put them back on the queue'` → 5e4ba39b6 (2026-08-20, warmup P1).

### fallbacks-024

- `rg -c 'impl Command for' crates/manifold-editing/src -g '!*tests*'` → 204; `rg -c 'fn was_applied' crates/manifold-editing/src/commands -g '!*tests*'` → 54.
- `ast-grep run --pattern 'impl Command for $T { $$$ }'` filtered to bodies with no `was_applied` and a bail-out in `do_execute` (`return`, let-else, `?;`, `let _ =`, `if let Some`) → 101 commands (approximate: some `if let Some` are real conditionals).
- Hand reads: `commands/settings.rs:616-670` (layer blend mode, opacity), `effects.rs:518-560`, `node_edit.rs`.
- Tests pinning the no-op: `crates/manifold-editing/tests/command_roundtrips.rs::change_effect_param_unknown_id_is_no_op` (871-915), `commands/preset.rs::tests::rename_embedded_preset_is_a_no_op_for_a_missing_id`, `commands/graph/card_pin.rs::tests::missing_param_is_a_no_op`, `commands/automation.rs::tests::move_missing_source_is_noop_and_undo_is_safe`.
- Model: `fire_param.rs` test `rejects_missing_target_without_mutating_project`.

### fallbacks-025

- `ast-grep run --pattern 'fn do_undo(&mut self, $P: &mut Project) { $$$ }'` filtered for `let _ = with_existing_target_graph_mut` → 33 sites (node_edit 7, scene 11, groups 4, modifiers 3, skin 2, physics 2, paste, split, expose, layer_plane 1 each).
- `rg -n 'ContentCommand::mutate\(' crates/manifold-app/src | wc -l` → 33 non-undo writers; only `mute_visibility_probe.rs` removes anything.
- Gap: writers outside `MutateProject` not audited.

### fallbacks-026

- `sed -n 1431,1575p crates/manifold-editing/src/commands/effects.rs` → the seed graph is installed and the structure version bumped before `host.params.get(&binding_id)?`; undo returns early when `reverse` is None; `was_applied` not overridden.
- `rg -n 'owner.graph = Some\(owner_default\?.clone\(\)\);' crates/manifold-core/src/project/queries.rs` → line 478, which runs before the closure.
- `rg -n 'nodes.iter_mut\(\).find\(\|n\| n.id == render_id\)\?' crates/manifold-editing/src/commands/graph/scene.rs` → 450, 793, 1249.
- Tests pinning the no-op: `effects.rs::tests::edit_mapping_unknown_binding_is_noop`, `::edit_unknown_stock_id_without_seed_is_noop`.

### fallbacks-027

- Lead re-read `crates/manifold-editing/src/commands/graph/node_edit.rs:125-170`: the `None` arm only `eprintln!`s; `rejection_reason` returns `self.rejection`; `was_applied` is `self.rejection.is_none()`.
- `rg -n 'rejection = Some' node_edit.rs` → set only by `refuses_second_scene`.
- `crates/manifold-editing/src/undo.rs:48` drops rejected commands.

### fallbacks-028

- Lead re-read `crates/manifold-core/src/types.rs:184-216`: Serialize is `serialize_i32`; Deserialize maps unknown numbers, unknown strings and any other JSON type to `LayerType::Video`, and `"Dmx" | "Led"` to Dmx.
- `rg -n --hidden -g '!.git/' '"layerType"\s*:\s*"Led"'` → only `crates/manifold-io/src/loader.rs:896` (a test) and `docs/LED_STRIPS_DESIGN.md:654`, which names the gate "the silent-fallback-to-Video failure mode".
- Corpus: `rg -o --no-filename '"layerType"\s*:\s*"[A-Za-z]+"' $P | sort | uniq -c` → 67 Generator, 7 Video, no Led; strings only in 1.0.0 and 1.1.0 files.
- Lead re-read `crates/manifold-core/src/preset_type_id.rs:304-333`: `n.as_i64().unwrap_or(0)` feeds `from_legacy_effect_discriminant` / `from_legacy_generator_discriminant`; effect discriminant 0 is Transform (test at 349-353).
- Helper: the same catch-all in BlendMode (72), ClockAuthority (265), OscSyncMode (322), QuantizeMode (378), ResolutionPreset (424), TempoPointSource (556), BeatDivision (616), DriverWaveform (846), MarkerColor (1013), and `percussion_analysis.rs:41`. Corpus values for every one of these fields are in range (`rg -o "\"$f\": ?[^,}\n]+"` per field).
- `loader.rs:183-195` refuses a file newer than the build before deserialize, so the old reason (an old build reading a newer file, LED_STRIPS_DESIGN.md:292) no longer applies.
- Tests: `loader.rs::layer_type_dmx_round_trip_tests::hand_built_json_layer_type_led_string_loads_as_dmx` (888-909) and the last asserts of `types.rs::tests::layer_type_dmx_wire_form_round_trip` (1134-1141).

### fallbacks-029

- Lead re-read `crates/manifold-node-engine/src/runtime/build.rs:450-470`: `graph.instance_by_node_id(node_id)?` and `graph.get_node(inst_id)?` inside `filter_map` drop the binding silently; a missing param logs a warning and drops it.
- Helper: `core.rs:1081-1092` records the same case as `ChainError::StaticBindingHandleMissing` on the effect path; `validate.rs:444` lints it as an error (tests `card_lint_error_dangling_binding_target_node`, `_param`).
- jq over the 94 bundled effect and generator presets → 0 dangling node bindings; over 670 corpus graph defs → 11 in 3 projects (019_ImageTesting 5, 040_MeshAudio 3, 072_SceneLadders 3).
- Gap: whether load migrations mint those node ids (a `project_tool` load of the three files answers it).

### fallbacks-030

- Helper: `groups.rs:144-194` `splice_card_with_canonical_fallback`, `core.rs:943-966`; `errors.rs:177-180` prints non-fatal chain errors; `rg -n '\.errors\(\)'` over app, compositor, nodes and renderer (non-test) → no reader of `PresetRuntime::errors`; `rg -n 'ChainBuildFailure|chain_failures'` → only total failures reach the inspector.
- `bd show BUG-38rup` → open P1, suspects include `groups.rs:162-176` and `core.rs:920` (canonical fallback keeping fused bindings).
- Test pinning it: `manifold_node_engine::runtime::groups_tests::failed_fused_splice_reports_canonical_fallback`.

### fallbacks-031

- Helper: `bound_graph.rs:542-577` → `let Ok(flat) = flatten_groups(def) else { return; }` and `let _ = graph.set_param(..)`; `graph.rs:424-450` lists ParamNotFound, NodeNotFound, PreparedParameterChanged.
- `rg -n 'apply_inner_param_overrides\('` → product callers `generator_renderer.rs:835`, `physics_events.rs:71`.
- `bd show BUG-38rup` names `bound_graph.rs:559-576` among its silent fallbacks.

### fallbacks-032

- Helper: `rg -n 'project: Option<|pub fn project\(&self\)|pub fn project_edit' crates/manifold-playback/src/engine.rs` → field 183, accessors 574 and 589; `self.project = None` only in `shutdown()` (771); `rg -n 'engine\.shutdown\(\)' crates` → one hit at 4183, inside `#[cfg(test)]` (from 3840).
- `rg -n 'PlaybackEngine::new\(' -A2 crates` → product constructors (`app.rs:1810`, `headless_harness.rs:62`, `ui_snapshot/script.rs:616`) call `initialize` on the next line.
- `rg -c 'engine\.project\(\)|engine\.project_edit\(\)'` over app src → content_commands 71, content_thread 27, content_reaction_budget 8, content_pipeline 7, audio_mod_runtime 7, content_export 4, content_reactions 3.
- None arms: `content_commands.rs:1192-1214, 1234, 1280` (Execute, ExecuteBatch, Undo), six `ok_or_else(|| "Project is no longer available")` (1011, 1039, 1059, 1068, 1104, 1128), `content_pipeline.rs:2993-3000` (`unwrap_or(4.0)`, `unwrap_or(0.5)`).
- `rg -n 'Option<Project>|engine\.project' docs/evidence/simplification_census_2026-10/` → not filed by engine-runtime.

### fallbacks-033

- Helper read `project_io.rs:1063-1100` (video branch adds to the local `video_library` and `timeline.add_layer` with no command), `app_lifecycle.rs:1010-1024` (only `AddClipCommand` reaches content), `app_lifecycle.rs:474-486` and `app.rs:2275-2277` (the drop path sends the library update first), `manifold-media/src/video_renderer.rs:203-206` and `clip_launcher.rs:122-127` (unknown clip skipped), `input_handler.rs:213-223` (Cmd+V with file URLs).
- Gap: not run. A ContentThread unit test that pastes an .mp4 and checks the content project's library would settle it.

### fallbacks-034

- Helper: `project_io.rs:1426-1438` returns `Seconds::ZERO` on decode error; `rg -n 'audio_source_duration\('` → callers `project_io.rs:1165`, `ui_bridge/dispatch/clip.rs:99`, `drag_hover.rs:34`; `tempo.rs:247-250` gives 0 beats for 0 seconds.
- Test pinning it: `project_io::tests::audio_source_duration_is_zero_for_unreadable_path` (1531).

### fallbacks-035

- Lead re-ran `rg -n -F 'UnnamedScene' crates -g '*.rs' | rg -v '/tests?/' | wc -l` → 11; `rg -l -F '"UnnamedScene"' $P | wc -l` → 0.
- Helper: `rg --files-without-match '"presetMetadata"'` over the four bundled preset folders → 0 of 113; jq over 80 corpus generator graphs → 0 without metadata; `rg -n 'preset_metadata: None' crates/manifold-editing` → 19 hits in 12 test-support files.
- Gap: GLB import's graph from scratch (`scene.rs:2767`) needs its metadata guarantee checked.

### fallbacks-036

- Helper: `instance.rs:761-786` (`synth_user_binding`, "Identity fallback when no manifest entry"); `instance_serde.rs:360-415` seeds the manifest from every user-added binding; `loaded_preset_view.rs:137-143` and `build.rs:472-480` default to `(0.0, 1.0)` and an empty label.
- jq over bundled presets → 1,010 bindings, 0 without a param; over 492 corpus embedded presets → 2 (019_ImageTesting `MriVolume#1`, `position` and `scale`).

### fallbacks-037

- Helper: `rg -n 'refresh_target_manifest\(' crates/manifold-editing/src -g '!*tests*' | wc -l` → 48; `reconcile_scene_modifier_parameters` has three error kinds; the refresh prints and returns after the edit landed (`graph/mod.rs:139-180`). `preset/modifier.rs` `replacement_candidate` shows the reconcile-before-commit shape.

### fallbacks-038

- Helper: `rg -n 'fn scene_physics_binding' -A50 | rg 'Err\(|Ok\('` and `sed -n 883,925p physics.rs` → seven error strings, two of which mean "not physical"; `split.rs:148` `.ok()`s them together.

### fallbacks-039

- Helper: `chain_spec.rs:112-131`: the error arm logs and returns None; `output: inst.output_endpoint?` returns None with no log. `rg -n 'splice_def_into_chain\('` → core 1, groups 2, `bundled_presets.rs` 4, tests 5.

### fallbacks-040

- Helper: `build.rs:350-406`, `core.rs:986-1026`, `graph_loader.rs:538-544`: the generator's second flatten is `.ok()`, skipping BeforeBindingCapture, the trigger-shadow lint, shadow detection (`bound_graph.rs:140-149` says None only where there is genuinely no def) and string-binding seeds; `instantiate_def` flattens the same def and errors properly.

### fallbacks-041

- Helper: `graph_loader.rs:695-704` returns `UnknownParam`; `736-746` has no else arm for an unknown exposure. jq over corpus and bundled presets against `docs/node_catalog.json` → 138 exposures, 0 off-surface. Gap: dynamic-surface node types are not in the static catalog.

### fallbacks-042

- Helper: `sed -n 420,560p crates/manifold-core/src/effects/instance_serde.rs` → the unknown-id drop prints and adds no `load_report` entry. Test `instance_serde_tests.rs::params_map_deserialize_drops_unknown_id` pins the drop.

### fallbacks-043

- Lead re-read `crates/manifold-io/src/loader.rs:38-64` and `133-149`: any `extract_json_from_zip` error (not a ZIP, no project.json, read failure) falls to `String::from_utf8(file_bytes)`.

### fallbacks-044

- Helper: `rg -n 'has_overlapping_clips|warn_overlapping_clips' crates` → only the loader repair and two debug calls in `service.rs:161-213`.

### fallbacks-045

- `expose.rs`: `rg -n 'inner_meta' crates` → one write (`Some(inner_convert)`, line 68), reads at 195 and 349-350 with `unwrap_or(ParamConvert::Float)` and `modifier.rs:108` with `unwrap_or_default()`.
- `layer.rs:656-678`: `roots.is_empty()` rejected before `.position(..).unwrap_or(0)`.
- Lead re-read `crates/manifold-node-engine/src/freeze/install.rs:222-231`: `Err(_) => return u64::MAX`. `rg -n 'Map<' crates/manifold-core/src/effect_graph_def.rs` → every map is keyed by String, so serialization cannot fail.
- `project_io.rs:49-53`: same serialize `else` arm; jq → 0 of 492 corpus embedded presets lack an id.
- `graph_loader.rs:1498-1500`: the plan is compiled from this graph.
- `binding_shape.rs:30-34`: `Declared::new` is None only without metadata, checked at line 27; `spec(&saved.id)` searches the list being iterated (`param_lookup.rs:467-479`).

### fallbacks-046

- Helper: `sed -n 205,300p crates/manifold-editing/src/commands/preset/modifier.rs` → `.ok()?` at 252, 253 and 268; the caller prints a generic message (140-154).

### fallbacks-047

- Helper: `editor_bridge.rs:333-504` and `1442-1466`: `resolve_level(..).unwrap_or((&snap.nodes, &snap.wires))` in three resolvers; the strict `resolve_preview_target_with_scope` (404-434) exists; jq over corpus and bundled group nodes → 2,939 groups, all with a nodeId.
- Tests to re-point: `editor_bridge::tests::{plain_node_previews_itself, group_output_boundary_resolves_to_container, group_input_boundary_resolves_to_external_feeder, final_output_resolves_to_its_input_producer}`.

### fallbacks-048

- Helper: `rg -n 'video_clip_id' crates -g '!**/tests/**'` → only `with_video_clip_id` sets Some, inside the fallback (`percussion_binding.rs:48-91`); every constructor in `percussion_settings.rs:419-491, 687+` passes None.

### fallbacks-049

- Helper: `rg -n 'trigger_mode.*unwrap_or' crates -g '!**/tests/**'` → 9 sites: `trigger_source.rs:101, 152` (Transient), `effects/instance.rs:1425` (Both), `manifold-playback/src/modulation.rs:813, 972, 1027, 1043, 1054, 1300`; the inspector derives its own predicate at `ui_bridge/projection/inspector.rs:1472-1479`.

### fallbacks-050

- Helper: render-scale clamps `content_commands.rs:1538` (0.01..1.0) and `content_pipeline.rs:3793` (0.25..1.0); `settings_popup.rs:280` offers 1.0, 0.75, 0.5; jq over corpus `settings.renderScale` → 97 at 1.0, 85 absent.
