# Simplification census: content sweep, proof

Main commit 37a9c180a. Candidate table: `content.md`. This file holds the commands behind each row and a one-line summary of each output.

## Shorthand

```
REPO="/Users/peterkiemann/MANIFOLD - Rust"
CORPUS=/tmp/simplification_census/corpus/projects
Q1: rg --hidden -g '!.git/' -g '!plugins/vendor/' -l -F '"ID"' "$REPO"
Q2: rg -l -F '"ID"' $CORPUS
QR: rg --hidden -g '!.git/' -l -w -F ID "$REPO/crates" -g '*.rs'          (unquoted id or struct name, Rust only)
HJ: git -C "$REPO" log --format='%h %ad %s' --date=short -S'"ID"' -- '*.json' ':!docs/node_catalog.json'
HO: git -C "$REPO" log --follow --diff-filter=A --format='%h %ad %s' --date=short -- FILE
```

Q1 and Q2 are the section 3.5 (Nodes, primitives and presets nothing uses) pair. Q1 always lists the node's own file and `docs/node_catalog.json`; the summaries below name only the other files. Q1 and Q2 for the node rows ran through a ten-line scratchpad loop (`id_search.sh`, not kept) because `raw/node_usage.tsv` gives counts, not file lists; nothing in `scripts/TOOLS.md` lists files per id. HJ answers "when did the last preset stop using it".

Scope-wide checks, run once:

- Corpus gap: `awk -F'\t' '{print $2}' corpus/skipped.tsv | while IFS= read -r p; do stat -f '%Sm|%N' -t '%Y-%m-%d' "$p"; done | sort`: 67 files, newest 2026-09-28 ("Corrosion Music Video V10 - Unified Thistles"), then 09-27 (HeadGlitchV2), 09-25 (HeadGlitch), 09-22 ("Corrosion Music Video V9 - MathView"). `stat` reads metadata only; no file was opened.
- HJ for each of the 90 ids with presets 0 and projects 0 in `raw/node_usage.tsv` (loop over `/tmp/simplification_census/content_unused_nodes.txt`): 56 never appear in any preset JSON in git history; 13 last left a preset in cb249a8a6; 14 in 9edb0198a (whitewater lifecycle, the hidden stage atoms); 2 in 8286d25c3; one each in f60596cd1, f4554bf6e, b2379b389, 6e580632a, 582914c17.
- `git -C "$REPO" show --diff-filter=D --name-only --format= cb249a8a6 | rg -c 'generator-presets/'`: 10; same for `reference-presets/`: 8 (WaveGrid, WaveRing, WaveSpiral, Garden, Lathe, Vine, Breathe, TwistColumn among them).
- Preset table: `fd -e json . assets/<folder> -x jq -r '[.presetMetadata.id, .presetMetadata.displayName, .presetMetadata.category, (.presetMetadata.available|tostring)]'` over the four folders: 39 effect, 55 generator, 18 scene modifier, 1 reference.
- How presets reach the browser: `crates/manifold-nodes/src/bundled_generator_presets.rs:1-17` (scanned from disk at startup, not embedded); `assets/reference-presets/README.md` (that folder is never scanned).
- Load path for a cut param: `crates/manifold-core/src/type_id_migration.rs:230-240`, `RETIRED_PARAMS` (a listed param loads with its value, wires and cards removed; an unlisted unknown param fails the load).
- Rename-with-seed mechanism: `type_id_migration.rs:197-218` (`PARAM_SEED_MIGRATIONS`) applied in `manifold-node-engine/src/load/graph_loader.rs:244-290`; seeding needs a `TYPE_ID_MIGRATIONS` rename and never overwrites a stored key.
- No load-time port rename exists: `rg -n -i 'port_rename|PORT_MIGRATIONS|rename_port|PortRename' crates -g '*.rs'`: only `manifold-core/examples/apply_grouping.rs`, an authoring tool.
- No per-type param visibility: `.claude/hooks/context-nudges/ui-invariants.md:15` "No conditionally visible UI"; `SCENE_SETUP_PANEL_DESIGN.md:263, 377` cite `feedback_no_conditionally_visible_ui`.
- Beads: `jq` over `.beads/issues.jsonl` for open issues whose title, description or notes contain each candidate name. Hits that name an item are cited in its row; substring noise is described under the row without an id.
- Denied commands: one, a stray `cd /dev/null` prefix on a read-only `rg` (persistent_cd_guard); rerun without it. One hook note flagged `${f#$R/}` as a comment in a loop; the command still ran and its output was used.

## Nodes (3.5)

### content-001
`node.watercolor`.
- Q1: `manifold-node-engine/src/descriptor.rs` (hand descriptor, "Legacy bundle pending decomposition"), `manifold-node-engine/src/freeze/classify.rs` (`CONVERSION_DEBT_LEDGER`, sole entry). Q2: 0.
- HJ on `node.watercolor"` filtered to `*Watercolor.json`: f4554bf6e 2026-05-29 "Decompose Watercolor into a feedback sim built from atoms". `jq -r '[.nodes[].typeId] | unique' effect-presets/Watercolor.json`: clamp, exposure, feedback, flow_field_noise, gaussian_blur, masked_mix, mix, scale_offset_image, slope_displace, uv_displace_by_flow, value; no `node.watercolor`.
- Projects that use the Watercolor preset do not embed its graph: of 11 corpus files naming `"Watercolor"`, 3 carry `"node.slope_displace"`, none `"node.watercolor"`.
- `rg -n 'include_str' watercolor.rs`: one shader, `effects/shaders/fx_watercolor_compute.wgsl` (424 lines); `rg -l fx_watercolor_compute crates`: watercolor.rs and `manifold-nodes/tests/support/custom_abi_cases.rs`.
- `rg -n 'ConversionDebt|CONVERSION_DEBT_LEDGER' crates -g '*.rs'`: the variant and ledger in classify.rs, watercolor.rs:340, the meta-test in `manifold-nodes/tests/contracts/freeze/classify.rs:14-60`, a comment in `region/census.rs` and `tests/contracts/freeze/region.rs:695`. With watercolor gone the ledger is empty and the variant has no holder.
- Other Rust mentions of "Watercolor" (`rg -n -w watercolor crates -g '*.rs'`) are the preset id in `param_storage_v14.rs:104`, the prewarm call at `manifold-nodes/src/registry.rs:81`, and comments about stateful feedback; none builds the node.
- Lines: 581 + 424 + about 60 (descriptor, ledger, variant, meta-test, prewarm, ABI case).

### content-002
`node.remove_drift_3d`.
- Q1: `manifold-nodes/tests/uniform_layout_proof.rs:572` only. Q2: 0. QR on `RemoveDrift3D`: own file only.
- HJ without the catalog exclusion: one hit, 3ccdd6a33, which touched only `docs/node_catalog.json`; `git log -S'"node.remove_drift_3d"' -- 'crates/*/assets/*'`: none, so no committed preset ever held it. `git show -s a8db47cad`: "node.remove_drift_3d ... is registered but UNCONSUMED: wiring it into the preset inverted and amplified the pooling ... the preset wiring was reverted and the atom stays on the shelf."
- Catalog: stratum `atom`, so it is in the palette. Created 2026-07-10, before the newest online-only project.
- Lines: 357 + 108 (shader) + 1.

### content-003
`node.render_copies` (file `render_instanced_3d_mesh.rs`).
- Q1: `manifold-core/src/type_id_migration.rs:124` (rename target of `node.render_instanced_3d_mesh`), `manifold-core/src/phong_migration.rs:124`, `param_tooltips_bulk.rs`, `tests/gpu_proofs/render_legacy_parity.rs`, `docs/landings/2026-07-11-realtime3d-p8-instancing.md`. Q2: 0. `raw/type_id_migrations.tsv`: `node.render_instanced_3d_mesh` 0 corpus files.
- HJ: last preset use removed in 6e580632a 2026-07-11 "REALTIME_3D section 10 P8: per-object instancing in node.render_scene". The landing doc lines 15, 47, 64: Garden rewired to `render_scene` with `instances_1`, `rg '"node.render_copies"'` on Garden zero hits.
- `render_legacy_parity.rs:91, 173`: an identity case comparing render_copies with render_scene.
- Bead check: BUG-5l4o (Material fidelity follow-through) description item 8: "`render_mesh` and `render_copies` upload only the older subset ... Consolidate material evaluation or make unsupported capabilities explicit at the connection."
- Lines: `fd -t f '^render_instanced_3d_mesh(\.rs|\.wgsl)$' crates -X cat | wc -l`: 464, plus the rename entry, the phong arm and the parity case.

### content-004
`node.dot_products`, `node.divide_by_value`.
- Q1 dot_products: `manifold-nodes-water/src/testkit/face_grid_scenes.rs`, `tests/.../liquid_extent.rs`, `tests/uniform_layout_proof.rs`, its own extent.rs. Q1 divide_by_value: `image_tests/divide.rs`, its extent.rs, `face_grid_scenes.rs`, `tests/contracts/freeze/region.rs`, `liquid_extent.rs`, `face_grid_extent_tests.rs`. Q2: 0 each.
- QR on `DotProducts`: own file, `manifold-water-gpu-flip/src/primitives/gpu_flip_atom_tests.rs`, `face_grid_scene_tests.rs`. QR on `DivideByValue`: own file, `image_tests/divide.rs`, `whitewater_emitters.rs`, `face_grid_scene_tests.rs`. All tests or testkit.
- `rg -n 'dot_products::|divide_by_value::' crates -g '*.rs'`: the dot extent's own `MAX_ROWS` import and the test imports above; no product caller.
- HJ: both last left a preset in 8286d25c3 2026-10-01 "Fold the GPU FLIP solve into one step node"; earlier d1fc047da "Retire the FFT pressure solve atoms"; HO dot_products: 29e09904a 2026-09-30 "SWASH P1: collar, chart and Krylov atoms".
- Bead check: two substring hits, one about the GPU FLIP per-body partial-sum cap and one about `node.combine_rows` fusion capacity; neither names these nodes as work.
- Lines: 146 + 73 + 20 + 84 + 14 + 10 + 32 = 379.

### content-005
`node.sample_triangle_grid`.
- Q1: nothing beyond own file and catalog. Q2: 0. QR on `SampleTriangleGrid`: own file only.
- HO: ce78a59d0 2026-09-13 "Add native Vortex Fragments Math View with sparse authored evaluation". HJ: 0e18b15b2 2026-09-13 "Register Math View ABI proofs and preserve the runtime file-size ceiling" removed it from JSON.
- The Math View nodes still in use are built by code: `rg -n -F node.sample_mesh_triangles crates -g '*.rs'` hits `load/expand/compiler.rs:739, 745` and `fragment_cuts.rs:686`; `node.render_mesh_diagram` hits `fragment_cuts.rs:650`. `node.sample_triangle_grid` has no such builder.
- Lines: 95 + 14.

### content-006
The four noise aliases.
- Q1 perlin, simplex, fbm: `descriptor.rs:389-414` and `noise.rs:197-217`. Q1 hash: also `generator-presets/OilyFluid.json:1758`. Q2: perlin 0, simplex 0, fbm 0, hash 4.
- `noise.rs:186-237`: four `inventory::submit!` factories (at 195, 203, 215, 227), each `Noise::new()` with `default_type` 1 or 2 or `default_octaves` 4; comment "Saved projects referencing the four pre-merge noise nodes resolve to this primitive with the matching defaults baked in. Old presets never stored a `type` param."
- `jq` over the catalog: `node.noise` params `type` (default 0), `octaves` (default 1); ports match because the aliases construct `Noise` itself.
- `rg -n 'default_type|default_octaves' noise.rs`: 18 lines carry the per-instance defaults.
- Mechanism: see the scope-wide `PARAM_SEED_MIGRATIONS` note; its one entry folds `node.rotate_vec2_90` into `node.rotate_vector` with a seeded angle.
- `rg -n 'perlin|simplex|fbm|hash_noise' type_id_migration.rs`: only `simplex_noise_force_*` and `*_per_instance` renames; none for these four.

### content-007
`node.texture_sum_5`.
- Q1: `param_tooltips_bulk.rs:261`, `image_tests/codegen.rs:551`, `tests/support/texture_abi_cases.rs:662`. Q2: 0. QR on `TextureSum5`: own file.
- HO multi_blend: 5076ee040 2026-05-31 "Add node.multi_blend (dynamic N-input sum); retire the fixed texture_sum_5". Catalog purpose: "Legacy fixed five-input sum, superseded by node.multi_blend ... still loads in saved graphs."
- `jq` ports: texture_sum_5 inputs a, b, c, d, e, param divisor; multi_blend inputs in_0 to in_4, params num_inputs and divisor. A `TYPE_ID_MIGRATIONS` rename would drop every wire.
- Lines: 110 + 40 + 8 + entries.

### content-008
`node.cut_out_box`.
- Q1: nothing beyond own file. Q2: 0. HJ (whole repo): f2beaf3d7, 7e45247e2 (Ocean P3), 6e42a7161 (Ocean Cliff P5); the last removed it.
- `OCEAN_SURFACE_DESIGN.md:129-143`: D7 chose the cut-out, then "Superseded for Ocean Cliff (2026-10-07, look gate) ... `node.cut_out_box` stays as a generic atom with no preset using it."
- Lines: 370 + 28.

### content-009
`node.reflect_array`.
- Q1: `tests/contracts/freeze/region.rs`, `docs/SCENE_MIRROR_DESIGN.md`. Q2: 0. `SCENE_MIRROR_DESIGN.md` status: P0 and P1 on main, P2 blocked.
- Bead check: open BUG-yqce (scene_mirror re-home after kind-framework removal): "P1 (node.reflect_array atom) is on main and fuses ... (B) hold and spec in a design session. Lead recommends B."
- Lines: 1,075 + 80.

### content-010
3D geometry atoms.
- Q1 for each of `node.copy_positions`, `node.displace_copies`, `node.wave_field_3d`, `node.scatter_on_mesh`, `node.revolve_curve`, `node.tube_from_path`, `node.extrude_curve`, `node.spawn_from_mesh`: own file and catalog only (census screen: no Rust reference outside its own file). Q2: 0 each.
- HJ: copy_positions, displace_copies, wave_field_3d, scatter_on_mesh, revolve_curve, tube_from_path last left JSON in cb249a8a6; extrude_curve and spawn_from_mesh only ever in catalog JSON (41553f158, 6392947f8).
- HO: copy_positions, displace_copies, wave_field_3d from 8796c6375 2026-09-11 "Add reusable wave motion atoms and three playable scene presets" (WaveGrid, WaveRing, WaveSpiral, all deleted in cb249a8a6); extrude_curve, revolve_curve, tube_from_path from 41553f158 (MESH_DEFORM P3); scatter_on_mesh from 4615e75cb (MESH_DEFORM P4, Garden.json); spawn_from_mesh from b893ad10d.
- Bead check: three substring hits for scatter_on_mesh, about codegen wording and first-render stalls; none plans this node.
- Lines: `fd -t f '^ID(\.rs|_body\.wgsl|\.wgsl)$' crates -E target -E tests -X cat | wc -l` per id: 125, 268, 248, 911, 453, 548, 405, 711 = 3,669.

### content-011
`node.film_grain`.
- Q1: `param_tooltips_table.rs:83`, `texture_abi_cases.rs:298`, `image_tests/codegen.rs:352`. Q2: 0.
- HJ: 582914c17 2026-05-30 "Watercolor: half-res warp-free flow, single instance, drop grain pass". `jq` FilmGrain.json node types: channel_mix, gaussian_blur, math, mix, noise, texture_size; HJ on FilmGrain.json for `"node.film_grain"`: no commit.
- Lines: 147 (file and two shaders).

### content-012
`node.peak`, `node.blob_overlay`.
- Q1 peak: nothing else. Q1 blob_overlay: `type_id_migration.rs:160` (rename from `node.blob_overlay_render`, 0 corpus files in `raw/type_id_migrations.tsv`), `param_tooltips_bulk.rs`, `texture_abi_cases.rs`. Q2: 0 each.
- HJ peak: only the catalog-era commit e1b8603ee. HJ blob_overlay: d9cbd3bf6 2026-07-03 (the vocabulary rename).
- Lines: 314 + 226.

## Presets (3.6)

### content-013
`WaterDamBreakMatter`, `WaterStillPoolMatter`, `WaterFloatingBoxMatter`.
- `rg --hidden -g '!.git/' -l -w -F ID "$REPO/crates" -g '*.rs'` per id (saved to the scratchpad `preset_searches.txt`). Every hit is one of: a test module or test file; testkit (`manifold-nodes/src/testkit/assets.rs`, `liquid_conformance_fixtures.rs`, `manifold-nodes-water/src/testkit/face_grid_scenes.rs`); the probe example `manifold-nodes/examples/fluid_capture.rs`; the dev subcommand `manifold-app/src/ui_snapshot/fixtures.rs:78`; `manifold-app/src/gpu_flip_export_demo.rs` (`#![cfg(all(test, feature = "journey-proofs", target_os = "macos"))]`); or an `examples: [...]` string in a primitive declaration (`grid_to_matter.rs`, `matter_*.rs`, `clamp_liquid_to_solids.rs`, `liquid_solid_distance.rs`, `whitewater_obstacle_source.rs`, `zero_array.rs`).
- App-side hits checked for test gating: `ui_bridge/project.rs:2249` sits after `#[test]` at 2219; `projection/scene.rs` hits at 696 and 795 follow `#[cfg(test)]` at 250; `projection/scene_card.rs` hits at 391 and later follow `#[cfg(test)]` at 286; `retired_cpu_flip.rs:133` is an assert.
- Q2: 0 each. HO: WaterDamBreakMatter f58e2aa97 2026-09-22; WaterStillPoolMatter 2eb9abc24 2026-09-30 "MPM P1 (6/n)"; WaterFloatingBoxMatter 147a3296f 2026-09-30 "GPU MPM P2b".
- Intent: memory `project_fluid_gpu_campaign.md` line 13, "MPM keeps goo/snow/sand/lava and melt, its water presets stay as test scenes". `manifold-app/src/ui_bridge/project.rs:14-16`: "The one liquid Add Fluid authors: GPU FLIP. No solver picker."
- Beads naming them (MPM test-scene work, unaffected by the move): BUG-75ko (MPM still pool surface is rough at rest), BUG-g93n (MPM P1 remaining deliverables after the kill-check call), BUG-5svo (Impulse cannot be fired at a body coupled to Live Matter water), BUG-cu6l1 (GPU FLIP over-noding).
- Files: 3 JSON (4,563 + 4,054 + 4,597 lines) and 6 thumbnail files (`fd` in `assets/preset-thumbnails/generators`).

### content-014
`WaterDamBreakParticles`.
- Rust loaders: `ui_bridge/project/tests/water_family.rs`, `ui_bridge/projection/scene.rs` (test module), `tests/renderer_contracts/gpu_proofs/liquid_conformance.rs`, `manifold-io/src/migrations/contacts_wire_v1200.rs:88` (inside `#[cfg(test)]` at 83), `manifold-nodes-water/src/presets/gpu_flip.rs:1006` (`PARTICLE_VIEW_PRESET`), and four catalog test files. Q2: 0.
- `gpu_flip.rs:1005-1056`: `particle_view_def()` and `particle_view_cards()` are `#[cfg(any(test, feature = "testkit"))]`; the shipped JSON is checked equal to the builder (`gpu_flip_preset.rs:600-611`).
- Physics View: `gpu_flip.rs:598-609, 809-822` add a `physics_view` toggle card to the Water section of the shipped Dam Break; `scene_card.rs:396` asserts the Water card rows are "Physics View", "Speed", "Reset".
- HO: 3a04062d6 2026-10-01; renamed in 2ae57e17f "Rename SWASH to GPU FLIP".
- Files: 1 JSON (3,460 lines) and 2 thumbnail files.

### content-015
Display names of `WaterDamBreakGpuFlip` and `WaterSeaWallGpuFlip`.
- `manifold-app/src/ui_bridge/project.rs:20-24` builds Add Fluid's body from `gpu_flip_liquid_body()`; `gpu_flip.rs:574-590` reads `SHIPPED_PRESET` = `WaterDamBreakGpuFlip` for the surface group. Q2 for WaterDamBreakGpuFlip: 9.
- Sea Wall: Rust loader only `examples/fluid_capture.rs:1567`; `git log --follow` on its JSON: 20dde8731 "Sea Wall (GPU FLIP) preset: 8 x 3 x 4 m tank at Resolution 112, collapsing column surges into a wall", db7c6f928 "Sea Wall cinematic pass". Q2: 0.
- Display names from the preset table above. Changing `displayName` leaves `id` and every saved reference intact.

### content-016
Hidden LED presets.
- Q1 per id: own JSON, thumbnails, `manifold-app/tests/renderer_contracts/led_preset_value_tests.rs`; LED Pulse also `manifold-ui/src/panels/picker_core.rs:425, 433` (a picker test fixture label). Q2: 0 each.
- `git log -S'"available": false'` per file: 28c3548c2 2026-09-05. Its message: "Chase (consolidates Chase Sweep / Step Chase / Step Scan) ... Superseded presets hidden via available:false (ids stay loadable for old projects)."
- Files: 4 JSON and 8 thumbnail files.

### content-017
Hidden scene-modifier presets.
- Q1 per id: `manifold-nodes/src/bundled_presets.rs:290-296` (test: "retained only for saved-project compatibility"), `manifold-app/src/ui_bridge/projection/cards.rs:1667` (test: "retired ... still in picker"), test files, and `scene_modifier_legacy_migration/fragment_masks.rs:16-22` (MaskedPeel and OrderedReconHit in `REPAIRED_STOCK_IDS`); SurfacePeelHit also `manifold-app/src/modifier_preset.rs:126` (test). Q2: 0 each (`raw/node_usage.tsv` projects column).
- `git log -S'"available": false'` per file: 3a21c2d67 2026-09-14 "Consolidate scene modifier cards with live clip trigger controls".
- Lines: `wc -l` 738 + 1,175 + 754 + 973 = 3,640.

### content-018
`ReactionDiffusion` reference preset.
- QR on `ReactionDiffusion`: `manifold-core/src/preset_type_id.rs` only (`REACTION_DIFFUSION`, legacy numeric index 11 at `:207`). Non-Rust: the JSON, the folder README, `docs/VISUAL_PIECES_GRAPH_DRAFTS.md`, beads. Q2: 0.
- `assets/reference-presets/README.md`: "not scanned by the preset loader ... shelved same day on Peter's look-pass: 'shows a circle and then fades out to black, not a great visual.'"
- `fusion_proof.rs:1118` and `stock_preset_round_trip.rs:14` mention the folder in comments only.
- `bd show BUG-9wa4` (ReactionDiffusion runs 4 substeps per display frame): open, P3.
- Lines: 1,020 + 20.

## Params (3.7)

Shared commands for this section:

```
jq -r --arg n TYPE '.nodes[] | select(.type_id==$n) | .params[] | [.name, .label, (.type|tostring), (.default|tostring), (.range|tostring)] | @tsv' docs/node_catalog.json
rg -l -F '"TYPE"' $CORPUS | wc -l
```

Corpus files holding each node: `node.light` 68, `node.bake_environment` 68, `node.pbr_material` 68, `node.physics_world` 16, `node.gpu_flip_domain` 11, `node.whitewater_step` 9. Any cut param of these needs its `RETIRED_PARAMS` line.

What the Scene tab shows comes from one table, `crates/manifold-nodes-scene/src/node_graph/scene_surface.rs` (`scene_param_facts`, `:101-135`); `setup(...)` rows show in the Scene tab, `card(...)` rows also on the card, `hidden(...)` rows nowhere.

### content-019
`node.light` type-specific params.
- Catalog: mode (Type) enum 0 to 2, range 30, falloff enum, inner_cone_angle 0, outer_cone_angle 0.785, plus position, target, colour, intensity, shadows, light_size.
- `light.rs:9-22, 36-37`: modes Sun, Point, Spot; falloffs "Legacy", "Inverse Square"; "`range` means different things per mode (Sun: shadow ortho half-extent, Point: attenuation half-distance)".
- `manifold-node-engine/src/scene/light.rs:54-60`: "`Legacy` preserves the saved-project half-distance curve; `InverseSquare` is the glTF physical falloff". `:347-370`: Sun attenuation is always 1; falloff and the cone factor apply to Point and Spot only.
- `scene_surface.rs:227-246`: `"mode" | "falloff" | "inner_cone_angle" | "outer_cone_angle" => setup(Light)` for every light.
- Bundled lights: `jq` over every preset holding `"node.light"`: mode 0 everywhere except one mode 1 in PhysicsSolids; no preset sets falloff or a cone. `gltf_import/scene.rs:444`: imported lights get `falloff` 1.
- Visibility rule: see the scope-wide note.

### content-020
`node.light` shadow internals.
- Catalog: shadow_bias 0.003 (0 to 0.1), shadow_resolution 2048 (128 to 8192). `light.rs:227` composition notes: "`shadow_resolution` rarely needs perform-time control".
- Same `jq` pass: WaterDamBreakGpuFlip, WaterSeaWallGpuFlip, the Matter scenes and Particles set shadow_bias 0.001; MetallicGlass 0.003; Lantern shadow_resolution 4096.

### content-021
`node.bake_environment` sun.
- `bake_equirect_envmap.rs:160-203` (sun_x, sun_y, sun_z, sun_disc_intensity default 0, sun_disc_size default 0); composition notes at `:236`: "Sun disc direction (`sun_x/y/z`) is consumed as-is ... F-P4 is responsible for binding it to the scene sun."
- `gltf_import/scene.rs:726` and `gltf_import/tests.rs:1858`: the importer binds light pos_x/y/z to sun_x/y/z. `manifold-app/src/ui_bridge/projection/scene.rs:423`: a test about the "sun-coherence fan-out".
- Presets: `jq` for bake_environment params and sun bindings: Lantern binds `1_sun_*` and `7_pos_*` together; PhysicsBoxes stores only fill, intensity, mode, so its disc intensity is 0.

### content-022
`node.bake_environment` mode split.
- `bake_equirect_envmap.rs:33-35` (mode 0 gradient "legacy studio", 1 softbox), `:65` purpose (emitters and sun disc are softbox terms), `:205-215` (Fill: "Gradient mode ignores it").
- Presets: MetallicGlass stores azimuth_variation, height, horizon_strength, width and no mode (Gradient); PhysicsBoxes, Scene, Lantern and WaterDamBreakGpuFlip store mode 1. WaterDamBreakGpuFlip stores width 1024, height 512.

### content-023
White Furnace.
- `bake_equirect_envmap.rs:49-56` (uniform field "RT_FURNACE_ORACLE ... a closed-form environment for correctness testing"), `:217-233` (param), `:317` (read).
- `rg -n '"uniform"' crates -g '*.rs'` filtered to environment use: `manifold-nodes/src/testkit/rt_dynamic.rs:34`, `manifold-app/tests/renderer_contracts/gpu_proofs/rt_dynamic_catalog.rs:152`, `manifold-nodes-scene/tests/gpu_proofs/rt_edc_enclosure.rs:355`, `rt_furnace_oracle.rs:263, 316, 433, 812`, and `scene_surface.rs:361` (`hidden(Environment)`, "White Furnace is a lighting test, not a look").
- Q2 on `"White Furnace"`: 0. Bead check: BUG-5l4o (Material fidelity follow-through) mentions white-furnace proofs as useful coverage, not this param.

### content-024
`node.physics_world` spawner.
- Catalog: gravity_x/y/z, speed, reset, copy_count "Count" 100, copy_spacing "Spacing" 1.25, copy_columns "Columns" 16, copy_layout "Arrangement" (Grid, Pile).
- `physics_world.rs:207` (`copies: RigidBody optional` input), `:433-463` (the copy values feed `RigidSceneInputs` beside `bodies` and `prototype`), `:352-355` (params).
- `scene_surface.rs:159-171`: copy_count and reset are card rows; spacing, columns, layout are Scene tab rows.

### content-025
Material feature modes.
- `pbr_material.rs:959-967`: seven `*_mode` params with values "From values", "Off", "On", "Removed" (`subsurface_feature_mode` elsewhere in the same list). `:1227-1236`: "`Removed` is a persisted compatibility value ... evaluate it exactly like `Off`".
- `manifold-ui/src/panels/scene_setup_panel/material_inspector.rs:654-686`: a hand-drawn "Enabled" label and button for every feature-mode row; `:1227-1260`: the toggle cycles 0 to 1, 1 to 2, 2 to 1. Test at `:1475` `material_inspector_removed_feature_stays_hidden_and_readds_without_reseeding`.
- Corpus: `rg -U -l '"(coat|iridescence|emission|glass|sheen|anisotropy|translucency|subsurface_feature)_mode": \{\s*"type": "[A-Za-z]+",\s*"value": (1|3)\b' $CORPUS`: 0. The 25 files that name `"glass_mode"` or `"coat_mode"` do so as binding targets, not stored values.

### content-026
Material Advanced group.
- `jq -r '.nodes[] | select(.type_id=="node.pbr_material") | .params[].name'`: 297 params.
- `rg -c` with the `advanced_map_metadata` patterns of `material_inspector.rs:113-158` (prefixes "", nrm_, mr_, occ_, em_ with uv_set or mip_filter; fourteen extension prefixes with uv_m00..uv_ty, tex_coord, wrap_u/v, mag/min/mip_filter): 178. Scalar Advanced names at `:41-44, 90` (ambient, specular, baked_look, normal_scale, occlusion_strength, specular_tint_r/g/b): 8. Total 186, the walkthrough's "(186)".
- `scene_surface.rs:145-157`: `rigid_body` already marks its import params (path, mesh_index, primitive_index, material_index, fit, recenter, translate_*) hidden.

### content-027
GPU FLIP solver settings.
- Catalog: resolution, solve_level "Coarse Solve" 0 (0 to 4), max_iterations 900 (1 to 900), sheet_fill_rate 0, particle_capacity "Max Particles" 0.
- `scene_surface.rs:413-425`: `flip_solver` = solve_level, max_iterations, sheet_fill_rate, particle_capacity, shown in the Scene tab and never on the card.
- GPU_FLIP_PRESSURE_CAP_DESIGN.md section 7 (Control contract: Max Iterations, default 900): the card and its migration. Memory `project_flip_scene_authoring.md` line 15: "Leave particle_capacity at 0 (the fill count) unless a scene has emitters."

### content-028
Whitewater settings.
- Catalog: 18 params on `node.whitewater_step`. `scene_surface.rs:398-410`: 14 shown as setup rows besides enabled and amount; capacity, min_energy, max_energy, preserve_foam hidden.
- Memory `project_fluid_gpu_campaign.md` line 14: "every GPU FLIP sim param ... equal what the FLIP Fluids engine uses".

## Rows considered and dropped

- WaterDamBreakGpuFlip as a test scene: dropped, Add Fluid reads it (content-015).
- Blob Track V2 as an engine comparison: dropped, `BLOB_TRACKING_V2_DESIGN.md` line 9.
- `MaskBlob` (effect id with no JSON, orphan-looking thumbnail): synthesized at load, `preset_loader.rs:690-717`.
- Whitewater stage atoms: kept by WHITEWATER_STAGE_FUSION_DESIGN.md D4 (the atom chain stays as the in-test oracle) and D9 (the 25 stage-internal atoms stay registered), lines 174 and 194.
- `node.render_mesh`: 1 preset, 2 corpus projects; named in BUG-5l4o item 8 too, but in use.
- `node.compose_vec3`, `node.cut_mesh_*`, `node.remap_*`, `node.render_mesh_diagram`, `node.sample_mesh_triangles`, glTF morph nodes, `node.plane_mesh`, `node.look_at_camera`: `rg -n -F node.ID crates -g '*.rs' -g '!**/tests/**'` finds product builders (`load/expand/compiler.rs:1223`, `fragment_cuts.rs`, `gltf_load.rs`, `layer_plane.rs:207`, `scene_camera.rs:130`).
- `force_lattices` on GPU FLIP and matter nodes (caught by a catalog name scan for debug, oracle, legacy and similar words): wired plumbing from the domain, not a Scene tab control.
- Duplicated card controls (the card's "Sun" is the light's Intensity, Exposure on card and Lens, camera framing on card and node): designed mirrors (`scene_surface.rs:227-232` `CardLabel::ItemName`, camera rows `card(...)`), not duplicate params.
