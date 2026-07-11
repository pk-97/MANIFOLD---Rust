# Realtime 3D — P8 (per-object scene instancing) landing

**Branch:** `feat/realtime3d-p8-instancing` · **Level reached:** L3 / target L3 (§10 — headless PNG read by the landing session; both the default-camera and a grazing-camera Garden render were inspected).
**Doc status line (quoted verbatim):** `Status: IN PROGRESS (status corrected + baseline-reviewed 2026-07-05; D3/D8 AMENDED 2026-07-06 by SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md — read its §8 before P6; D3/D4/§3/§6/§7.3 AMENDED 2026-07-10 (F2 coherence audit) — shadow-caster cap MAX_SHADOW_CASTING_LIGHTS = 4 replaces the dead "8 objects, 4 lights" budget, read D4 before P2). Shipped: P0 (MATERIAL M1–M6, all verified in-tree), P1 node.render_scene @ 8daa89fc, P4 camera atoms (both node.free_camera + node.look_at_camera in-tree), §9 node.spawn_from_mesh, P2 shadow maps + P3 atmosphere/fog @ feat/realtime3d-p2p3 2026-07-11 (gpu-proofs render_scene_shadows + render_scene_fog, PNG-verified; lights also moved to a ring-buffered storage buffer), P8 scene instancing @ feat/realtime3d-p8-instancing 2026-07-11 (§10 D11 — each object group grows an optional instances_n: Array(InstanceTransform) port; wired draws instance_count = buffer_size / 32 copies with model_n · T_instance in both the main pass and every caster's shadow pass; unwired binds a cached 1-entry identity stub, byte-identical to pre-P8 output; gpu-proofs render_scene_instances 4/4 green — identity parity, occlusion, instanced-shadow, instanced-fog; Garden.json re-wired single-pass, the node.mix Max-blend composite deleted). The P1 "transforms not port-shadowed" deviation is retired by amendment, not by shadows: per-object transforms move to node.transform_3d atoms feeding transform_n: Transform ports (SCENE_BUILD P2). Remaining: P5 viewport navigate, P6 gizmos, P7 scene starter preset. · designed 2026-07-03 · Fable`

## What shipped

**On stage:** `render_scene` objects can now carry any number of instanced copies (a scatter field, a crowd, a wall of repeated geometry) in the SAME shared depth buffer as every other object in the scene — a scattered instance behind a hill is correctly hidden, drops a real shadow, and fades into fog exactly like an ordinary object. Previously this required a second `render_copies` pass composited with `node.mix` in Max mode, which could not resolve occlusion between the two passes (a flower behind a ridge always drew on top). Garden — the reference scatter-field demo — is re-wired to prove it: one `render_scene` pass, terrain as object 0, flowers as an instanced object 1, the compositor deleted.

- **`instances_n: Array(InstanceTransform) optional`** added to every object group's port set (`rebuild()` in `render_scene.rs`) alongside `mesh_n`/`material_n`/`base_color_map_n`/`transform_n`. No migration needed — the port simply appears unwired on reload.
- **Instance TRS composes before the group transform:** `world = model_n · T_instance`, copied from `render_instanced_3d_mesh.wgsl`'s `euler_xyz` (forked, not shared, per this file's convention) into both `render_scene.wgsl`'s `vs_main` and `shadow_depth.wgsl`'s `vs_main`.
- **One always-instanced pipeline per `MaterialKind`** — no separate instanced/non-instanced variant. An unwired object binds a cached 1-entry identity stub (`pos_scale=[0,0,0,1]`, `rot_pad=[0,0,0,0]`) and draws with `instance_count=1`, following the same always-bind ABI-stub pattern the shadow bindings already use.
- **The shadow pass instances too:** `shadow_depth.wgsl` grew the same `Instance` buffer + `euler_xyz` + per-instance TRS, so a shadow-casting object drawn via `instances_n` still darkens the ground correctly.
- **No per-object `instance_count` param** (D11) — count is `buffer_size / 32`; density control lives on the producer (e.g. `scatter_on_mesh`'s port-shadowed `count`).
- **Garden.json** re-wired: object 0 = terrain (unchanged), object 1 = the flower mesh with `instances_1` wired straight to the existing `scatter_on_mesh` output; the `node.render_copies` "flowers" node and the `node.mix` Max-blend "composite" node are both deleted; `render_scene`'s `color` output feeds `final_output` directly.

## Gate results (verbatim)

**gpu-proofs (`cargo test -p manifold-renderer --test gpu_proofs --features gpu-proofs render_scene`, 10/10 green):**
```
test render_scene_fog::blue_fog_tints_the_scene_toward_the_fog_color ... ok
test render_scene_fog::density_zero_atmosphere_is_byte_identical_to_no_atmosphere ... ok
test render_scene_instances::far_instance_shifts_toward_the_fog_color ... ok
test render_scene_instances::instance_fully_behind_an_occluder_contributes_no_pixels ... ok
test render_scene_instances::instanced_occluder_still_casts_a_shadow_that_darkens_the_ground ... ok
test render_scene_instances::wired_identity_instance_buffer_renders_byte_identical_to_unwired ... ok
test render_scene_lights::eight_lights_render_past_the_old_cap_of_four ... ok
test render_scene_lights::zero_lights_render_without_validation_error ... ok
test render_scene_shadows::more_than_k_casters_still_render_finite_and_lit ... ok
test render_scene_shadows::occluder_casts_shadow_that_darkens_the_ground ... ok
test result: ok. 10 passed; 0 failed
```
- `wired_identity_instance_buffer_renders_byte_identical_to_unwired` — pixel-exact `assert_eq!` on the full readback, proving the "unwired object costs nothing observable" invariant at the byte level.
- `instance_fully_behind_an_occluder_contributes_no_pixels` — near-instance centre reads pure `(1.000, 0.000, 0.000)`, far-instance centre reads exactly the occluder's flat `(0.500, 0.500, 0.500)` — zero pixel contribution when occluded.
- `instanced_occluder_still_casts_a_shadow_that_darkens_the_ground` — luma off=12895.2 on=12443.4, drop=3.50% (identical numbers to the pre-existing non-instanced `render_scene_shadows` proof, confirming the instance path reproduces the object path exactly at identity).

**Focused (`cargo nextest run -p manifold-renderer -p manifold-app`): 1279 + 1 passed, 0 failed** (rebuild/port tests, `garden_preset_round_trip`).

**Full workspace (`cargo nextest run --workspace`, warm main checkout): 3066 tests run, 3066 passed, 8 skipped.** (One transient failure on the first pass — `manifold-core::params::tests::bench_resolve`, a nanosecond-level perf-ceiling benchmark unrelated to this diff — reproduced at 203.91 ns/op < 271.5 ceiling in isolation on a re-run; contention from the parallel cargo processes run during this session, not a regression.)

**`clippy --workspace -- -D warnings`:** clean.
**`cargo deny check bans`:** `bans ok`.
**`check-presets`:** `52 presets: 52 ok, 0 failed`.

**Negative gates:**
- `rg 'Arc<Mutex'` on all touched files → zero hits.
- `rg '"node.mix"'` / `rg '"node.render_copies"'` on `Garden.json` → zero hits (the two-pass composite is fully deleted, not paralleled).

## Deviations from brief

None. The brief's forbidden moves were all avoided: no reuse of `render_instanced_3d_mesh`'s shader/pipeline inside the scene, no depth output exposed for compositing, no per-object `instance_count` param, no CPU loop issuing one draw per instance, no non-uniform instance scale added.

## Shortcuts confessed (rolled up from phase reports)

None. The occlusion GPU test's first draft used a sampling window too large relative to the marker's screen-space footprint (diluted the "near" case's redness with correctly-rendered background) — caught and fixed before landing, not shipped as a gap.

## Verification debt

None opened, none carried.

## Click-script for Peter (≤2 minutes)

1. `cargo run -p manifold-renderer --bin render-generator-preset -- Garden --size 1024x768 --frames 3 --out /tmp/garden.png` — expect: terrain with scattered flowers, all correctly shaded and grounded, single render pass.
2. Open the Garden preset's graph in the editor — expect: one `node.render_scene` (objects=2), no `node.render_copies`, no `node.mix`; object 1's `instances_1` port wired to `scatter`'s `instances` output.
3. On any `render_scene` node, add a second object, wire a `node.scatter_on_mesh` into its new `instances_n` port, leave `transform_n` unwired — expect: the instanced copies render at the scene's origin, correctly occluded by/occluding every other object, and a live sweep of the scatter node's `count` fader changes instance density without needing to touch `render_scene` itself.
