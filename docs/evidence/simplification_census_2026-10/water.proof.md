# Simplification census: water sweep, proofs

Companion of `water.md` (BUG-yn979 (Simplification pass)). Main commit 37a9c180a; tree read at 821ffc20b (crates identical). Beads named here: BUG-hkbdp (Renderer crate split epic) and its child BUG-hkbdp.6 (Post-T2 water cleanup), BUG-65r39 (manifold-fluids proof-only), BUG-2fmpk (whitewater engine oracle).

Common commands, run for every DEAD and TEST-ONLY row (`REPO` = `/Users/peterkiemann/MANIFOLD - Rust`). The sweep ran the first with `-c` to get per-file counts and `sed` to strip the prefix; the hits are the same set:

```
rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F NAME "$REPO"
rg -l -w -F NAME /tmp/simplification_census/corpus/projects
```

`-w -F NAME` also matches the name inside a quoted string, a `stringify!` or a `paste!` argument, so serde-attribute, macro and string uses would have shown as hits. The corpus search printed 0 files for all 83 names. None of the nine crates has a `.m`, `.swift` or `.mm` caller (no such files hold any of the names).

## water-001

```
$ rg --hidden -g '!.git/' -n -w -F capture_face_v REPO          # 1 hit: particles.rs:82 (definition)
$ rg --hidden -g '!.git/' -n -w -e 'face-oracle' -e face_oracle REPO -g '!*.jsonl'
```
- `capture_face_v`: definition only; corpus 0.
- `face-oracle`: `particles.rs:73,77` (the two `cfg`), `Cargo.toml:12` (declaration), `native/bridge.h:130` (comment), `docs/WATER_CRATES_DESIGN.md:167` (one sentence listing the oracles), `scripts/fixtures/gate-p1.json` (a recorded old gate census). No other Cargo.toml forwards `manifold-fluids/face-oracle`; nodes-water forwards only `manifold-fluids/whitewater-oracle`.
- The Rust build reads it at `build.rs:160` (`CARGO_FEATURE_FACE_ORACLE`) and defines `MANIFOLD_FACE_ORACLE`, which gates `bridge.cpp:1718-1748` (31 lines) and `bridge.h:129-133` (5 lines).
```
$ git -C REPO log -S'capture_face_v' --format='%h %ad %s' --date=short
```
- Two commits: 949d6cb19 (2026-10-06, added with "Hydrostatic rest bound from native projected faces") and 6b2c54990 (2026-10-10, BUG-hkbdp.6.11 water test cut), whose diff removes the only call (`native.capture_face_v(&mut faces_v)` in `face_grid_scene_tests.rs`).
```
$ rg -o '.{80}face-oracle.{80}' REPO/.beads/issues.jsonl         # no match; no bead names the feature or the method
```
- Line count: 31 + 5 (native) + 20 (`particles.rs`: extern declaration 2, impl block 18) + 2 (Cargo.toml feature and its comment) + 3 (`build.rs`) = 61.

## water-002

```
$ rg --hidden -g '!.git/' -n -w -F UPSTREAM_REVISION REPO       # lib.rs:34, .beads/issues.jsonl:1
$ rg --hidden -g '!.git/' -n -w -F NUMERICS_REVISION REPO       # lib.rs:36, .beads/issues.jsonl:1
$ git -C REPO log -S'NUMERICS_REVISION' --format='%h %ad %s' --date=short
$ git -C REPO show 66fba804e -S'NUMERICS_REVISION' -p --format= | rg '^[-+].*NUMERICS_REVISION'
$ rg --hidden -g '!.git/' -n -F '70a0e954018fe39e1f9c3631264989569752bb7a' REPO -g '!*.jsonl'
```
- Definition only in code. The bead hit is a design note describing the old take header (`fluid/take.rs`, `fluid_cache.rs` persisted both); the same note lists them as persisted items, not as upcoming.
- 66fba804e (2026-10-10, BUG-hkbdp.6.6 CPU FLIP removal) deletes the two `numerics_revision: manifold_fluids::NUMERICS_REVISION` uses.
- The upstream commit hash is also in `crates/manifold-fluids/native/PROVENANCE.md:4,7` and `docs/WATER_SIMULATION_DESIGN.md:13`, so `UPSTREAM_REVISION` adds no credit that the repo does not already carry (THIRD_PARTY_NOTICES.md names the vendored engine separately).
- Lines: `lib.rs:34-36` (const, doc, const) = 3.

## water-003

```
$ rg --hidden -g '!.git/' -n -w -e SOURCE_IDENTITY -e ENGINE_SOURCE_IDENTITY REPO/crates REPO/scripts -g '*.rs' -g '*.py'
$ rg --hidden -g '!.git/' -n 'SOURCE_IDENTITY|source_identity' REPO -g '!*.jsonl' -g '!plugins/vendor/**'
$ fd -t f build.rs REPO/crates -d 2
$ rg -n 'emit_source_identity|emit_owned_source_identity' REPO/crates -g build.rs
```
- Four hits for the constants, all definitions: `manifold-core/src/lib.rs:6`, `manifold-physics/src/lib.rs:11`, `manifold-fluids/src/lib.rs:38`, `manifold-node-engine/src/scene/source_asset.rs:8`. No `use`, no `::SOURCE_IDENTITY`, no test, no script, no doc reads any of them.
- The env names `MANIFOLD_PHYSICS_SOURCE_IDENTITY` and `MANIFOLD_FLUIDS_SOURCE_IDENTITY` appear in exactly `lib.rs` (the `env!`) and `build.rs` (the `emit_source_identity` argument) of their crates.
- `emit_source_identity` (the non-owned variant) is called by physics and fluids `build.rs` only; core, node-engine and nodes-scene call `emit_owned_source_identity`. After this row lands, the non-owned function in `scripts/native_source_identity.rs` has no caller.
- Reader history: the identity existed for recorded CPU FLIP takes and the T2 move proof; both are gone (BUG-hkbdp.6.6 CPU FLIP removal; the move scripts are on BUG-hkbdp.6.5's retirement list).
- `sha2` in `[build-dependencies]` of physics and fluids: 0 direct hits in `build.rs`; used only inside the included `scripts/native_source_identity.rs`.
- Lines: physics (build.rs `#[path]` and `mod` 2, call 6, lib.rs const and doc 2, Cargo.toml 1) = 11; fluids (same shape) = 11; total 22.

## water-004

```
$ for DEP in arrayvec glam half image log serde:
    rg -n "\bDEP::|use DEP\b|extern crate DEP" crates/manifold-nodes-water
    rg -n "dep:DEP|\"DEP/" crates/manifold-nodes-water/Cargo.toml
$ rg -n 'ArrayVec|glam|Serialize|Deserialize|serde|\blog::|(info|warn|debug|error|trace)!\(|image::|DynamicImage|png' crates/manifold-nodes-water/src
$ rg -n -i 'sha2|zstd|manifold_playback|manifold-playback' crates/manifold-nodes-water
```
- arrayvec, half, image, log, serde: no match in either search; the broader pattern finds only `serde_json` (a separate dependency that stays) and comments containing the word "half".
- glam: only `Cargo.toml:55` (`"dep:glam"` inside `testkit`) plus its optional block and the dev-dependency; no `glam` token in `src`.
- sha2, zstd, manifold-playback (dev-dependencies, not in machete's output): `Cargo.toml` lines 38, 45, 46 only; no `tests/` or `examples/` directory exists in the crate.
- objc2 is used (`testkit/whitewater_scene.rs:419,520`) and stays. naga: water-006.
- Matches BUG-hkbdp.6.5 (Water Cargo hygiene), which asks to re-derive the list from use sites.
- Lines: 5 + 3 + 4 = 12.

## water-005

```
$ rg -n '\blog::|use log\b|extern crate log|\b(info|warn|debug|error|trace)!\(' crates/manifold-water-whitewater/src
$ rg -n -w serde_json crates/manifold-water-whitewater/src crates/manifold-water-whitewater/tests crates/manifold-water-whitewater/examples
```
- Both empty. `Cargo.toml:17` (`log`) and `:24` (`serde_json`, dev). Lines 2.

## water-006

```
$ rg -l 'naga::' crates/manifold-nodes-water/src
$ rg -n -B2 'live_sim_clock_reference' crates/manifold-nodes-water/src/lib.rs
$ sed -n 1,16p crates/manifold-nodes-water/src/primitives/mod.rs
```
- `primitives/liquid_bricks_consumer_tests.rs` (declared `#[cfg(test)]` in `primitives/mod.rs`) and `live_sim_clock_reference.rs` (declared `#[cfg(test)]` at `lib.rs:19`). No product file in the crate names `naga::`. Other water crates use naga from production files; not touched.

## water-007

Rows: `test_recipient_id`, `test_recipient_target` (nodes-water), `capacity_for_test` (water-gpu-flip). Common commands above.
- Each: 1 whole-word hit (the definition); corpus 0.
- The siblings in the same blocks have callers: `test_recipient_count`, `test_stamp`, `test_stamp_storage`, `test_field`, `narrow_band_for_test`, `cells_for_test`, `step_dt_for_test`.
- `testkit.rs` imports `ImpulseTarget` and `NodeId` under `cfg(feature = "gpu-proofs")` for these two functions only (the whole 30-line file was read).
- Lines: the two nodes-water functions are 1 line each plus 2 attribute lines each (6), their two gated imports are 4, so 10; `capacity_for_test` is 1; total 11.

## water-008

```
$ rg --hidden -g '!.git/' -n -w -F stats_failed REPO            # grid.rs:168 def, 301/304/305 (test), docs/WATER_CRATES_DESIGN.md:208
$ rg -n 'NARROW_BAND_SHORTAGE_WORD|stats_has_shortage' crates -g '*.rs'
$ git -C REPO log -S'stats_failed' --format='%h %ad %s' --date=short
```
- Function body is 3 lines (`grid.rs:168-170`) plus a 2-line doc comment. Its only callers are the 3 asserts of `failed_stats_include_narrow_band_capacity_shortage` (`grid.rs:299-306`, 8 lines).
- The real "did this tick fail" decision happens in the shaders and through `LiquidStats` (`liquid_stats.rs:103`); `grid.rs:247` uses the private `stats_has_shortage`. `stats_failed` is a CPU mirror nothing calls.
- WATER_CRATES_DESIGN.md section 4.2 (the liquid contract) lists `stats_failed` among "what a solver gets from the seam" in a "(cut 2)" export list. That is the gap that makes this B: the doc line goes with the function.
- History: a0d1bd473 (2026-10-03, narrow-band stage 2) added it; be9717df3 (design: water crates) lists it. Lines: 5 + 8 = 13.

## water-009

```
$ rg -n 'capture_scene_impulse|\.is_scheduled\(' crates -g '*.rs'
$ rg -n '^fn |#\[test\]' crates/manifold-app/tests/contracts/node_graph/catalog_tests/physics_impulses.rs
$ sed -n 104,135p crates/manifold-nodes-water/src/runtime/physics_impulses.rs
$ git -C REPO log -S'capture_scene_impulse(' --format='%h %ad %s' --date=short -- crates
```
- `capture_scene_impulse_at_source` is the only capture entry the product calls (`runtime/scene_impulses.rs:175`); `source_tests.rs` tests it 14 times.
- `capture_scene_impulse` (`physics_impulses.rs:292-314`, 23 lines with its 5-line doc) is called at `physics_impulses.rs` (tests) lines 192 (the `capture()` helper, used by 6 tests), 265, 273, 282, 304, 373, 402. Tests using it: `scene_impulse_captures_fields_and_targets_before_edits_and_applies_once`, `..._requires_initialized_recipients_and_explicit_clock_mapping`, `..._does_not_overwrite_pending_capture_or_reallocate_recipient_storage`, `..._rejects_reset_and_replacement_worlds`, `..._invalid_field_never_reuses_previous_value`, `..._samples_clock_at_capture_without_rewriting_graph_controls`, `..._partial_admission_retry_does_not_duplicate_successful_world`, `..._captures_spatial_shape_before_center_edits`.
- `is_scheduled` (`physics_impulses.rs:99-101`, 3 lines): asserted at test lines 224, 351, 532.
- Gap: `_at_source` calls `observe_physics_at_source`, so the fixture runtime must have evaluated a frame; the deleting lane decides whether the 8 tests repoint or the explicit-clock test is dropped. Added by 431762712 (2026-09-27).
- `physics_impulse_epoch`, which the wrapper calls, keeps other callers (`physics_impulses.rs:74,401`, `trigger_lane_force_tests.rs:380`), so there is no cascade. Lines: wrapper 23 + getter 3 = 26 (test edits not counted).

## water-010

```
$ rg --hidden -g '!.git/' -n -w -e 'fluid-perf-proofs' -e fluid_perf_proofs REPO -g '!*.jsonl'
$ rg --hidden -g '!.git/' -n 'water_perf' REPO -g '!*.jsonl'
$ sed -n 250,258p crates/manifold-app/tests/crate_layering.rs
```
- In nodes-water the feature is `Cargo.toml:57` (`["gpu-proofs"]`) and the `WATER_EXTRA_FEATURES` pin at `crate_layering.rs:255`. No `cfg(feature = "fluid-perf-proofs")` anywhere in nodes-water.
- The only `cfg` readers are four sites in `manifold-nodes-scene/.../render_scene.rs` (lines 143, 5116, 5159, 5179, 5185) writing the `water_perf` module (lines 144-221); `rg water_perf` finds the module and its single `record` call and nothing that reads the capture.
- Other Cargo lines: `manifold-nodes/Cargo.toml:116`, `manifold-nodes-scene/Cargo.toml:49`, `manifold-app/Cargo.toml:96`; script tests `scripts/test_gpu_proofs_gate.py`, `scripts/test_gate_passes.py` use the name as an example.
- Owner: the comment on BUG-hkbdp.6.5 (Water Cargo hygiene) rules the feature dead and gives the fix shape; kept as is. Total across crates about 78 + 8 + 6 + 3 = 95-100 lines; this sweep's own files hold 3.

## water-011

```
$ rg --hidden -g '!.git/' -n -w -F set_contact_tuning REPO                       # lib.rs:691 only
$ rg --hidden -g '!.git/' -n -w -F manifold_box3d_world_set_contact_tuning REPO  # lib.rs:57, lib.rs:707, native/box3d/bridge.c:222
$ git -C REPO show 82ccfa3ab -S'set_contact_tuning' -p --format= | rg '^[-+].*set_contact_tuning'
$ rg --hidden -g '!.git/' -n -i 'contact.tuning' REPO/docs REPO/scripts
```
- One definition; the FFI declaration (6 lines) and the C entry (4 lines) exist only for it. 82ccfa3ab (2026-09-24) deleted `world.set_contact_tuning(120.0, 10.0, 3.0)?;` from `flower_mesh_drop.rs`, the last caller. No doc or script mentions it. `native/box3d/include/box3d/box3d.h:178` is the upstream declaration and stays.
- Lines: 20 + 6 + 4 + 1 blank = 31. Grade C because manifold-physics is public engine API (memory note physics-engine-boundary; PHYSICS_ENGINE_BOUNDARY_DESIGN.md) and the brief asks `ask` for such rows.

## water-012

```
$ rg -n -w add_triangle_mesh REPO/crates REPO/docs
$ rg -n 'triangle_mesh_mass_properties|manifold_box3d_mesh_body_create|manifold_box3d_destroy_mesh|OwnedGeometry::Mesh' crates/manifold-physics
$ git -C REPO log -S'add_triangle_mesh' --format='%h %ad %s' --date=short -- crates
```
- Callers: `manifold-physics/src/mesh.rs:450` inside `scene_physics_body_mesh_rejects_foreign_handles_and_triangle_terrain` (a `#[test]`, which also checks foreign handles and so keeps its other asserts). Docs: `PHYSICS_ENGINE_BOUNDARY_DESIGN.md:132` (lists it under "the existing concrete API" for creation) and `FLUID_ENGINE_INTEGRATION_PLAN.md:33` (a merge note).
- Cascade sizes: method `lib.rs:581-665` (85), `triangle_mesh_mass_properties` `lib.rs:1422-1544` (123), FFI declaration `lib.rs:83-104` (22) and `destroy_mesh` (1), `bridge.c:358-443` (86) and `destroy_mesh` (about 6), `OwnedGeometry::Mesh` arms (about 14). About 340 lines with the shared test trimmed. Origin: f28dbcf80 to 36d8fb380 (2026-09-24 to 2026-09-27, flower mesh prototypes).
- Grade C: public API named in a design doc.

## water-013

```
$ rg -n -w with_growing_capacity REPO/crates                       # input.rs:81 def; tests at 349, 369, 382, 452
$ rg -n 'InputHistory' REPO/crates -g '*.rs'
$ sed -n 50,182p crates/manifold-physics/src/input.rs
```
- Four test uses (three `growing_history_*` tests and one assertion in `malformed_input_is_atomic_and_does_not_poison_valid_history`). The one product construction of `InputHistory` is `manifold-water-rigid/src/physics.rs:439` with `with_capacity`; `physics_sampling.rs` only mentions the type in a comment.
- Cascade: constructor and doc (16 + 3), the `growing` field and its two initialisers (3), the growth branch in `record` (`input.rs:135-150`, 17), three tests (`input.rs:347-394`, about 48) and the one assertion block (4). About 90 lines. `HistoryError::AllocationFailed` stays only if a fixed-capacity path uses it; the lane checks with `cargo check`.

## water-014

```
$ rg -n -w -e visit_hits -e StepAction -e pressure_impulse_delta_velocity REPO/crates REPO/docs
$ git -C REPO log -S'visit_hits' --format='%h %ad %s' --date=short
$ git -C REPO log -S'pressure_impulse_delta_velocity' --format='%h %ad %s' --date=short
```
- All hits are inside `manifold-physics/src/stepping.rs`: definitions (48, 68, 824) and the tests at 947-956 and 1039. No doc, bead or other crate. Added by f2f561574 and 60d1169e0 (2026-10-03, "Live sim clock", both marked unreviewed).
- Lines: `visit_hits` 16, `StepAction` 6, its test 21, `pressure_impulse_delta_velocity` 6 with doc, plus one test assert = about 50.

## water-015

Rows: `set_liquid_options` (11), `set_time_step_options` (13), `set_speed_limit_interval` (13), `set_whitewater_options` (15), `set_emitter` (19), `clear_obstacle` (4), `step_live_with_fields` (13); 88 lines.
- Whole-word hits (from the per-name runs): all seven appear only in `crates/manifold-fluids/src/{lib.rs, coupling/tests*.rs, coupling/owner/tests.rs, frame/tests.rs, seeded_tests.rs, surface.rs}` and `crates/manifold-water-rigid/src/physics/coupling_tests.rs`; `step_live_with_fields` also in `docs/PHYSICS_ENGINE_BOUNDARY_DESIGN.md:125` (the CPU API signature list). Corpus 0.
- `rg -n manifold_fluids REPO/crates` shows product use of `manifold-fluids` limited to record types and `validate_mesh`; `FluidWorld` is constructed only by tests and examples.
- Reference engine kept by Peter's ruling on BUG-hkbdp.6.1 (CPU FLIP decision): "manifold-fluids stays as the native reference". Native entries behind each setter are not counted.

## water-016

Rows: `add_mesh` (30), `add_fluid_mesh` (28), `set_mesh_motion` (34), `set_mesh_enabled` (16), `set_inflow_options` (18), `set_collider_friction` (17), `remove_mesh` (7), `set_boundary_collisions` (11); 161 lines in `mesh.rs`.
- Hits are `mesh.rs` itself, `coupling/tests*.rs`, `coupling/owner/tests.rs`, `seeded_tests.rs`, `water-rigid/.../coupling_tests.rs` (1 each for `add_mesh`, `set_liquid_options`). `docs/FLUID_ENGINE_INTEGRATION_PLAN.md` mentions each once (a port plan). Corpus 0.
- Fluids tests that exist for this surface: `coupling/tests.rs` 349, `coupling/tests/{boundaries 455, compound 416, gravity 376, viscous_motion 223}`, `coupling/owner/tests.rs` 538, `tests/coupling.rs` 417, `tests/scheduled_fields.rs` 244. They go with the decision; not counted in lines.

## water-017

```
$ rg -c -w -F RigidFluidCoupling REPO/crates REPO/scripts
$ rg -n RigidFluidCoupling crates/manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs crates/manifold-water-gpu-flip/src/primitives/shaders/gpu_flip_clock.wgsl
```
- Outside manifold-fluids: `water-rigid/.../coupling_tests.rs` (3 hits, a test), `gpu_flip_step.rs:1787` and `gpu_flip_clock.wgsl:271` (both comments). No product caller. `prepare_rigid_coupling` (8 lines) and `completed_bodies` (6 lines): tests only. Lines 14.

## water-018

```
$ wc -l crates/manifold-fluids/native/coupling_*probe*.cpp crates/manifold-fluids/native/coupling_*probe*.h
$ rg -n 'fn manifold_fluids_[a-z_]*probe' crates/manifold-fluids/src crates/manifold-fluids/examples
$ rg -n 'extern "C"' crates/manifold-fluids/native/bridge.cpp | rg -i probe
$ sed -n 1185,1200p crates/manifold-fluids/src/lib.rs
$ cargo tree --offline --manifest-path REPO/Cargo.toml -p manifold-app -e normal -i manifold-fluids
```
- Five probe files: 481 + 269 + 477 + 601 + 602 = 2,430 lines of C++; five headers 93 lines; nine wrapper entries in `bridge.cpp:714-777`. The nine `manifold_fluids_coupling_*_probe*` externs are declared at `src/tests/coupling.rs:98-121` (declared in `mod tests` under `#[cfg(test)]`, `lib.rs:1193`) and used by `examples/boundary_probe.rs`.
- `build.rs` lists the five probe `.cpp` files in the unconditional source list, so they compile into the library.
- `cargo tree ... -i manifold-fluids` (resolves manifests only; nothing built) shows manifold-app reaching manifold-fluids through nodes-water, nodes (via nodes-water), water-liquid (via gpu-flip, gpu-mpm, surface, whitewater and nodes-water) and water-whitewater. BUG-65r39 (manifold-fluids proof-only) states the goal that the app stops linking it.
- THIRD_PARTY_NOTICES.md line 41 names `coupling_*probe.*`; a deleting lane edits that line.
- Lines about 2,430 + 93 + 70 = 2,600.

## water-019

```
$ rg --hidden -g '!.git/' -n 'whitewater-oracle|whitewater_oracle|MANIFOLD_WHITEWATER_ORACLE' REPO -g '!*.jsonl' -g '!docs/**'
$ awk (ifdef blocks of MANIFOLD_WHITEWATER_ORACLE) over native/bridge.cpp, bridge.h, flip_engine/diffuseparticlesimulation.{cpp,h}
$ wc -l crates/manifold-fluids/src/whitewater_oracle.rs crates/manifold-fluids/src/sheet_oracle.rs
$ rg -n 'whitewater-oracle' scripts .config .claude --hidden
```
- Rust modules: `whitewater_oracle.rs` 364 + `sheet_oracle.rs` 461 = 825 lines, both `#[cfg(feature = "whitewater-oracle")]` (`lib.rs:28-31`). Native: `bridge.cpp` 33-35, 2043-2047, 2070-2072, 2413-2725 (324 lines), `bridge.h` 179-203 (25), `diffuseparticlesimulation.cpp` 1891-1893 (3), `.h` 129-133 (5) = 357.
- Gated users: `water-whitewater/.../whitewater_field_tests.rs` (109, 213, 225), `whitewater_emitter_cpu.rs` (175, 249), `whitewater_engine_cpu.rs` (203), `whitewater_engine_gpu_tests.rs` (8, 522), `nodes-water/src/testkit/whitewater_scene.rs` (559, 574), `manifold-nodes/tests/.../whitewater_scene.rs` (381 onward). Sizes of these test blocks not measured.
- `scripts/`, `.config/` and `.claude/` do not name the feature (the search printed only docs and Cargo/crate hits), so no gate or nightly leg enables it.
- THIRD_PARTY_NOTICES.md line 51 names `whitewater_oracle.rs`; a deleting lane edits that line.
- Owners: the comment on BUG-hkbdp.6.9 (Consolidate water test-only compilation) asks "fix it and add it to `feature_matrix.py`, or delete it with the oracle proofs folded into gpu-proofs" and reports the compile failure; BUG-2fmpk (whitewater engine oracle) depends on it. This row keeps that decision open.
- Related Cargo fact: `manifold-fluids` is a normal dependency of nodes-water but is named in `src` only by `primitives/whitewater_extent_tests.rs:167` (module declared `#[cfg(test)]`, `primitives/mod.rs:8-9`). Cargo cannot forward `manifold-fluids/whitewater-oracle` through a dev-dependency, so the move waits for this decision.

## water-020

```
$ rg --hidden -g '!.git/' -l -w -F boundary_probe REPO -g '!*.jsonl'
$ rg --hidden -g '!.git/' -l -w -F surface_tuning REPO -g '!*.jsonl'
$ git -C REPO log -1 --format='%ad %s' --date=short -- crates/manifold-fluids/examples/surface_tuning.rs
```
- Each name appears in its own file and in `scripts/fixtures/gate-p1.json` (an old recorded gate census) only. `wc -l`: `boundary_probe.rs` 184, `surface_tuning.rs` 214. Last commit for both 2026-09-29. `tests/boundary_regression.rs` (46 lines) is a test, not listed. Peter may run these by hand, hence C.

## Kept rows (52), one line each

Format: name, definition, other files holding the name (count), corpus. All have corpus 0. "n:" = `crates/manifold-nodes/tests/...`, "a:" = `crates/manifold-app/tests/...`.

nodes-water (13): `REST_PER_CELL` presets/gpu_flip.rs:141 (n: catalog_tests/whitewater_scene.rs 2, water/primitives/gpu_flip_scene_tests.rs 3, WATER_CRATES_DESIGN.md); `race_dam_break` :172 (n: gpu_flip_scene_tests.rs 1); `slosh` :214 (n: gpu_flip_scene_tests.rs 4, docs, beads); `family_outputs`, `surface_detail_offset` presets/gpu_flip/testkit.rs (n: catalog_tests/gpu_flip_preset.rs 3 and 2); `test_stamp` (a: physics_impulses/source_tests.rs 6), `test_stamp_storage` (a: physics_impulses.rs 2), `test_field` (a: physics_impulses.rs 8) in runtime/physics_impulses/testkit.rs; `sampling_mask` (n: catalog_tests/physics_sampling.rs 5), `last_physics_frame_time` (a: source_tests.rs 3, n: physics_sampling.rs 1), `set_last_physics_frame_time` (n: physics_sampling.rs 1), `reset_impulse_routes` (a: scene_routes_tests.rs 1), `surface_inputs` (n: catalog_tests/gpu_flip_surface.rs 2) in runtime/testkit.rs. All are `cfg(test)`, `testkit` or `gpu-proofs` gated; callers are other crates' tests.

water-gpu-flip (23): `box_sites_for_test` (n: gpu_flip_scene_tests.rs 1), `pool_sites_for_test` (n: same 1), `lattice_for_test` (n: gpu_flip_preset.rs 3), `setup_for_test` (n: gpu_flip_preset.rs 3, gpu_flip_scene_tests.rs 2), `sheet_fill_rate_for_test` (n: gpu_flip_preset.rs 1) in gpu_flip_domain.rs:1219-1231; `template_stats` (n: gpu_flip_body.rs 2; own tests 6), `copy_scalars` (n: gpu_flip_body.rs 1; own 4), `seed_records` (n: gpu_flip_body.rs 1, fixtures/gpu_flip_body_golden.txt 1; own 3), `set_keep_ranges` (n 2; own 2), `set_round_chunk` (n 1; own 2) in gpu_flip_pressure.rs; `set_poison` (n: gpu_flip_body.rs 3, gpu_flip_tile_tests.rs 5), `set_gate_off` (n: gpu_flip_body.rs 6), `set_force_solid_velocity` (n: gpu_flip_scene_tests.rs 3), `narrow_band_for_test`, `cells_for_test`, `step_dt_for_test` (n: gpu_flip_tile_tests.rs 1 each), `with_grid_for_test` (3), `with_gather_for_test` (1), `with_tiles_for_test` (3) (n: gpu_flip_tile_tests.rs), `dispatch_pass` (n: gpu_flip_tile_tests.rs 3; own tests 9), `set_force_dense_extend` (n: gpu_flip_scene_tests.rs 5), `set_force_indirect_pockets` (n: same 3) in gpu_flip_step.rs; `volume_and_area` gpu_flip_volume.rs:14 (n: gpu_flip_scene_tests.rs 2, beads 1).

water-whitewater (8): `set_reference_capture_for_test`, `set_reference_enabled_for_test`, `particle_snapshots_for_test`, `turbulence_dispatches_for_test`, `turbulence_for_test` (n: water/primitives/whitewater_step.rs 1 each), `unpacked_faces_for_test` (n: same 5), `reference_proof_node`, `fused_proof_node` (nodes-water/src/testkit/whitewater_scene.rs 1 each); definitions in primitives/whitewater_step.rs.

Other kept: water-rigid `native_ticks_on_this_thread` (a: renderer_contracts/gpu_proofs/liquid_conformance.rs 3, n: gpu_proofs/physics_boxes.rs 5); water-liquid `face_coords` (nodes-water face_grid_tests.rs 2, testkit/conformance.rs 3), `FACE_SAMPLE_COMPONENT_BODY` (water-whitewater fused_tests.rs 1), `cell_coords` (water-gpu-flip gpu_flip_step_tests.rs 14), `in_use` (own bound test: `frame_history.rs:603` and the assert at `:960`; the 7 hits in `node-engine/runtime/chain_dispatch.rs` are an unrelated `in_use`); physics `signed_distance_lattice` (n: gpu_proofs/matter_bodies.rs 4, own tests 4); fluids `capture_particle_frame` (n: gpu_flip_scene_tests.rs 1), `DEFAULT_FILL_THRESHOLD` (water-whitewater whitewater_engine_gpu_tests.rs 1; inside the `whitewater-oracle` module).

Totals check: 31 rows in candidates (physics 5, fluids 20, nodes-water 4, water-liquid 1, water-gpu-flip 1) + 52 kept = 83.
