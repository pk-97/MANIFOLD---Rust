# P0 engine seam findings

The hook rejected the requested .claude/orchestration/crate-split-seams.md destination as CC configuration. This root-level file is the reviewable fallback; the lead must place it at the requested destination.

Read-back: base fcf9f189c and branch lane/crate-split-p0-seams verified; entry tree clean. D5 preserves direct hub/core passes and runs registered migrations in two stages, cloning the current definition for each pass and retaining only a true result. D10 cuts are independent; blocked items are recorded and the lane continues (lead ruling). D11 fixes the built-in set. No new crate, physical file move, migration-body change, script edit, unapproved visibility widening, GPU execution, push or landing. The lead owns all commits, the trial carve and landing. Census oracles: check-presets, node-catalog --check, and test census; migration ordering and loader round-trip remain required.

## D5

Both glTF passes and private helpers/tests: node_graph/gltf_import/migration.rs (scene-family graph assembler). Four liquid passes and tests: node_graph/liquid/migration.rs. wire_blob_bounds and tests: node_graph/primitives/blob_bounds.rs. Seven function bodies checked byte-identical against the base; no loader test names lost. Shared bare-node/metadata test fixtures copied locally without widening helper visibility.

BeforeFlatten: 200 migrate_gltf_anim_v2; 210 migrate_gltf_ao_mask.
AfterFlatten: 300 wire_liquid_intervals; 310 wire_gpu_flip_grid; 320 wire_liquid_frame_cursor; 330 wire_retained_whitewater; 400 wire_blob_bounds.

## D10

(a) Cut under the follow-up ruling: blanket AsAny supertrait on EffectNode; the scene probe is an inherent RenderScene method and a free function in render_scene/rt_proof.rs. EffectNode and PresetRuntime no longer expose rt_probe_scene; all three integration-proof callers use the free function. PresetRuntime.graph is already public, so no accessor was added. rt_probe_rays is unchanged. The first GPU-proof compile found render_scene itself is pub(crate); feature-gated public module visibility is pending lead approval. No re-export added.

(b) D5 above.

(c) wgsl_compute remains a hub built-in; freeze/install.rs requires no path change before the physical carve. D11 records it.

(d) Cut under the follow-up ruling: primitives::{gpu_flip_preset,liquid_stats} widened from pub(crate) mod to pub mod. Removed the gpu_flip_preset, SOLVER_WORDS and with_liquid_stats_layout re-exports. Repointed manifold-app/src/ui_bridge/project.rs and renderer integration tests wgsl_validation.rs and gpu_proofs/liquid_conformance.rs to the owning paths.

(e) Cut: existing mesh_common, mesh_pipeline and compute_common files are declared as crate::mesh, crate::mesh::pipeline and crate::particles via path attributes, with their original public visibility. Importers repointed; no physical file move or re-export facade. Other D3 helper paths remain for P1.

(f) composites are image-family. The prescribed composites:: search returned no matches (exit 1). No code change.

(g) P5, unchanged: preset_runtime/core.rs:118 stores math_views: Vec<super::math_view::MathViewRuntime>. math_view.rs calls RenderMeshDiagram::prewarm_pipelines and math_view_events.rs uses BeatEnvelopeDurations/BeatEnvelopeState. This is stored runtime state, not a stateless wrapper.

(h) Approved follow-up seam implemented: config/error vocabulary stays in the hub; EffectNode constructs a boxed ViewportPass on target changes; render_scene/scene_viewport.rs owns the existing pass and forwards the trait methods. Executor reports constructor failure to PresetRuntime, which clears and returns NotSceneRenderer. Per the final ruling the type-id string check is removed: returning a pass is the capability test. Both trait and override document fresh renderer/history ownership, never the source node state. New CPU tests cover same-target reuse, target changes and a missing constructor. Existing pass bodies/tests moved unchanged. The strict RenderScene search still finds two pre-existing CPU test references at preset_runtime/physics_impulses/tests.rs:20,124; production sites in its requested scope are clear. Those unrelated test bodies were not changed.

## D11

The const list records wgsl_compute, standalone_pipeline (helper, no factory), Mix, MaskedMix, MuxTexture, Value and Gain. builtins_match_registry resolves and constructs each node through PrimitiveRegistry::with_builtin. The list is compiled under cfg(test) in P0; production registration remains inventory-driven.

The source-ownership assertion is deferred/escalated to P1: this monolithic crate still contains family modules, inline tests, and the expressly retained P5 math-view dependency on RenderMeshDiagram. A raw search treats all of those as hub code. Asserting exact hub factories requires the actual carve and a decision about the retained P5 exception; silently adding RenderMeshDiagram to D11 would contradict the brief. Raw D11 search output follows below.

## P5 inventory

Math-view stored runtime state and prewarming (D10g). Existing physics/fluid/liquid adapter and scheduling reaches remain P5 under D9; scene_modifier_expand remains its named P5 candidate. Full compiler-derived site inventory is the lead's trial carve, not established by this lane.

## Visibility and remaining compile escalation

Approved widenings in crates/manifold-renderer/src/node_graph/primitives/mod.rs:369 (liquid_stats) and :441 (gpu_flip_preset): pub(crate) mod to pub mod. New public contracts AsAny and ViewportPass are explicitly approved; no graph accessor was needed.

The final GPU-proof compile command, CARGO_BUILD_JOBS=4 RUSTC_WRAPPER='' cargo check --manifest-path <worktree>/Cargo.toml -p manifold-renderer --features gpu-proofs --tests, exited 101 with E0603: primitives::render_scene is private at primitives/mod.rs:255, blocking the new free-function calls in rt_dynamic_catalog.rs and rt_dynamic_current_frame.rs. rt_dynamic_perf.rs uses the same path but its additional feature was not checked. Requested ruling: pub mod render_scene only under gpu-proofs, retaining pub(crate) otherwise. Pending approval; no widening or re-export workaround applied.

## INV-3 and validation

Entry seams: scripts/crate_closure.py seams --sites and scripts/crate_closure.py seams: 109 edges / 230 sites.

CARGO_BUILD_JOBS=4 cargo run --manifest-path <worktree>/Cargo.toml -p manifold-renderer --bin check-presets: exit 101, sccache Operation not permitted. Retry with RUSTC_WRAPPER='' started compiling, then stopped (exit 130) before binary execution: check_presets.rs:84 creates GpuDevice::new_queued by default, conflicting with the lane's no-GPU/no-lock rule. Full baseline exit and primitive count are UNVERIFIED and owed by lead. This binary does not currently print the registry's primitive count. No GPU lock was taken.

Build commands use CARGO_BUILD_JOBS=4 and RUSTC_WRAPPER='' because the sandbox cannot run sccache. No configuration files changed.

D5 GPU-proof compile: exit 0; two unused-import warnings corrected. D5 clippy: exit 0. D10(e) GPU-proof compile and subsequent clippy: exit 0.

The initial migration-only nextest gate was stopped during compilation (exit 130) when the final rulings arrived; superseded by the requested combined gate below. It had run no tests.

Final IO command: CARGO_BUILD_JOBS=4 RUSTC_WRAPPER='' cargo nextest run --manifest-path <worktree>/Cargo.toml -p manifold-io --test load_project: exit 100, 15 passed / 1 failed. All four LiveSchool tests passed. load_waypoints_large_project failed at crates/manifold-io/tests/load_project.rs:405: clip count 2016, expected 2014. No IO/core files changed and manifold-io has no renderer dependency. Not repaired in this lane; no claim of a reproduced base run. Failure transcript retained at /tmp/p0-load-project-junit.xml.

Final renderer clippy (CARGO_BUILD_JOBS=4 RUSTC_WRAPPER='' cargo clippy --manifest-path <worktree>/Cargo.toml -p manifold-renderer -- -D warnings): exit 0.

Requested combined renderer gate (CARGO_BUILD_JOBS=4 RUSTC_WRAPPER='' cargo nextest run --manifest-path <worktree>/Cargo.toml -p manifold-renderer -E 'test(graph_loader) | test(migration) | test(builtins) | test(freeze::markers) | test(scene_viewport) | test(liquid) | test(gltf) | test(blob_bounds)'): exit 100. Selected 418; ran 68: 67 passed (one leaky), one failed; 350 unrun after fail-fast. corrupted_assembler_output_fails_validation_naming_the_node tries to create a Metal device and fails with No Metal device found. Built-ins, both new viewport constructor tests, all five moved glTF migration tests and fused_wgsl_snapshot_unchanged passed. Transcript: /tmp/p0-renderer-junit.xml. No GPU device was obtained.

Focused follow-up for unrun migrated tests (same env/manifest, cargo nextest run -p manifold-renderer --lib -E 'test(graph_loader) | test(node_graph::migration::) | test(liquid::migration::) | test(blob_bounds::migration_tests) | test(primitives::render_scene::scene_viewport::)'): exit 100; 35 passed, one failed, one unrun. All nine moved liquid migration tests, blob-bounds migration test, migration_order_matches_table, and both existing viewport unit tests passed. The filter still included legacy graph_loader GPU allocation tests: audit_fires_on_unbound_array_resource failed with No Metal device found; pre_allocate_resources_accepts_fully_bound_plan was not run. These two require the lead's GPU environment; no further retries. Transcript: /tmp/p0-migration-junit.xml.

Final seams: 96 edges / 211 sites, down from 109 / 230.

CARGO_BUILD_JOBS=4 RUSTC_WRAPPER='' scripts/dev.py node-catalog --check: exit 0, node catalog in sync, no DRIFT. git diff --check: exit 0. Full check-presets and before/after primitive-count measurement remain unverified because of the no-GPU rule; the catalog result is not a substitute for that baseline.

Exact-path git add of the three new migration modules failed, exit 128, because the sandbox cannot create /Users/peterkiemann/MANIFOLD - Rust/.git/worktrees/slot-9/index.lock. The lead confirmed this is expected and owns all P0 commits. No further staging/commit attempts. No push or landing attempted. The complete changed-file list is below.

## D11 verification output

```text
crates/manifold-renderer/src/node_graph/bound_graph.rs:primitives::AffineTransform
crates/manifold-renderer/src/node_graph/effect_node.rs:primitives::Feedback
crates/manifold-renderer/src/node_graph/execution.rs:primitives::MorphMesh
crates/manifold-renderer/src/node_graph/execution.rs:primitives::MuxTexture
crates/manifold-renderer/src/node_graph/execution.rs:primitives::NormalWaveMesh
crates/manifold-renderer/src/node_graph/execution.rs:primitives::Value
crates/manifold-renderer/src/node_graph/graph_loader.rs:primitives::SeedParticles
crates/manifold-renderer/src/node_graph/material_inspector.rs:primitives::PbrMaterial
crates/manifold-renderer/src/node_graph/metal_backend.rs:primitives::GltfTextureSource
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::BLUR_TYPE_ID
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::BRIGHTNESS_TYPE_ID
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::CHANNEL_MIX_TYPE_ID
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::COLOR_RAMP_TYPE_ID
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::FEEDBACK_TYPE_ID
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::GAUSSIAN_BLUR_TYPE_ID
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::MIX_TYPE_ID
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::Mix
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::THRESHOLD_TYPE_ID
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::WATERCOLOR_TYPE_ID
crates/manifold-renderer/src/node_graph/persistence.rs:primitives::WET_DRY_TYPE_ID
crates/manifold-renderer/src/node_graph/ports.rs:primitives::SceneObjectNode
crates/manifold-renderer/src/node_graph/resource_allocation.rs:primitives::MeshSpatialMask
crates/manifold-renderer/src/node_graph/validation.rs:primitives::CelMaterial
crates/manifold-renderer/src/node_graph/validation.rs:primitives::UnlitMaterial
crates/manifold-renderer/src/preset_runtime/groups.rs:primitives::MaskedMix
crates/manifold-renderer/src/preset_runtime/math_view.rs:primitives::RenderMeshDiagram
crates/manifold-renderer/src/preset_runtime/mod.rs:primitives::Mix
```

## Complete changed-file inventory

Modified (186):

```text
crates/manifold-app/src/ui_bridge/project.rs
crates/manifold-renderer/examples/flower_mesh_drop.rs
crates/manifold-renderer/examples/fluid_capture.rs
crates/manifold-renderer/src/generators/mesh_common.rs
crates/manifold-renderer/src/generators/mod.rs
crates/manifold-renderer/src/generators/platonic_geometry.rs
crates/manifold-renderer/src/lib.rs
crates/manifold-renderer/src/node_graph/camera.rs
crates/manifold-renderer/src/node_graph/decode_cache.rs
crates/manifold-renderer/src/node_graph/effect_node.rs
crates/manifold-renderer/src/node_graph/execution.rs
crates/manifold-renderer/src/node_graph/execution_plan.rs
crates/manifold-renderer/src/node_graph/fluid.rs
crates/manifold-renderer/src/node_graph/fluid/native.rs
crates/manifold-renderer/src/node_graph/fluid/take.rs
crates/manifold-renderer/src/node_graph/fluid_cache.rs
crates/manifold-renderer/src/node_graph/fluid_mesh_upload.rs
crates/manifold-renderer/src/node_graph/fragment_mask_continuity_tests.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/gpu_tests.rs
crates/manifold-renderer/src/node_graph/freeze/install.rs
crates/manifold-renderer/src/node_graph/gltf_import/mod.rs
crates/manifold-renderer/src/node_graph/gltf_load.rs
crates/manifold-renderer/src/node_graph/graph_loader.rs
crates/manifold-renderer/src/node_graph/instance_upload.rs
crates/manifold-renderer/src/node_graph/light.rs
crates/manifold-renderer/src/node_graph/liquid.rs
crates/manifold-renderer/src/node_graph/liquid/extent.rs
crates/manifold-renderer/src/node_graph/mesh_boundary.rs
crates/manifold-renderer/src/node_graph/mesh_source.rs
crates/manifold-renderer/src/node_graph/metal_backend.rs
crates/manifold-renderer/src/node_graph/mod.rs
crates/manifold-renderer/src/node_graph/physics_mesh.rs
crates/manifold-renderer/src/node_graph/ports.rs
crates/manifold-renderer/src/node_graph/primitives/analytic_echo_instances.rs
crates/manifold-renderer/src/node_graph/primitives/analytic_echo_instances_gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/anti_clump_particles.rs
crates/manifold-renderer/src/node_graph/primitives/apply_radial_burst_3d_to_particles.rs
crates/manifold-renderer/src/node_graph/primitives/apply_radial_burst_to_particles.rs
crates/manifold-renderer/src/node_graph/primitives/array_connect_nearest.rs
crates/manifold-renderer/src/node_graph/primitives/array_diffuse_particles.rs
crates/manifold-renderer/src/node_graph/primitives/array_feedback.rs
crates/manifold-renderer/src/node_graph/primitives/array_replicate_polyline_rings.rs
crates/manifold-renderer/src/node_graph/primitives/bend_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/blob_bounds.rs
crates/manifold-renderer/src/node_graph/primitives/consecutive_edges.rs
crates/manifold-renderer/src/node_graph/primitives/container_bounds_3d.rs
crates/manifold-renderer/src/node_graph/primitives/container_repel_force_3d.rs
crates/manifold-renderer/src/node_graph/primitives/copy_positions.rs
crates/manifold-renderer/src/node_graph/primitives/cut_out_box.rs
crates/manifold-renderer/src/node_graph/primitives/cylinder_wrap_field.rs
crates/manifold-renderer/src/node_graph/primitives/diffuse_force_3d_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/digital_plants_render.rs
crates/manifold-renderer/src/node_graph/primitives/displace_copies.rs
crates/manifold-renderer/src/node_graph/primitives/displace_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/edges_from_grid_uv.rs
crates/manifold-renderer/src/node_graph/primitives/edges_from_hypercube.rs
crates/manifold-renderer/src/node_graph/primitives/edges_from_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/euler_step_particles.rs
crates/manifold-renderer/src/node_graph/primitives/euler_step_particles_3d.rs
crates/manifold-renderer/src/node_graph/primitives/extrude_curve.rs
crates/manifold-renderer/src/node_graph/primitives/facet_normals.rs
crates/manifold-renderer/src/node_graph/primitives/flatten_to_camera_plane.rs
crates/manifold-renderer/src/node_graph/primitives/fluid_role_source.rs
crates/manifold-renderer/src/node_graph/primitives/fluid_role_source/geometry.rs
crates/manifold-renderer/src/node_graph/primitives/fluid_surface.rs
crates/manifold-renderer/src/node_graph/primitives/fold_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/generate_cube_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/generate_grid_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/generate_instance_transforms.rs
crates/manifold-renderer/src/node_graph/primitives/glitch_jitter.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_mesh_source.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_morph_deltas_source.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_skeleton_pose.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_skinned_mesh_source.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_render_smoke_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_scene_tests.rs
crates/manifold-renderer/src/node_graph/primitives/hypercube_vertices.rs
crates/manifold-renderer/src/node_graph/primitives/instance_position_jitter.rs
crates/manifold-renderer/src/node_graph/primitives/instance_rotation_jitter.rs
crates/manifold-renderer/src/node_graph/primitives/lerp_instance_fields.rs
crates/manifold-renderer/src/node_graph/primitives/lightning_bolt.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_bricks_gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_surface_tests.rs
crates/manifold-renderer/src/node_graph/primitives/melt_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_cut_map.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_cut_remap.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_ramp.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_snapshot.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_spatial_mask.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_stagger_envelope.rs
crates/manifold-renderer/src/node_graph/primitives/mod.rs
crates/manifold-renderer/src/node_graph/primitives/morph_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/morph_targets_blend.rs
crates/manifold-renderer/src/node_graph/primitives/neighbor_smooth.rs
crates/manifold-renderer/src/node_graph/primitives/nested_cubes_geometry.rs
crates/manifold-renderer/src/node_graph/primitives/noise_displace.rs
crates/manifold-renderer/src/node_graph/primitives/normal_wave_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/ocean_displace.rs
crates/manifold-renderer/src/node_graph/primitives/ordered_recon_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/pack_curve_xy.rs
crates/manifold-renderer/src/node_graph/primitives/pack_vec4.rs
crates/manifold-renderer/src/node_graph/primitives/particle_frame_blend_tests.rs
crates/manifold-renderer/src/node_graph/primitives/particles_to_copies.rs
crates/manifold-renderer/src/node_graph/primitives/physics_world.rs
crates/manifold-renderer/src/node_graph/primitives/plane_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/platonic_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/polytope_edges.rs
crates/manifold-renderer/src/node_graph/primitives/polytope_vertices.rs
crates/manifold-renderer/src/node_graph/primitives/project_3d.rs
crates/manifold-renderer/src/node_graph/primitives/project_4d.rs
crates/manifold-renderer/src/node_graph/primitives/projected_grid.rs
crates/manifold-renderer/src/node_graph/primitives/push_along_normals.rs
crates/manifold-renderer/src/node_graph/primitives/reflect_array.rs
crates/manifold-renderer/src/node_graph/primitives/relax_surface_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/remap_cut_weights.rs
crates/manifold-renderer/src/node_graph/primitives/remap_mesh_cut.rs
crates/manifold-renderer/src/node_graph/primitives/remove_drift_3d.rs
crates/manifold-renderer/src/node_graph/primitives/render_3d_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/render_instanced_3d_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/render_lines.rs
crates/manifold-renderer/src/node_graph/primitives/render_mesh_diagram.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene/gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene/rt_proof.rs
crates/manifold-renderer/src/node_graph/primitives/revolve_curve.rs
crates/manifold-renderer/src/node_graph/primitives/rigid_body.rs
crates/manifold-renderer/src/node_graph/primitives/ripple_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/rotate_3d.rs
crates/manifold-renderer/src/node_graph/primitives/rotate_4d.rs
crates/manifold-renderer/src/node_graph/primitives/sample_mesh_triangles.rs
crates/manifold-renderer/src/node_graph/primitives/sample_texture_3d_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/sample_texture_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/sample_triangle_grid.rs
crates/manifold-renderer/src/node_graph/primitives/scatter_on_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/scatter_particles.rs
crates/manifold-renderer/src/node_graph/primitives/scatter_particles_3d.rs
crates/manifold-renderer/src/node_graph/primitives/scatter_particles_camera.rs
crates/manifold-renderer/src/node_graph/primitives/scene_array.rs
crates/manifold-renderer/src/node_graph/primitives/scene_fx_default_passthrough.rs
crates/manifold-renderer/src/node_graph/primitives/scene_object.rs
crates/manifold-renderer/src/node_graph/primitives/seed_particles.rs
crates/manifold-renderer/src/node_graph/primitives/seed_particles_from_texture.rs
crates/manifold-renderer/src/node_graph/primitives/shatter_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/simplex_noise_force_3d_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/simplex_noise_force_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/skin_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/slice_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/smooth_surface_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/spawn_from_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/surface_mesh_freeze_tests.rs
crates/manifold-renderer/src/node_graph/primitives/surface_mesh_normals.rs
crates/manifold-renderer/src/node_graph/primitives/taper_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/test_multi_output_atomic_fixture.rs
crates/manifold-renderer/src/node_graph/primitives/torus_wrap_field.rs
crates/manifold-renderer/src/node_graph/primitives/transform_mesh_patches.rs
crates/manifold-renderer/src/node_graph/primitives/triangulate_grid.rs
crates/manifold-renderer/src/node_graph/primitives/tube_from_path.rs
crates/manifold-renderer/src/node_graph/primitives/twist_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/volume_optics_tests.rs
crates/manifold-renderer/src/node_graph/primitives/volume_surface_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/voxelize_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/wave_field_3d.rs
crates/manifold-renderer/src/node_graph/primitives/wave_shear_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs
crates/manifold-renderer/src/node_graph/primitives/wrap_particles_torus.rs
crates/manifold-renderer/src/node_graph/resource_allocation.rs
crates/manifold-renderer/src/node_graph/scene_viewport.rs
crates/manifold-renderer/src/node_graph/source_asset.rs
crates/manifold-renderer/src/node_graph/substeps.rs
crates/manifold-renderer/src/node_graph/validation.rs
crates/manifold-renderer/src/preset_runtime/build.rs
crates/manifold-renderer/src/preset_runtime/scene_viewport.rs
crates/manifold-renderer/tests/gpu_proofs/encode_replay.rs
crates/manifold-renderer/tests/gpu_proofs/fluid_array_growth.rs
crates/manifold-renderer/tests/gpu_proofs/liquid_conformance.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_glass.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_subsurface.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_uv1_preservation.rs
crates/manifold-renderer/tests/gpu_proofs/rt_dynamic_catalog.rs
crates/manifold-renderer/tests/gpu_proofs/rt_dynamic_current_frame.rs
crates/manifold-renderer/tests/gpu_proofs/rt_dynamic_perf.rs
crates/manifold-renderer/tests/gpu_proofs/rt_dynamic_shading.rs
crates/manifold-renderer/tests/gpu_proofs/rt_emissive_instancing.rs
crates/manifold-renderer/tests/gpu_proofs/rt_instancing.rs
crates/manifold-renderer/tests/gpu_proofs/substeps.rs
crates/manifold-renderer/tests/wgsl_validation.rs
```

New (6):

```text
crate-split-seams.md
crates/manifold-renderer/src/node_graph/builtins.rs
crates/manifold-renderer/src/node_graph/gltf_import/migration.rs
crates/manifold-renderer/src/node_graph/liquid/migration.rs
crates/manifold-renderer/src/node_graph/migration.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene/scene_viewport.rs
```
