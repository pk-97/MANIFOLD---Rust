# fallbacks sweep

- **Sweep:** `fallbacks` (census section 3.8, Silent fallbacks, legacy paths, migrations and their tests), brief 11.8.
- **Model:** Opus (claude-opus-5-5), with two Opus helpers for the silent-fallback reads (core, io and editing; node-engine and app load glue). Their claims were spot-checked line by line before going in.
- **Main commit:** 37a9c180a (screens). HEAD d4d34a0c2 has no change under `crates/`.
- **Raw files used:** `raw/fallback_files.txt`, `raw/fallback_fns.txt`, `raw/project_versions.txt`, `raw/type_id_migrations.tsv`, `raw/open_cleanup_beads.txt`, the corpus (`index.tsv`, `skipped.tsv`, 182 projects), bundled presets.
- **New oracle, the window test:** a legacy shape can only sit in a file last saved between the day the shape was first written and the day its migration landed. Dates come from `git log -S`; online-only Dropbox files are dated with `stat` (metadata only, nothing opened). Where the corpus has zero files with a shape and no online-only file is dated inside its window, the record is grade B; where an online-only file falls in the window, it is C.

## In one paragraph

Old shows load through about sixty separate load-time passes spread over five places: the JSON version ladder, tolerant serde readers, post-load passes in core and io, app-side passes that need the node registry, and graph migrations that rerun on every graph build. Nine of those passes handle a shape no saved file carries and no online-only file could carry; they can go (about 1,500 lines with their tests). Eight more handle shapes that lived for days or weeks; whether to keep them is Peter's call, since one or more online-only files were saved inside the window (about 4,000 lines). The worst silent fallbacks are on edit and save paths, not load. An export throws away every command except Cancel. A save over an archive whose manifest can't be read wipes its history. Stale region ids turn a delete into a delete on the top layer. About a hundred edit commands record a do-nothing undo step when their target is gone. Graph renames are applied to a copy at build time and never written back, so the rename table can never shrink.

## Migration cost table

One line per load-time pass. "Corpus" counts saved projects that carry the input shape, or the version span for a ladder rung. "Window" counts online-only files that could carry it. Lines are product / tests. Keep means saved files need it; the candidate column points to a row below.

| Pass | Input it handles | Corpus | Window | Lines | Verdict |
|---|---|---|---|---|---|
| 1.0→1.1 nest percussion and generator fields | Unity-era flat layer fields | 68 (WAYPOINTS, PATTERN, STAGNATE, Burn) | 20 | 76 / 22 | keep |
| 1.1→1.2, 1.2→1.3 | nothing (empty functions) | — | — | 36 / 0 | fallbacks-001 |
| 1.3→1.4 fold `userParamBindings` (+ node-engine completion) | per-effect user bindings | 0 | 1 (Liveschool V6 VERTICAL) | 317 / 295 | fallbacks-009 |
| 1.4→1.5 layer `generatorGraph` | graph override on the layer | 2 test projects | — | 60 / 75 | keep |
| 1.5→1.6 container envelopes | layer and clip `envelopes` | 4 (Liveschool V6 LEDS ×2, V4 GROUPS) | — | 73 / 43 | keep |
| 1.6→1.7 `WireframeDepthGraph` | retired type id | 0 | 0 | 20 / 49 | fallbacks-002 |
| 1.7→1.8 generator saved as effect kind | `genParams.effectType` | 0 | 0 | 39 / 72 | fallbacks-003 |
| 1.8→1.9 feature kind rename | `brightness`, `liveliness` | 0 | 0 | 33 / 63 | fallbacks-004 |
| 1.9→1.10 send source | `source: {layer}` or `"capture"` | 0 | 0 | 44 / 33 | fallbacks-005 |
| 1.10→1.11 param storage | four positional and keyed `paramValues` shapes | 82 | — | 491 / 393 | keep |
| 1.11→1.12 scene transform atoms | `pos_x_k` on render_scene | 1 test project | — | 553 / 352 | keep |
| 1.12→1.13 cinematic tail | 3D graph without CoC chain | 34 | — | 461 / 285 | keep |
| 1.13→1.14 CoC scene scale | coc graph with `sceneBounds` | 1 | — | 193 / 144 | keep |
| 1.14→1.15, 1.20→1.21 | stamp only | — | — | 8 / 0 | fallbacks-001 |
| 1.15→1.16 reversed depth (audit note) | custom raw-depth consumers | 42 audited | — | 56 / 48 | keep |
| 1.16→1.17 rigid body density | `mass` param | 7 | — | 100 / 43 | keep |
| 1.17→1.18, 1.18→1.19, 1.19→1.20 GPU FLIP cards and wire | domain and step without card or wire | 3, 6, 8 | — | 437 / 440 | keep |
| integer preset ids, `BasicShapesSnap` | Unity-era discriminants | 54, 9 | — | ~150 / ~40 | keep |
| `paramIndex` shapes + `resolve_legacy_param_ids` | V1.1 positional addressing | 57 | — | ~600 / ~960 | keep |
| macro `layerEffect` and `param_index` targets | pre-EffectId macro mappings | 5 | — | ~200 / ~150 | keep |
| `handleNode` binding target | pre-node-id bindings | 1 | — | small | keep |
| V1.0 flat fields (41, in settings, layer, clip, instance) | Unity flat amounts nothing reads | 45 to 53 | — | ~366 / ~30 | fallbacks-007 |
| `ClipDurationMode` + V1 `migrate_duration_modes` | one-variant enum | 63 plain JSON | — | ~45 / 0 | fallbacks-008 |
| U5 `audioTrigger` | shape that shipped for one day | 0 | 0 | ~70 / ~125 | fallbacks-018 |
| `deviceName` | pre-UID audio device | 0 | 1 | ~25 / ~30 | fallbacks-013 |
| send channels to stereo, send `triggers` to clip triggers | single-channel sends, trigger routes | 1, 2 | — | ~85 / ~120 | keep |
| clear `rate_of_change` | Delta flag set before 2026-07-11 | 0 | 3 | 40 / 78 | fallbacks-012 |
| step/random clip-edge to envelopes | Step or Random mod firing on clip edge | 0 | 5 | 64 / 107 | fallbacks-011 |
| Mirror `legacy_value_aliases` | Mirror mode 0..2 | 26 files with Mirror | — | 46 / small | keep |
| `normalize_override_node_ids` | nodes without `nodeId` | 2 test projects | — | 53 / 66 | keep |
| `reconcile_generator_identity` | NONE type with graph id | 0 | 0 | 52 / 66 | fallbacks-003 |
| fork display-name backfill | `#` ids without a display name | 0 of 53 | 3 | 37 / 42 | fallbacks-014 |
| `repair_overlapping_clips` | overlapping clips (Unity allowed them) | 38, all 1.0 and 1.1, including WAYPOINTS, PATTERN, STAGNATE | — | 50 / — | keep; deletes hundreds of clips per old show on open (see Seen) |
| retired CPU FLIP notice | CPU FLIP presets or `node.fluid_surface` | 7 | — | ~250 / ~100 | keep |
| Phong to PBR | `node.phong_material` | 1 test project | 48 | ~753 / ~400 | fallbacks-015 |
| scene object wires (at load and at every build) | render_scene per-object ports | 3 test projects | 37 | ~680 / ~180 | fallbacks-016 |
| glTF material upgrade | imports before 2026-09-26 | not measured (54 files import glTF) | — | 1,185 / 1,079 | keep (not measured) |
| legacy scene modifiers (loop, photoscan, sources, fragment masks) | pre-v3 modifier graphs | up to 47 | — | 2,714 / ~1,200 | keep (not measured) |
| legacy embedded Math View | Vortex Fragments with Math View controls | 1 test project | 3 | ~650 / ~350 | fallbacks-010 |
| graph renames, `PARAM_SEED`, `RETIRED_PARAMS` (every build) | 114 renamed ids | 42 ids in 40 files | — | ~250 / ~150 | fallbacks-017 |
| glTF anim v2 (every build) | baked keyframe tables | 0 | 0 | ~158 / ~170 | fallbacks-006 |
| glTF AO mask (every build) | pre-AM4 AO group | up to 13 | — | ~250 / ~120 | keep |
| water graph migrations, six passes (every build) | GPU FLIP graphs before the clock, cursor and grid changes | 11 water projects | — | ~600 / ~420 | keep, never baked (fallbacks-017) |
| whitewater `level_set` path | old level-set wiring | 2 test projects | — | — | BUG-wx0s (Delete whitewater_step legacy level-set path once saved projects are migrated) owns it |

**Dropping old formats.** Converting every project below 1.11 once (107 corpus files plus the online-only ones, the Liveschool, WAYPOINTS, PATTERN and STAGNATE shows among them) and then cutting the ladder below 1.11, the param-storage migration, the integer and `paramIndex` readers, the macro legacy targets and the flat fields would remove roughly 4,500 lines, about half of them tests. Rough sum of the rows above; Peter's call (census section 10, Decisions owed).

## Candidates

| id | location | category | proof | risk | grade | action | lines |
|---|---|---|---|---|---|---|---|
| fallbacks-001 | `crates/manifold-io/src/migrate.rs:23-185` | 3.8b | every gate reads `version` captured at line 17; no migration reads the stamped value; four rungs stamp only, two call empty functions | stage | B | simplify: one `projectVersion = CURRENT_PROJECT_VERSION` after the ladder, drop stamp-only rungs, the empty functions and the wrong "NOT CURRENT_PROJECT_VERSION" comments | ~90 |
| fallbacks-002 | `crates/manifold-io/src/migrate.rs:309-328` | 3.8c | `"WireframeDepthGraph"` in 0 projects and 0 presets; window 05-28 to 06-12 has 0 online-only files | stage | C | delete rung, function and test | 69 |
| fallbacks-003 | `migrate.rs:269-307`, `crates/manifold-core/src/layer.rs:900-934`, `crates/manifold-io/src/loader.rs:458-474` | 3.8c | both repair one bug (generator saved as effect kind, 06-06 to 06-17); 0 corpus files carry either shape; 0 online-only in window | stage | C | delete both repairs and their tests | ~229 |
| fallbacks-004 | `migrate.rs:235-267` | 3.8c | `brightness`/`liveliness` kinds in 0 projects; the names lived one day (06-17); 0 online-only | stage | C | delete | 96 |
| fallbacks-005 | `migrate.rs:190-233` | 3.8c | no corpus project has any send `source`; shape lived 06-15 to 06-19; 0 online-only | stage | C | delete | 77 |
| fallbacks-006 | `crates/manifold-nodes-scene/src/node_graph/gltf_import/migration.rs:5-153` | 3.8c | 10 old table keys in 0 projects and 0 assets; shape lived 07-16 to 07-18; 0 online-only; runs at every graph build | stage | C | delete pass, registration and its two tests | ~330 |
| fallbacks-007 | `crates/manifold-core/src/settings.rs:158-266`, `layer.rs:170-254`, `clip.rs:147-177`, `effects/instance.rs:128-132` | 3.8b | 41 V1.0 flat fields: read only by the audit fingerprint (and one `generator_type` branch the 1.0→1.1 rung makes unreachable); no `deny_unknown_fields`; only 1.0.0 files carry them, beside the `masterEffects` that superseded them | stage | B | delete fields, defaults, audit entries; old files load the same | ~396 |
| fallbacks-008 | `crates/manifold-core/src/types.rs:883-917`, `project/load_migration.rs:826-834`, `loader.rs:68-76` | 3.8b | one-variant enum whose Deserialize ignores its input; two Option fields read only by a debug log; V1 loads force it anyway (also a 3.9 one-value mode) | stage | B | delete enum, fields, migration | ~45 |
| fallbacks-018 | `crates/manifold-core/src/effects/instance_serde.rs:713-751`, `audio_trigger.rs:134-154` | 3.8c | `audioTrigger` in 0 projects; shape added and replaced the same day (07-07); 0 online-only files dated in July | stage | C | delete U5 migration, raw fields and tests | ~195 |
| fallbacks-009 | `migrate.rs:465-704`, `crates/manifold-node-engine/src/load/binding_migration.rs` | 3.8c | `userParamBindings` in 0 projects; window 05-08 to 06-05 holds one online-only show file (Liveschool V6 VERTICAL, 05-13); the fold also invents `nodeId = innerParam` for target-less bindings | stage | C | ask: open V6 VERTICAL once (or accept the risk), then delete fold, completion pass and tests | ~612 |
| fallbacks-010 | `crates/manifold-core/src/scene_modifier_math_view.rs:76-552`, `crates/manifold-app/src/project_io.rs:237-506` | 3.8c | carrier recipe in 1 test project (mathTesting); shape lived 09-13 to 09-17; 3 online-only files in window (Corrosion V6, Corrosion No Thistles, Head Split); six of its journey tests are red, BUG-cavux (Math View migration journey tests red on the scene camera notice) | stage | C | ask: convert those three once, then delete | ~1,000 |
| fallbacks-011 | `project/load_migration.rs:249-312` | 3.8c | no corpus project has any Step or Random mod; 5 online-only files in the 07-08 to 08-19 window | stage | C | ask | 171 |
| fallbacks-012 | `project/load_migration.rs:208-247` | 3.8c | `rateOfChange: true` in 0 projects; 3 online-only in window; keeps a runtime field no UI can set | stage | C | ask | 118 |
| fallbacks-013 | `crates/manifold-core/src/audio_setup.rs:337-403` | 3.8c | `deviceName` in 0 of 32 audio setups; 1 online-only file in window (V6 VERTICAL) | stage | C | ask (same file as fallbacks-009) | ~55 |
| fallbacks-014 | `project/load_migration.rs:800-824`, `loader.rs:476-487` | 3.8c | 53 `#` fork ids, all already named; 3 online-only in window | stage | C | ask | 79 |
| fallbacks-015 | `crates/manifold-core/src/phong_migration.rs`, `loader.rs:369-411` | 3.8c | Phong in 1 test project (fluidTesting); 48 online-only files saved while Phong existed, including the Corrosion and Right Where I Need You videos | stage | C | ask: convert the online-only 3D projects once, then delete | ~1,150 |
| fallbacks-016 | `scene_object_migration.rs`, `graph_loader.rs:519-526`, `project_io.rs:529` | 3.8c | old per-object wiring in 3 Downloads test projects; 37 online-only saved before 07-17; runs at load and again at every build | stage | C | ask | ~860 |
| fallbacks-017 | `crates/manifold-node-engine/src/load/graph_loader.rs:496-546`, `load/migration.rs`, `crates/manifold-core/src/type_id_migration.rs` | 3.8b | renames and `GraphMigration` passes run on a copy at every build; embedded presets and effect graphs are never rewritten, so Corrosion V5 (saved 09-11) still holds `node.channel_mix`, renamed 07-03; 72 of 114 renamed ids are in no project; the module doc says the table is empty | stage | C | simplify: apply every graph migration once at load to every graph in the project (layer, effect, clip, embedded preset) so the next save writes current ids; then the table can shrink after a convert-once pass | ~0 now |
| fallbacks-020 | `crates/manifold-app/src/content_export.rs:440-447` | 3.8a | export loop drains the command channel and keeps only CancelExport: Shutdown, FramePresented and any edit that slips past the modal are dropped with no log; breaks "commands are never dropped" | stage | B | simplify: buffer non-cancel commands and replay them after export | ~0 (fix) |
| fallbacks-021 | `crates/manifold-io/src/archive.rs:84-98, 226-237` | 3.8a | `read_manifest` turns every error into None; a save over a V2 archive with an unreadable manifest starts an empty history and the copy step drops every old snapshot | stage | B | simplify: a V2 archive whose manifest can't be read refuses the save with an error | ~0 (fix) |
| fallbacks-022 | `crates/manifold-editing/src/service.rs:357-359` (and 524, 706, 855), `crates/manifold-core/src/selection.rs:118-123` | 3.8a | UI prunes `selected_layer_ids` but not `start_layer_id`/`end_layer_id`; a stale end id collapses the range, both stale gives `unwrap_or((0, 0))`, so delete, split, duplicate and copy act on layer 0 | prep | B | simplify: iterate `selected_layer_ids`; none resolve, no commands | ~10 |
| fallbacks-023 | `crates/manifold-app/src/content_commands.rs:337-352` | 3.8a | warmup pops a queued command and re-sends it to the back, reordering the FIFO on every project open (SeekTo, ResizeContent and SetProject are always queued behind LoadProject) | stage | B | simplify: hold drained commands in a local Vec and replay them in order | ~6 |
| fallbacks-024 | `crates/manifold-editing/src/commands/` (101 of 150 commands without `was_applied`) | 3.8a | a command built from a stale snapshot finds no target, does nothing, still counts as applied and pushes a dead undo step; four tests pin the no-op | prep | B | ask: make `do_execute` return a rejection on a missing target (`fire_param.rs` is the model) | ~0 (fix; ~60 test lines shrink) |
| fallbacks-025 | 33 `do_undo` bodies, `let _ = with_existing_target_graph_mut(…)` | 3.8a | undo silently skips when the graph is gone, then moves to the redo stack, so history and project disagree | prep | B | simplify: expect, the undo order guarantees the target | ~0 (fix) |
| fallbacks-026 | `crates/manifold-editing/src/commands/effects.rs:1466-1475`; `graph/scene.rs:450, 793, 1249` | 3.8a | creates the private graph first, then bails on an unknown binding or render node: the layer stops tracking its preset, undo can't restore it, and the step counts as applied | prep | A | simplify: check before writing, reject on a miss | ~30 (tests shrink) |
| fallbacks-027 | `crates/manifold-editing/src/commands/graph/node_edit.rs:130-139` | 3.8a | unresolved target only prints; `rejection` exists but stays None, so a dead "Add Graph Node" undo step is recorded | prep | A | simplify: set `rejection` | ~0 |
| fallbacks-028 | `crates/manifold-core/src/types.rs` (10 enum deserializers, e.g. 190-215), `percussion_analysis.rs:41`, `preset_type_id.rs:313, 331` | 3.8a | unknown values become a default (a corrupt `layerType` loads and saves as Video; a non-integer number becomes effect id 0, Transform); the forward-version guard already refuses newer files; the `"Led"` alias was never written by any build (Serialize is int; 0 corpus) | stage | B | simplify: unknown value is a deserialize error; drop the `"Led"` arm and its test | ~-20 net |
| fallbacks-029 | `crates/manifold-node-engine/src/runtime/build.rs:455, 569` | 3.8a | generator build drops a card binding whose node is missing (a dead fader on stage); the effect path records the same case as a chain error and the linter calls it an error; 11 dangling bindings in 3 corpus projects | stage | B | simplify: record the chain error, as the effect path does | ~0 |
| fallbacks-030 | `crates/manifold-node-engine/src/runtime/groups.rs:144-194`, `core.rs:943-966` | 3.8a | an edited effect graph that fails to splice is swapped for the stock preset; non-fatal chain errors only reach stderr (no product reader of `PresetRuntime::errors`); named by BUG-38rup (Effect chains: effects depend on neighbours, silent fallbacks kill params) | stage | C | per BUG-38rup: chain builds fully or fails loud on the card | ~25 |
| fallbacks-031 | `crates/manifold-node-engine/src/exec/bound_graph.rs:542-577` | 3.8a | `apply_inner_param_overrides` swallows a flatten error and every `set_param` error, so an edited inner value may never reach the kernel; named by BUG-38rup | stage | B | per BUG-38rup | ~4 |
| fallbacks-032 | `crates/manifold-playback/src/engine.rs:183`, `crates/manifold-app/src/content_commands.rs:1011-1470` | 3.8a | `project: Option<Project>` is always Some in the app; Execute, ExecuteBatch and Undo would silently drop a command on None; six "Project is no longer available" strings and two magic defaults guard it | stage | B | simplify: engine owns a `Project`; delete the None arms | ~200 |
| fallbacks-033 | `crates/manifold-app/src/project_io.rs:1063-1100` | 3.8a | pasted video files add the library entry and auto layers to the UI copy only; the content thread skips the unknown clip, so it renders black | prep | B | simplify: route paste through `import_video_files`, as drops do | ~60 |
| fallbacks-034 | `crates/manifold-app/src/project_io.rs:1426-1438` | 3.8a | an unreadable audio file returns duration 0, which Replace Audio saves and a drop turns into a 0-beat clip; one test pins the zero | prep | B | simplify: return an error, alert like video import | ~15 |
| fallbacks-035 | 11 sites, e.g. `scene.rs:257` | 3.8a | edits invent `PresetMetadata { id: "UnnamedScene" }` for a scene graph with no metadata; 0 corpus graphs and 0 presets lack metadata; it serves test fixtures | prep | B | delete: reject the edit; fixtures get metadata from one helper | ~190 |
| fallbacks-036 | `crates/manifold-core/src/effects/instance.rs:761-786`, `crates/manifold-node-engine/src/load/loaded_preset_view.rs:137-143`, `runtime/build.rs:472-480` | 3.8a | an orphan binding gets range 0..1 and a blank label; 2 such bindings in 019_ImageTesting | stage | B | ask: report and drop orphan Node bindings; keep identity for Composite | ~12 |
| fallbacks-037 | `crates/manifold-editing/src/commands/graph/mod.rs:139-180` | 3.8a | scene-modifier reconcile failure after the edit landed only prints; host manifest and graph drift (48 callers) | prep | B | simplify: reconcile the candidate before commit and reject | ~0 |
| fallbacks-038 | the Split Object command (deleted) | 3.8a | `.ok()` merges "not physical" with "malformed binding", so a malformed physical object splits into pieces without physics | prep | B | resolved: the command is deleted; Shatter replaced it | 0 |
| fallbacks-039 | `crates/manifold-node-engine/src/load/chain_spec.rs:112-131` | 3.8a | splice error collapses to Option; a def with no wire into `final_output` returns None with no log | stage | B | simplify: return `Result` with a variant for the missing output | ~10 |
| fallbacks-040 | `runtime/build.rs:354`, `runtime/core.rs:990` | 3.8a | second group flatten `.ok()`'d: on failure the binding-capture migration, shadow lints and string-binding seeds are skipped silently | stage | B | simplify: propagate the error (generator), expect (effect) | ~12 |
| fallbacks-041 | `crates/manifold-node-engine/src/load/graph_loader.rs:736-746` | 3.8a | an unknown name in `exposedParams` is skipped while an unknown param fails the load; 0 of 138 corpus exposures are off-surface | stage | B | simplify: `UnknownParam` like the params loop | ~3 |
| fallbacks-042 | `crates/manifold-core/src/effects/instance_serde.rs:520-525` | 3.8a | unknown param ids are dropped with an eprintln only; unknown effects and unresolved templates do reach the load report | stage | B | ask: count drops into `load_report` | +5 |
| fallbacks-043 | `crates/manifold-io/src/loader.rs:42-63` | 3.8a | any ZIP error falls through to a UTF-8 or JSON error, so a damaged show file reports the wrong cause | stage | B | simplify: plain JSON only when the bytes lack the `PK` signature | ~0 |
| fallbacks-044 | `crates/manifold-editing/src/service.rs:161-213` | 3.8a | debug overlap check only warns "for projects saved before the fix", but load repairs overlaps, so a hit is a write-path bug | dev | B | simplify: `debug_assert!` | ~8 |
| fallbacks-045 | `commands/graph/expose.rs:68, 195, 349`; `commands/layer.rs:673-678`; `freeze/install.rs:225-230, 277-280`; `project_io.rs:49-53`; `graph_loader.rs:1498-1500`; `runtime/binding_shape.rs:30-34` | 3.8a | six branches for states that cannot happen: an Option always Some, an index after a non-empty check, serialization of string-keyed maps, a plan step from this graph, a re-check of a checked list | prep | A | simplify: plain types and `expect` | ~25 |
| fallbacks-046 | `crates/manifold-editing/src/commands/preset/modifier.rs:252-268` | 3.8a | schema and reconcile errors become a generic "invalid recipe" | prep | A | simplify: return the message as the rejection reason | ~0 |
| fallbacks-047 | `crates/manifold-app/src/editor_bridge.rs:333-399` | 3.8a | lax preview resolver falls back to the root level for a stale scope and can preview the wrong node; its strict twin exists | prep | B | delete lax trio, use `resolve_preview_target_with_scope` | ~67 |
| fallbacks-048 | `crates/manifold-core/src/percussion_binding.rs:48-91` | 3.8a | every binding is built with `video_clip_id: None`, so the "first clip on the layer, else first library clip" fallback is the only video path | prep | C | ask: is video-clip percussion placement a feature? | ~45 |
| fallbacks-049 | `trigger_source.rs:101, 152`, `effects/instance.rs:1425`, `manifold-playback/src/modulation.rs` ×6, inspector projection | 3.8a | absent `trigger_mode` means Both or Transient depending on the reader; inspector and playback can disagree | stage | B | merge: one `fire_mode(is_gate)` owner | ~0 |
| fallbacks-050 | `content_commands.rs:1538`, `content_pipeline.rs:3793` | 3.8a | render scale stored with clamp 0.01..1, rendered with 0.25..1; UI offers only 1, 0.75, 0.5 | stage | B | simplify: one clamp at the edit boundary | ~2 |

**Lines.** Grade B deletes and simplifications in migrations: about 1,530. Grade C migrations, Peter's call: about 4,040. Silent fallbacks: about 650 removable (fallbacks-032, 035, 033, 047, 048, 045 and smaller), the rest are fixes that remove no lines but stop a bug hiding. Old formats below 1.11, if converted once: about 4,500 more.

## The over-engineering patterns behind these

- **Five migration homes.** A rename or reshape can land in the JSON ladder, a tolerant serde reader, a post-load pass in core or io, an app pass that needs the registry, or a graph pass that reruns every build. Each new one picks a home; nothing says which. Rule: one load pipeline in a fixed order, and every pass writes into the project so the next save carries the current shape.
- **Migrations that never bake.** Graph renames, `scene_object_migration` and the nine `GraphMigration` passes rewrite a copy at build time. The file keeps the old shape forever, so the pass can never retire (fallbacks-017). Rule: a pass that does not write back needs a named reason.
- **Repair beside the fix.** `reconcile_generator_identity` and the 1.7→1.8 rung repair the same bug in two places; scene object wiring is migrated at load and again at every build. Rule: one repair per bug, at the earliest seam.
- **Migrations for shapes that lived a day.** WireframeDepthGraph, feature kinds, send sources, U5 `audioTrigger`, glTF anim tables, embedded Math View: each shipped for one to five days on one machine, then got a permanent migration and tests. Rule: a shape that never left Peter's machine is converted by hand with `project_tool`, not by a permanent pass.
- **No-op instead of reject.** About a hundred edit commands and 33 undo bodies silently do nothing on a missing target and still count as applied; tests pin the no-op as the contract. Rule: a command whose target is gone rejects, and the undo manager drops it.
- **Defaults for impossible states.** `Option<Project>` always Some, `inner_meta` always Some, an invented "UnnamedScene" identity, `u64::MAX` cache keys, a `"Led"` alias nobody writes. Rule: make the impossible state unrepresentable, or `expect` it with the reason.
- **A test that pins the fallback.** LED_STRIPS_DESIGN pins "the silent-fallback-to-Video failure mode" with a test instead of removing it; four editing tests pin the stale-target no-op. Rule: a test may pin an error, never a silent default.

## Kept

- `PhysicsSettings::legacy()` and `SimRate::legacy()`: 168 projects have no physics block (pilot).
- Every keep row in the cost table: saved files carry the input.
- Display, audio, per-frame and media fallbacks: `tempo.rs` BPM lookups, NaN-safe sort keys, layer-skin 1×1 texture, missing VideoClip warn-and-skip, per-frame `set_param` for time and beat, preset hot-reload keeping the last good catalog, fusion refusals rendering unfused, a failed chain build passing the layer through (and showing on the card).
- Saver and collect filesystem fallbacks (canonicalize, hash, size) and best-effort temp cleanup.
- `raw_project_version` → "1.0.0": an absent version means a pre-version V1 file; all 182 corpus files carry one.
- `migrate_if_needed` returning the input on a parse error: the typed deserialize then fails with the real message.
- `strip_unknown_effects`, `purge_orphaned_references`, `placeholder_spec`: lossy, but reported in `load_report`.
- Absent-list-means-empty `unwrap_or(&[])` and documented absent-field defaults (`exposed` true, `node_id` from handle, Texture3D format).

## Seen, not judged

- **Opening an old show deletes clips.** `repair_overlapping_clips` removes the shorter clip of every overlapping pair. 38 corpus projects overlap, all 1.0 or 1.1: WAYPOINTS 297 pairs, PATTERN 573, STAGNATE 983, STAGNATE V3 3,232. The count reaches the "opened with repairs" toast, but the clips are gone after a save. Not a fallback; Peter should know.
- `clear_legacy_rate_on_flags` keeps `AudioModShape::rate_of_change` compiled "for a possible future re-wire" though no UI can set it (patterns, 3.9).
- `Project::validate()` only logs what `purge_orphaned_references` then removes (3.11).
- Three copies of `max_node_id_recursive` (scene object migration, glTF AO migration, `modifiers.rs`) and of "node id defaults to handle" (3.11).
- `PresetRuntime::try_build` has no product caller (3.2).
- `SelectionRegion::new_with_ids` and `set_with_ids`, `BlendMode::from_index`, `MarkerColor::from_index` have no callers (3.1).
- Ableton mapping targets address effects by type, so a second effect of the same type on a layer is unreachable (design).
- `AddClipCommand` no-ops on a missing layer, which amplifies fallbacks-023 and 033.
- `BackgroundWorker::try_recv` treats a dead worker like "no result yet" and never logs.
- Empty-string effect `groupId` in 10+ projects works only because the lookup fails.
- Each archive save reads the file four times; `instantiate_def` clones every def for the scene object pass (performance).
- `type_id_migration.rs` module doc says the table is empty in shipped builds; it holds 114 entries. Census section 2 says 44 of 116 old ids appear in projects; the right count is 42 of 114 (the screen caught the two `RETIRED_PARAMS` rows).

## Not covered

- The 67 online-only Dropbox projects (dated, never opened). Every C row turns on them.
- Passes not measured against the corpus: glTF material upgrade, embedded scene-bounds backfill (reads the GLB on every load), scene camera and exposure prep, legacy scene modifier adoption, glTF AO mask, the six water graph passes. Measuring them needs a `project_tool` load, which this sweep did not run.
- Nothing was built, run or loaded; every reachability claim is from reading code. fallbacks-020, 022, 023 and 033 are the ones to reproduce first.
- Silent fallbacks outside core, io, editing, node-engine load and graph build, and the app's load and command glue: the per-frame execute path, compositor, playback (beyond fallbacks-032 and 049), UI, water and scene crates, and `physics_match.rs` and the fluid commands in editing.
- Non-undo writers outside `MutateProject` (percussion replan, recording, MIDI phantom commit) for fallbacks-025.
