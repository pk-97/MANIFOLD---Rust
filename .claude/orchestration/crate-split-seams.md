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

## P1 stage 1 read-back

Base 743c855e0, branch lane/crate-split-p1-carve and clean entry tree verified. D13 fixes the grouped layout; D2 scene vocabulary, D3 helpers, D5 migration ordering, D7 features/test ownership, D8 compiler-demand visibility, D9 v1 water and the exact D11 seven bind this carve. Only physical moves, module/import/feature wiring and path rewrites are implemented. No family promotion, renderer dependency, new facade, function-body change beyond paths, test-behaviour edit, GPU execution, staging, commit or landing. The existing phase bead is BUG-9hndn (P1 carve manifold-node-engine).

Stage 1 stops here for the lead's rulings. This is an intentionally red working tree, not a landing. The lead retains the active slot for stage 2; no release or retirement was attempted.

Counts: 500 source files moved (378 Rust, 122 WGSL); 487 canonical rewrite-map entries; 304 distinct (file, line, item) reaches, including 11 non-D1 test dependencies; 96 asset sites; 20 source-walking sites in 10 files; 38 remaining-renderer consumers of moved shaders. The build script is copied/repathed separately, with its renderer original removed. Counts are source sites, not repeated compiler diagnostics. Type-inference follow-on errors are listed separately.

- external: manifold-editing: 3
- external: manifold-io: 8
- manifold-compositor: 45
- manifold-node-engine (D7 testkit candidate): 3
- manifold-nodes: 56
- manifold-nodes (cross-family tests): 1
- manifold-nodes-image: 49
- manifold-nodes-scene: 64
- manifold-nodes-water: 75

D1 ownership follows the family roles in the design. generator_math's line-generator constant is assigned to image for this census, but D1 does not explicitly name that file: the lead must rule on it. The three clear_texture_committed calls target the omitted renderer-lib test helper; D7 suggests engine testkit ownership, but no helper was copied or moved. The IO/editing entries are forbidden engine test edges, recorded separately without adding those dependencies.

## P1 move list

This list was written before the moves and expanded before moving the compiler-discovered sibling test. Mix is implemented by compose.rs; it moves to the binding D13 mix.rs. sort_particles_into_cells_gpu_tests.rs follows its owning water primitive's existing #[path] declaration. All moves used plain mv. The helpers' P0 #[path] attributes are removed. mesh_cut remains test-only, matching its original oracle declaration.

```text
crates/manifold-renderer/src/background_worker.rs → crates/manifold-node-engine/src/runtime/background_worker.rs
crates/manifold-renderer/src/chain_dispatch.rs → crates/manifold-node-engine/src/runtime/chain_dispatch.rs
crates/manifold-renderer/src/effect.rs → crates/manifold-node-engine/src/runtime/effect.rs
crates/manifold-renderer/src/effects/compute_blit_helper.rs → crates/manifold-node-engine/src/runtime/effects/compute_blit_helper.rs
crates/manifold-renderer/src/effects/compute_dual_blit_helper.rs → crates/manifold-node-engine/src/runtime/effects/compute_dual_blit_helper.rs
crates/manifold-renderer/src/effects/mod.rs → crates/manifold-node-engine/src/runtime/effects/mod.rs
crates/manifold-renderer/src/effects/shaders/aces_tonemap_compute.wgsl → crates/manifold-node-engine/src/runtime/effects/shaders/aces_tonemap_compute.wgsl
crates/manifold-renderer/src/effects/shaders/fsr1_easu_compute.wgsl → crates/manifold-node-engine/src/runtime/effects/shaders/fsr1_easu_compute.wgsl
crates/manifold-renderer/src/effects/shaders/fsr1_rcas_compute.wgsl → crates/manifold-node-engine/src/runtime/effects/shaders/fsr1_rcas_compute.wgsl
crates/manifold-renderer/src/effects/shaders/fx_watercolor_compute.wgsl → crates/manifold-node-engine/src/runtime/effects/shaders/fx_watercolor_compute.wgsl
crates/manifold-renderer/src/effects/shaders/linear_to_pq_compute.wgsl → crates/manifold-node-engine/src/runtime/effects/shaders/linear_to_pq_compute.wgsl
crates/manifold-renderer/src/effects/shaders/presentation.wgsl → crates/manifold-node-engine/src/runtime/effects/shaders/presentation.wgsl
crates/manifold-renderer/src/effects/shaders/tonemap_common.wgsl → crates/manifold-node-engine/src/runtime/effects/shaders/tonemap_common.wgsl
crates/manifold-renderer/src/frame_status.rs → crates/manifold-node-engine/src/runtime/frame_status.rs
crates/manifold-renderer/src/generators/clip_trigger.rs → crates/manifold-node-engine/src/clip_trigger.rs
crates/manifold-renderer/src/generators/compute_common.rs → crates/manifold-node-engine/src/particles.rs
crates/manifold-renderer/src/generators/line_pipeline.rs → crates/manifold-node-engine/src/line.rs
crates/manifold-renderer/src/generators/mesh_common.rs → crates/manifold-node-engine/src/mesh.rs
crates/manifold-renderer/src/generators/mesh_pipeline.rs → crates/manifold-node-engine/src/mesh/pipeline.rs
crates/manifold-renderer/src/generators/platonic_geometry.rs → crates/manifold-node-engine/src/platonic.rs
crates/manifold-renderer/src/generators/shaders/fluid_blur_3d.wgsl → crates/manifold-node-engine/src/shaders/fluid_blur_3d.wgsl
crates/manifold-renderer/src/generators/shaders/generator_lines.wgsl → crates/manifold-node-engine/src/shaders/generator_lines.wgsl
crates/manifold-renderer/src/generators/shaders/mesh_pipeline.wgsl → crates/manifold-node-engine/src/shaders/mesh_pipeline.wgsl
crates/manifold-renderer/src/generators/shaders/particle_common.wgsl → crates/manifold-node-engine/src/shaders/particle_common.wgsl
crates/manifold-renderer/src/generators/stateful_base.rs → crates/manifold-node-engine/src/stateful.rs
crates/manifold-renderer/src/gpu.rs → crates/manifold-node-engine/src/gpu/context.rs
crates/manifold-renderer/src/gpu_encoder.rs → crates/manifold-node-engine/src/gpu/gpu_encoder.rs
crates/manifold-renderer/src/gpu_types.rs → crates/manifold-node-engine/src/gpu/gpu_types.rs
crates/manifold-renderer/src/layer_skin.rs → crates/manifold-node-engine/src/runtime/layer_skin.rs
crates/manifold-renderer/src/node_graph/atmosphere.rs → crates/manifold-node-engine/src/scene/atmosphere.rs
crates/manifold-renderer/src/node_graph/atomic/fluid_sim.rs → crates/manifold-node-engine/src/atomic/fluid_sim.rs
crates/manifold-renderer/src/node_graph/atomic/glitch.rs → crates/manifold-node-engine/src/atomic/glitch.rs
crates/manifold-renderer/src/node_graph/atomic/mod.rs → crates/manifold-node-engine/src/atomic/mod.rs
crates/manifold-renderer/src/node_graph/atomic/plasma.rs → crates/manifold-node-engine/src/atomic/plasma.rs
crates/manifold-renderer/src/node_graph/backend.rs → crates/manifold-node-engine/src/exec/backend.rs
crates/manifold-renderer/src/node_graph/binding_migration.rs → crates/manifold-node-engine/src/load/binding_migration.rs
crates/manifold-renderer/src/node_graph/bindings.rs → crates/manifold-node-engine/src/bindings.rs
crates/manifold-renderer/src/node_graph/bound_graph.rs → crates/manifold-node-engine/src/exec/bound_graph.rs
crates/manifold-renderer/src/node_graph/boundary_nodes.rs → crates/manifold-node-engine/src/scene/boundary_nodes.rs
crates/manifold-renderer/src/node_graph/builtins.rs → crates/manifold-node-engine/src/builtins.rs
crates/manifold-renderer/src/node_graph/camera.rs → crates/manifold-node-engine/src/scene/camera.rs
crates/manifold-renderer/src/node_graph/chain_spec.rs → crates/manifold-node-engine/src/load/chain_spec.rs
crates/manifold-renderer/src/node_graph/channel_names.rs → crates/manifold-node-engine/src/channel_names.rs
crates/manifold-renderer/src/node_graph/content_revision.rs → crates/manifold-node-engine/src/content_revision.rs
crates/manifold-renderer/src/node_graph/depth_rule.rs → crates/manifold-node-engine/src/scene/depth_rule.rs
crates/manifold-renderer/src/node_graph/descriptor.rs → crates/manifold-node-engine/src/descriptor.rs
crates/manifold-renderer/src/node_graph/effect_node.rs → crates/manifold-node-engine/src/exec/effect_node.rs
crates/manifold-renderer/src/node_graph/execution.rs → crates/manifold-node-engine/src/exec/execution.rs
crates/manifold-renderer/src/node_graph/execution/array_growth.rs → crates/manifold-node-engine/src/exec/execution/array_growth.rs
crates/manifold-renderer/src/node_graph/execution/coupled_physics.rs → crates/manifold-node-engine/src/exec/execution/coupled_physics.rs
crates/manifold-renderer/src/node_graph/execution/substep_region.rs → crates/manifold-node-engine/src/exec/execution/substep_region.rs
crates/manifold-renderer/src/node_graph/execution_plan.rs → crates/manifold-node-engine/src/exec/execution_plan.rs
crates/manifold-renderer/src/node_graph/fluid.rs → crates/manifold-node-engine/src/water/fluid.rs
crates/manifold-renderer/src/node_graph/fluid/coupled.rs → crates/manifold-node-engine/src/water/fluid/coupled.rs
crates/manifold-renderer/src/node_graph/fluid/coupled/native.rs → crates/manifold-node-engine/src/water/fluid/coupled/native.rs
crates/manifold-renderer/src/node_graph/fluid/coupled/tests.rs → crates/manifold-node-engine/src/water/fluid/coupled/tests.rs
crates/manifold-renderer/src/node_graph/fluid/coupled/tests/vortex.rs → crates/manifold-node-engine/src/water/fluid/coupled/tests/vortex.rs
crates/manifold-renderer/src/node_graph/fluid/domain.rs → crates/manifold-node-engine/src/water/fluid/domain.rs
crates/manifold-renderer/src/node_graph/fluid/identity.rs → crates/manifold-node-engine/src/water/fluid/identity.rs
crates/manifold-renderer/src/node_graph/fluid/impulses.rs → crates/manifold-node-engine/src/water/fluid/impulses.rs
crates/manifold-renderer/src/node_graph/fluid/impulses/tests.rs → crates/manifold-node-engine/src/water/fluid/impulses/tests.rs
crates/manifold-renderer/src/node_graph/fluid/native.rs → crates/manifold-node-engine/src/water/fluid/native.rs
crates/manifold-renderer/src/node_graph/fluid/particle_ring.rs → crates/manifold-node-engine/src/water/fluid/particle_ring.rs
crates/manifold-renderer/src/node_graph/fluid/particle_tests.rs → crates/manifold-node-engine/src/water/fluid/particle_tests.rs
crates/manifold-renderer/src/node_graph/fluid/playback_tests.rs → crates/manifold-node-engine/src/water/fluid/playback_tests.rs
crates/manifold-renderer/src/node_graph/fluid/race_probe.rs → crates/manifold-node-engine/src/water/fluid/race_probe.rs
crates/manifold-renderer/src/node_graph/fluid/roles.rs → crates/manifold-node-engine/src/water/fluid/roles.rs
crates/manifold-renderer/src/node_graph/fluid/take.rs → crates/manifold-node-engine/src/water/fluid/take.rs
crates/manifold-renderer/src/node_graph/fluid/take/geometry.rs → crates/manifold-node-engine/src/water/fluid/take/geometry.rs
crates/manifold-renderer/src/node_graph/fluid/take/playback.rs → crates/manifold-node-engine/src/water/fluid/take/playback.rs
crates/manifold-renderer/src/node_graph/fluid/take/source_tests.rs → crates/manifold-node-engine/src/water/fluid/take/source_tests.rs
crates/manifold-renderer/src/node_graph/fluid/take/tempo.rs → crates/manifold-node-engine/src/water/fluid/take/tempo.rs
crates/manifold-renderer/src/node_graph/fluid/take/tests.rs → crates/manifold-node-engine/src/water/fluid/take/tests.rs
crates/manifold-renderer/src/node_graph/fluid/take/timing.rs → crates/manifold-node-engine/src/water/fluid/take/timing.rs
crates/manifold-renderer/src/node_graph/fluid_cache.rs → crates/manifold-node-engine/src/water/fluid_cache.rs
crates/manifold-renderer/src/node_graph/fluid_mesh_upload.rs → crates/manifold-node-engine/src/water/fluid_mesh_upload.rs
crates/manifold-renderer/src/node_graph/fluid_particles.rs → crates/manifold-node-engine/src/water/fluid_particles.rs
crates/manifold-renderer/src/node_graph/fluid_role.rs → crates/manifold-node-engine/src/water/fluid_role.rs
crates/manifold-renderer/src/node_graph/fragment_mask_continuity_tests.rs → crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs
crates/manifold-renderer/src/node_graph/freeze/classify.rs → crates/manifold-node-engine/src/freeze/classify.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/binding_contract_tests.rs → crates/manifold-node-engine/src/freeze/codegen/binding_contract_tests.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/dispatch_contract_tests.rs → crates/manifold-node-engine/src/freeze/codegen/dispatch_contract_tests.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/entry_points.rs → crates/manifold-node-engine/src/freeze/codegen/entry_points.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/fused.rs → crates/manifold-node-engine/src/freeze/codegen/fused.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/fused_buffer.rs → crates/manifold-node-engine/src/freeze/codegen/fused_buffer.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/gpu_tests.rs → crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/mod.rs → crates/manifold-node-engine/src/freeze/codegen/mod.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/params_struct.rs → crates/manifold-node-engine/src/freeze/codegen/params_struct.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/standalone.rs → crates/manifold-node-engine/src/freeze/codegen/standalone.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/types.rs → crates/manifold-node-engine/src/freeze/codegen/types.rs
crates/manifold-renderer/src/node_graph/freeze/codegen/uniforms.rs → crates/manifold-node-engine/src/freeze/codegen/uniforms.rs
crates/manifold-renderer/src/node_graph/freeze/derived_uniform_registry.rs → crates/manifold-node-engine/src/freeze/derived_uniform_registry.rs
crates/manifold-renderer/src/node_graph/freeze/diff.rs → crates/manifold-node-engine/src/freeze/diff.rs
crates/manifold-renderer/src/node_graph/freeze/fusion_report.rs → crates/manifold-node-engine/src/freeze/fusion_report.rs
crates/manifold-renderer/src/node_graph/freeze/install.rs → crates/manifold-node-engine/src/freeze/install.rs
crates/manifold-renderer/src/node_graph/freeze/markers.rs → crates/manifold-node-engine/src/freeze/markers.rs
crates/manifold-renderer/src/node_graph/freeze/mod.rs → crates/manifold-node-engine/src/freeze/mod.rs
crates/manifold-renderer/src/node_graph/freeze/proof.rs → crates/manifold-node-engine/src/freeze/proof.rs
crates/manifold-renderer/src/node_graph/freeze/proof/audio_visual.rs → crates/manifold-node-engine/src/freeze/proof/audio_visual.rs
crates/manifold-renderer/src/node_graph/freeze/reference.rs → crates/manifold-node-engine/src/freeze/reference.rs
crates/manifold-renderer/src/node_graph/freeze/region.rs → crates/manifold-node-engine/src/freeze/region.rs
crates/manifold-renderer/src/node_graph/freeze/region/census.rs → crates/manifold-node-engine/src/freeze/region/census.rs
crates/manifold-renderer/src/node_graph/freeze/segment.rs → crates/manifold-node-engine/src/freeze/segment.rs
crates/manifold-renderer/src/node_graph/freeze/shaders/colorgrade_fused.wgsl → crates/manifold-node-engine/src/freeze/shaders/colorgrade_fused.wgsl
crates/manifold-renderer/src/node_graph/freeze/shaders/diff_reduce.wgsl → crates/manifold-node-engine/src/freeze/shaders/diff_reduce.wgsl
crates/manifold-renderer/src/node_graph/freeze/shaders/gain_fused.wgsl → crates/manifold-node-engine/src/freeze/shaders/gain_fused.wgsl
crates/manifold-renderer/src/node_graph/freeze/space.rs → crates/manifold-node-engine/src/freeze/space.rs
crates/manifold-renderer/src/node_graph/gltf_anim_identity.rs → crates/manifold-node-engine/src/scene/gltf_anim_identity.rs
crates/manifold-renderer/src/node_graph/graph.rs → crates/manifold-node-engine/src/graph.rs
crates/manifold-renderer/src/node_graph/graph_loader.rs → crates/manifold-node-engine/src/load/graph_loader.rs
crates/manifold-renderer/src/node_graph/instance_upload.rs → crates/manifold-node-engine/src/exec/instance_upload.rs
crates/manifold-renderer/src/node_graph/light.rs → crates/manifold-node-engine/src/scene/light.rs
crates/manifold-renderer/src/node_graph/liquid.rs → crates/manifold-node-engine/src/water/liquid.rs
crates/manifold-renderer/src/node_graph/liquid/bodies.rs → crates/manifold-node-engine/src/water/liquid/bodies.rs
crates/manifold-renderer/src/node_graph/liquid/body_buffers.rs → crates/manifold-node-engine/src/water/liquid/body_buffers.rs
crates/manifold-renderer/src/node_graph/liquid/clock.rs → crates/manifold-node-engine/src/water/liquid/clock.rs
crates/manifold-renderer/src/node_graph/liquid/conformance.rs → crates/manifold-node-engine/src/water/liquid/conformance.rs
crates/manifold-renderer/src/node_graph/liquid/coupling.rs → crates/manifold-node-engine/src/water/liquid/coupling.rs
crates/manifold-renderer/src/node_graph/liquid/display_cursor.rs → crates/manifold-node-engine/src/water/liquid/display_cursor.rs
crates/manifold-renderer/src/node_graph/liquid/extent.rs → crates/manifold-node-engine/src/water/liquid/extent.rs
crates/manifold-renderer/src/node_graph/liquid/fields.rs → crates/manifold-node-engine/src/water/liquid/fields.rs
crates/manifold-renderer/src/node_graph/liquid/fields/tests.rs → crates/manifold-node-engine/src/water/liquid/fields/tests.rs
crates/manifold-renderer/src/node_graph/liquid/frame_history.rs → crates/manifold-node-engine/src/water/liquid/frame_history.rs
crates/manifold-renderer/src/node_graph/liquid/frame_ring.rs → crates/manifold-node-engine/src/water/liquid/frame_ring.rs
crates/manifold-renderer/src/node_graph/liquid/grid.rs → crates/manifold-node-engine/src/water/liquid/grid.rs
crates/manifold-renderer/src/node_graph/liquid/lattice.rs → crates/manifold-node-engine/src/water/liquid/lattice.rs
crates/manifold-renderer/src/node_graph/liquid/migration.rs → crates/manifold-node-engine/src/water/liquid/migration.rs
crates/manifold-renderer/src/node_graph/liquid/scene_contract.rs → crates/manifold-node-engine/src/water/liquid/scene_contract.rs
crates/manifold-renderer/src/node_graph/liquid/substep_history.rs → crates/manifold-node-engine/src/water/liquid/substep_history.rs
crates/manifold-renderer/src/node_graph/liquid/tick_samples.rs → crates/manifold-node-engine/src/water/liquid/tick_samples.rs
crates/manifold-renderer/src/node_graph/live_extent.rs → crates/manifold-node-engine/src/scene/live_extent.rs
crates/manifold-renderer/src/node_graph/loaded_preset_view.rs → crates/manifold-node-engine/src/load/loaded_preset_view.rs
crates/manifold-renderer/src/node_graph/material.rs → crates/manifold-node-engine/src/scene/material.rs
crates/manifold-renderer/src/node_graph/matter.rs → crates/manifold-node-engine/src/water/matter.rs
crates/manifold-renderer/src/node_graph/matter/coupling.rs → crates/manifold-node-engine/src/water/matter/coupling.rs
crates/manifold-renderer/src/node_graph/matter/look.rs → crates/manifold-node-engine/src/water/matter/look.rs
crates/manifold-renderer/src/node_graph/matter/reference.rs → crates/manifold-node-engine/src/water/matter/reference.rs
crates/manifold-renderer/src/node_graph/mesh_boundary.rs → crates/manifold-node-engine/src/scene/mesh_boundary.rs
crates/manifold-renderer/src/node_graph/mesh_change.rs → crates/manifold-node-engine/src/scene/mesh_change.rs
crates/manifold-renderer/src/node_graph/mesh_cut.rs → crates/manifold-node-engine/src/scene/mesh_cut.rs
crates/manifold-renderer/src/node_graph/mesh_partition.rs → crates/manifold-node-engine/src/scene/mesh_partition.rs
crates/manifold-renderer/src/node_graph/mesh_source.rs → crates/manifold-node-engine/src/scene/mesh_source.rs
crates/manifold-renderer/src/node_graph/metal_backend.rs → crates/manifold-node-engine/src/exec/metal_backend.rs
crates/manifold-renderer/src/node_graph/migration.rs → crates/manifold-node-engine/src/load/migration.rs
crates/manifold-renderer/src/node_graph/palette.rs → crates/manifold-node-engine/src/palette.rs
crates/manifold-renderer/src/node_graph/param_binding.rs → crates/manifold-node-engine/src/param_binding.rs
crates/manifold-renderer/src/node_graph/param_doc.rs → crates/manifold-node-engine/src/param_doc.rs
crates/manifold-renderer/src/node_graph/param_tooltips_bulk.rs → crates/manifold-node-engine/src/param_tooltips_bulk.rs
crates/manifold-renderer/src/node_graph/param_tooltips_table.rs → crates/manifold-node-engine/src/param_tooltips_table.rs
crates/manifold-renderer/src/node_graph/parameters.rs → crates/manifold-node-engine/src/parameters.rs
crates/manifold-renderer/src/node_graph/persistence.rs → crates/manifold-node-engine/src/persistence.rs
crates/manifold-renderer/src/node_graph/physics.rs → crates/manifold-node-engine/src/water/physics.rs
crates/manifold-renderer/src/node_graph/physics/coupling_tests.rs → crates/manifold-node-engine/src/water/physics/coupling_tests.rs
crates/manifold-renderer/src/node_graph/physics/impulses.rs → crates/manifold-node-engine/src/water/physics/impulses.rs
crates/manifold-renderer/src/node_graph/physics/impulses/tests.rs → crates/manifold-node-engine/src/water/physics/impulses/tests.rs
crates/manifold-renderer/src/node_graph/physics/serialization.rs → crates/manifold-node-engine/src/water/physics/serialization.rs
crates/manifold-renderer/src/node_graph/physics/targeted_fields.rs → crates/manifold-node-engine/src/water/physics/targeted_fields.rs
crates/manifold-renderer/src/node_graph/physics/worker.rs → crates/manifold-node-engine/src/water/physics/worker.rs
crates/manifold-renderer/src/node_graph/physics_events.rs → crates/manifold-node-engine/src/water/physics_events.rs
crates/manifold-renderer/src/node_graph/physics_mesh.rs → crates/manifold-node-engine/src/scene/physics_mesh.rs
crates/manifold-renderer/src/node_graph/physics_metrics.rs → crates/manifold-node-engine/src/water/physics_metrics.rs
crates/manifold-renderer/src/node_graph/physics_scene.rs → crates/manifold-node-engine/src/water/physics_scene.rs
crates/manifold-renderer/src/node_graph/ports.rs → crates/manifold-node-engine/src/ports.rs
crates/manifold-renderer/src/node_graph/preview_encoding.rs → crates/manifold-node-engine/src/preview_encoding.rs
crates/manifold-renderer/src/node_graph/primitive.rs → crates/manifold-node-engine/src/primitive.rs
crates/manifold-renderer/src/node_graph/primitives/compose.rs → crates/manifold-node-engine/src/primitives/mix.rs
crates/manifold-renderer/src/node_graph/primitives/count_surface_triangles.rs → crates/manifold-node-engine/src/water/primitives/count_surface_triangles.rs
crates/manifold-renderer/src/node_graph/primitives/crossing_distance.rs → crates/manifold-node-engine/src/water/primitives/crossing_distance.rs
crates/manifold-renderer/src/node_graph/primitives/dot_products.rs → crates/manifold-node-engine/src/water/primitives/dot_products.rs
crates/manifold-renderer/src/node_graph/primitives/emission_count.rs → crates/manifold-node-engine/src/water/primitives/emission_count.rs
crates/manifold-renderer/src/node_graph/primitives/energy_potential.rs → crates/manifold-node-engine/src/water/primitives/energy_potential.rs
crates/manifold-renderer/src/node_graph/primitives/extend_lattice.rs → crates/manifold-node-engine/src/water/primitives/extend_lattice.rs
crates/manifold-renderer/src/node_graph/primitives/face_sample_component.rs → crates/manifold-node-engine/src/water/primitives/face_sample_component.rs
crates/manifold-renderer/src/node_graph/primitives/fluid_surface.rs → crates/manifold-node-engine/src/water/primitives/fluid_surface.rs
crates/manifold-renderer/src/node_graph/primitives/gain.rs → crates/manifold-node-engine/src/primitives/gain.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_atom_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_atom_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_bodies.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_bodies.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_body_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_body_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_clock.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_clock.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_domain.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_domain.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_extension_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_extension_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_lentine.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_lentine.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_narrow_band.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_narrow_band.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_narrow_band_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_narrow_band_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_preset.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_pressure.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_pressure.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_pressure_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_pressure_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_race_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_race_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_render_smoke_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_render_smoke_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_scene_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_scene_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_sheeting.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_sheeting.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_sheeting_cpu_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_sheeting_cpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_sheeting_step_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_sheeting_step_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_sheeting_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_sheeting_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_step.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_step.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_step_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_step_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_still.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_still.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_tile_tests.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_tile_tests.rs
crates/manifold-renderer/src/node_graph/primitives/gpu_flip_volume.rs → crates/manifold-node-engine/src/water/primitives/gpu_flip_volume.rs
crates/manifold-renderer/src/node_graph/primitives/keep_whitewater.rs → crates/manifold-node-engine/src/water/primitives/keep_whitewater.rs
crates/manifold-renderer/src/node_graph/primitives/lattice_bricks.rs → crates/manifold-node-engine/src/water/primitives/lattice_bricks.rs
crates/manifold-renderer/src/node_graph/primitives/lattice_closing_gpu_tests.rs → crates/manifold-node-engine/src/water/primitives/lattice_closing_gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/lattice_closing_tests.rs → crates/manifold-node-engine/src/water/primitives/lattice_closing_tests.rs
crates/manifold-renderer/src/node_graph/primitives/lattice_curvature.rs → crates/manifold-node-engine/src/water/primitives/lattice_curvature.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_bricks.rs → crates/manifold-node-engine/src/water/primitives/liquid_bricks.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_bricks_gpu_tests.rs → crates/manifold-node-engine/src/water/primitives/liquid_bricks_gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_bricks_tests.rs → crates/manifold-node-engine/src/water/primitives/liquid_bricks_tests.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_cells.rs → crates/manifold-node-engine/src/water/primitives/liquid_cells.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_fill.rs → crates/manifold-node-engine/src/water/primitives/liquid_fill.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_frame.rs → crates/manifold-node-engine/src/water/primitives/liquid_frame.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_prepare_tests.rs → crates/manifold-node-engine/src/water/primitives/liquid_prepare_tests.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_solid_distance.rs → crates/manifold-node-engine/src/water/primitives/liquid_solid_distance.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_state.rs → crates/manifold-node-engine/src/water/primitives/liquid_state.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_stats.rs → crates/manifold-node-engine/src/water/primitives/liquid_stats.rs
crates/manifold-renderer/src/node_graph/primitives/liquid_surface_tests.rs → crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs
crates/manifold-renderer/src/node_graph/primitives/masked_mix.rs → crates/manifold-node-engine/src/primitives/masked_mix.rs
crates/manifold-renderer/src/node_graph/primitives/matter_body_reaction.rs → crates/manifold-node-engine/src/water/primitives/matter_body_reaction.rs
crates/manifold-renderer/src/node_graph/primitives/matter_common.rs → crates/manifold-node-engine/src/water/primitives/matter_common.rs
crates/manifold-renderer/src/node_graph/primitives/matter_domain.rs → crates/manifold-node-engine/src/water/primitives/matter_domain.rs
crates/manifold-renderer/src/node_graph/primitives/matter_face_component.rs → crates/manifold-node-engine/src/water/primitives/matter_face_component.rs
crates/manifold-renderer/src/node_graph/primitives/matter_fill.rs → crates/manifold-node-engine/src/water/primitives/matter_fill.rs
crates/manifold-renderer/src/node_graph/primitives/matter_frame.rs → crates/manifold-node-engine/src/water/primitives/matter_frame.rs
crates/manifold-renderer/src/node_graph/primitives/matter_grid_update.rs → crates/manifold-node-engine/src/water/primitives/matter_grid_update.rs
crates/manifold-renderer/src/node_graph/primitives/matter_move_bodies.rs → crates/manifold-node-engine/src/water/primitives/matter_move_bodies.rs
crates/manifold-renderer/src/node_graph/primitives/matter_state.rs → crates/manifold-node-engine/src/water/primitives/matter_state.rs
crates/manifold-renderer/src/node_graph/primitives/matter_stats.rs → crates/manifold-node-engine/src/water/primitives/matter_stats.rs
crates/manifold-renderer/src/node_graph/primitives/matter_to_grid.rs → crates/manifold-node-engine/src/water/primitives/matter_to_grid.rs
crates/manifold-renderer/src/node_graph/primitives/mux_texture.rs → crates/manifold-node-engine/src/primitives/mux_texture.rs
crates/manifold-renderer/src/node_graph/primitives/nearest_crossing.rs → crates/manifold-node-engine/src/water/primitives/nearest_crossing.rs
crates/manifold-renderer/src/node_graph/primitives/pad_distance_lattice.rs → crates/manifold-node-engine/src/water/primitives/pad_distance_lattice.rs
crates/manifold-renderer/src/node_graph/primitives/particle_identity.rs → crates/manifold-node-engine/src/water/primitives/particle_identity.rs
crates/manifold-renderer/src/node_graph/primitives/particle_publication.rs → crates/manifold-node-engine/src/water/primitives/particle_publication.rs
crates/manifold-renderer/src/node_graph/primitives/particle_volume.rs → crates/manifold-node-engine/src/water/primitives/particle_volume.rs
crates/manifold-renderer/src/node_graph/primitives/physics_world.rs → crates/manifold-node-engine/src/water/primitives/physics_world.rs
crates/manifold-renderer/src/node_graph/primitives/prefix_scan.rs → crates/manifold-node-engine/src/water/primitives/prefix_scan.rs
crates/manifold-renderer/src/node_graph/primitives/preserve_foam.rs → crates/manifold-node-engine/src/water/primitives/preserve_foam.rs
crates/manifold-renderer/src/node_graph/primitives/push_out_of_solid.rs → crates/manifold-node-engine/src/water/primitives/push_out_of_solid.rs
crates/manifold-renderer/src/node_graph/primitives/running_total.rs → crates/manifold-node-engine/src/water/primitives/running_total.rs
crates/manifold-renderer/src/node_graph/primitives/shaders/abs_texture.wgsl → crates/manifold-node-engine/src/primitives/shaders/abs_texture.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/advect_whitewater_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/advect_whitewater_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/age_whitewater_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/age_whitewater_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/basic_shape.wgsl → crates/manifold-node-engine/src/primitives/shaders/basic_shape.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/block_displace_field.wgsl → crates/manifold-node-engine/src/primitives/shaders/block_displace_field.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/clamp_liquid_to_solids_dense_reference.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/clamp_liquid_to_solids_dense_reference.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/clamp_liquid_to_solids_element.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/clamp_liquid_to_solids_element.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/coarse_inverse.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/coarse_inverse.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/count_surface_triangles_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/count_surface_triangles_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/count_surface_triangles_dense_reference.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/count_surface_triangles_dense_reference.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/crossing_distance_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/crossing_distance_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/curl_slope_force_3d.wgsl → crates/manifold-node-engine/src/primitives/shaders/curl_slope_force_3d.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/dither.wgsl → crates/manifold-node-engine/src/primitives/shaders/dither.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/dot_products.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/dot_products.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/downsample.wgsl → crates/manifold-node-engine/src/primitives/shaders/downsample.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/dust_potential_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/dust_potential_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/emission_count_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/emission_count_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/energy_potential_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/energy_potential_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/extend_lattice_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/extend_lattice_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/face_sample_component_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/face_sample_component_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/fluid_mesh_upload.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/fluid_mesh_upload.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gain.wgsl → crates/manifold-node-engine/src/primitives/shaders/gain.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gain_body.wgsl → crates/manifold-node-engine/src/primitives/shaders/gain_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gaussian_blur_variable_width.wgsl → crates/manifold-node-engine/src/primitives/shaders/gaussian_blur_variable_width.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gpu_flip_bodies.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/gpu_flip_bodies.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gpu_flip_clock.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/gpu_flip_clock.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gpu_flip_commit_mask.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/gpu_flip_commit_mask.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gpu_flip_lentine.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/gpu_flip_lentine.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gpu_flip_narrow_band.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/gpu_flip_narrow_band.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gpu_flip_pressure.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/gpu_flip_pressure.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gpu_flip_sheeting.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/gpu_flip_sheeting.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gpu_flip_step.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/gpu_flip_step.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gradient_central_diff_3d.wgsl → crates/manifold-node-engine/src/primitives/shaders/gradient_central_diff_3d.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/gradient_ramp.wgsl → crates/manifold-node-engine/src/primitives/shaders/gradient_ramp.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/grid_to_matter_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/grid_to_matter_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/hash_field_by_seed.wgsl → crates/manifold-node-engine/src/primitives/shaders/hash_field_by_seed.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/inside_turbulence_potential_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/inside_turbulence_potential_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/jitter_particles_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/jitter_particles_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/keep_whitewater_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/keep_whitewater_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/lattice_bricks.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/lattice_bricks.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/lattice_bricks_gather_reference.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/lattice_bricks_gather_reference.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/lattice_curvature_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/lattice_curvature_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/lic_integrate.wgsl → crates/manifold-node-engine/src/primitives/shaders/lic_integrate.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_body_upload.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_body_upload.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_bricks_common.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_bricks_common.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_cells_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_cells_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_collider.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_collider.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_faces.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_faces.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_field.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_field.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_fill_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_fill_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_frame_faces.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_frame_faces.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_pose.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_pose.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_solid_distance_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_solid_distance_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/liquid_stats.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/liquid_stats.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/marching_cubes_common.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/marching_cubes_common.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/masked_mix_body.wgsl → crates/manifold-node-engine/src/primitives/shaders/masked_mix_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/matter_body_reaction_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/matter_body_reaction_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/matter_face_component_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/matter_face_component_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/matter_fill_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/matter_fill_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/matter_frame.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/matter_frame.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/matter_grid_update_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/matter_grid_update_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/matter_move_bodies_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/matter_move_bodies_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/matter_stats.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/matter_stats.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/matter_to_grid.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/matter_to_grid.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/matter_walls.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/matter_walls.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/mix.wgsl → crates/manifold-node-engine/src/primitives/shaders/mix.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/mix_body.wgsl → crates/manifold-node-engine/src/primitives/shaders/mix_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/nearest_crossing_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/nearest_crossing_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/pack_channels.wgsl → crates/manifold-node-engine/src/primitives/shaders/pack_channels.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/pad_distance_lattice.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/pad_distance_lattice.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/particle_identity.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/particle_identity.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/particle_publication.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/particle_publication.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/particle_volume_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/particle_volume_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/particle_volume_dense_reference.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/particle_volume_dense_reference.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/physics_instance_upload.wgsl → crates/manifold-node-engine/src/primitives/shaders/physics_instance_upload.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/prefix_scan.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/prefix_scan.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/preserve_foam_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/preserve_foam_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/push_out_of_solid_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/push_out_of_solid_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/relax_surface_mesh_dense_reference.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/relax_surface_mesh_dense_reference.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/remap.wgsl → crates/manifold-node-engine/src/primitives/shaders/remap.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/retype_whitewater_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/retype_whitewater_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/running_total.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/running_total.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/sample_faces_at_particles_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/sample_faces_at_particles_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/sample_volume_2d.wgsl → crates/manifold-node-engine/src/primitives/shaders/sample_volume_2d.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/separable_gaussian.wgsl → crates/manifold-node-engine/src/primitives/shaders/separable_gaussian.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/smooth_lattice_dense_reference.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/smooth_lattice_dense_reference.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/sort_particles_into_cells.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/sort_particles_into_cells.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/spawn_whitewater_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/spawn_whitewater_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/surface_crossings_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/surface_crossings_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/surface_edge_index.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/surface_edge_index.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/surface_edge_ownership.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/surface_edge_ownership.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/trig_texture.wgsl → crates/manifold-node-engine/src/primitives/shaders/trig_texture.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/turbulence_emission_count_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/turbulence_emission_count_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/turbulence_field_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/turbulence_field_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/upwind_distance_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/upwind_distance_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/vignette.wgsl → crates/manifold-node-engine/src/primitives/shaders/vignette.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/volume_surface_mesh_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/volume_surface_mesh_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/volume_surface_mesh_dense_reference.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/volume_surface_mesh_dense_reference.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/voronoi_2d.wgsl → crates/manifold-node-engine/src/primitives/shaders/voronoi_2d.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/wavecrest_potential_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/wavecrest_potential_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/whitewater_common.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/whitewater_common.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/whitewater_distance.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/whitewater_distance.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/whitewater_emitter_velocity_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/whitewater_emitter_velocity_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/whitewater_fused.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/whitewater_fused.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/whitewater_influence_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/whitewater_influence_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/whitewater_obstacle_source_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/whitewater_obstacle_source_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/whitewater_step.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/whitewater_step.wgsl
crates/manifold-renderer/src/node_graph/primitives/shaders/whitewater_type_body.wgsl → crates/manifold-node-engine/src/water/primitives/shaders/whitewater_type_body.wgsl
crates/manifold-renderer/src/node_graph/primitives/sort_particles_into_cells.rs → crates/manifold-node-engine/src/water/primitives/sort_particles_into_cells.rs
crates/manifold-renderer/src/node_graph/primitives/sort_particles_into_cells_gpu_tests.rs → crates/manifold-node-engine/src/water/primitives/sort_particles_into_cells_gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/standalone_pipeline.rs → crates/manifold-node-engine/src/primitives/standalone_pipeline.rs
crates/manifold-renderer/src/node_graph/primitives/surface_crossings.rs → crates/manifold-node-engine/src/water/primitives/surface_crossings.rs
crates/manifold-renderer/src/node_graph/primitives/upwind_distance.rs → crates/manifold-node-engine/src/water/primitives/upwind_distance.rs
crates/manifold-renderer/src/node_graph/primitives/value.rs → crates/manifold-node-engine/src/primitives/value.rs
crates/manifold-renderer/src/node_graph/primitives/volume_surface_mesh.rs → crates/manifold-node-engine/src/water/primitives/volume_surface_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs → crates/manifold-node-engine/src/primitives/wgsl_compute.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_copy_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_copy_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_cpu.rs → crates/manifold-node-engine/src/water/primitives/whitewater_cpu.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_distance.rs → crates/manifold-node-engine/src/water/primitives/whitewater_distance.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_emitter_cpu.rs → crates/manifold-node-engine/src/water/primitives/whitewater_emitter_cpu.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_emitter_dispatch.rs → crates/manifold-node-engine/src/water/primitives/whitewater_emitter_dispatch.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_emitter_gpu_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_emitter_gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_emitter_velocity.rs → crates/manifold-node-engine/src/water/primitives/whitewater_emitter_velocity.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_engine_cpu.rs → crates/manifold-node-engine/src/water/primitives/whitewater_engine_cpu.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_engine_gpu_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_engine_gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_extent_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_extent_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_field_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_field_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_fused_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_fused_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_golden_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_golden_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_grid_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_grid_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_handoff_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_handoff_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_influence.rs → crates/manifold-node-engine/src/water/primitives/whitewater_influence.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_lifecycle.rs → crates/manifold-node-engine/src/water/primitives/whitewater_lifecycle.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_obstacle_source.rs → crates/manifold-node-engine/src/water/primitives/whitewater_obstacle_source.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_particle_cpu.rs → crates/manifold-node-engine/src/water/primitives/whitewater_particle_cpu.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_particle_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_particle_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_pool_cpu.rs → crates/manifold-node-engine/src/water/primitives/whitewater_pool_cpu.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_pool_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_pool_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_reference.rs → crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_scene_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_scene_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_step.rs → crates/manifold-node-engine/src/water/primitives/whitewater_step.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_step_tests.rs → crates/manifold-node-engine/src/water/primitives/whitewater_step_tests.rs
crates/manifold-renderer/src/node_graph/primitives/whitewater_type.rs → crates/manifold-node-engine/src/water/primitives/whitewater_type.rs
crates/manifold-renderer/src/node_graph/render_mode.rs → crates/manifold-node-engine/src/scene/render_mode.rs
crates/manifold-renderer/src/node_graph/resource_allocation.rs → crates/manifold-node-engine/src/exec/resource_allocation.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand.rs → crates/manifold-node-engine/src/load/expand.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/acceleration.rs → crates/manifold-node-engine/src/load/expand/acceleration.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/bindings.rs → crates/manifold-node-engine/src/load/expand/bindings.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/buffer_budget.rs → crates/manifold-node-engine/src/load/expand/buffer_budget.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler.rs → crates/manifold-node-engine/src/load/expand/compiler.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/acceleration_tests.rs → crates/manifold-node-engine/src/load/expand/compiler/acceleration_tests.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/camera_endpoint_tests.rs → crates/manifold-node-engine/src/load/expand/compiler/camera_endpoint_tests.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/conformance.rs → crates/manifold-node-engine/src/load/expand/compiler/conformance.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/math_events.rs → crates/manifold-node-engine/src/load/expand/compiler/math_events.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/parameter_guard_tests.rs → crates/manifold-node-engine/src/load/expand/compiler/parameter_guard_tests.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/shatter.rs → crates/manifold-node-engine/src/load/expand/compiler/shatter.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/tests.rs → crates/manifold-node-engine/src/load/expand/compiler/tests.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/control_state.rs → crates/manifold-node-engine/src/load/expand/control_state.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/coupling.rs → crates/manifold-node-engine/src/load/expand/coupling.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/event_state.rs → crates/manifold-node-engine/src/load/expand/event_state.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/fragment_cuts.rs → crates/manifold-node-engine/src/load/expand/fragment_cuts.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/frames.rs → crates/manifold-node-engine/src/load/expand/frames.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/impulses.rs → crates/manifold-node-engine/src/load/expand/impulses.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/math_view.rs → crates/manifold-node-engine/src/load/expand/math_view.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/namespace.rs → crates/manifold-node-engine/src/load/expand/namespace.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/parameter_guards.rs → crates/manifold-node-engine/src/load/expand/parameter_guards.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/routes.rs → crates/manifold-node-engine/src/load/expand/routes.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/value_sources.rs → crates/manifold-node-engine/src/load/expand/value_sources.rs
crates/manifold-renderer/src/node_graph/scene_modifier_expand/value_writes.rs → crates/manifold-node-engine/src/load/expand/value_writes.rs
crates/manifold-renderer/src/node_graph/scene_object.rs → crates/manifold-node-engine/src/scene/scene_object.rs
crates/manifold-renderer/src/node_graph/scene_viewport.rs → crates/manifold-node-engine/src/scene/scene_viewport.rs
crates/manifold-renderer/src/node_graph/snapshot.rs → crates/manifold-node-engine/src/snapshot.rs
crates/manifold-renderer/src/node_graph/snapshot/scene_modifier_tests.rs → crates/manifold-node-engine/src/snapshot/scene_modifier_tests.rs
crates/manifold-renderer/src/node_graph/source_asset.rs → crates/manifold-node-engine/src/scene/source_asset.rs
crates/manifold-renderer/src/node_graph/state_store.rs → crates/manifold-node-engine/src/state_store.rs
crates/manifold-renderer/src/node_graph/substeps.rs → crates/manifold-node-engine/src/exec/substeps.rs
crates/manifold-renderer/src/node_graph/temporal_reset.rs → crates/manifold-node-engine/src/exec/temporal_reset.rs
crates/manifold-renderer/src/node_graph/transform.rs → crates/manifold-node-engine/src/scene/transform.rs
crates/manifold-renderer/src/node_graph/trigger_shadow_lint.rs → crates/manifold-node-engine/src/trigger_shadow_lint.rs
crates/manifold-renderer/src/node_graph/validate.rs → crates/manifold-node-engine/src/validate.rs
crates/manifold-renderer/src/node_graph/validation.rs → crates/manifold-node-engine/src/validation.rs
crates/manifold-renderer/src/node_graph/vector_field.rs → crates/manifold-node-engine/src/scene/vector_field.rs
crates/manifold-renderer/src/node_graph/viewport_camera.rs → crates/manifold-node-engine/src/scene/viewport_camera.rs
crates/manifold-renderer/src/node_graph/whitewater.rs → crates/manifold-node-engine/src/water/whitewater.rs
crates/manifold-renderer/src/node_graph/whitewater_handoff.rs → crates/manifold-node-engine/src/water/whitewater_handoff.rs
crates/manifold-renderer/src/plugin_prewarm.rs → crates/manifold-node-engine/src/runtime/plugin_prewarm.rs
crates/manifold-renderer/src/preset_context.rs → crates/manifold-node-engine/src/runtime/preset_context.rs
crates/manifold-renderer/src/preset_loader.rs → crates/manifold-node-engine/src/load/preset_loader.rs
crates/manifold-renderer/src/preset_loader/blob_mask.rs → crates/manifold-node-engine/src/load/preset_loader/blob_mask.rs
crates/manifold-renderer/src/preset_runtime/bindings.rs → crates/manifold-node-engine/src/runtime/bindings.rs
crates/manifold-renderer/src/preset_runtime/build.rs → crates/manifold-node-engine/src/runtime/build.rs
crates/manifold-renderer/src/preset_runtime/convert_heal.rs → crates/manifold-node-engine/src/runtime/convert_heal.rs
crates/manifold-renderer/src/preset_runtime/core.rs → crates/manifold-node-engine/src/runtime/core.rs
crates/manifold-renderer/src/preset_runtime/debug.rs → crates/manifold-node-engine/src/runtime/debug.rs
crates/manifold-renderer/src/preset_runtime/device.rs → crates/manifold-node-engine/src/runtime/device.rs
crates/manifold-renderer/src/preset_runtime/dump_sets.rs → crates/manifold-node-engine/src/runtime/dump_sets.rs
crates/manifold-renderer/src/preset_runtime/errors.rs → crates/manifold-node-engine/src/runtime/errors.rs
crates/manifold-renderer/src/preset_runtime/errors/generator_load.rs → crates/manifold-node-engine/src/runtime/errors/generator_load.rs
crates/manifold-renderer/src/preset_runtime/gpu_flip_surface.rs → crates/manifold-node-engine/src/water/runtime/gpu_flip_surface.rs
crates/manifold-renderer/src/preset_runtime/groups.rs → crates/manifold-node-engine/src/runtime/groups.rs
crates/manifold-renderer/src/preset_runtime/instrumentation.rs → crates/manifold-node-engine/src/runtime/instrumentation.rs
crates/manifold-renderer/src/preset_runtime/lifecycle.rs → crates/manifold-node-engine/src/runtime/lifecycle.rs
crates/manifold-renderer/src/preset_runtime/math_view.rs → crates/manifold-node-engine/src/runtime/math_view.rs
crates/manifold-renderer/src/preset_runtime/math_view_events.rs → crates/manifold-node-engine/src/runtime/math_view_events.rs
crates/manifold-renderer/src/preset_runtime/mod.rs → crates/manifold-node-engine/src/runtime/mod.rs
crates/manifold-renderer/src/preset_runtime/modifier_preview.rs → crates/manifold-node-engine/src/runtime/modifier_preview.rs
crates/manifold-renderer/src/preset_runtime/modifier_runtime.rs → crates/manifold-node-engine/src/runtime/modifier_runtime.rs
crates/manifold-renderer/src/preset_runtime/physics_asset_take_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_asset_take_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_carry.rs → crates/manifold-node-engine/src/water/runtime/physics_carry.rs
crates/manifold-renderer/src/preset_runtime/physics_carry_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_carry_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_collection_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_collection_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_history_drain_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_history_drain_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_host_modulation_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_host_modulation_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_impulses.rs → crates/manifold-node-engine/src/water/runtime/physics_impulses.rs
crates/manifold-renderer/src/preset_runtime/physics_impulses/coupled_playback_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_impulses/coupled_playback_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_impulses/scene_routes_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_impulses/scene_routes_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_impulses/source_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_impulses/source_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_impulses/tests.rs → crates/manifold-node-engine/src/water/runtime/physics_impulses/tests.rs
crates/manifold-renderer/src/preset_runtime/physics_sampling.rs → crates/manifold-node-engine/src/water/runtime/physics_sampling.rs
crates/manifold-renderer/src/preset_runtime/physics_sampling_inputs_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_sampling_inputs_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_source_asset_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_source_asset_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_source_chain.rs → crates/manifold-node-engine/src/water/runtime/physics_source_chain.rs
crates/manifold-renderer/src/preset_runtime/physics_source_controls.rs → crates/manifold-node-engine/src/water/runtime/physics_source_controls.rs
crates/manifold-renderer/src/preset_runtime/physics_source_controls_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_source_controls_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_source_path_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_source_path_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_source_runtime.rs → crates/manifold-node-engine/src/water/runtime/physics_source_runtime.rs
crates/manifold-renderer/src/preset_runtime/physics_source_state.rs → crates/manifold-node-engine/src/water/runtime/physics_source_state.rs
crates/manifold-renderer/src/preset_runtime/physics_source_state_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_source_state_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_sources.rs → crates/manifold-node-engine/src/water/runtime/physics_sources.rs
crates/manifold-renderer/src/preset_runtime/physics_sources_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_sources_tests.rs
crates/manifold-renderer/src/preset_runtime/physics_string_binding_tests.rs → crates/manifold-node-engine/src/water/runtime/physics_string_binding_tests.rs
crates/manifold-renderer/src/preset_runtime/resize.rs → crates/manifold-node-engine/src/runtime/resize.rs
crates/manifold-renderer/src/preset_runtime/scene_impulses.rs → crates/manifold-node-engine/src/water/runtime/scene_impulses.rs
crates/manifold-renderer/src/preset_runtime/scene_viewport.rs → crates/manifold-node-engine/src/runtime/scene_viewport.rs
crates/manifold-renderer/src/preset_runtime/segments.rs → crates/manifold-node-engine/src/runtime/segments.rs
crates/manifold-renderer/src/preset_runtime/tests/amount_zero_passthrough.rs → crates/manifold-node-engine/src/runtime/tests/amount_zero_passthrough.rs
crates/manifold-renderer/src/preset_runtime/tests/array_buffers.rs → crates/manifold-node-engine/src/runtime/tests/array_buffers.rs
crates/manifold-renderer/src/preset_runtime/tests/binding_seed.rs → crates/manifold-node-engine/src/runtime/tests/binding_seed.rs
crates/manifold-renderer/src/preset_runtime/tests/blob_grain_probe.rs → crates/manifold-node-engine/src/runtime/tests/blob_grain_probe.rs
crates/manifold-renderer/src/preset_runtime/tests/bool_convert_heal.rs → crates/manifold-node-engine/src/runtime/tests/bool_convert_heal.rs
crates/manifold-renderer/src/preset_runtime/tests/bound_param_survives_rebuild.rs → crates/manifold-node-engine/src/runtime/tests/bound_param_survives_rebuild.rs
crates/manifold-renderer/src/preset_runtime/tests/bug080_manifest_gate.rs → crates/manifold-node-engine/src/runtime/tests/bug080_manifest_gate.rs
crates/manifold-renderer/src/preset_runtime/tests/chain_error.rs → crates/manifold-node-engine/src/runtime/tests/chain_error.rs
crates/manifold-renderer/src/preset_runtime/tests/chain_fusion.rs → crates/manifold-node-engine/src/runtime/tests/chain_fusion.rs
crates/manifold-renderer/src/preset_runtime/tests/compile_contract_p2.rs → crates/manifold-node-engine/src/runtime/tests/compile_contract_p2.rs
crates/manifold-renderer/src/preset_runtime/tests/generator_input.rs → crates/manifold-node-engine/src/runtime/tests/generator_input.rs
crates/manifold-renderer/src/preset_runtime/tests/generator_runtime.rs → crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs
crates/manifold-renderer/src/preset_runtime/tests/group_mask.rs → crates/manifold-node-engine/src/runtime/tests/group_mask.rs
crates/manifold-renderer/src/preset_runtime/tests/layer_skin.rs → crates/manifold-node-engine/src/runtime/tests/layer_skin.rs
crates/manifold-renderer/src/preset_runtime/tests/math_view.rs → crates/manifold-node-engine/src/runtime/tests/math_view.rs
crates/manifold-renderer/src/preset_runtime/tests/modifier_events.rs → crates/manifold-node-engine/src/runtime/tests/modifier_events.rs
crates/manifold-renderer/src/preset_runtime/tests/mosh.rs → crates/manifold-node-engine/src/runtime/tests/mosh.rs
crates/manifold-renderer/src/preset_runtime/tests/multi_segment.rs → crates/manifold-node-engine/src/runtime/tests/multi_segment.rs
crates/manifold-renderer/src/preset_runtime/tests/persistent_slot.rs → crates/manifold-node-engine/src/runtime/tests/persistent_slot.rs
crates/manifold-renderer/src/preset_runtime/tests/segment_prewarm.rs → crates/manifold-node-engine/src/runtime/tests/segment_prewarm.rs
crates/manifold-renderer/src/preset_runtime/tests/topology_hash.rs → crates/manifold-node-engine/src/runtime/tests/topology_hash.rs
crates/manifold-renderer/src/preset_runtime/tests/transient_slot.rs → crates/manifold-node-engine/src/runtime/tests/transient_slot.rs
crates/manifold-renderer/src/preset_runtime/tests/trigger_initialization.rs → crates/manifold-node-engine/src/runtime/tests/trigger_initialization.rs
crates/manifold-renderer/src/preset_runtime/tests/user_binding.rs → crates/manifold-node-engine/src/runtime/tests/user_binding.rs
crates/manifold-renderer/src/render_target.rs → crates/manifold-node-engine/src/gpu/render_target.rs
crates/manifold-renderer/src/render_target_pool.rs → crates/manifold-node-engine/src/gpu/render_target_pool.rs
crates/manifold-renderer/src/uniform_arena.rs → crates/manifold-node-engine/src/gpu/uniform_arena.rs
```

Crate setup: crates/manifold-renderer/build.rs → crates/manifold-node-engine/build.rs (copy, identity paths repathed, original removed because no remaining renderer file reads MANIFOLD_PHYSICS_INTEGRATION_IDENTITY). The engine feature table equals the entry renderer table verbatim; renderer features forward to the engine. The PresetAssetsRoot inventory registration stays in renderer and now names its engine-owned type directly. No asset was copied. Native FLIP includes are repathed to their original files.

## P1 rewrite map

Canonical prefix rules, OLD::=NEW:: per line. crate, $crate, relative super, grouped imports and the app's former node_graph-as-rg alias resolve through these rules. Existing root exports move from node_graph/mod.rs to lib.rs; nothing re-exports the engine from renderer.

```text
manifold_renderer::background_worker::=manifold_node_engine::runtime::background_worker::
manifold_renderer::chain_dispatch::=manifold_node_engine::runtime::chain_dispatch::
manifold_renderer::effect::=manifold_node_engine::runtime::effect::
manifold_renderer::effects::=manifold_node_engine::runtime::effects::
manifold_renderer::frame_status::=manifold_node_engine::runtime::frame_status::
manifold_renderer::generators::clip_trigger::=manifold_node_engine::clip_trigger::
manifold_renderer::generators::compute_common::=manifold_node_engine::particles::
manifold_renderer::generators::line_pipeline::=manifold_node_engine::line::
manifold_renderer::generators::mesh_common::=manifold_node_engine::mesh::
manifold_renderer::generators::mesh_pipeline::=manifold_node_engine::mesh::pipeline::
manifold_renderer::generators::platonic_geometry::=manifold_node_engine::platonic::
manifold_renderer::generators::stateful_base::=manifold_node_engine::stateful::
manifold_renderer::gpu::=manifold_node_engine::gpu::context::
manifold_renderer::gpu_encoder::=manifold_node_engine::gpu::gpu_encoder::
manifold_renderer::gpu_types::=manifold_node_engine::gpu::gpu_types::
manifold_renderer::layer_skin::=manifold_node_engine::runtime::layer_skin::
manifold_renderer::mesh::=manifold_node_engine::mesh::
manifold_renderer::node_graph::ArrayMatchMode::=manifold_node_engine::ArrayMatchMode::
manifold_renderer::node_graph::ArrayType::=manifold_node_engine::ArrayType::
manifold_renderer::node_graph::Backend::=manifold_node_engine::Backend::
manifold_renderer::node_graph::BindingCacheEntry::=manifold_node_engine::BindingCacheEntry::
manifold_renderer::node_graph::BindingSource::=manifold_node_engine::BindingSource::
manifold_renderer::node_graph::BoundGraph::=manifold_node_engine::BoundGraph::
manifold_renderer::node_graph::BoundaryHandling::=manifold_node_engine::BoundaryHandling::
manifold_renderer::node_graph::BuildWireSide::=manifold_node_engine::BuildWireSide::
manifold_renderer::node_graph::Camera::=manifold_node_engine::Camera::
manifold_renderer::node_graph::CameraMode::=manifold_node_engine::CameraMode::
manifold_renderer::node_graph::Category::=manifold_node_engine::Category::
manifold_renderer::node_graph::ChannelElementType::=manifold_node_engine::ChannelElementType::
manifold_renderer::node_graph::ChannelMismatchInfo::=manifold_node_engine::ChannelMismatchInfo::
manifold_renderer::node_graph::ChannelMismatchReason::=manifold_node_engine::ChannelMismatchReason::
manifold_renderer::node_graph::ChannelName::=manifold_node_engine::ChannelName::
manifold_renderer::node_graph::ChannelSnapshot::=manifold_node_engine::ChannelSnapshot::
manifold_renderer::node_graph::ChannelSpec::=manifold_node_engine::ChannelSpec::
manifold_renderer::node_graph::ContentVersion::=manifold_node_engine::ContentVersion::
manifold_renderer::node_graph::EffectGraphDefExt::=manifold_node_engine::EffectGraphDefExt::
manifold_renderer::node_graph::EffectNode::=manifold_node_engine::EffectNode::
manifold_renderer::node_graph::EffectNodeContext::=manifold_node_engine::EffectNodeContext::
manifold_renderer::node_graph::EffectNodeType::=manifold_node_engine::EffectNodeType::
manifold_renderer::node_graph::ExecutionPlan::=manifold_node_engine::ExecutionPlan::
manifold_renderer::node_graph::ExecutionStep::=manifold_node_engine::ExecutionStep::
manifold_renderer::node_graph::Executor::=manifold_node_engine::Executor::
manifold_renderer::node_graph::FINAL_OUTPUT_TYPE_ID::=manifold_node_engine::FINAL_OUTPUT_TYPE_ID::
manifold_renderer::node_graph::FinalOutput::=manifold_node_engine::FinalOutput::
manifold_renderer::node_graph::FluidRole::=manifold_node_engine::FluidRole::
manifold_renderer::node_graph::FluidRoleKind::=manifold_node_engine::FluidRoleKind::
manifold_renderer::node_graph::FrameTime::=manifold_node_engine::FrameTime::
manifold_renderer::node_graph::FusedRetarget::=manifold_node_engine::FusedRetarget::
manifold_renderer::node_graph::FusionReport::=manifold_node_engine::FusionReport::
manifold_renderer::node_graph::GENERATOR_INPUT_TYPE_ID::=manifold_node_engine::GENERATOR_INPUT_TYPE_ID::
manifold_renderer::node_graph::GRAPH_DOCUMENT_VERSION::=manifold_node_engine::GRAPH_DOCUMENT_VERSION::
manifold_renderer::node_graph::GeneratorInput::=manifold_node_engine::GeneratorInput::
manifold_renderer::node_graph::Graph::=manifold_node_engine::Graph::
manifold_renderer::node_graph::GraphBuildError::=manifold_node_engine::GraphBuildError::
manifold_renderer::node_graph::GraphDocument::=manifold_node_engine::GraphDocument::
manifold_renderer::node_graph::GraphError::=manifold_node_engine::GraphError::
manifold_renderer::node_graph::GraphSnapshot::=manifold_node_engine::GraphSnapshot::
manifold_renderer::node_graph::GroupSnapshot::=manifold_node_engine::GroupSnapshot::
manifold_renderer::node_graph::HandleScope::=manifold_node_engine::HandleScope::
manifold_renderer::node_graph::ImpulseTarget::=manifold_node_engine::ImpulseTarget::
manifold_renderer::node_graph::KnownItem::=manifold_node_engine::KnownItem::
manifold_renderer::node_graph::LastAppliedCache::=manifold_node_engine::LastAppliedCache::
manifold_renderer::node_graph::Light::=manifold_node_engine::Light::
manifold_renderer::node_graph::LightMode::=manifold_node_engine::LightMode::
manifold_renderer::node_graph::LiveNodeParams::=manifold_node_engine::LiveNodeParams::
manifold_renderer::node_graph::LoadError::=manifold_node_engine::LoadError::
manifold_renderer::node_graph::LoadedPresetView::=manifold_node_engine::LoadedPresetView::
manifold_renderer::node_graph::MAX_FLUID_ROLES::=manifold_node_engine::MAX_FLUID_ROLES::
manifold_renderer::node_graph::MatchMode::=manifold_node_engine::MatchMode::
manifold_renderer::node_graph::Material::=manifold_node_engine::Material::
manifold_renderer::node_graph::MaterialKind::=manifold_node_engine::MaterialKind::
manifold_renderer::node_graph::MeshAspect::=manifold_node_engine::MeshAspect::
manifold_renderer::node_graph::MeshDependency::=manifold_node_engine::MeshDependency::
manifold_renderer::node_graph::MeshOutputRule::=manifold_node_engine::MeshOutputRule::
manifold_renderer::node_graph::MeshRevision::=manifold_node_engine::MeshRevision::
manifold_renderer::node_graph::MeshRevisionRule::=manifold_node_engine::MeshRevisionRule::
manifold_renderer::node_graph::MeshSource::=manifold_node_engine::MeshSource::
manifold_renderer::node_graph::MetalBackend::=manifold_node_engine::MetalBackend::
manifold_renderer::node_graph::MockBackend::=manifold_node_engine::MockBackend::
manifold_renderer::node_graph::NodeConstructor::=manifold_node_engine::NodeConstructor::
manifold_renderer::node_graph::NodeDescriptor::=manifold_node_engine::NodeDescriptor::
manifold_renderer::node_graph::NodeDocument::=manifold_node_engine::NodeDocument::
manifold_renderer::node_graph::NodeErrorTap::=manifold_node_engine::NodeErrorTap::
manifold_renderer::node_graph::NodeFusionInfo::=manifold_node_engine::NodeFusionInfo::
manifold_renderer::node_graph::NodeInput::=manifold_node_engine::NodeInput::
manifold_renderer::node_graph::NodeInputs::=manifold_node_engine::NodeInputs::
manifold_renderer::node_graph::NodeInstance::=manifold_node_engine::NodeInstance::
manifold_renderer::node_graph::NodeInstanceId::=manifold_node_engine::NodeInstanceId::
manifold_renderer::node_graph::NodeInstantiation::=manifold_node_engine::NodeInstantiation::
manifold_renderer::node_graph::NodeOutput::=manifold_node_engine::NodeOutput::
manifold_renderer::node_graph::NodeOutputs::=manifold_node_engine::NodeOutputs::
manifold_renderer::node_graph::NodePort::=manifold_node_engine::NodePort::
manifold_renderer::node_graph::NodeRequires::=manifold_node_engine::NodeRequires::
manifold_renderer::node_graph::NodeSnapshot::=manifold_node_engine::NodeSnapshot::
manifold_renderer::node_graph::NodeState::=manifold_node_engine::NodeState::
manifold_renderer::node_graph::NodeWire::=manifold_node_engine::NodeWire::
manifold_renderer::node_graph::OuterParamRouting::=manifold_node_engine::OuterParamRouting::
manifold_renderer::node_graph::OuterParamSource::=manifold_node_engine::OuterParamSource::
manifold_renderer::node_graph::OwnerKey::=manifold_node_engine::OwnerKey::
manifold_renderer::node_graph::PaletteAtom::=manifold_node_engine::PaletteAtom::
manifold_renderer::node_graph::ParamBinding::=manifold_node_engine::ParamBinding::
manifold_renderer::node_graph::ParamConvert::=manifold_node_engine::ParamConvert::
manifold_renderer::node_graph::ParamDef::=manifold_node_engine::ParamDef::
manifold_renderer::node_graph::ParamDoc::=manifold_node_engine::ParamDoc::
manifold_renderer::node_graph::ParamId::=manifold_node_engine::ParamId::
manifold_renderer::node_graph::ParamSnapshot::=manifold_node_engine::ParamSnapshot::
manifold_renderer::node_graph::ParamSnapshotKind::=manifold_node_engine::ParamSnapshotKind::
manifold_renderer::node_graph::ParamTarget::=manifold_node_engine::ParamTarget::
manifold_renderer::node_graph::ParamType::=manifold_node_engine::ParamType::
manifold_renderer::node_graph::ParamValue::=manifold_node_engine::ParamValue::
manifold_renderer::node_graph::ParamValues::=manifold_node_engine::ParamValues::
manifold_renderer::node_graph::PortKind::=manifold_node_engine::PortKind::
manifold_renderer::node_graph::PortKindSnapshot::=manifold_node_engine::PortKindSnapshot::
manifold_renderer::node_graph::PortSnapshot::=manifold_node_engine::PortSnapshot::
manifold_renderer::node_graph::PortType::=manifold_node_engine::PortType::
manifold_renderer::node_graph::PreAllocationError::=manifold_node_engine::PreAllocationError::
manifold_renderer::node_graph::PreparedFluidGeometry::=manifold_node_engine::PreparedFluidGeometry::
manifold_renderer::node_graph::PreparedMeshOutputRule::=manifold_node_engine::PreparedMeshOutputRule::
manifold_renderer::node_graph::PreparedMeshRevisionRule::=manifold_node_engine::PreparedMeshRevisionRule::
manifold_renderer::node_graph::PreparedMeshRules::=manifold_node_engine::PreparedMeshRules::
manifold_renderer::node_graph::PreparedMetalBackendResize::=manifold_node_engine::PreparedMetalBackendResize::
manifold_renderer::node_graph::PreviewEncoding::=manifold_node_engine::PreviewEncoding::
manifold_renderer::node_graph::PreviewScalarIo::=manifold_node_engine::PreviewScalarIo::
manifold_renderer::node_graph::Primitive::=manifold_node_engine::Primitive::
manifold_renderer::node_graph::PrimitiveDescription::=manifold_node_engine::PrimitiveDescription::
manifold_renderer::node_graph::PrimitiveRegistry::=manifold_node_engine::PrimitiveRegistry::
manifold_renderer::node_graph::PrimitiveSpec::=manifold_node_engine::PrimitiveSpec::
manifold_renderer::node_graph::RegionSummary::=manifold_node_engine::RegionSummary::
manifold_renderer::node_graph::Reshape::=manifold_node_engine::Reshape::
manifold_renderer::node_graph::ResolvedBinding::=manifold_node_engine::ResolvedBinding::
manifold_renderer::node_graph::ResolvedNodeImpulse::=manifold_node_engine::ResolvedNodeImpulse::
manifold_renderer::node_graph::ResolvedTarget::=manifold_node_engine::ResolvedTarget::
manifold_renderer::node_graph::ResourceId::=manifold_node_engine::ResourceId::
manifold_renderer::node_graph::Role::=manifold_node_engine::Role::
manifold_renderer::node_graph::RtQuality::=manifold_node_engine::RtQuality::
manifold_renderer::node_graph::SOURCE_TYPE_ID::=manifold_node_engine::SOURCE_TYPE_ID::
manifold_renderer::node_graph::ScalarType::=manifold_node_engine::ScalarType::
manifold_renderer::node_graph::SceneObject::=manifold_node_engine::SceneObject::
manifold_renderer::node_graph::SerializedParamValue::=manifold_node_engine::SerializedParamValue::
manifold_renderer::node_graph::ShadowSoftness::=manifold_node_engine::ShadowSoftness::
manifold_renderer::node_graph::ShadowedDefParam::=manifold_node_engine::ShadowedDefParam::
manifold_renderer::node_graph::Slot::=manifold_node_engine::Slot::
manifold_renderer::node_graph::Source::=manifold_node_engine::Source::
manifold_renderer::node_graph::SpliceResult::=manifold_node_engine::SpliceResult::
manifold_renderer::node_graph::StateStore::=manifold_node_engine::StateStore::
manifold_renderer::node_graph::StepProfile::=manifold_node_engine::StepProfile::
manifold_renderer::node_graph::StorageRevision::=manifold_node_engine::StorageRevision::
manifold_renderer::node_graph::TextureChannelMismatchInfo::=manifold_node_engine::TextureChannelMismatchInfo::
manifold_renderer::node_graph::TextureChannelMismatchReason::=manifold_node_engine::TextureChannelMismatchReason::
manifold_renderer::node_graph::TextureChannels::=manifold_node_engine::TextureChannels::
manifold_renderer::node_graph::Transform::=manifold_node_engine::Transform::
manifold_renderer::node_graph::ValidateKind::=manifold_node_engine::ValidateKind::
manifold_renderer::node_graph::ValidationIssue::=manifold_node_engine::ValidationIssue::
manifold_renderer::node_graph::ValidationReport::=manifold_node_engine::ValidationReport::
manifold_renderer::node_graph::ViewportCamera::=manifold_node_engine::ViewportCamera::
manifold_renderer::node_graph::WireDocument::=manifold_node_engine::WireDocument::
manifold_renderer::node_graph::WireSide::=manifold_node_engine::WireSide::
manifold_renderer::node_graph::WireSnapshot::=manifold_node_engine::WireSnapshot::
manifold_renderer::node_graph::WireWalkMode::=manifold_node_engine::WireWalkMode::
manifold_renderer::node_graph::allocate_resources::=manifold_node_engine::allocate_resources::
manifold_renderer::node_graph::apply_binding_defaults::=manifold_node_engine::apply_binding_defaults::
manifold_renderer::node_graph::apply_bindings::=manifold_node_engine::apply_bindings::
manifold_renderer::node_graph::apply_inner_param_overrides::=manifold_node_engine::apply_inner_param_overrides::
manifold_renderer::node_graph::atmosphere::=manifold_node_engine::scene::atmosphere::
manifold_renderer::node_graph::atomic::=manifold_node_engine::atomic::
manifold_renderer::node_graph::audible_shadow_findings::=manifold_node_engine::audible_shadow_findings::
manifold_renderer::node_graph::backend::=manifold_node_engine::exec::backend::
manifold_renderer::node_graph::binding_migration::=manifold_node_engine::load::binding_migration::
manifold_renderer::node_graph::binding_value::=manifold_node_engine::binding_value::
manifold_renderer::node_graph::bindings::=manifold_node_engine::bindings::
manifold_renderer::node_graph::bound_graph::=manifold_node_engine::exec::bound_graph::
manifold_renderer::node_graph::boundary_nodes::=manifold_node_engine::scene::boundary_nodes::
manifold_renderer::node_graph::builtins::=manifold_node_engine::builtins::
manifold_renderer::node_graph::camera::=manifold_node_engine::scene::camera::
manifold_renderer::node_graph::catalog_graph_def_for::=manifold_node_engine::catalog_graph_def_for::
manifold_renderer::node_graph::chain_spec::=manifold_node_engine::load::chain_spec::
manifold_renderer::node_graph::channel_names::=manifold_node_engine::channel_names::
manifold_renderer::node_graph::channels_compatible::=manifold_node_engine::channels_compatible::
manifold_renderer::node_graph::collect_node_handles::=manifold_node_engine::collect_node_handles::
manifold_renderer::node_graph::compile::=manifold_node_engine::compile::
manifold_renderer::node_graph::content_revision::=manifold_node_engine::content_revision::
manifold_renderer::node_graph::convert_param_value::=manifold_node_engine::convert_param_value::
manifold_renderer::node_graph::depth_rule::=manifold_node_engine::scene::depth_rule::
manifold_renderer::node_graph::descriptor::=manifold_node_engine::descriptor::
manifold_renderer::node_graph::descriptor_for::=manifold_node_engine::descriptor_for::
manifold_renderer::node_graph::effect_node::=manifold_node_engine::exec::effect_node::
manifold_renderer::node_graph::execution::=manifold_node_engine::exec::execution::
manifold_renderer::node_graph::execution_plan::=manifold_node_engine::exec::execution_plan::
manifold_renderer::node_graph::find_shadowed_def_params::=manifold_node_engine::find_shadowed_def_params::
manifold_renderer::node_graph::fluid::=manifold_node_engine::water::fluid::
manifold_renderer::node_graph::fluid_cache::=manifold_node_engine::water::fluid_cache::
manifold_renderer::node_graph::fluid_mesh_upload::=manifold_node_engine::water::fluid_mesh_upload::
manifold_renderer::node_graph::fluid_particles::=manifold_node_engine::water::fluid_particles::
manifold_renderer::node_graph::fluid_role::=manifold_node_engine::water::fluid_role::
manifold_renderer::node_graph::fragment_mask_continuity_tests::=manifold_node_engine::fragment_mask_continuity_tests::
manifold_renderer::node_graph::freeze::=manifold_node_engine::freeze::
manifold_renderer::node_graph::fusion_report::=manifold_node_engine::fusion_report::
manifold_renderer::node_graph::gltf_anim_identity::=manifold_node_engine::scene::gltf_anim_identity::
manifold_renderer::node_graph::graph::=manifold_node_engine::graph::
manifold_renderer::node_graph::graph_loader::=manifold_node_engine::load::graph_loader::
manifold_renderer::node_graph::has_retired_params::=manifold_node_engine::has_retired_params::
manifold_renderer::node_graph::instance_upload::=manifold_node_engine::exec::instance_upload::
manifold_renderer::node_graph::instantiate_def::=manifold_node_engine::instantiate_def::
manifold_renderer::node_graph::intern_name::=manifold_node_engine::intern_name::
manifold_renderer::node_graph::is_baseline_shadow::=manifold_node_engine::is_baseline_shadow::
manifold_renderer::node_graph::light::=manifold_node_engine::scene::light::
manifold_renderer::node_graph::liquid::=manifold_node_engine::water::liquid::
manifold_renderer::node_graph::live_extent::=manifold_node_engine::scene::live_extent::
manifold_renderer::node_graph::loaded_preset_view::=manifold_node_engine::load::loaded_preset_view::
manifold_renderer::node_graph::loaded_preset_view_by_id::=manifold_node_engine::loaded_preset_view_by_id::
manifold_renderer::node_graph::log_build_error::=manifold_node_engine::log_build_error::
manifold_renderer::node_graph::material::=manifold_node_engine::scene::material::
manifold_renderer::node_graph::matter::=manifold_node_engine::water::matter::
manifold_renderer::node_graph::mesh_boundary::=manifold_node_engine::scene::mesh_boundary::
manifold_renderer::node_graph::mesh_change::=manifold_node_engine::scene::mesh_change::
manifold_renderer::node_graph::mesh_cut::=manifold_node_engine::scene::mesh_cut::
manifold_renderer::node_graph::mesh_partition::=manifold_node_engine::scene::mesh_partition::
manifold_renderer::node_graph::mesh_source::=manifold_node_engine::scene::mesh_source::
manifold_renderer::node_graph::metal_backend::=manifold_node_engine::exec::metal_backend::
manifold_renderer::node_graph::migrate_user_param_bindings_to_node_id::=manifold_node_engine::migrate_user_param_bindings_to_node_id::
manifold_renderer::node_graph::migration::=manifold_node_engine::load::migration::
manifold_renderer::node_graph::outer_routings_from_bindings::=manifold_node_engine::outer_routings_from_bindings::
manifold_renderer::node_graph::outer_routings_from_view::=manifold_node_engine::outer_routings_from_view::
manifold_renderer::node_graph::palette::=manifold_node_engine::palette::
manifold_renderer::node_graph::palette_atoms::=manifold_node_engine::palette_atoms::
manifold_renderer::node_graph::param_binding::=manifold_node_engine::param_binding::
manifold_renderer::node_graph::param_default_to_f32::=manifold_node_engine::param_default_to_f32::
manifold_renderer::node_graph::param_doc::=manifold_node_engine::param_doc::
manifold_renderer::node_graph::param_tooltips_bulk::=manifold_node_engine::param_tooltips_bulk::
manifold_renderer::node_graph::param_tooltips_table::=manifold_node_engine::param_tooltips_table::
manifold_renderer::node_graph::parameters::=manifold_node_engine::parameters::
manifold_renderer::node_graph::persistence::=manifold_node_engine::persistence::
manifold_renderer::node_graph::physics::=manifold_node_engine::water::physics::
manifold_renderer::node_graph::physics_events::=manifold_node_engine::water::physics_events::
manifold_renderer::node_graph::physics_mesh::=manifold_node_engine::scene::physics_mesh::
manifold_renderer::node_graph::physics_metrics::=manifold_node_engine::water::physics_metrics::
manifold_renderer::node_graph::physics_scene::=manifold_node_engine::water::physics_scene::
manifold_renderer::node_graph::ports::=manifold_node_engine::ports::
manifold_renderer::node_graph::pre_allocate_resources::=manifold_node_engine::pre_allocate_resources::
manifold_renderer::node_graph::preview_encoding::=manifold_node_engine::preview_encoding::
manifold_renderer::node_graph::primitive::=manifold_node_engine::primitive::
manifold_renderer::node_graph::primitives::DEFAULT_WGSL_COMPUTE::=manifold_node_engine::primitives::DEFAULT_WGSL_COMPUTE::
manifold_renderer::node_graph::primitives::Gain::=manifold_node_engine::primitives::Gain::
manifold_renderer::node_graph::primitives::LiquidSolidDistance::=manifold_node_engine::water::primitives::LiquidSolidDistance::
manifold_renderer::node_graph::primitives::MATTER_STATE_PORTS::=manifold_node_engine::water::primitives::MATTER_STATE_PORTS::
manifold_renderer::node_graph::primitives::MIX_MODES::=manifold_node_engine::primitives::MIX_MODES::
manifold_renderer::node_graph::primitives::MIX_TYPE_ID::=manifold_node_engine::primitives::MIX_TYPE_ID::
manifold_renderer::node_graph::primitives::MaskedMix::=manifold_node_engine::primitives::MaskedMix::
manifold_renderer::node_graph::primitives::MatterBodyReaction::=manifold_node_engine::water::primitives::MatterBodyReaction::
manifold_renderer::node_graph::primitives::MatterDomain::=manifold_node_engine::water::primitives::MatterDomain::
manifold_renderer::node_graph::primitives::MatterFill::=manifold_node_engine::water::primitives::MatterFill::
manifold_renderer::node_graph::primitives::MatterFrame::=manifold_node_engine::water::primitives::MatterFrame::
manifold_renderer::node_graph::primitives::MatterGridUpdate::=manifold_node_engine::water::primitives::MatterGridUpdate::
manifold_renderer::node_graph::primitives::MatterMoveBodies::=manifold_node_engine::water::primitives::MatterMoveBodies::
manifold_renderer::node_graph::primitives::MatterState::=manifold_node_engine::water::primitives::MatterState::
manifold_renderer::node_graph::primitives::MatterStats::=manifold_node_engine::water::primitives::MatterStats::
manifold_renderer::node_graph::primitives::MatterToGrid::=manifold_node_engine::water::primitives::MatterToGrid::
manifold_renderer::node_graph::primitives::Mix::=manifold_node_engine::primitives::Mix::
manifold_renderer::node_graph::primitives::MuxTexture::=manifold_node_engine::primitives::MuxTexture::
manifold_renderer::node_graph::primitives::PushOutOfSolid::=manifold_node_engine::water::primitives::PushOutOfSolid::
manifold_renderer::node_graph::primitives::Value::=manifold_node_engine::primitives::Value::
manifold_renderer::node_graph::primitives::WgslCompute::=manifold_node_engine::primitives::WgslCompute::
manifold_renderer::node_graph::primitives::compose::=manifold_node_engine::primitives::mix::
manifold_renderer::node_graph::primitives::count_surface_triangles::=manifold_node_engine::water::primitives::count_surface_triangles::
manifold_renderer::node_graph::primitives::crossing_distance::=manifold_node_engine::water::primitives::crossing_distance::
manifold_renderer::node_graph::primitives::dot_products::=manifold_node_engine::water::primitives::dot_products::
manifold_renderer::node_graph::primitives::emission_count::=manifold_node_engine::water::primitives::emission_count::
manifold_renderer::node_graph::primitives::energy_potential::=manifold_node_engine::water::primitives::energy_potential::
manifold_renderer::node_graph::primitives::extend_lattice::=manifold_node_engine::water::primitives::extend_lattice::
manifold_renderer::node_graph::primitives::face_sample_component::=manifold_node_engine::water::primitives::face_sample_component::
manifold_renderer::node_graph::primitives::fluid_surface::=manifold_node_engine::water::primitives::fluid_surface::
manifold_renderer::node_graph::primitives::gain::=manifold_node_engine::primitives::gain::
manifold_renderer::node_graph::primitives::gpu_flip_atom_tests::=manifold_node_engine::water::primitives::gpu_flip_atom_tests::
manifold_renderer::node_graph::primitives::gpu_flip_bodies::=manifold_node_engine::water::primitives::gpu_flip_bodies::
manifold_renderer::node_graph::primitives::gpu_flip_body_tests::=manifold_node_engine::water::primitives::gpu_flip_body_tests::
manifold_renderer::node_graph::primitives::gpu_flip_clock::=manifold_node_engine::water::primitives::gpu_flip_clock::
manifold_renderer::node_graph::primitives::gpu_flip_domain::=manifold_node_engine::water::primitives::gpu_flip_domain::
manifold_renderer::node_graph::primitives::gpu_flip_extension_tests::=manifold_node_engine::water::primitives::gpu_flip_extension_tests::
manifold_renderer::node_graph::primitives::gpu_flip_lentine::=manifold_node_engine::water::primitives::gpu_flip_lentine::
manifold_renderer::node_graph::primitives::gpu_flip_narrow_band::=manifold_node_engine::water::primitives::gpu_flip_narrow_band::
manifold_renderer::node_graph::primitives::gpu_flip_narrow_band_tests::=manifold_node_engine::water::primitives::gpu_flip_narrow_band_tests::
manifold_renderer::node_graph::primitives::gpu_flip_preset::=manifold_node_engine::water::primitives::gpu_flip_preset::
manifold_renderer::node_graph::primitives::gpu_flip_pressure::=manifold_node_engine::water::primitives::gpu_flip_pressure::
manifold_renderer::node_graph::primitives::gpu_flip_pressure_tests::=manifold_node_engine::water::primitives::gpu_flip_pressure_tests::
manifold_renderer::node_graph::primitives::gpu_flip_race_tests::=manifold_node_engine::water::primitives::gpu_flip_race_tests::
manifold_renderer::node_graph::primitives::gpu_flip_render_smoke_tests::=manifold_node_engine::water::primitives::gpu_flip_render_smoke_tests::
manifold_renderer::node_graph::primitives::gpu_flip_scene_tests::=manifold_node_engine::water::primitives::gpu_flip_scene_tests::
manifold_renderer::node_graph::primitives::gpu_flip_sheeting::=manifold_node_engine::water::primitives::gpu_flip_sheeting::
manifold_renderer::node_graph::primitives::gpu_flip_sheeting_cpu_tests::=manifold_node_engine::water::primitives::gpu_flip_sheeting_cpu_tests::
manifold_renderer::node_graph::primitives::gpu_flip_sheeting_step_tests::=manifold_node_engine::water::primitives::gpu_flip_sheeting_step_tests::
manifold_renderer::node_graph::primitives::gpu_flip_sheeting_tests::=manifold_node_engine::water::primitives::gpu_flip_sheeting_tests::
manifold_renderer::node_graph::primitives::gpu_flip_step::=manifold_node_engine::water::primitives::gpu_flip_step::
manifold_renderer::node_graph::primitives::gpu_flip_step_tests::=manifold_node_engine::water::primitives::gpu_flip_step_tests::
manifold_renderer::node_graph::primitives::gpu_flip_still::=manifold_node_engine::water::primitives::gpu_flip_still::
manifold_renderer::node_graph::primitives::gpu_flip_tile_tests::=manifold_node_engine::water::primitives::gpu_flip_tile_tests::
manifold_renderer::node_graph::primitives::gpu_flip_volume::=manifold_node_engine::water::primitives::gpu_flip_volume::
manifold_renderer::node_graph::primitives::keep_whitewater::=manifold_node_engine::water::primitives::keep_whitewater::
manifold_renderer::node_graph::primitives::lattice_bricks::=manifold_node_engine::water::primitives::lattice_bricks::
manifold_renderer::node_graph::primitives::lattice_closing_gpu_tests::=manifold_node_engine::water::primitives::lattice_closing_gpu_tests::
manifold_renderer::node_graph::primitives::lattice_closing_tests::=manifold_node_engine::water::primitives::lattice_closing_tests::
manifold_renderer::node_graph::primitives::lattice_curvature::=manifold_node_engine::water::primitives::lattice_curvature::
manifold_renderer::node_graph::primitives::liquid_bricks::=manifold_node_engine::water::primitives::liquid_bricks::
manifold_renderer::node_graph::primitives::liquid_bricks_gpu_tests::=manifold_node_engine::water::primitives::liquid_bricks_gpu_tests::
manifold_renderer::node_graph::primitives::liquid_bricks_tests::=manifold_node_engine::water::primitives::liquid_bricks_tests::
manifold_renderer::node_graph::primitives::liquid_cells::=manifold_node_engine::water::primitives::liquid_cells::
manifold_renderer::node_graph::primitives::liquid_fill::=manifold_node_engine::water::primitives::liquid_fill::
manifold_renderer::node_graph::primitives::liquid_frame::=manifold_node_engine::water::primitives::liquid_frame::
manifold_renderer::node_graph::primitives::liquid_prepare_tests::=manifold_node_engine::water::primitives::liquid_prepare_tests::
manifold_renderer::node_graph::primitives::liquid_solid_distance::=manifold_node_engine::water::primitives::liquid_solid_distance::
manifold_renderer::node_graph::primitives::liquid_state::=manifold_node_engine::water::primitives::liquid_state::
manifold_renderer::node_graph::primitives::liquid_stats::=manifold_node_engine::water::primitives::liquid_stats::
manifold_renderer::node_graph::primitives::liquid_surface_tests::=manifold_node_engine::water::primitives::liquid_surface_tests::
manifold_renderer::node_graph::primitives::masked_mix::=manifold_node_engine::primitives::masked_mix::
manifold_renderer::node_graph::primitives::matter_body_reaction::=manifold_node_engine::water::primitives::matter_body_reaction::
manifold_renderer::node_graph::primitives::matter_common::=manifold_node_engine::water::primitives::matter_common::
manifold_renderer::node_graph::primitives::matter_domain::=manifold_node_engine::water::primitives::matter_domain::
manifold_renderer::node_graph::primitives::matter_face_component::=manifold_node_engine::water::primitives::matter_face_component::
manifold_renderer::node_graph::primitives::matter_fill::=manifold_node_engine::water::primitives::matter_fill::
manifold_renderer::node_graph::primitives::matter_frame::=manifold_node_engine::water::primitives::matter_frame::
manifold_renderer::node_graph::primitives::matter_grid_update::=manifold_node_engine::water::primitives::matter_grid_update::
manifold_renderer::node_graph::primitives::matter_move_bodies::=manifold_node_engine::water::primitives::matter_move_bodies::
manifold_renderer::node_graph::primitives::matter_state::=manifold_node_engine::water::primitives::matter_state::
manifold_renderer::node_graph::primitives::matter_stats::=manifold_node_engine::water::primitives::matter_stats::
manifold_renderer::node_graph::primitives::matter_to_grid::=manifold_node_engine::water::primitives::matter_to_grid::
manifold_renderer::node_graph::primitives::mesh_cut_map::oracle::=manifold_node_engine::scene::mesh_cut::
manifold_renderer::node_graph::primitives::mux_texture::=manifold_node_engine::primitives::mux_texture::
manifold_renderer::node_graph::primitives::nearest_crossing::=manifold_node_engine::water::primitives::nearest_crossing::
manifold_renderer::node_graph::primitives::pad_distance_lattice::=manifold_node_engine::water::primitives::pad_distance_lattice::
manifold_renderer::node_graph::primitives::particle_identity::=manifold_node_engine::water::primitives::particle_identity::
manifold_renderer::node_graph::primitives::particle_publication::=manifold_node_engine::water::primitives::particle_publication::
manifold_renderer::node_graph::primitives::particle_volume::=manifold_node_engine::water::primitives::particle_volume::
manifold_renderer::node_graph::primitives::physics_world::=manifold_node_engine::water::primitives::physics_world::
manifold_renderer::node_graph::primitives::prefix_scan::=manifold_node_engine::water::primitives::prefix_scan::
manifold_renderer::node_graph::primitives::preserve_foam::=manifold_node_engine::water::primitives::preserve_foam::
manifold_renderer::node_graph::primitives::push_out_of_solid::=manifold_node_engine::water::primitives::push_out_of_solid::
manifold_renderer::node_graph::primitives::running_total::=manifold_node_engine::water::primitives::running_total::
manifold_renderer::node_graph::primitives::sort_particles_into_cells::=manifold_node_engine::water::primitives::sort_particles_into_cells::
manifold_renderer::node_graph::primitives::standalone_pipeline::=manifold_node_engine::primitives::standalone_pipeline::
manifold_renderer::node_graph::primitives::surface_crossings::=manifold_node_engine::water::primitives::surface_crossings::
manifold_renderer::node_graph::primitives::upwind_distance::=manifold_node_engine::water::primitives::upwind_distance::
manifold_renderer::node_graph::primitives::value::=manifold_node_engine::primitives::value::
manifold_renderer::node_graph::primitives::volume_surface_mesh::=manifold_node_engine::water::primitives::volume_surface_mesh::
manifold_renderer::node_graph::primitives::wgsl_compute::=manifold_node_engine::primitives::wgsl_compute::
manifold_renderer::node_graph::primitives::whitewater_copy_tests::=manifold_node_engine::water::primitives::whitewater_copy_tests::
manifold_renderer::node_graph::primitives::whitewater_cpu::=manifold_node_engine::water::primitives::whitewater_cpu::
manifold_renderer::node_graph::primitives::whitewater_distance::=manifold_node_engine::water::primitives::whitewater_distance::
manifold_renderer::node_graph::primitives::whitewater_emitter_cpu::=manifold_node_engine::water::primitives::whitewater_emitter_cpu::
manifold_renderer::node_graph::primitives::whitewater_emitter_dispatch::=manifold_node_engine::water::primitives::whitewater_emitter_dispatch::
manifold_renderer::node_graph::primitives::whitewater_emitter_gpu_tests::=manifold_node_engine::water::primitives::whitewater_emitter_gpu_tests::
manifold_renderer::node_graph::primitives::whitewater_emitter_velocity::=manifold_node_engine::water::primitives::whitewater_emitter_velocity::
manifold_renderer::node_graph::primitives::whitewater_engine_cpu::=manifold_node_engine::water::primitives::whitewater_engine_cpu::
manifold_renderer::node_graph::primitives::whitewater_engine_gpu_tests::=manifold_node_engine::water::primitives::whitewater_engine_gpu_tests::
manifold_renderer::node_graph::primitives::whitewater_extent_tests::=manifold_node_engine::water::primitives::whitewater_extent_tests::
manifold_renderer::node_graph::primitives::whitewater_field_tests::=manifold_node_engine::water::primitives::whitewater_field_tests::
manifold_renderer::node_graph::primitives::whitewater_fused_tests::=manifold_node_engine::water::primitives::whitewater_fused_tests::
manifold_renderer::node_graph::primitives::whitewater_golden_tests::=manifold_node_engine::water::primitives::whitewater_golden_tests::
manifold_renderer::node_graph::primitives::whitewater_grid_tests::=manifold_node_engine::water::primitives::whitewater_grid_tests::
manifold_renderer::node_graph::primitives::whitewater_handoff_tests::=manifold_node_engine::water::primitives::whitewater_handoff_tests::
manifold_renderer::node_graph::primitives::whitewater_influence::=manifold_node_engine::water::primitives::whitewater_influence::
manifold_renderer::node_graph::primitives::whitewater_lifecycle::=manifold_node_engine::water::primitives::whitewater_lifecycle::
manifold_renderer::node_graph::primitives::whitewater_obstacle_source::=manifold_node_engine::water::primitives::whitewater_obstacle_source::
manifold_renderer::node_graph::primitives::whitewater_particle_cpu::=manifold_node_engine::water::primitives::whitewater_particle_cpu::
manifold_renderer::node_graph::primitives::whitewater_particle_tests::=manifold_node_engine::water::primitives::whitewater_particle_tests::
manifold_renderer::node_graph::primitives::whitewater_pool_cpu::=manifold_node_engine::water::primitives::whitewater_pool_cpu::
manifold_renderer::node_graph::primitives::whitewater_pool_tests::=manifold_node_engine::water::primitives::whitewater_pool_tests::
manifold_renderer::node_graph::primitives::whitewater_reference::=manifold_node_engine::water::primitives::whitewater_reference::
manifold_renderer::node_graph::primitives::whitewater_scene_tests::=manifold_node_engine::water::primitives::whitewater_scene_tests::
manifold_renderer::node_graph::primitives::whitewater_step::=manifold_node_engine::water::primitives::whitewater_step::
manifold_renderer::node_graph::primitives::whitewater_step_tests::=manifold_node_engine::water::primitives::whitewater_step_tests::
manifold_renderer::node_graph::primitives::whitewater_type::=manifold_node_engine::water::primitives::whitewater_type::
manifold_renderer::node_graph::render_mode::=manifold_node_engine::scene::render_mode::
manifold_renderer::node_graph::resource_allocation::=manifold_node_engine::exec::resource_allocation::
manifold_renderer::node_graph::retire_params::=manifold_node_engine::retire_params::
manifold_renderer::node_graph::scene_modifier_expand::=manifold_node_engine::load::expand::
manifold_renderer::node_graph::scene_object::=manifold_node_engine::scene::scene_object::
manifold_renderer::node_graph::scene_viewport::=manifold_node_engine::scene::scene_viewport::
manifold_renderer::node_graph::shadow_baseline_entries::=manifold_node_engine::shadow_baseline_entries::
manifold_renderer::node_graph::snapshot::=manifold_node_engine::snapshot::
manifold_renderer::node_graph::snapshot_for_view::=manifold_node_engine::snapshot_for_view::
manifold_renderer::node_graph::source_asset::=manifold_node_engine::scene::source_asset::
manifold_renderer::node_graph::splice_def_into_chain::=manifold_node_engine::splice_def_into_chain::
manifold_renderer::node_graph::state_store::=manifold_node_engine::state_store::
manifold_renderer::node_graph::std430_layout::=manifold_node_engine::std430_layout::
manifold_renderer::node_graph::std430_stride::=manifold_node_engine::std430_stride::
manifold_renderer::node_graph::std430_stride_and_align::=manifold_node_engine::std430_stride_and_align::
manifold_renderer::node_graph::substeps::=manifold_node_engine::exec::substeps::
manifold_renderer::node_graph::temporal_reset::=manifold_node_engine::exec::temporal_reset::
manifold_renderer::node_graph::texture_channels_compatible::=manifold_node_engine::texture_channels_compatible::
manifold_renderer::node_graph::tooltip_for::=manifold_node_engine::tooltip_for::
manifold_renderer::node_graph::topological_sort::=manifold_node_engine::topological_sort::
manifold_renderer::node_graph::transform::=manifold_node_engine::scene::transform::
manifold_renderer::node_graph::trigger_shadow_lint::=manifold_node_engine::trigger_shadow_lint::
manifold_renderer::node_graph::unretarget_shadow::=manifold_node_engine::unretarget_shadow::
manifold_renderer::node_graph::validate::=manifold_node_engine::validate::
manifold_renderer::node_graph::validate_def::=manifold_node_engine::validate_def::
manifold_renderer::node_graph::validation::=manifold_node_engine::validation::
manifold_renderer::node_graph::vector_field::=manifold_node_engine::scene::vector_field::
manifold_renderer::node_graph::viewport_camera::=manifold_node_engine::scene::viewport_camera::
manifold_renderer::node_graph::whitewater::=manifold_node_engine::water::whitewater::
manifold_renderer::node_graph::whitewater_handoff::=manifold_node_engine::water::whitewater_handoff::
manifold_renderer::param_tooltips!::=manifold_node_engine::param_tooltips!::
manifold_renderer::particles::=manifold_node_engine::particles::
manifold_renderer::plugin_prewarm::=manifold_node_engine::runtime::plugin_prewarm::
manifold_renderer::preset_context::=manifold_node_engine::runtime::preset_context::
manifold_renderer::preset_loader::=manifold_node_engine::load::preset_loader::
manifold_renderer::preset_runtime::=manifold_node_engine::runtime::
manifold_renderer::preset_runtime::BindingSource::=manifold_node_engine::BindingSource::
manifold_renderer::preset_runtime::BoundGraph::=manifold_node_engine::BoundGraph::
manifold_renderer::preset_runtime::CapturedSceneImpulse::=manifold_node_engine::water::runtime::CapturedSceneImpulse::
manifold_renderer::preset_runtime::EffectGraphDefExt::=manifold_node_engine::EffectGraphDefExt::
manifold_renderer::preset_runtime::EffectSlot::=manifold_node_engine::runtime::core::EffectSlot::
manifold_renderer::preset_runtime::ExecutionPlan::=manifold_node_engine::ExecutionPlan::
manifold_renderer::preset_runtime::Executor::=manifold_node_engine::Executor::
manifold_renderer::preset_runtime::FINAL_OUTPUT_TYPE_ID::=manifold_node_engine::FINAL_OUTPUT_TYPE_ID::
manifold_renderer::preset_runtime::FinalOutput::=manifold_node_engine::FinalOutput::
manifold_renderer::preset_runtime::FrameTime::=manifold_node_engine::FrameTime::
manifold_renderer::preset_runtime::GENERATOR_INPUT_TYPE_ID::=manifold_node_engine::GENERATOR_INPUT_TYPE_ID::
manifold_renderer::preset_runtime::GRAPH_FORMAT::=manifold_node_engine::runtime::core::GRAPH_FORMAT::
manifold_renderer::preset_runtime::GpuEncoder::=manifold_node_engine::gpu::gpu_encoder::GpuEncoder::
manifold_renderer::preset_runtime::Graph::=manifold_node_engine::Graph::
manifold_renderer::preset_runtime::GraphError::=manifold_node_engine::GraphError::
manifold_renderer::preset_runtime::LoadError::=manifold_node_engine::LoadError::
manifold_renderer::preset_runtime::LoadedPresetView::=manifold_node_engine::LoadedPresetView::
manifold_renderer::preset_runtime::MetalBackend::=manifold_node_engine::MetalBackend::
manifold_renderer::preset_runtime::Mix::=manifold_node_engine::primitives::Mix::
manifold_renderer::preset_runtime::NodeInstanceId::=manifold_node_engine::NodeInstanceId::
manifold_renderer::preset_runtime::OpenGroup::=manifold_node_engine::runtime::groups::OpenGroup::
manifold_renderer::preset_runtime::ParamBinding::=manifold_node_engine::ParamBinding::
manifold_renderer::preset_runtime::ParamValue::=manifold_node_engine::ParamValue::
manifold_renderer::preset_runtime::PreparedSceneImpulse::=manifold_node_engine::water::runtime::PreparedSceneImpulse::
manifold_renderer::preset_runtime::PresetContext::=manifold_node_engine::runtime::preset_context::PresetContext::
manifold_renderer::preset_runtime::PresetIo::=manifold_node_engine::runtime::core::PresetIo::
manifold_renderer::preset_runtime::PrimitiveRegistry::=manifold_node_engine::PrimitiveRegistry::
manifold_renderer::preset_runtime::RelightParamWrite::=manifold_node_engine::runtime::bindings::RelightParamWrite::
manifold_renderer::preset_runtime::RenderTarget::=manifold_node_engine::gpu::render_target::RenderTarget::
manifold_renderer::preset_runtime::ResolvedBinding::=manifold_node_engine::ResolvedBinding::
manifold_renderer::preset_runtime::ResolvedTarget::=manifold_node_engine::ResolvedTarget::
manifold_renderer::preset_runtime::ResourceId::=manifold_node_engine::ResourceId::
manifold_renderer::preset_runtime::SceneImpulseDiagnostics::=manifold_node_engine::water::runtime::SceneImpulseDiagnostics::
manifold_renderer::preset_runtime::SegmentMember::=manifold_node_engine::runtime::segments::SegmentMember::
manifold_renderer::preset_runtime::Slot::=manifold_node_engine::Slot::
manifold_renderer::preset_runtime::Source::=manifold_node_engine::Source::
manifold_renderer::preset_runtime::SpliceResult::=manifold_node_engine::SpliceResult::
manifold_renderer::preset_runtime::StateStore::=manifold_node_engine::StateStore::
manifold_renderer::preset_runtime::StringBindingResolution::=manifold_node_engine::runtime::bindings::StringBindingResolution::
manifold_renderer::preset_runtime::apply_binding_defaults::=manifold_node_engine::apply_binding_defaults::
manifold_renderer::preset_runtime::assert_manifest_gate::=manifold_node_engine::runtime::core::assert_manifest_gate::
manifold_renderer::preset_runtime::assign_texture2d_slots::=manifold_node_engine::runtime::build::assign_texture2d_slots::
manifold_renderer::preset_runtime::build_relight_writes::=manifold_node_engine::runtime::bindings::build_relight_writes::
manifold_renderer::preset_runtime::build_segment_cards::=manifold_node_engine::runtime::segments::build_segment_cards::
manifold_renderer::preset_runtime::chain_active_effects::=manifold_node_engine::runtime::groups::chain_active_effects::
manifold_renderer::preset_runtime::classify_segment_member::=manifold_node_engine::runtime::segments::classify_segment_member::
manifold_renderer::preset_runtime::close_mix_group::=manifold_node_engine::runtime::groups::close_mix_group::
manifold_renderer::preset_runtime::compile::=manifold_node_engine::compile::
manifold_renderer::preset_runtime::compute_topology_hash::=manifold_node_engine::runtime::build::compute_topology_hash::
manifold_renderer::preset_runtime::def_string_param_value::=manifold_node_engine::runtime::bindings::def_string_param_value::
manifold_renderer::preset_runtime::gpu_flip_surface::=manifold_node_engine::water::runtime::gpu_flip_surface::
manifold_renderer::preset_runtime::loaded_preset_view_by_id::=manifold_node_engine::loaded_preset_view_by_id::
manifold_renderer::preset_runtime::physics_asset_take_tests::=manifold_node_engine::water::runtime::physics_asset_take_tests::
manifold_renderer::preset_runtime::physics_carry::=manifold_node_engine::water::runtime::physics_carry::
manifold_renderer::preset_runtime::physics_carry_tests::=manifold_node_engine::water::runtime::physics_carry_tests::
manifold_renderer::preset_runtime::physics_collection_tests::=manifold_node_engine::water::runtime::physics_collection_tests::
manifold_renderer::preset_runtime::physics_history_drain_tests::=manifold_node_engine::water::runtime::physics_history_drain_tests::
manifold_renderer::preset_runtime::physics_host_modulation_tests::=manifold_node_engine::water::runtime::physics_host_modulation_tests::
manifold_renderer::preset_runtime::physics_impulses::=manifold_node_engine::water::runtime::physics_impulses::
manifold_renderer::preset_runtime::physics_sampling::=manifold_node_engine::water::runtime::physics_sampling::
manifold_renderer::preset_runtime::physics_sampling_inputs_tests::=manifold_node_engine::water::runtime::physics_sampling_inputs_tests::
manifold_renderer::preset_runtime::physics_source_asset_tests::=manifold_node_engine::water::runtime::physics_source_asset_tests::
manifold_renderer::preset_runtime::physics_source_chain::=manifold_node_engine::water::runtime::physics_source_chain::
manifold_renderer::preset_runtime::physics_source_controls::=manifold_node_engine::water::runtime::physics_source_controls::
manifold_renderer::preset_runtime::physics_source_controls_tests::=manifold_node_engine::water::runtime::physics_source_controls_tests::
manifold_renderer::preset_runtime::physics_source_path_tests::=manifold_node_engine::water::runtime::physics_source_path_tests::
manifold_renderer::preset_runtime::physics_source_runtime::=manifold_node_engine::water::runtime::physics_source_runtime::
manifold_renderer::preset_runtime::physics_source_state::=manifold_node_engine::water::runtime::physics_source_state::
manifold_renderer::preset_runtime::physics_source_state_tests::=manifold_node_engine::water::runtime::physics_source_state_tests::
manifold_renderer::preset_runtime::physics_sources::=manifold_node_engine::water::runtime::physics_sources::
manifold_renderer::preset_runtime::physics_sources_tests::=manifold_node_engine::water::runtime::physics_sources_tests::
manifold_renderer::preset_runtime::physics_string_binding_tests::=manifold_node_engine::water::runtime::physics_string_binding_tests::
manifold_renderer::preset_runtime::record_chain_error::=manifold_node_engine::runtime::errors::record_chain_error::
manifold_renderer::preset_runtime::scene_impulses::=manifold_node_engine::water::runtime::scene_impulses::
manifold_renderer::preset_runtime::segment_run::=manifold_node_engine::runtime::segments::segment_run::
manifold_renderer::preset_runtime::splice_def_into_chain::=manifold_node_engine::splice_def_into_chain::
manifold_renderer::preset_runtime::validate_mask_groups::=manifold_node_engine::runtime::groups::validate_mask_groups::
manifold_renderer::primitive!::=manifold_node_engine::primitive!::
manifold_renderer::render_target::=manifold_node_engine::gpu::render_target::
manifold_renderer::render_target_pool::=manifold_node_engine::gpu::render_target_pool::
manifold_renderer::uniform_arena::=manifold_node_engine::gpu::uniform_arena::
```

## P1 family reaches — record only

Compiler diagnostics plus the source census; current file:line locations. No entry below was repaired by promoting a family file.

- crates/manifold-node-engine/src/atomic/mod.rs:92 — `crate::node_graph::primitives::Threshold` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/exec/bound_graph.rs:601 — `crate::node_graph::primitives::AffineTransform` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/exec/execution.rs:5518 — `crate::node_graph::primitives::NormalWaveMesh` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/exec/execution.rs:5842 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/exec/execution.rs:5858 — `crate::node_graph::primitives::NormalWaveMesh::new` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/exec/execution.rs:5868 — `crate::node_graph::primitives::MorphMesh::new` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/exec/execution_plan.rs:1377 — `crate::node_graph::primitives::Feedback` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/exec/execution_plan.rs:1377 — `crate::node_graph::primitives::GltfTextureSource` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/exec/metal_backend.rs:1173 — `crate::node_graph::primitives::GltfTextureSource` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:669 — `crate::node_graph::primitives::ArrayFeedback` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:670 — `crate::node_graph::primitives::ContainerBounds3D` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:671 — `crate::node_graph::primitives::GenerateCubeMesh` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:672 — `crate::node_graph::primitives::ResolveAccumulator` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:673 — `crate::node_graph::primitives::ScatterParticles` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:674 — `crate::node_graph::primitives::SceneObjectNode` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:675 — `crate::node_graph::primitives::SeedParticles` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:677 — `crate::node_graph::primitives::WaveShearMesh` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:1210 — `crate::node_graph::primitives::MeshSpatialMask::new` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:1472 — `crate::node_graph::bundled_presets::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:1472 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:13 — `crate::node_graph::primitives::MeshSpatialMask` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:13 — `crate::node_graph::primitives::MeshStaggerEnvelope` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:13 — `crate::node_graph::primitives::MorphMesh` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:13 — `crate::node_graph::primitives::RemapCutWeights` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:13 — `crate::node_graph::primitives::RemapMeshCut` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/freeze/codegen/fused.rs:994 — `crate::node_graph::primitives::LerpInstanceFields` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/fused.rs:994 — `crate::node_graph::primitives::NeighborSmooth` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:305 — `crate::node_graph::primitives::CocFromDepth` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:388 — `crate::node_graph::primitives::InstanceRotationJitter` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:453 — `crate::node_graph::primitives::LerpInstanceFields` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:511 — `crate::node_graph::primitives::GaussianBlur` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:588 — `crate::node_graph::primitives::Invert` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:588 — `crate::node_graph::primitives::Sharpen` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:653 — `crate::node_graph::primitives::Contrast` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:655 — `crate::node_graph::primitives::Invert` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:3163 — `crate::node_graph::primitives::test_multi_output_atomic_fixture::MOMENTUM_WORDS` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:3163 — `crate::node_graph::primitives::test_multi_output_atomic_fixture::TestMultiOutputAtomic` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:3163 — `crate::node_graph::primitives::test_multi_output_atomic_fixture::Uniforms` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:3163 — `crate::node_graph::primitives::test_multi_output_atomic_fixture::cpu_reference` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/diff.rs:201 — `crate::clear_texture_committed` — manifold-node-engine (D7 testkit candidate) — GPU test texture clear helper remains in renderer lib.rs; omitted from move list.
- crates/manifold-node-engine/src/freeze/install.rs:497 — `crate::node_graph::relight::relight_augment` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/freeze/install.rs:1207 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/install.rs:2620 — `crate::node_graph::primitives::MorphMesh` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/freeze/install.rs:2620 — `crate::node_graph::primitives::NormalWaveMesh` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/freeze/markers.rs:474 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/markers.rs:497 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/markers.rs:501 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:842 — `crate::node_graph::primitives::test_camera_pointwise_fixture::TestCameraPointwise` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/freeze/proof.rs:1254 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:1384 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:1385 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:1480 — `crate::node_graph::bundled_presets::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:1532 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:2283 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:2400 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:2653 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:2763 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:2862 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:2982 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:3053 — `crate::node_graph::primitives::FlowFieldNoise` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/proof.rs:3208 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:3311 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:3479 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:3574 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:3700 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:3775 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:3829 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:3907 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:3987 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:4190 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof.rs:4562 — `crate::node_graph::primitives::test_multi_output_atomic_fixture::MOMENTUM_WORDS` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/proof.rs:4562 — `crate::node_graph::primitives::test_multi_output_atomic_fixture::TYPE_ID` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/proof.rs:4562 — `crate::node_graph::primitives::test_multi_output_atomic_fixture::TestMultiOutputAtomic` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/freeze/proof/audio_visual.rs:70 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/freeze/proof/audio_visual.rs:196 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/freeze/proof/audio_visual.rs:228 — `crate::node_graph::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof/audio_visual.rs:352 — `crate::node_graph::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/proof/audio_visual.rs:397 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/freeze/region.rs:3854 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/region.rs:4325 — `crate::node_graph::primitives::test_face_lattice_fixture::TYPE_ID` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/freeze/region.rs:4325 — `crate::node_graph::primitives::test_face_lattice_fixture::TestFaceLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/freeze/region/census.rs:455 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/region/census.rs:464 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/region/census.rs:467 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/region/census.rs:613 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/region/census.rs:620 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/freeze/region/census.rs:623 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/line.rs:1 — `crate::generators::generator_math::DEFAULT_DOT_RADIUS` — manifold-nodes-image — shared line-generator projection constant; D1 ownership inferred from image generator role, lead ruling needed.
- crates/manifold-node-engine/src/load/binding_migration.rs:35 — `crate::node_graph::bundled_presets::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/load/chain_spec.rs:94 — `crate::node_graph::relight::relight_augment` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/load/expand/buffer_budget.rs:285 — `crate::node_graph::primitives::cut_map_scratch_bytes` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/load/expand/compiler/acceleration_tests.rs:4 — `crate::node_graph::scene_modifier_authoring::prepare_new_scene_modifier` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/load/expand/compiler/acceleration_tests.rs:33 — `crate::node_graph::scene_modifier_authoring::scene_modifier_objects` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/load/expand/compiler/acceleration_tests.rs:38 — `crate::node_graph::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/load/expand/impulses.rs:132 — `crate::node_graph::scene_modifier_authoring::prepare_new_scene_modifier` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/load/graph_loader.rs:2208 — `crate::node_graph::primitives::EulerStepParticles` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/load/graph_loader.rs:2208 — `crate::node_graph::primitives::GridUvField` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/load/graph_loader.rs:2208 — `crate::node_graph::primitives::SeedParticles` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/load/graph_loader.rs:2250 — `crate::node_graph::primitives::SeedParticles` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/load/loaded_preset_view.rs:43 — `crate::node_graph::bundled_presets::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/load/loaded_preset_view.rs:138 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/load/loaded_preset_view.rs:352 — `crate::node_graph::gltf_import::assemble_import_graph` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/load/preset_loader.rs:804 — `crate::node_graph::loaded_presets_from_bundled` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/load/preset_loader.rs:806 — `crate::generators::bundled_generator_presets::loaded_generator_presets_from_bundled` — manifold-nodes — legacy bundled generator preset catalog.
- crates/manifold-node-engine/src/load/preset_loader.rs:807 — `crate::node_graph::loaded_scene_modifier_presets_from_bundled` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/palette.rs:22 — `crate::node_graph::bundled_presets::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/palette.rs:321 — `crate::node_graph::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/param_binding.rs:61 — `crate::node_graph::composites::CompositeHandle` — manifold-nodes-image — composite graph handle or composite builder.
- crates/manifold-node-engine/src/param_binding.rs:838 — `crate::node_graph::composites::CompositeHandle` — manifold-nodes-image — composite graph handle or composite builder.
- crates/manifold-node-engine/src/param_binding.rs:1013 — `crate::node_graph::primitives::AffineTransform` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/persistence.rs:866 — `crate::node_graph::primitives::Blur` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/persistence.rs:866 — `crate::node_graph::primitives::Threshold` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/persistence.rs:866 — `crate::node_graph::primitives::self` — manifold-nodes (cross-family tests) — family primitive namespace used by a cross-family test.
- crates/manifold-node-engine/src/ports.rs:780 — `crate::node_graph::primitives::SceneObjectNode::new` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/primitives/gain.rs:188 — `crate::clear_texture_committed` — manifold-node-engine (D7 testkit candidate) — GPU test texture clear helper remains in renderer lib.rs; omitted from move list.
- crates/manifold-node-engine/src/primitives/gain.rs:287 — `crate::clear_texture_committed` — manifold-node-engine (D7 testkit candidate) — GPU test texture clear helper remains in renderer lib.rs; omitted from move list.
- crates/manifold-node-engine/src/runtime/bindings.rs:88 — `crate::node_graph::relight::relight_field_targets` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/runtime/instrumentation.rs:240 — `crate::compositor::ArrayDump` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/instrumentation.rs:285 — `crate::compositor::ArrayDump` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/instrumentation.rs:409 — `crate::compositor::ArrayDump` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/instrumentation.rs:453 — `crate::compositor::ArrayDump` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/layer_skin.rs:307 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/math_view.rs:266 — `crate::node_graph::primitives::RenderMeshDiagram::prewarm_pipelines` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/runtime/math_view_events.rs:3 — `crate::node_graph::primitives::BeatEnvelopeDurations` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/runtime/math_view_events.rs:3 — `crate::node_graph::primitives::BeatEnvelopeState` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/runtime/segments.rs:20 — `crate::node_graph::relight::relight_augment` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/runtime/tests/amount_zero_passthrough.rs:19 — `crate::headless_readback` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/amount_zero_passthrough.rs:30 — `crate::preset_thumbnail::build_test_card_input` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/amount_zero_passthrough.rs:30 — `crate::preset_thumbnail::output_resource` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/amount_zero_passthrough.rs:30 — `crate::preset_thumbnail::test_card_pixel` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/blob_grain_probe.rs:109 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/bound_param_survives_rebuild.rs:19 — `crate::node_graph::scene_exposure::metadata_for_node_type` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/runtime/tests/chain_fusion.rs:566 — `crate::node_graph::relight::is_relight_node_id` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/runtime/tests/group_mask.rs:46 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/group_mask.rs:125 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/group_mask.rs:174 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/group_mask.rs:175 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/layer_skin.rs:21 — `crate::compositor::CompositeLayerDescriptor` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/layer_skin.rs:21 — `crate::compositor::Compositor` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/layer_skin.rs:21 — `crate::compositor::CompositorFrame` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/layer_skin.rs:25 — `crate::layer_compositor::CompositeClipDescriptor` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/layer_skin.rs:25 — `crate::layer_compositor::LayerCompositor` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/layer_skin.rs:32 — `crate::tonemap::TonemapSettings` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/layer_skin.rs:640 — `crate::headless_readback::linear_to_srgb8` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/layer_skin.rs:646 — `crate::headless_readback::encode_rgba8_png` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:205 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:213 — `crate::headless_readback::readback_to_srgb_png_linear` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:227 — `crate::headless_readback::readback_to_srgb_png_linear` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:279 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:308 — `crate::headless_readback::mean_abs_half_diff` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:314 — `crate::headless_readback::readback_to_srgb_png_linear` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:397 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:608 — `crate::headless_readback::readback_srgb_rgba8` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:618 — `crate::headless_readback::encode_rgba8_png` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:634 — `crate::headless_readback::mean_abs_half_diff` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:643 — `crate::headless_readback::mean_abs_half_diff` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:788 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:848 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:853 — `crate::headless_readback::readback_to_srgb_png_linear` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:931 — `crate::node_graph::scene_modifier_authoring::prepare_new_scene_modifier` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/runtime/tests/mosh.rs:4 — `crate::headless_readback::readback_raw_halves` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/runtime/tests/persistent_slot.rs:25 — `crate::node_graph::primitives::AffineTransform` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/runtime/tests/persistent_slot.rs:26 — `crate::node_graph::primitives::Feedback` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/runtime/tests/persistent_slot.rs:29 — `crate::node_graph::primitives::Vignette` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/runtime/tests/transient_slot.rs:3 — `crate::node_graph::primitives::AudioSpectrum` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/runtime/tests/transient_slot.rs:4 — `crate::node_graph::primitives::Checkerboard` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/scene/gltf_anim_identity.rs:3 — `crate::node_graph::gltf_anim_cache::ChannelKind` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/scene/gltf_anim_identity.rs:4 — `crate::node_graph::gltf_anim_cache::GltfAnimSet` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/scene/gltf_anim_identity.rs:5 — `crate::node_graph::gltf_load::GltfInterp` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/scene/material.rs:417 — `crate::node_graph::gltf_load::VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/scene/physics_mesh.rs:5 — `crate::node_graph::decode_cache::cached_load_gltf_mesh` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/scene/physics_mesh.rs:7 — `crate::node_graph::gltf_load::DEFAULT_MATERIAL_MESH_PARAM` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/scene/physics_mesh.rs:8 — `crate::node_graph::gltf_load::GltfMeshSelector` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/scene/physics_mesh.rs:12 — `crate::node_graph::primitives::gltf_mesh_source::apply_mesh_fit` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/scene/physics_mesh.rs:13 — `crate::node_graph::primitives::gltf_mesh_source::apply_translate` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/snapshot.rs:1282 — `crate::node_graph::composites::build_soft_focus` — manifold-nodes-image — composite graph handle or composite builder.
- crates/manifold-node-engine/src/snapshot.rs:1333 — `crate::node_graph::composites::build_soft_focus` — manifold-nodes-image — composite graph handle or composite builder.
- crates/manifold-node-engine/src/validation.rs:2042 — `crate::node_graph::primitives::CelMaterial` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/validation.rs:2066 — `crate::node_graph::primitives::CelMaterial` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/validation.rs:2066 — `crate::node_graph::primitives::LightNode` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/validation.rs:2084 — `crate::node_graph::primitives::UnlitMaterial` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/water/fluid/coupled/native.rs:13 — `crate::node_graph::primitives::quat_to_render_scene_euler` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/water/liquid/conformance.rs:18 — `crate::node_graph::bundled_presets::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/water/liquid/conformance.rs:29 — `crate::node_graph::primitives::face_grid_scenes::matter_dam_break_faces` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/liquid/extent.rs:1932 — `crate::node_graph::bundled_presets::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/water/liquid/extent.rs:1932 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/water/liquid/lattice.rs:407 — `crate::node_graph::bundled_presets::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/water/liquid/lattice.rs:407 — `crate::node_graph::bundled_presets::bundled_preset_type_ids` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/water/physics.rs:1488 — `crate::node_graph::primitives::quat_to_render_scene_euler` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/water/physics.rs:1530 — `crate::node_graph::primitives::quat_to_render_scene_euler` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/water/primitives/gpu_flip_atom_tests.rs:12 — `crate::node_graph::primitives::divide_by_value::DivideByValue` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs:25 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs:747 — `crate::node_graph::scene_exposure::look_metadata` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs:748 — `crate::node_graph::scene_exposure::metadata_for_node_type` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs:1747 — `crate::node_graph::scene_vm::SceneObjectVm` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs:1747 — `crate::node_graph::scene_vm::SceneVm` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs:1748 — `crate::node_graph::bundled_preset_def` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/water/primitives/gpu_flip_render_smoke_tests.rs:30 — `crate::headless_readback::encode_rgba8_png` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/water/primitives/gpu_flip_render_smoke_tests.rs:30 — `crate::headless_readback::readback_srgb_rgba8` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/water/primitives/lattice_closing_gpu_tests.rs:2 — `crate::node_graph::primitives::offset_lattice::OffsetLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/lattice_closing_gpu_tests.rs:3 — `crate::node_graph::primitives::redistance_lattice::RedistanceLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/lattice_closing_tests.rs:276 — `crate::node_graph::primitives::offset_lattice::OffsetLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/lattice_closing_tests.rs:277 — `crate::node_graph::primitives::redistance_lattice::RedistanceLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/lattice_closing_tests.rs:321 — `crate::node_graph::primitives::redistance_lattice::RedistanceLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/lattice_closing_tests.rs:322 — `crate::node_graph::primitives::offset_lattice::OffsetLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_bricks_gpu_tests.rs:11 — `crate::node_graph::primitives::clamp_liquid_to_solids::ClampLiquidToSolids` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_bricks_gpu_tests.rs:21 — `crate::node_graph::primitives::smooth_lattice::SmoothLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_bricks_gpu_tests.rs:22 — `crate::node_graph::primitives::relax_surface_mesh::RelaxSurfaceMesh` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/water/primitives/liquid_bricks_gpu_tests.rs:154 — `crate::node_graph::primitives::blob_bounds::BlobBounds::new` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/water/primitives/liquid_bricks_tests.rs:167 — `crate::node_graph::primitives::clamp_liquid_to_solids::ClampLiquidToSolids` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_bricks_tests.rs:170 — `crate::node_graph::primitives::relax_surface_mesh::RelaxSurfaceMesh` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/water/primitives/liquid_bricks_tests.rs:171 — `crate::node_graph::primitives::smooth_lattice::SmoothLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_bricks_tests.rs:230 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:14 — `crate::node_graph::primitives::shape_particle_blobs::ShapeParticleBlobs` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:288 — `crate::node_graph::primitives::blob_bounds::BlobBounds::new` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:1249 — `crate::node_graph::primitives::count_surface_edges::CountSurfaceEdges` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:1733 — `crate::node_graph::primitives::offset_lattice::OffsetLattice::new` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:1742 — `crate::node_graph::primitives::redistance_lattice::RedistanceLattice::new` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:2151 — `crate::node_graph::primitives::smooth_lattice::SmoothLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:2230 — `crate::node_graph::primitives::clamp_liquid_to_solids::ClampLiquidToSolids` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:2351 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:2364 — `crate::preset_thumbnail::render_preset_thumbnail` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:2499 — `crate::node_graph::primitives::relax_surface_mesh::RelaxSurfaceMesh` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/water/primitives/liquid_surface_tests.rs:2799 — `crate::node_graph::bundled_presets::bundled_preset_json` — manifold-nodes — bundled preset catalog lookup or preset enumeration.
- crates/manifold-node-engine/src/water/primitives/volume_surface_mesh.rs:756 — `crate::node_graph::primitives::surface_mesh_parity::CORNERS` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/volume_surface_mesh.rs:757 — `crate::node_graph::primitives::surface_mesh_parity::EDGES` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/volume_surface_mesh.rs:758 — `crate::node_graph::primitives::surface_mesh_parity::triangle_table` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_cpu.rs:69 — `crate::node_graph::primitives::turbulence_field::TurbulenceField` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_cpu.rs:70 — `crate::node_graph::primitives::inside_turbulence_potential::InsideTurbulencePotential` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_cpu.rs:71 — `crate::node_graph::primitives::turbulence_emission_count::TurbulenceEmissionCount` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_cpu.rs:75 — `crate::node_graph::primitives::dust_potential::DustPotential` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_gpu_tests.rs:8 — `crate::node_graph::primitives::divide_by_value::DivideByValue` — manifold-nodes-image — image, math, particle, trigger or generic test primitive.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_gpu_tests.rs:9 — `crate::node_graph::primitives::dust_potential::DustPotential` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_gpu_tests.rs:11 — `crate::node_graph::primitives::inside_turbulence_potential::InsideTurbulencePotential` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_gpu_tests.rs:12 — `crate::node_graph::primitives::turbulence_emission_count::TurbulenceEmissionCount` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_gpu_tests.rs:13 — `crate::node_graph::primitives::turbulence_field::TurbulenceField` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_gpu_tests.rs:729 — `crate::node_graph::primitives::advect_whitewater::AdvectWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_emitter_gpu_tests.rs:730 — `crate::node_graph::primitives::age_whitewater::AgeWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_engine_cpu.rs:161 — `crate::node_graph::primitives::advect_whitewater::AdvectWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_engine_gpu_tests.rs:5 — `crate::node_graph::primitives::advect_whitewater::AdvectWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_engine_gpu_tests.rs:6 — `crate::node_graph::primitives::offset_lattice::OffsetLattice` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_extent_tests.rs:201 — `crate::node_graph::primitives::jitter_particles::JitterParticles` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_extent_tests.rs:202 — `crate::node_graph::primitives::sample_faces_at_particles::SampleFacesAtParticles` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_extent_tests.rs:203 — `crate::node_graph::primitives::wavecrest_potential::WavecrestPotential` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_extent_tests.rs:228 — `crate::node_graph::primitives::spawn_whitewater::SpawnWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_golden_tests.rs:201 — `manifold_io::saver::save_project` — external: manifold-io — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/primitives/whitewater_golden_tests.rs:202 — `manifold_io::loader::load_project` — external: manifold-io — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/primitives/whitewater_particle_tests.rs:8 — `crate::node_graph::primitives::jitter_particles::JitterParticles` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_particle_tests.rs:11 — `crate::node_graph::primitives::sample_faces_at_particles::SampleFacesAtParticles` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_particle_tests.rs:12 — `crate::node_graph::primitives::spawn_whitewater::SpawnWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_particle_tests.rs:13 — `crate::node_graph::primitives::wavecrest_potential::WavecrestPotential` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_pool_tests.rs:6 — `crate::node_graph::primitives::advect_whitewater::AdvectWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_pool_tests.rs:7 — `crate::node_graph::primitives::age_whitewater::AgeWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_pool_tests.rs:10 — `crate::node_graph::primitives::retype_whitewater::RetypeWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:3 — `crate::node_graph::primitives::turbulence_field::TurbulenceField` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:4 — `crate::node_graph::primitives::age_whitewater::AgeWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:5 — `crate::node_graph::primitives::retype_whitewater::RetypeWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:6 — `crate::node_graph::primitives::advect_whitewater::AdvectWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:8 — `crate::node_graph::primitives::spawn_whitewater::SpawnWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:10 — `crate::node_graph::primitives::turbulence_emission_count::TurbulenceEmissionCount` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:11 — `crate::node_graph::primitives::dust_potential::DustPotential` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:13 — `crate::node_graph::primitives::inside_turbulence_potential::InsideTurbulencePotential` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:14 — `crate::node_graph::primitives::jitter_particles::JitterParticles` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:15 — `crate::node_graph::primitives::sample_faces_at_particles::SampleFacesAtParticles` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_reference.rs:16 — `crate::node_graph::primitives::wavecrest_potential::WavecrestPotential` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_scene_tests.rs:25 — `crate::headless_readback::encode_rgba8_png` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/water/primitives/whitewater_scene_tests.rs:25 — `crate::headless_readback::readback_srgb_rgba8` — manifold-compositor — compositor diagnostics, composition or GPU readback/thumbnail test support.
- crates/manifold-node-engine/src/water/primitives/whitewater_scene_tests.rs:1469 — `crate::node_graph::primitives::jitter_particles::JitterParticles` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_scene_tests.rs:1471 — `crate::node_graph::primitives::sample_faces_at_particles::SampleFacesAtParticles` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_scene_tests.rs:1472 — `crate::node_graph::primitives::spawn_whitewater::SpawnWhitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_scene_tests.rs:1474 — `crate::node_graph::primitives::wavecrest_potential::WavecrestPotential` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step.rs:207 — `crate::node_graph::primitives::wavecrest_potential::MIN_CURVATURE` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step.rs:208 — `crate::node_graph::primitives::wavecrest_potential::MAX_CURVATURE` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step.rs:209 — `crate::node_graph::primitives::wavecrest_potential::SHARPNESS` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step.rs:308 — `crate::node_graph::primitives::spawn_whitewater::MIN_LIFETIME` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step.rs:309 — `crate::node_graph::primitives::spawn_whitewater::MAX_LIFETIME` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step.rs:310 — `crate::node_graph::primitives::spawn_whitewater::LIFETIME_VARIANCE` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step.rs:320 — `crate::node_graph::primitives::advect_whitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step.rs:321 — `crate::node_graph::primitives::age_whitewater` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step_tests.rs:19 — `crate::node_graph::primitives::spawn_whitewater::LIFETIME_VARIANCE` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step_tests.rs:20 — `crate::node_graph::primitives::spawn_whitewater::MAX_LIFETIME` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step_tests.rs:21 — `crate::node_graph::primitives::spawn_whitewater::MIN_LIFETIME` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step_tests.rs:22 — `crate::node_graph::primitives::wavecrest_potential::MAX_CURVATURE` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step_tests.rs:23 — `crate::node_graph::primitives::wavecrest_potential::MIN_CURVATURE` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/primitives/whitewater_step_tests.rs:24 — `crate::node_graph::primitives::wavecrest_potential::SHARPNESS` — manifold-nodes-water — water atom or water test reference outside the authorized candidate set.
- crates/manifold-node-engine/src/water/runtime/physics_collection_tests.rs:296 — `manifold_io::collect::collect_all_and_save` — external: manifold-io — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/runtime/physics_collection_tests.rs:299 — `manifold_io::loader::load_project` — external: manifold-io — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/runtime/physics_collection_tests.rs:320 — `manifold_io::loader::load_project` — external: manifold-io — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/runtime/physics_collection_tests.rs:335 — `manifold_io::loader::load_project` — external: manifold-io — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/runtime/physics_collection_tests.rs:358 — `manifold_io::loader::load_project` — external: manifold-io — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/runtime/physics_collection_tests.rs:368 — `manifold_io::loader::load_project` — external: manifold-io — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/runtime/physics_host_modulation_tests.rs:178 — `crate::node_graph::scene_modifier_authoring::prepare_new_scene_modifier` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/coupled_playback_tests.rs:12 — `manifold_editing::command::Command` — external: manifold-editing — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/coupled_playback_tests.rs:13 — `manifold_editing::commands::graph::AddSceneFluidCommand` — external: manifold-editing — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/coupled_playback_tests.rs:35 — `crate::node_graph::scene_exposure::metadata_for_node_type` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/coupled_playback_tests.rs:36 — `crate::node_graph::scene_exposure::metadata_for_node_type` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/coupled_playback_tests.rs:37 — `crate::node_graph::scene_exposure::metadata_for_node_type` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/coupled_playback_tests.rs:38 — `crate::node_graph::scene_exposure::metadata_for_node_type` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/coupled_playback_tests.rs:39 — `manifold_editing::commands::graph::flip_scene_fluid_template` — external: manifold-editing — cross-crate project round-trip or editing fixture.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/coupled_playback_tests.rs:42 — `crate::node_graph::scene_exposure::metadata_for_node_type` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/coupled_playback_tests.rs:45 — `crate::node_graph::scene_exposure::metadata_for_node_type` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/tests.rs:26 — `crate::node_graph::primitives::render_scene::RenderScene` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/water/runtime/physics_impulses/tests.rs:130 — `crate::node_graph::primitives::render_scene::RenderScene::new` — manifold-nodes-scene — scene primitive, mesh operation or scene test fixture.
- crates/manifold-node-engine/src/water/runtime/physics_sources_tests.rs:46 — `crate::node_graph::scene_modifier_authoring::prepare_new_scene_modifier` — manifold-nodes-scene — scene import/cache vocabulary, authoring, metadata or relight graph expansion.

## P1 asset reaches — record only

Shipped presets, golden files and fixtures referenced by engine code/tests. Workspace fixture paths are included even when their existing relative depth remains valid; these are references, not 96 proven missing files. Shader source walks are in the next section.

- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:549 — `include_str!("../../assets/scene-modifier-presets/OrderedRecon.json")`
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:553 — `include_str!("../../assets/scene-modifier-presets/MaskedPeel.json")`
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:557 — `include_str!("../../assets/scene-modifier-presets/SurfacePeel.json")`
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:561 — `include_str!("../../assets/scene-modifier-presets/OrderedReconHit.json")`
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:565 — `include_str!("../../assets/scene-modifier-presets/VortexFragments.json")`
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:572 — `include_str!("../../assets/scene-modifier-presets/OrderedRecon.json")`
- crates/manifold-node-engine/src/fragment_mask_continuity_tests.rs:576 — `include_str!("../../assets/scene-modifier-presets/OrderedReconHit.json")`
- crates/manifold-node-engine/src/freeze/fusion_report.rs:246 — `walks shipped assets/effect-presets or assets/generator-presets from manifest directory`
- crates/manifold-node-engine/src/freeze/fusion_report.rs:293 — `walks shipped assets/effect-presets or assets/generator-presets from manifest directory`
- crates/manifold-node-engine/src/freeze/install.rs:2962 — `env!("CARGO_MANIFEST_DIR"), "/assets/effect-presets/ColorGrade.json" )) .expect("read ColorGrade.json");`
- crates/manifold-node-engine/src/freeze/markers.rs:526 — `std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")) .join("tests/fixtures/fused_wgsl_snapshot.txt") } /// P1 hard gate (FUSION_SOTA_DESIGN D1): the marker refactor must emit /// byte-identical WGSL for every bundled preset. The golden fixture was /// captured from origin/main HEAD (6888ea28, pre-refactor codegen) by /// temporarily stashing only `freeze/codegen.rs` + `freeze/install.rs` (the /// emit sites), running this test with `UPDATE_FUSION_GOLDEN=1`, then /// restoring the refactor and re-running normally. Regenerate the fixture /// (`UPDATE_FUSION_GOLDEN=1 cargo test …`) only for an INTENTIONAL codegen /// change — never to make this phase's refactor pass. #[test] fn fused_wgsl_snapshot_unchanged() { let actual = capture_all_fused_wgsl();`
- crates/manifold-node-engine/src/freeze/proof.rs:397 — `env!("CARGO_MANIFEST_DIR"), "/assets/effect-presets/ColorGrade.json" )) .expect("read ColorGrade.json");`
- crates/manifold-node-engine/src/freeze/proof.rs:617 — `env!("CARGO_MANIFEST_DIR"), "/assets/effect-presets/ColorGrade.json" )) .expect("read ColorGrade.json");`
- crates/manifold-node-engine/src/freeze/proof.rs:746 — `env!("CARGO_MANIFEST_DIR"), "/assets/effect-presets/ColorGrade.json" )) .expect("read ColorGrade.json");`
- crates/manifold-node-engine/src/freeze/region.rs:3017 — `env!("CARGO_MANIFEST_DIR"), "/assets/effect-presets/ColorGrade.json" )) .expect("read ColorGrade.json");`
- crates/manifold-node-engine/src/freeze/region.rs:3026 — `env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/StrangeAttractor.json" )) .expect("read StrangeAttractor.json");`
- crates/manifold-node-engine/src/load/expand/acceleration.rs:334 — `include_str!( "../../../assets/generator-presets/WaterDamBreakGpu.json" )`
- crates/manifold-node-engine/src/load/expand/acceleration.rs:363 — `include_str!( "../../../assets/generator-presets/WaterDamBreakMatter.json" )`
- crates/manifold-node-engine/src/load/expand/compiler/acceleration_tests.rs:13 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/PhysicsSolids.json" ))`
- crates/manifold-node-engine/src/load/expand/compiler/acceleration_tests.rs:17 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/UniformForce.json" ))`
- crates/manifold-node-engine/src/load/expand/compiler/shatter.rs:456 — `include_str!( "../../../../assets/generator-presets/PhysicsSolids.json" )`
- crates/manifold-node-engine/src/load/expand/compiler/shatter.rs:487 — `include_str!( "../../../../assets/scene-modifier-presets/Shatter.json" )`
- crates/manifold-node-engine/src/load/expand/compiler/tests.rs:9 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json" ))`
- crates/manifold-node-engine/src/load/expand/compiler/tests.rs:17 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/ElasticSculpture.json" ))`
- crates/manifold-node-engine/src/load/expand/compiler/tests.rs:1468 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/SceneLoop.json" ))`
- crates/manifold-node-engine/src/load/expand/frames.rs:447 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json" ))`
- crates/manifold-node-engine/src/load/expand/impulses.rs:141 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/PhysicsSolids.json" ))`
- crates/manifold-node-engine/src/load/expand/impulses.rs:145 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/UniformForce.json" ))`
- crates/manifold-node-engine/src/load/expand/math_view.rs:25 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json" ))`
- crates/manifold-node-engine/src/load/expand/math_view.rs:48 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/VortexFragments.json" ))`
- crates/manifold-node-engine/src/load/expand/math_view.rs:53 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/MathView.json" ))`
- crates/manifold-node-engine/src/load/expand/math_view.rs:145 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/SpatialEchoes.json" ))`
- crates/manifold-node-engine/src/load/expand/parameter_guards.rs:190 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scene-modifiers/surface_peel_applied_v2.json" ))`
- crates/manifold-node-engine/src/load/loaded_preset_view.rs:349 — `let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")) .join("../../tests/fixtures/gltf/hostile/two_material_pbr.glb");`
- crates/manifold-node-engine/src/runtime/tests/array_buffers.rs:11 — `include_str!("../../../assets/generator-presets/Lissajous.json")`
- crates/manifold-node-engine/src/runtime/tests/array_buffers.rs:90 — `include_str!("../../../assets/generator-presets/Cymatics.json")`
- crates/manifold-node-engine/src/runtime/tests/array_buffers.rs:164 — `include_str!("../../../assets/generator-presets/StrangeAttractor.json")`
- crates/manifold-node-engine/src/runtime/tests/array_buffers.rs:236 — `include_str!("../../../assets/generator-presets/StrangeAttractor.json")`
- crates/manifold-node-engine/src/runtime/tests/bool_convert_heal.rs:20 — `include_str!("../../../assets/generator-presets/Lissajous.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:102 — `include_str!("../../../assets/generator-presets/Lissajous.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:165 — `include_str!("../../../assets/generator-presets/Lissajous.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:212 — `include_str!("../../../assets/generator-presets/Lissajous.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:290 — `include_str!("../../../assets/generator-presets/Plasma.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:336 — `include_str!("../../../assets/generator-presets/Text.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:386 — `include_str!("../../../assets/generator-presets/Text.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:422 — `include_str!("../../../assets/generator-presets/Text.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:466 — `include_str!("../../../assets/generator-presets/Plasma.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:514 — `include_str!("../../../assets/generator-presets/Plasma.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:1072 — `include_str!("../../../assets/generator-presets/StrangeAttractor.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:1090 — `include_str!("../../../assets/generator-presets/Plasma.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:1119 — `include_str!("../../../assets/generator-presets/FluidSim2D.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:1186 — `include_str!("../../../tests/fixtures/presets/TrivialPassthrough.json")`
- crates/manifold-node-engine/src/runtime/tests/generator_runtime.rs:1256 — `include_str!("../../../assets/generator-presets/FluidSim2D.json")`
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:13 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/../manifold-core/tests/fixtures/math-view-legacy/vortex-fragments-events-96c78f522.json" ))`
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:908 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/WaterDamBreakGpuFlip.json" ))`
- crates/manifold-node-engine/src/runtime/tests/math_view.rs:925 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/UniformForce.json" ))`
- crates/manifold-node-engine/src/runtime/tests/modifier_events.rs:130 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json" ))`
- crates/manifold-node-engine/src/validate.rs:673 — `walks shipped assets/effect-presets or assets/generator-presets from manifest directory`
- crates/manifold-node-engine/src/validate.rs:730 — `walks shipped assets/effect-presets or assets/generator-presets from manifest directory`
- crates/manifold-node-engine/src/water/liquid/extent.rs:1983 — `include_str!("../../../assets/generator-presets/WaterDamBreakParticles.json")`
- crates/manifold-node-engine/src/water/liquid/extent.rs:1989 — `include_str!("../../../assets/generator-presets/WaterDamBreakGpuFlip.json")`
- crates/manifold-node-engine/src/water/liquid/extent.rs:1990 — `include_str!("../../../assets/generator-presets/WaterDamBreakParticles.json")`
- crates/manifold-node-engine/src/water/liquid/migration.rs:504 — `include_str!( "../../../../manifold-io/tests/fixtures/water_layer_graph_v1160.json" )`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_body_tests.rs:1215 — `let path = format!("{}/tests/fixtures/{BODY_GOLDEN}", env!("CARGO_MANIFEST_DIR"));`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_body_tests.rs:1239 — `let path = format!("{}/tests/fixtures/{BODY_GOLDEN}", env!("CARGO_MANIFEST_DIR"));`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_body_tests.rs:1260 — `let golden = std::fs::read_to_string(format!("{}/tests/fixtures/{BODY_GOLDEN}", env!("CARGO_MANIFEST_DIR"))).expect("golden fixture reads");`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_body_tests.rs:1270 — `let golden = std::fs::read_to_string(format!("{}/tests/fixtures/{BODY_GOLDEN}", env!("CARGO_MANIFEST_DIR"))).expect("golden fixture reads");`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs:1765 — `let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/generator-presets/{SHIPPED_PRESET}.json"));`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs:1782 — `let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/generator-presets/{PARTICLE_VIEW_PRESET}.json"));`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_pressure_tests.rs:25 — `let path = format!("{}/tests/fixtures/{name}.bin.zst", env!("CARGO_MANIFEST_DIR"));`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_pressure_tests.rs:1544 — `let path = format!("{}/tests/fixtures/{GOLDEN}", env!("CARGO_MANIFEST_DIR"));`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_pressure_tests.rs:2206 — `let golden = std::fs::read_to_string(format!("{}/tests/fixtures/{GOLDEN}", env!("CARGO_MANIFEST_DIR"))).expect("golden fixture reads");`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_pressure_tests.rs:2220 — `let golden = std::fs::read_to_string(format!("{}/tests/fixtures/{GOLDEN}", env!("CARGO_MANIFEST_DIR"))).expect("golden fixture reads");`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_pressure_tests.rs:2240 — `let golden = std::fs::read_to_string(format!("{}/tests/fixtures/{GOLDEN}", env!("CARGO_MANIFEST_DIR"))).expect("golden fixture reads");`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_pressure_tests.rs:2468 — `let golden = std::fs::read_to_string(format!("{}/tests/fixtures/{GOLDEN}", env!("CARGO_MANIFEST_DIR"))).expect("golden fixture reads");`
- crates/manifold-node-engine/src/water/primitives/gpu_flip_render_smoke_tests.rs:843 — `let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/generator-presets").join(file);`
- crates/manifold-node-engine/src/water/primitives/liquid_prepare_tests.rs:29 — `let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/generator-presets").join(file);`
- crates/manifold-node-engine/src/water/primitives/whitewater_golden_tests.rs:337 — `let path = format!("{}/tests/fixtures/{CANDIDATE}", env!("CARGO_MANIFEST_DIR"));`
- crates/manifold-node-engine/src/water/primitives/whitewater_golden_tests.rs:371 — `let path = format!("{}/tests/fixtures/{GOLDEN}", env!("CARGO_MANIFEST_DIR"));`
- crates/manifold-node-engine/src/water/primitives/whitewater_scene_tests.rs:55 — `let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/generator-presets").join(file);`
- crates/manifold-node-engine/src/water/primitives/whitewater_scene_tests.rs:432 — `include_str!("../../../tests/fixtures/whitewater_vendored_group.json")`
- crates/manifold-node-engine/src/water/runtime/gpu_flip_surface.rs:174 — `include_str!("../../assets/generator-presets/WaterDamBreakGpuFlip.json")`
- crates/manifold-node-engine/src/water/runtime/gpu_flip_surface.rs:235 — `include_str!( "../../../manifold-io/tests/fixtures/water_layer_graph_v1160.json" )`
- crates/manifold-node-engine/src/water/runtime/physics_carry_tests.rs:332 — `include_str!( "../../assets/generator-presets/WaterBasin.json" )`
- crates/manifold-node-engine/src/water/runtime/physics_host_modulation_tests.rs:172 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/UniformForce.json" ))`
- crates/manifold-node-engine/src/water/runtime/physics_impulses/scene_routes_tests.rs:8 — `include_str!( "../../../assets/scene-modifier-presets/UniformForce.json" )`
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:591 — `include_str!( "../../assets/generator-presets/WaterBasin.json" )`
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:643 — `include_str!("../../assets/generator-presets/WaterBasin.json")`
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:677 — `include_str!("../../assets/generator-presets/WaterFloatingBoxMatter.json")`
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:692 — `include_str!( "../../assets/generator-presets/PhysicsSolids.json" )`
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:749 — `include_str!( "../../assets/generator-presets/PhysicsSolids.json" )`
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:806 — `include_str!("../../assets/generator-presets/PhysicsSolids.json")`
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:822 — `include_str!( "../../assets/generator-presets/PhysicsSolids.json" )`
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:874 — `include_str!("../../assets/generator-presets/OceanCliff.json")`
- crates/manifold-node-engine/src/water/runtime/physics_sources_tests.rs:12 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/PhysicsSolids.json" ))`
- crates/manifold-node-engine/src/water/runtime/physics_sources_tests.rs:16 — `include_str!(concat!( env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/UniformForce.json" ))`

## P1 source-walking tests — record only

Includes directory walkers, shader-source parsers, single-file source assertions and the pinned-git source comparison. No source-walker expectations were updated.

- crates/manifold-node-engine/src/freeze/classify.rs:935 — source/shader file walk: let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives");
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:797 — source/shader file walk: concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:1115 — source/shader file walk: concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:1172 — source/shader file walk: concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:1447 — source/shader file walk: concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:1514 — source/shader file walk: concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:2381 — source/shader file walk: concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:3010 — source/shader file walk: concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
- crates/manifold-node-engine/src/freeze/install.rs:3699 — source/shader file walk: let freeze_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")) .join("src/node_graph/freeze");
- crates/manifold-node-engine/src/freeze/markers.rs:430 — source/shader file walk: let src_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
- crates/manifold-node-engine/src/water/primitives/gpu_flip_step.rs:3031 — parses included Rust source: include_str!("gpu_flip_step.rs")
- crates/manifold-node-engine/src/water/primitives/whitewater_fused_tests.rs:200 — parses included Rust source: include_str!("whitewater_step.rs")
- crates/manifold-node-engine/src/water/primitives/whitewater_golden_tests.rs:316 — git source walk against pinned renderer module list and source tree
- crates/manifold-renderer/tests/uniform_layout_extended.rs:41 — renderer source/shader walk retains the original source-root assumption: env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives"); let registry = PrimitiveRegistry::with_builtin(); let mut failures = Vec::new(); for &(source
- crates/manifold-renderer/tests/uniform_layout_extended.rs:137 — renderer source/shader walk retains the original source-root assumption: env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives"); let mut failures = Vec::new(); for case in CASES { let Some(node) = registry.construct(ca
- crates/manifold-renderer/tests/uniform_layout_extended.rs:199 — renderer source/shader walk retains the original source-root assumption: env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives"); let mut failures = Vec::new(); for case in custom_abi_cases::CASES { let result = (|| {
- crates/manifold-renderer/tests/uniform_layout_extended.rs:258 — renderer source/shader walk retains the original source-root assumption: env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives"); let source = std::fs::read_to_string(root.join(case.source)).expect("cut-map source"); assert!(
- crates/manifold-renderer/tests/uniform_layout_extended.rs:279 — renderer source/shader walk retains the original source-root assumption: env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives"); let mut expected: BTreeSet<(String, String)> = texture_abi_cases::CASES .iter() .map(
- crates/manifold-renderer/tests/uniform_layout_proof.rs:313 — renderer source/shader walk retains the original source-root assumption: env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives") } fn primitive_files(dir: &Path, out: &mut Vec<PathBuf>) { for entry in std::fs::read_dir(dir).expect("primi
- crates/manifold-renderer/tests/wgsl_validation.rs:9 — renderer source/shader walk retains the original source-root assumption: env!("CARGO_MANIFEST_DIR")).join("src") } /// Recursively find all .wgsl files under a directory. fn find_wgsl_files(dir: &std::path::Path) -> Vec<PathBuf> { let mut file

## P1 shared shader reaches

These renderer consumers now include the shader at its engine-owned path; shader bytes are unchanged. The lead must decide ownership at the next boundary.

- crates/manifold-renderer/tests/wgsl_validation.rs:75 → crates/manifold-node-engine/src/runtime/effects/shaders/tonemap_common.wgsl
- crates/manifold-renderer/tests/wgsl_validation.rs:81 → crates/manifold-node-engine/src/water/primitives/shaders/liquid_pose.wgsl
- crates/manifold-renderer/tests/wgsl_validation.rs:83 → crates/manifold-node-engine/src/water/primitives/shaders/liquid_collider.wgsl
- crates/manifold-renderer/tests/wgsl_validation.rs:85 → crates/manifold-node-engine/src/water/primitives/shaders/liquid_field.wgsl
- crates/manifold-renderer/tests/wgsl_validation.rs:103 → crates/manifold-node-engine/src/water/primitives/shaders/whitewater_common.wgsl
- crates/manifold-renderer/tests/wgsl_validation.rs:104 → crates/manifold-node-engine/src/water/primitives/shaders/liquid_faces.wgsl
- crates/manifold-renderer/tests/wgsl_validation.rs:105 → crates/manifold-node-engine/src/water/primitives/shaders/liquid_field.wgsl
- crates/manifold-renderer/src/pq_encoder.rs:32 → crates/manifold-node-engine/src/runtime/effects/shaders/linear_to_pq_compute.wgsl
- crates/manifold-renderer/src/fsr1.rs:78 → crates/manifold-node-engine/src/runtime/effects/shaders/fsr1_easu_compute.wgsl
- crates/manifold-renderer/src/fsr1.rs:83 → crates/manifold-node-engine/src/runtime/effects/shaders/fsr1_rcas_compute.wgsl
- crates/manifold-renderer/src/live_sim_clock_reference.rs:605 → crates/manifold-node-engine/src/water/primitives/shaders/gpu_flip_clock.wgsl
- crates/manifold-renderer/src/tonemap.rs:89 → crates/manifold-node-engine/src/runtime/effects/shaders/tonemap_common.wgsl
- crates/manifold-renderer/src/tonemap.rs:90 → crates/manifold-node-engine/src/runtime/effects/shaders/aces_tonemap_compute.wgsl
- crates/manifold-renderer/src/metalfx_upscaler.rs:78 → crates/manifold-node-engine/src/runtime/effects/shaders/fsr1_rcas_compute.wgsl
- crates/manifold-renderer/src/presentation.rs:266 → crates/manifold-node-engine/src/runtime/effects/shaders/tonemap_common.wgsl
- crates/manifold-renderer/src/presentation.rs:267 → crates/manifold-node-engine/src/runtime/effects/shaders/presentation.wgsl
- crates/manifold-renderer/src/node_graph/primitives/clamp_liquid_to_solids.rs:90 → crates/manifold-node-engine/src/water/primitives/shaders/clamp_liquid_to_solids_element.wgsl
- crates/manifold-renderer/src/node_graph/primitives/clamp_liquid_to_solids.rs:94 → crates/manifold-node-engine/src/water/primitives/shaders/clamp_liquid_to_solids_element.wgsl
- crates/manifold-renderer/src/node_graph/primitives/count_surface_edges.rs:19 → crates/manifold-node-engine/src/water/primitives/shaders/surface_edge_ownership.wgsl
- crates/manifold-renderer/src/node_graph/primitives/advect_whitewater.rs:156 → crates/manifold-node-engine/src/water/primitives/shaders/advect_whitewater_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/dust_potential.rs:93 → crates/manifold-node-engine/src/water/primitives/shaders/dust_potential_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/age_whitewater.rs:67 → crates/manifold-node-engine/src/water/primitives/shaders/age_whitewater_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/relax_surface_mesh.rs:79 → crates/manifold-node-engine/src/water/primitives/shaders/surface_edge_ownership.wgsl
- crates/manifold-renderer/src/node_graph/primitives/relax_surface_mesh.rs:79 → crates/manifold-node-engine/src/water/primitives/shaders/surface_edge_index.wgsl
- crates/manifold-renderer/src/node_graph/primitives/turbulence_field.rs:67 → crates/manifold-node-engine/src/water/primitives/shaders/turbulence_field_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/turbulence_emission_count.rs:74 → crates/manifold-node-engine/src/water/primitives/shaders/turbulence_emission_count_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/sample_faces_at_particles.rs:92 → crates/manifold-node-engine/src/water/primitives/shaders/sample_faces_at_particles_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/redistance_lattice.rs:66 → crates/manifold-node-engine/src/water/primitives/shaders/marching_cubes_common.wgsl
- crates/manifold-renderer/src/node_graph/primitives/watercolor.rs:46 → crates/manifold-node-engine/src/runtime/effects/shaders/fx_watercolor_compute.wgsl
- crates/manifold-renderer/src/node_graph/primitives/grid_to_matter.rs:119 → crates/manifold-node-engine/src/water/primitives/shaders/grid_to_matter_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/grid_to_matter.rs:283 → crates/manifold-node-engine/src/water/primitives/shaders/grid_to_matter_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/inside_turbulence_potential.rs:91 → crates/manifold-node-engine/src/water/primitives/shaders/inside_turbulence_potential_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/retype_whitewater.rs:95 → crates/manifold-node-engine/src/water/primitives/shaders/retype_whitewater_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/spawn_whitewater.rs:130 → crates/manifold-node-engine/src/water/primitives/shaders/spawn_whitewater_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/surface_mesh_normals.rs:47 → crates/manifold-node-engine/src/water/primitives/shaders/surface_edge_ownership.wgsl
- crates/manifold-renderer/src/node_graph/primitives/surface_mesh_normals.rs:47 → crates/manifold-node-engine/src/water/primitives/shaders/surface_edge_index.wgsl
- crates/manifold-renderer/src/node_graph/primitives/wavecrest_potential.rs:97 → crates/manifold-node-engine/src/water/primitives/shaders/wavecrest_potential_body.wgsl
- crates/manifold-renderer/src/node_graph/primitives/jitter_particles.rs:59 → crates/manifold-node-engine/src/water/primitives/shaders/jitter_particles_body.wgsl

## P1 compiler-demand visibility

56 widenings, all to pub(crate); no public widening was guessed while the renderer compile is blocked. Declaration locations are current.

- crates/manifold-node-engine/src/exec/mod.rs:3 — effect_node: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/exec/mod.rs:1 — backend: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/exec/mod.rs:5 — execution_plan: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/scene/mod.rs:14 — boundary_nodes: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/load/mod.rs:3 — graph_loader: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/load/mod.rs:1 — binding_migration: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/exec/mod.rs:2 — bound_graph: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/load/mod.rs:2 — chain_spec: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/load/mod.rs:4 — loaded_preset_view: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/exec/mod.rs:8 — metal_backend: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/water/runtime/mod.rs:1 — physics_sampling: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/water/runtime/mod.rs:3 — scene_impulses: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/water/runtime/mod.rs:8 — physics_source_state: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/water/runtime/mod.rs:12 — gpu_flip_surface: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/water/runtime/mod.rs:5 — physics_sources: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-1.log.
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:251 — physics_sample_steps: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/mod.rs:91 — core: private → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:78 — retain_physics_setup_outputs: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:99 — PhysicsInputSnapshot: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/scene_impulses.rs:22 — SceneImpulses: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_source_state.rs:22 — PhysicsSourceState: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/gpu_flip_surface.rs:21 — prepare: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_sources.rs:42 — prepare: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_source_runtime.rs:33 — observe_physics_source_strings: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:181 — prepare: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_source_chain.rs:53 — initialize_chain_physics_sources: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_source_runtime.rs:26 — install_physics_source_identities: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_source_state.rs:250 — set_instance: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_sampling.rs:403 — sample_physics_history: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/scene_impulses.rs:213 — observe_impulse_setup: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_source_runtime.rs:74 — refresh_physics_source_graphs: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/scene_impulses.rs:240 — reset_modifier_impulses: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_source_runtime.rs:11 — apply_physics_source_graphs: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/water/runtime/scene_impulses.rs:47 — prepare_modifier_impulses: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:85 — physics_project_tempo: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:82 — physics_sample_steps: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:119 — math_views: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:84 — last_physics_frame_time: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:83 — physics_input_snapshot: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:111 — executor: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:116 — effect_nodes: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:79 — impulse_identity: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:103 — forced_outputs_stale: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:95 — last_forced_outputs_epoch: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:80 — scene_impulses: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:194 — type_id: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:133 — width: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:134 — height: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-2.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:116 — EffectSlot: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-3.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:235 — EffectSlot: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-3.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:300 — bound: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-3.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:318 — def_content_key: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-3.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:236 — physics_sources: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-3.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:260 — node_map: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-3.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:340 — card_prefix: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-3.jsonl.
- crates/manifold-node-engine/src/runtime/core.rs:247 — legacy_index: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-3.jsonl.
- crates/manifold-node-engine/src/water/runtime/physics_source_chain.rs:10 — refresh_chain_physics_source: pub(super) → pub(crate); compiler evidence /tmp/p1-engine-gpu-4.jsonl.

Restricted visibility path relocation: crates/manifold-node-engine/src/water/fluid/identity.rs:5, solver_identity, pub(in crate::node_graph) → pub(in crate). Other pub(in ...) restrictions follow their moved ancestor paths, including the water primitive walker; they are not new public APIs.

## P1 compiler census and gates

Every Cargo invocation used CARGO_BUILD_JOBS=4 and RUSTC_WRAPPER=, one at a time, with the absolute manifest below. Every Cargo command exited 101. No tests or GPU programs were executed.

```sh
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo check --manifest-path '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-4/Cargo.toml' -p manifold-node-engine --all-targets --features gpu-proofs --message-format=json
```
Exit 101. error: could not compile `manifold-node-engine` (lib) due to 57 previous errors; 1 warning emitted; error: could not compile `manifold-node-engine` (lib test) due to 356 previous errors; 4 warnings emitted. Logs: /tmp/p1-engine-gpu-final.jsonl and /tmp/p1-engine-gpu-final.log.

```sh
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo check --manifest-path '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-4/Cargo.toml' -p manifold-node-engine --all-targets --message-format=json
```
Exit 101. error: could not compile `manifold-node-engine` (lib) due to 55 previous errors; 1 warning emitted; error: could not compile `manifold-node-engine` (lib test) due to 216 previous errors; 2 warnings emitted. Logs: /tmp/p1-engine-default-final.jsonl and /tmp/p1-engine-default-final.log.

```sh
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo check --manifest-path '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-4/Cargo.toml' -p manifold-renderer --all-targets --features gpu-proofs --message-format=json
```
Exit 101. error: could not compile `manifold-node-engine` (lib) due to 57 previous errors; 1 warning emitted. Logs: /tmp/p1-renderer-gpu-final.jsonl and /tmp/p1-renderer-gpu-final.log.

```sh
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo check --manifest-path '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-4/Cargo.toml' -p manifold-renderer --all-targets --message-format=json
```
Exit 101. error: could not compile `manifold-node-engine` (lib) due to 55 previous errors; 1 warning emitted. Logs: /tmp/p1-renderer-default-final.jsonl and /tmp/p1-renderer-default-final.log.

```sh
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo clippy --manifest-path '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-4/Cargo.toml' -p manifold-node-engine --tests --message-format=json -- -D warnings
```
Exit 101. error: could not compile `manifold-node-engine` (lib test) due to 218 previous errors. Logs: /tmp/p1-clippy-engine.jsonl and /tmp/p1-clippy-engine.log.

```sh
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo clippy --manifest-path '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-4/Cargo.toml' -p manifold-renderer --tests --message-format=json -- -D warnings
```
Exit 101. error: could not compile `manifold-node-engine` (lib) due to 56 previous errors. Logs: /tmp/p1-clippy-renderer.jsonl and /tmp/p1-clippy-renderer.log.

```sh
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo clippy --manifest-path '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-4/Cargo.toml' -p manifold-app --tests --message-format=json -- -D warnings
```
Exit 101. error: could not compile `manifold-node-engine` (lib) due to 56 previous errors. Logs: /tmp/p1-clippy-app.jsonl and /tmp/p1-clippy-app.log.


The engine GPU-enabled final check reports 57 lib errors and 356 lib-test errors; default features report 55 and 216. Renderer checks and renderer/app clippy stop while compiling the engine dependency: their own Rust targets have not been type-checked. Visibility and import errors on that side may therefore remain for stage 2. No stub, cfg suppression or engine→renderer dependency was used to get past the reaches.

Diagnostic passes, also exit 101: the first two GPU-enabled engine checks used the same command without --message-format=json (/tmp/p1-engine-gpu-initial.log: manifest assembly error, corrected; /tmp/p1-engine-gpu-1.log: missing sibling test plus module visibility and reaches). Three subsequent JSON checks are retained at /tmp/p1-engine-gpu-2.{jsonl,log}, -3.{jsonl,log}, -4.{jsonl,log}; each followed mechanical fixes. The four final census commands were repeated only after path/import or cfg-wiring corrections; their retained logs above are the last invocation. The clippy commands each ran once.

git -C '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-4' diff --check: initially reported four trailing blank lines after module removal; corrected, final exit 0.

Static verification: all 500 old source paths are absent and all 500 destinations exist; all 122 moved WGSL files match their entry git blob byte-for-byte; the feature table is verbatim and workspace lints are enabled; #[test] attributes total 3,921 before and after across renderer plus engine (a textual sanity count, not INV-5). The non-mod.rs moved Rust token sanity check has zero non-wiring residue after path, import, module, cfg-wiring and recorded visibility normalization. Temporary audit scripts and JSON are in /tmp/p1_*; no scripts or gate configuration in the repository were changed. Formal INV-2 and compiled test census are unverified: the former requires the lead's staged/committed move and the latter is blocked by the unresolved crate.

## P1 follow-on diagnostics

These additional type-inference diagnostics follow unresolved family imports/catalog calls in the same functions. They are recorded without adding annotations or changing bodies; whether all disappear after rulings is unverified.

- crates/manifold-node-engine/src/freeze/region/census.rs:467 — E0277: the size for values of type `str` cannot be known at compilation time.
- crates/manifold-node-engine/src/load/binding_migration.rs:88 — E0282: type annotations needed.
- crates/manifold-node-engine/src/scene/gltf_anim_identity.rs:41 — E0282: type annotations needed.
- crates/manifold-node-engine/src/scene/gltf_anim_identity.rs:45 — E0282: type annotations needed.
- crates/manifold-node-engine/src/scene/gltf_anim_identity.rs:48 — E0282: type annotations needed.
- crates/manifold-node-engine/src/scene/gltf_anim_identity.rs:58 — E0282: type annotations needed.
- crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs:764 — E0282: type annotations needed.
- crates/manifold-node-engine/src/freeze/codegen/gpu_tests.rs:3192 — E0282: type annotations needed.
- crates/manifold-node-engine/src/freeze/markers.rs:476 — E0282: type annotations needed.
- crates/manifold-node-engine/src/freeze/markers.rs:499 — E0282: type annotations needed.
- crates/manifold-node-engine/src/freeze/markers.rs:501 — E0277: the size for values of type `str` cannot be known at compilation time.
- crates/manifold-node-engine/src/freeze/markers.rs:514 — E0282: type annotations needed.
- crates/manifold-node-engine/src/freeze/region/census.rs:623 — E0277: the size for values of type `str` cannot be known at compilation time.
- crates/manifold-node-engine/src/freeze/proof.rs:1255 — E0282: type annotations needed.
- crates/manifold-node-engine/src/freeze/proof.rs:1385 — E0277: the size for values of type `str` cannot be known at compilation time.
- crates/manifold-node-engine/src/freeze/proof.rs:1402 — E0282: type annotations needed.
- crates/manifold-node-engine/src/exec/resource_allocation.rs:1498 — E0282: type annotations needed.
- crates/manifold-node-engine/src/runtime/tests/group_mask.rs:48 — E0282: type annotations needed.
- crates/manifold-node-engine/src/water/liquid/extent.rs:1978 — E0282: type annotations needed.
- crates/manifold-node-engine/src/water/liquid/lattice.rs:419 — E0282: type annotations needed.

## P1 remaining rulings and verification limits

Rule on the family reaches, omitted clear_texture_committed helper, forbidden IO/editing test edges, shared shaders, assets and source walkers before stage 2. The D13 INV-7 text is transcribed, but builtins_match_registry still has P0's membership-only implementation: changing that test body is forbidden in stage 1. D7 graph testkit, the layering row, source-path gate maps, regrowth rows, freeze-map documentation and the complete public importer surface remain stage-2 work. The perf/oracle feature declarations are preserved; those additional feature combinations were not compiled. Runtime, pixels, census parity, catalog completeness, migration round-trip and landing gates remain unverified.

Shortcuts taken: none. No semantic workaround, extra family promotion, test weakening, GPU run or workspace test sweep. Demo artifact: none — L1 compiler/source census only.

## P1 commit plan and exact additional file list

No commits or staging. After rulings and stage 2, the lead makes the design's single P1 commit, proposed subject: Carve the grouped manifold-node-engine crate. Its exact paths are both columns of the P1 move list above plus every path below. This includes crate skeletons, importer rewrites, manifests/lockfile, the copied build script and its removed original, the transcribed design and this findings file. Do not land the stage-1 tree.

```text
.claude/orchestration/crate-split-seams.md
Cargo.lock
Cargo.toml
crates/manifold-app/Cargo.toml
crates/manifold-app/src/app.rs
crates/manifold-app/src/app_render.rs
crates/manifold-app/src/bug219_verify.rs
crates/manifold-app/src/content_commands.rs
crates/manifold-app/src/content_export.rs
crates/manifold-app/src/content_pipeline.rs
crates/manifold-app/src/content_state.rs
crates/manifold-app/src/content_thread.rs
crates/manifold-app/src/editor_bridge.rs
crates/manifold-app/src/fluid_domain_edit.rs
crates/manifold-app/src/frame_time.rs
crates/manifold-app/src/gpu_flip_export_demo.rs
crates/manifold-app/src/graph_dump.rs
crates/manifold-app/src/headless_harness.rs
crates/manifold-app/src/import_worker.rs
crates/manifold-app/src/perf_soak.rs
crates/manifold-app/src/perf_soak_import.rs
crates/manifold-app/src/project_io.rs
crates/manifold-app/src/rt_dynamic_export_tests.rs
crates/manifold-app/src/rt_dynamic_held_out.rs
crates/manifold-app/src/scene_modifier_edit.rs
crates/manifold-app/src/scene_modifier_edit/force_tests.rs
crates/manifold-app/src/scene_modifier_journey.rs
crates/manifold-app/src/scene_modifier_journey/periodic.rs
crates/manifold-app/src/scene_modifier_performance.rs
crates/manifold-app/src/scene_modifier_transfer.rs
crates/manifold-app/src/scene_viewport.rs
crates/manifold-app/src/scene_viewport_proof.rs
crates/manifold-app/src/ui_bridge/dispatch/resolve.rs
crates/manifold-app/src/ui_bridge/project.rs
crates/manifold-app/src/ui_bridge/projection/cards.rs
crates/manifold-app/src/ui_bridge/projection/material.rs
crates/manifold-app/src/ui_snapshot/fixtures.rs
crates/manifold-app/src/ui_snapshot/mod.rs
crates/manifold-app/src/ui_snapshot/render.rs
crates/manifold-app/src/ui_snapshot/script.rs
crates/manifold-app/src/ui_translate.rs
crates/manifold-app/src/user_library.rs
crates/manifold-app/src/viewport_input.rs
crates/manifold-app/src/viewport_p5c_demo.rs
crates/manifold-app/src/viewport_p6_demo.rs
crates/manifold-app/src/workspace.rs
crates/manifold-app/tests/project_local_preset_reload.rs
crates/manifold-app/tests/scene_skin_binding_e2e.rs
crates/manifold-app/tests/stock_preset_round_trip.rs
crates/manifold-core/src/generator.rs
crates/manifold-core/src/phong_migration.rs
crates/manifold-core/src/preset_definition_registry.rs
crates/manifold-core/src/project/load_migration.rs
crates/manifold-core/src/project/presets.rs
crates/manifold-core/src/scene_modifier_math_view.rs
crates/manifold-core/src/scene_modifier_preset.rs
crates/manifold-node-engine/Cargo.toml
crates/manifold-node-engine/build.rs
crates/manifold-node-engine/src/exec/mod.rs
crates/manifold-node-engine/src/gpu/mod.rs
crates/manifold-node-engine/src/lib.rs
crates/manifold-node-engine/src/load/mod.rs
crates/manifold-node-engine/src/primitives/mod.rs
crates/manifold-node-engine/src/scene/mod.rs
crates/manifold-node-engine/src/water/mod.rs
crates/manifold-node-engine/src/water/primitives/mod.rs
crates/manifold-node-engine/src/water/runtime/mod.rs
crates/manifold-renderer/Cargo.toml
crates/manifold-renderer/build.rs
crates/manifold-renderer/examples/flower_mesh_drop.rs
crates/manifold-renderer/examples/fluid_capture.rs
crates/manifold-renderer/examples/fusion_census.rs
crates/manifold-renderer/examples/physics_benchmark.rs
crates/manifold-renderer/examples/physics_mesh_benchmark.rs
crates/manifold-renderer/src/bin/check_presets.rs
crates/manifold-renderer/src/bin/freeze_profile.rs
crates/manifold-renderer/src/bin/graph_tool.rs
crates/manifold-renderer/src/bin/render_generator_preset.rs
crates/manifold-renderer/src/bin/render_import.rs
crates/manifold-renderer/src/compositor.rs
crates/manifold-renderer/src/denoiser.rs
crates/manifold-renderer/src/fsr1.rs
crates/manifold-renderer/src/generator_renderer.rs
crates/manifold-renderer/src/generator_renderer/physics_events.rs
crates/manifold-renderer/src/generators/bundled_generator_presets.rs
crates/manifold-renderer/src/generators/mod.rs
crates/manifold-renderer/src/generators/registry.rs
crates/manifold-renderer/src/gpu_readback.rs
crates/manifold-renderer/src/layer_compositor.rs
crates/manifold-renderer/src/lib.rs
crates/manifold-renderer/src/live_sim_clock_reference.rs
crates/manifold-renderer/src/metalfx_temporal_upscaler.rs
crates/manifold-renderer/src/metalfx_upscaler.rs
crates/manifold-renderer/src/node_graph/bundled_presets.rs
crates/manifold-renderer/src/node_graph/catalog_gen.rs
crates/manifold-renderer/src/node_graph/composites/infrared.rs
crates/manifold-renderer/src/node_graph/composites/mod.rs
crates/manifold-renderer/src/node_graph/composites/soft_focus.rs
crates/manifold-renderer/src/node_graph/composites/strobe_opacity.rs
crates/manifold-renderer/src/node_graph/decode_cache.rs
crates/manifold-renderer/src/node_graph/gltf_anim_cache.rs
crates/manifold-renderer/src/node_graph/gltf_import/card_precedence_tests.rs
crates/manifold-renderer/src/node_graph/gltf_import/materials/params.rs
crates/manifold-renderer/src/node_graph/gltf_import/migration.rs
crates/manifold-renderer/src/node_graph/gltf_import/scene.rs
crates/manifold-renderer/src/node_graph/gltf_import/tests.rs
crates/manifold-renderer/src/node_graph/gltf_import/upgrade/calibration.rs
crates/manifold-renderer/src/node_graph/gltf_import/upgrade/params.rs
crates/manifold-renderer/src/node_graph/gltf_import/upgrade/tests.rs
crates/manifold-renderer/src/node_graph/gltf_load.rs
crates/manifold-renderer/src/node_graph/material_inspector.rs
crates/manifold-renderer/src/node_graph/mod.rs
crates/manifold-renderer/src/node_graph/primitives/abs_texture.rs
crates/manifold-renderer/src/node_graph/primitives/advect_whitewater.rs
crates/manifold-renderer/src/node_graph/primitives/affine_scalar.rs
crates/manifold-renderer/src/node_graph/primitives/affine_transform.rs
crates/manifold-renderer/src/node_graph/primitives/age_whitewater.rs
crates/manifold-renderer/src/node_graph/primitives/analytic_echo_instances.rs
crates/manifold-renderer/src/node_graph/primitives/analytic_echo_instances_gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/anti_clump_particles.rs
crates/manifold-renderer/src/node_graph/primitives/apply_radial_burst_3d_to_particles.rs
crates/manifold-renderer/src/node_graph/primitives/apply_radial_burst_to_particles.rs
crates/manifold-renderer/src/node_graph/primitives/array_connect_nearest.rs
crates/manifold-renderer/src/node_graph/primitives/array_diffuse_particles.rs
crates/manifold-renderer/src/node_graph/primitives/array_feedback.rs
crates/manifold-renderer/src/node_graph/primitives/array_filter_detections.rs
crates/manifold-renderer/src/node_graph/primitives/array_math.rs
crates/manifold-renderer/src/node_graph/primitives/array_replicate_polyline_rings.rs
crates/manifold-renderer/src/node_graph/primitives/array_unpack_vec2.rs
crates/manifold-renderer/src/node_graph/primitives/atmosphere.rs
crates/manifold-renderer/src/node_graph/primitives/audio_waveform.rs
crates/manifold-renderer/src/node_graph/primitives/bake_equirect_envmap.rs
crates/manifold-renderer/src/node_graph/primitives/basic_shape.rs
crates/manifold-renderer/src/node_graph/primitives/beat_gate.rs
crates/manifold-renderer/src/node_graph/primitives/beat_ramp.rs
crates/manifold-renderer/src/node_graph/primitives/bend_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/bilateral_blur.rs
crates/manifold-renderer/src/node_graph/primitives/blinn_specular.rs
crates/manifold-renderer/src/node_graph/primitives/blob_bounds.rs
crates/manifold-renderer/src/node_graph/primitives/blob_detect_ffi.rs
crates/manifold-renderer/src/node_graph/primitives/blob_overlay_render.rs
crates/manifold-renderer/src/node_graph/primitives/block_displace_field.rs
crates/manifold-renderer/src/node_graph/primitives/block_sample.rs
crates/manifold-renderer/src/node_graph/primitives/blur_3d_separable.rs
crates/manifold-renderer/src/node_graph/primitives/bokeh_gather.rs
crates/manifold-renderer/src/node_graph/primitives/bokeh_gather_tests.rs
crates/manifold-renderer/src/node_graph/primitives/box_mask.rs
crates/manifold-renderer/src/node_graph/primitives/camera_lens.rs
crates/manifold-renderer/src/node_graph/primitives/camera_orbit.rs
crates/manifold-renderer/src/node_graph/primitives/camera_sky.rs
crates/manifold-renderer/src/node_graph/primitives/camera_switch.rs
crates/manifold-renderer/src/node_graph/primitives/canvas_area_scale.rs
crates/manifold-renderer/src/node_graph/primitives/cel_material.rs
crates/manifold-renderer/src/node_graph/primitives/centered_uv.rs
crates/manifold-renderer/src/node_graph/primitives/checkerboard.rs
crates/manifold-renderer/src/node_graph/primitives/chroma_key.rs
crates/manifold-renderer/src/node_graph/primitives/chromatic_displace.rs
crates/manifold-renderer/src/node_graph/primitives/clamp_liquid_to_solids.rs
crates/manifold-renderer/src/node_graph/primitives/clamp_texture.rs
crates/manifold-renderer/src/node_graph/primitives/clip_trigger_cycle.rs
crates/manifold-renderer/src/node_graph/primitives/clip_trigger_index.rs
crates/manifold-renderer/src/node_graph/primitives/coc_dilate.rs
crates/manifold-renderer/src/node_graph/primitives/coc_from_depth.rs
crates/manifold-renderer/src/node_graph/primitives/color.rs
crates/manifold-renderer/src/node_graph/primitives/color_sample.rs
crates/manifold-renderer/src/node_graph/primitives/colorize.rs
crates/manifold-renderer/src/node_graph/primitives/compose_vec3.rs
crates/manifold-renderer/src/node_graph/primitives/compressor_envelope.rs
crates/manifold-renderer/src/node_graph/primitives/consecutive_edges.rs
crates/manifold-renderer/src/node_graph/primitives/container_bounds_3d.rs
crates/manifold-renderer/src/node_graph/primitives/container_repel_force_3d.rs
crates/manifold-renderer/src/node_graph/primitives/contrast.rs
crates/manifold-renderer/src/node_graph/primitives/convolution_2d_9tap.rs
crates/manifold-renderer/src/node_graph/primitives/copy_positions.rs
crates/manifold-renderer/src/node_graph/primitives/count_surface_edges.rs
crates/manifold-renderer/src/node_graph/primitives/curl_slope_force_3d.rs
crates/manifold-renderer/src/node_graph/primitives/cut_out_box.rs
crates/manifold-renderer/src/node_graph/primitives/cycle_table_row.rs
crates/manifold-renderer/src/node_graph/primitives/cylinder_wrap_field.rs
crates/manifold-renderer/src/node_graph/primitives/depth_estimate_midas.rs
crates/manifold-renderer/src/node_graph/primitives/detect_regions.rs
crates/manifold-renderer/src/node_graph/primitives/diffuse_force_3d_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/digital_plants_render.rs
crates/manifold-renderer/src/node_graph/primitives/displace_copies.rs
crates/manifold-renderer/src/node_graph/primitives/displace_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/distance_to_point.rs
crates/manifold-renderer/src/node_graph/primitives/dither.rs
crates/manifold-renderer/src/node_graph/primitives/dither_pattern.rs
crates/manifold-renderer/src/node_graph/primitives/divide_by_value.rs
crates/manifold-renderer/src/node_graph/primitives/downsample.rs
crates/manifold-renderer/src/node_graph/primitives/draw_connections.rs
crates/manifold-renderer/src/node_graph/primitives/draw_dots.rs
crates/manifold-renderer/src/node_graph/primitives/draw_gauge.rs
crates/manifold-renderer/src/node_graph/primitives/draw_markers.rs
crates/manifold-renderer/src/node_graph/primitives/draw_scanlines.rs
crates/manifold-renderer/src/node_graph/primitives/draw_ticks.rs
crates/manifold-renderer/src/node_graph/primitives/dust_potential.rs
crates/manifold-renderer/src/node_graph/primitives/edge_detect.rs
crates/manifold-renderer/src/node_graph/primitives/edges_from_grid_uv.rs
crates/manifold-renderer/src/node_graph/primitives/edges_from_hypercube.rs
crates/manifold-renderer/src/node_graph/primitives/edges_from_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/ellipse_mask.rs
crates/manifold-renderer/src/node_graph/primitives/envelope_beats.rs
crates/manifold-renderer/src/node_graph/primitives/envelope_decay.rs
crates/manifold-renderer/src/node_graph/primitives/envelope_follower_ar.rs
crates/manifold-renderer/src/node_graph/primitives/euler_step_particles.rs
crates/manifold-renderer/src/node_graph/primitives/euler_step_particles_3d.rs
crates/manifold-renderer/src/node_graph/primitives/extrude_curve.rs
crates/manifold-renderer/src/node_graph/primitives/face_grid_extent_tests.rs
crates/manifold-renderer/src/node_graph/primitives/face_grid_scene_tests.rs
crates/manifold-renderer/src/node_graph/primitives/face_grid_tests.rs
crates/manifold-renderer/src/node_graph/primitives/facet_normals.rs
crates/manifold-renderer/src/node_graph/primitives/fbm_per_instance.rs
crates/manifold-renderer/src/node_graph/primitives/field_combine.rs
crates/manifold-renderer/src/node_graph/primitives/film_grain.rs
crates/manifold-renderer/src/node_graph/primitives/filter.rs
crates/manifold-renderer/src/node_graph/primitives/flash.rs
crates/manifold-renderer/src/node_graph/primitives/flatten_to_camera_plane.rs
crates/manifold-renderer/src/node_graph/primitives/flow_field_noise.rs
crates/manifold-renderer/src/node_graph/primitives/fluid_role_source.rs
crates/manifold-renderer/src/node_graph/primitives/fluid_role_source/geometry.rs
crates/manifold-renderer/src/node_graph/primitives/fold_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/fract_texture.rs
crates/manifold-renderer/src/node_graph/primitives/free_camera.rs
crates/manifold-renderer/src/node_graph/primitives/frequency_ratio.rs
crates/manifold-renderer/src/node_graph/primitives/fresnel_rim.rs
crates/manifold-renderer/src/node_graph/primitives/gaussian_blur_variable_width.rs
crates/manifold-renderer/src/node_graph/primitives/generate_cube_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/generate_grid_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/generate_grid_uv.rs
crates/manifold-renderer/src/node_graph/primitives/generate_instance_transforms.rs
crates/manifold-renderer/src/node_graph/primitives/generate_range.rs
crates/manifold-renderer/src/node_graph/primitives/glitch_jitter.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_anim_shared.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_animation_source.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_mesh_source.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_morph_deltas_source.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_morph_weights.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_skeleton_pose.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_skinned_mesh_source.rs
crates/manifold-renderer/src/node_graph/primitives/gltf_texture_source.rs
crates/manifold-renderer/src/node_graph/primitives/glyph_atlas.rs
crates/manifold-renderer/src/node_graph/primitives/gradient_central_diff.rs
crates/manifold-renderer/src/node_graph/primitives/gradient_central_diff_3d.rs
crates/manifold-renderer/src/node_graph/primitives/gradient_ramp.rs
crates/manifold-renderer/src/node_graph/primitives/grid_to_matter.rs
crates/manifold-renderer/src/node_graph/primitives/grid_uv_field.rs
crates/manifold-renderer/src/node_graph/primitives/hash_field_by_seed.rs
crates/manifold-renderer/src/node_graph/primitives/hdr_retention_mix.rs
crates/manifold-renderer/src/node_graph/primitives/hdri_source.rs
crates/manifold-renderer/src/node_graph/primitives/heightfield_shadow.rs
crates/manifold-renderer/src/node_graph/primitives/heightmap_to_normal.rs
crates/manifold-renderer/src/node_graph/primitives/hue_saturation.rs
crates/manifold-renderer/src/node_graph/primitives/hypercube_vertices.rs
crates/manifold-renderer/src/node_graph/primitives/image_folder.rs
crates/manifold-renderer/src/node_graph/primitives/inject_burst.rs
crates/manifold-renderer/src/node_graph/primitives/inside_turbulence_potential.rs
crates/manifold-renderer/src/node_graph/primitives/instance_position_jitter.rs
crates/manifold-renderer/src/node_graph/primitives/instance_rotation_jitter.rs
crates/manifold-renderer/src/node_graph/primitives/interpolate_particle_frames.rs
crates/manifold-renderer/src/node_graph/primitives/inverse_fft_2d.rs
crates/manifold-renderer/src/node_graph/primitives/invert.rs
crates/manifold-renderer/src/node_graph/primitives/jitter_particles.rs
crates/manifold-renderer/src/node_graph/primitives/lambert_directional.rs
crates/manifold-renderer/src/node_graph/primitives/layer_source.rs
crates/manifold-renderer/src/node_graph/primitives/length_vec2.rs
crates/manifold-renderer/src/node_graph/primitives/lerp_instance_fields.rs
crates/manifold-renderer/src/node_graph/primitives/levels.rs
crates/manifold-renderer/src/node_graph/primitives/lfo.rs
crates/manifold-renderer/src/node_graph/primitives/lic_integrate.rs
crates/manifold-renderer/src/node_graph/primitives/light.rs
crates/manifold-renderer/src/node_graph/primitives/lightning_bolt.rs
crates/manifold-renderer/src/node_graph/primitives/linear_gradient.rs
crates/manifold-renderer/src/node_graph/primitives/live_draw_args.rs
crates/manifold-renderer/src/node_graph/primitives/look_at_camera.rs
crates/manifold-renderer/src/node_graph/primitives/loop_camera.rs
crates/manifold-renderer/src/node_graph/primitives/luminance.rs
crates/manifold-renderer/src/node_graph/primitives/lut1d.rs
crates/manifold-renderer/src/node_graph/primitives/magnitude_db.rs
crates/manifold-renderer/src/node_graph/primitives/mask_extrema.rs
crates/manifold-renderer/src/node_graph/primitives/matcap_two_tone.rs
crates/manifold-renderer/src/node_graph/primitives/math.rs
crates/manifold-renderer/src/node_graph/primitives/melt_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_cut_map.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_cut_remap.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_ramp.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_snapshot.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_spatial_mask.rs
crates/manifold-renderer/src/node_graph/primitives/mesh_stagger_envelope.rs
crates/manifold-renderer/src/node_graph/primitives/mirror_axis.rs
crates/manifold-renderer/src/node_graph/primitives/mirror_fold_uv.rs
crates/manifold-renderer/src/node_graph/primitives/mix_arrays.rs
crates/manifold-renderer/src/node_graph/primitives/mod.rs
crates/manifold-renderer/src/node_graph/primitives/morph_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/morph_targets_blend.rs
crates/manifold-renderer/src/node_graph/primitives/motion_blur.rs
crates/manifold-renderer/src/node_graph/primitives/multi_blend.rs
crates/manifold-renderer/src/node_graph/primitives/mux_array.rs
crates/manifold-renderer/src/node_graph/primitives/mux_scalar.rs
crates/manifold-renderer/src/node_graph/primitives/neighbor_smooth.rs
crates/manifold-renderer/src/node_graph/primitives/nested_cubes_geometry.rs
crates/manifold-renderer/src/node_graph/primitives/noise.rs
crates/manifold-renderer/src/node_graph/primitives/noise_displace.rs
crates/manifold-renderer/src/node_graph/primitives/normal_wave_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/normalize_vec2.rs
crates/manifold-renderer/src/node_graph/primitives/ocean_displace.rs
crates/manifold-renderer/src/node_graph/primitives/ocean_spectrum.rs
crates/manifold-renderer/src/node_graph/primitives/offset_lattice.rs
crates/manifold-renderer/src/node_graph/primitives/one_euro_filter.rs
crates/manifold-renderer/src/node_graph/primitives/optical_flow_estimate.rs
crates/manifold-renderer/src/node_graph/primitives/ordered_recon_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/over.rs
crates/manifold-renderer/src/node_graph/primitives/pack_channels.rs
crates/manifold-renderer/src/node_graph/primitives/pack_curve_xy.rs
crates/manifold-renderer/src/node_graph/primitives/pack_vec4.rs
crates/manifold-renderer/src/node_graph/primitives/particle_frame_blend_tests.rs
crates/manifold-renderer/src/node_graph/primitives/particle_publication_gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/particles_to_copies.rs
crates/manifold-renderer/src/node_graph/primitives/pbr_material.rs
crates/manifold-renderer/src/node_graph/primitives/peak.rs
crates/manifold-renderer/src/node_graph/primitives/person_segment.rs
crates/manifold-renderer/src/node_graph/primitives/plane_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/platonic_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/polar_field.rs
crates/manifold-renderer/src/node_graph/primitives/polytope_edges.rs
crates/manifold-renderer/src/node_graph/primitives/polytope_vertices.rs
crates/manifold-renderer/src/node_graph/primitives/posterize.rs
crates/manifold-renderer/src/node_graph/primitives/power_texture.rs
crates/manifold-renderer/src/node_graph/primitives/project_3d.rs
crates/manifold-renderer/src/node_graph/primitives/project_4d.rs
crates/manifold-renderer/src/node_graph/primitives/projected_grid.rs
crates/manifold-renderer/src/node_graph/primitives/push_along_normals.rs
crates/manifold-renderer/src/node_graph/primitives/radial_burst_force_field.rs
crates/manifold-renderer/src/node_graph/primitives/radial_fold_uv.rs
crates/manifold-renderer/src/node_graph/primitives/radial_offset_field.rs
crates/manifold-renderer/src/node_graph/primitives/redistance_lattice.rs
crates/manifold-renderer/src/node_graph/primitives/reflect_array.rs
crates/manifold-renderer/src/node_graph/primitives/region_mask.rs
crates/manifold-renderer/src/node_graph/primitives/reinhard_tone_map.rs
crates/manifold-renderer/src/node_graph/primitives/relax_surface_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/remap.rs
crates/manifold-renderer/src/node_graph/primitives/remap_cut_weights.rs
crates/manifold-renderer/src/node_graph/primitives/remap_mesh_cut.rs
crates/manifold-renderer/src/node_graph/primitives/remove_drift_3d.rs
crates/manifold-renderer/src/node_graph/primitives/render_3d_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/render_filled_rects.rs
crates/manifold-renderer/src/node_graph/primitives/render_glyph_grid.rs
crates/manifold-renderer/src/node_graph/primitives/render_instanced_3d_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/render_lines.rs
crates/manifold-renderer/src/node_graph/primitives/render_mesh_diagram.rs
crates/manifold-renderer/src/node_graph/primitives/render_mesh_diagram/gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/render_mesh_diagram_depth_tests.rs
crates/manifold-renderer/src/node_graph/primitives/render_mode.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene/gpu_tests.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene/rt_changes.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene/rt_proof.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene/scene_viewport.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene/subsurface.rs
crates/manifold-renderer/src/node_graph/primitives/render_scene/tests.rs
crates/manifold-renderer/src/node_graph/primitives/render_text.rs
crates/manifold-renderer/src/node_graph/primitives/render_value_overlay.rs
crates/manifold-renderer/src/node_graph/primitives/resize_limit.rs
crates/manifold-renderer/src/node_graph/primitives/resolve_3d_accumulator.rs
crates/manifold-renderer/src/node_graph/primitives/resolve_accumulator.rs
crates/manifold-renderer/src/node_graph/primitives/retype_whitewater.rs
crates/manifold-renderer/src/node_graph/primitives/revolve_curve.rs
crates/manifold-renderer/src/node_graph/primitives/rgb_distance.rs
crates/manifold-renderer/src/node_graph/primitives/rigid_body.rs
crates/manifold-renderer/src/node_graph/primitives/ripple_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/rotate_2d.rs
crates/manifold-renderer/src/node_graph/primitives/rotate_3d.rs
crates/manifold-renderer/src/node_graph/primitives/rotate_4d.rs
crates/manifold-renderer/src/node_graph/primitives/rotate_vec2_by_angle.rs
crates/manifold-renderer/src/node_graph/primitives/sample_and_hold.rs
crates/manifold-renderer/src/node_graph/primitives/sample_faces_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/sample_mesh_triangles.rs
crates/manifold-renderer/src/node_graph/primitives/sample_texture_3d_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/sample_texture_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/sample_triangle_grid.rs
crates/manifold-renderer/src/node_graph/primitives/sample_volume_2d.rs
crates/manifold-renderer/src/node_graph/primitives/saturation.rs
crates/manifold-renderer/src/node_graph/primitives/scalar_array_accumulator.rs
crates/manifold-renderer/src/node_graph/primitives/scale_offset_texture.rs
crates/manifold-renderer/src/node_graph/primitives/scanline_jitter_field.rs
crates/manifold-renderer/src/node_graph/primitives/scatter_on_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/scatter_particles.rs
crates/manifold-renderer/src/node_graph/primitives/scatter_particles_3d.rs
crates/manifold-renderer/src/node_graph/primitives/scatter_particles_camera.rs
crates/manifold-renderer/src/node_graph/primitives/scene_array.rs
crates/manifold-renderer/src/node_graph/primitives/scene_fx_default_passthrough.rs
crates/manifold-renderer/src/node_graph/primitives/scene_object.rs
crates/manifold-renderer/src/node_graph/primitives/sea_horizon_env.rs
crates/manifold-renderer/src/node_graph/primitives/seed_particles.rs
crates/manifold-renderer/src/node_graph/primitives/seed_particles_from_texture.rs
crates/manifold-renderer/src/node_graph/primitives/separable_gaussian.rs
crates/manifold-renderer/src/node_graph/primitives/set_alpha.rs
crates/manifold-renderer/src/node_graph/primitives/shape_particle_blobs.rs
crates/manifold-renderer/src/node_graph/primitives/sharpen.rs
crates/manifold-renderer/src/node_graph/primitives/shatter_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/simplex_field_2d.rs
crates/manifold-renderer/src/node_graph/primitives/simplex_noise_force_3d_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/simplex_noise_force_at_particles.rs
crates/manifold-renderer/src/node_graph/primitives/simplex_per_instance.rs
crates/manifold-renderer/src/node_graph/primitives/sin_term.rs
crates/manifold-renderer/src/node_graph/primitives/skin_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/slice_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/slope_displace.rs
crates/manifold-renderer/src/node_graph/primitives/smooth_lattice.rs
crates/manifold-renderer/src/node_graph/primitives/smooth_surface_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/smoothing.rs
crates/manifold-renderer/src/node_graph/primitives/smoothstep_texture.rs
crates/manifold-renderer/src/node_graph/primitives/spawn_from_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/spawn_whitewater.rs
crates/manifold-renderer/src/node_graph/primitives/ssao_gtao.rs
crates/manifold-renderer/src/node_graph/primitives/surface_mesh_freeze_tests.rs
crates/manifold-renderer/src/node_graph/primitives/surface_mesh_normals.rs
crates/manifold-renderer/src/node_graph/primitives/taper_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/temporal.rs
crates/manifold-renderer/src/node_graph/primitives/terminal_analysis.rs
crates/manifold-renderer/src/node_graph/primitives/terminal_stream.rs
crates/manifold-renderer/src/node_graph/primitives/test_camera_pointwise_fixture.rs
crates/manifold-renderer/src/node_graph/primitives/test_face_lattice_fixture.rs
crates/manifold-renderer/src/node_graph/primitives/test_multi_output_atomic_fixture.rs
crates/manifold-renderer/src/node_graph/primitives/texture_advect.rs
crates/manifold-renderer/src/node_graph/primitives/texture_dimensions.rs
crates/manifold-renderer/src/node_graph/primitives/texture_sum_5.rs
crates/manifold-renderer/src/node_graph/primitives/tone_map.rs
crates/manifold-renderer/src/node_graph/primitives/torus_wrap_field.rs
crates/manifold-renderer/src/node_graph/primitives/track_persist.rs
crates/manifold-renderer/src/node_graph/primitives/track_regions.rs
crates/manifold-renderer/src/node_graph/primitives/transform_3d.rs
crates/manifold-renderer/src/node_graph/primitives/transform_components.rs
crates/manifold-renderer/src/node_graph/primitives/transform_mesh_patches.rs
crates/manifold-renderer/src/node_graph/primitives/transform_shake.rs
crates/manifold-renderer/src/node_graph/primitives/triangulate_grid.rs
crates/manifold-renderer/src/node_graph/primitives/trig_texture.rs
crates/manifold-renderer/src/node_graph/primitives/trigger_ease_to.rs
crates/manifold-renderer/src/node_graph/primitives/trigger_gate.rs
crates/manifold-renderer/src/node_graph/primitives/tube_from_path.rs
crates/manifold-renderer/src/node_graph/primitives/turbulence_emission_count.rs
crates/manifold-renderer/src/node_graph/primitives/turbulence_field.rs
crates/manifold-renderer/src/node_graph/primitives/twist_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/unlit_material.rs
crates/manifold-renderer/src/node_graph/primitives/uv_displace_by_flow.rs
crates/manifold-renderer/src/node_graph/primitives/uv_field.rs
crates/manifold-renderer/src/node_graph/primitives/uv_strip_clamp.rs
crates/manifold-renderer/src/node_graph/primitives/vector_fields.rs
crates/manifold-renderer/src/node_graph/primitives/vignette.rs
crates/manifold-renderer/src/node_graph/primitives/volume_optics_tests.rs
crates/manifold-renderer/src/node_graph/primitives/voronoi_2d.rs
crates/manifold-renderer/src/node_graph/primitives/voxelize_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/watercolor.rs
crates/manifold-renderer/src/node_graph/primitives/wave_field_3d.rs
crates/manifold-renderer/src/node_graph/primitives/wave_shear_mesh.rs
crates/manifold-renderer/src/node_graph/primitives/wavecrest_potential.rs
crates/manifold-renderer/src/node_graph/primitives/wet_dry_mix.rs
crates/manifold-renderer/src/node_graph/primitives/wrap_particles_torus.rs
crates/manifold-renderer/src/node_graph/primitives/zero_array.rs
crates/manifold-renderer/src/node_graph/relight.rs
crates/manifold-renderer/src/node_graph/scene_exposure.rs
crates/manifold-renderer/src/node_graph/scene_modifier_authoring.rs
crates/manifold-renderer/src/node_graph/scene_modifier_legacy_migration.rs
crates/manifold-renderer/src/node_graph/scene_modifier_legacy_migration/sources.rs
crates/manifold-renderer/src/node_graph/scene_vm.rs
crates/manifold-renderer/src/node_graph/viewport_gizmo.rs
crates/manifold-renderer/src/node_graph/viewport_overlay.rs
crates/manifold-renderer/src/node_graph/viewport_render.rs
crates/manifold-renderer/src/node_graph/viewport_session.rs
crates/manifold-renderer/src/pq_encoder.rs
crates/manifold-renderer/src/presentation.rs
crates/manifold-renderer/src/preset_thumbnail.rs
crates/manifold-renderer/src/tonemap.rs
crates/manifold-renderer/tests/ableton_picker_scroll_proof.rs
crates/manifold-renderer/tests/automation_stroke.rs
crates/manifold-renderer/tests/blob_v2_demo.rs
crates/manifold-renderer/tests/blob_v2_presets.rs
crates/manifold-renderer/tests/card_binding_shadow_corpus.rs
crates/manifold-renderer/tests/code_terminal.rs
crates/manifold-renderer/tests/dropdown_clip_proof.rs
crates/manifold-renderer/tests/file_loader_exhaustiveness.rs
crates/manifold-renderer/tests/fluid_preset.rs
crates/manifold-renderer/tests/fragment_cut_scene.rs
crates/manifold-renderer/tests/glb_conformance.rs
crates/manifold-renderer/tests/gpu_proofs/alpha_contract.rs
crates/manifold-renderer/tests/gpu_proofs/bug237_light_camera_commit_render_proof.rs
crates/manifold-renderer/tests/gpu_proofs/camera_conformance.rs
crates/manifold-renderer/tests/gpu_proofs/cinematic_scene_tail.rs
crates/manifold-renderer/tests/gpu_proofs/encode_replay.rs
crates/manifold-renderer/tests/gpu_proofs/film_grain_decorrelation.rs
crates/manifold-renderer/tests/gpu_proofs/fluid_array_growth.rs
crates/manifold-renderer/tests/gpu_proofs/fluid_pause.rs
crates/manifold-renderer/tests/gpu_proofs/fluid_surface_perf.rs
crates/manifold-renderer/tests/gpu_proofs/fragment_storage.rs
crates/manifold-renderer/tests/gpu_proofs/gbuffer_depth.rs
crates/manifold-renderer/tests/gpu_proofs/gbuffer_velocity.rs
crates/manifold-renderer/tests/gpu_proofs/gpu_flip_frame_perf.rs
crates/manifold-renderer/tests/gpu_proofs/harness.rs
crates/manifold-renderer/tests/gpu_proofs/liquid_conformance.rs
crates/manifold-renderer/tests/gpu_proofs/liquid_indexed.rs
crates/manifold-renderer/tests/gpu_proofs/matter_bodies.rs
crates/manifold-renderer/tests/gpu_proofs/matter_coupling.rs
crates/manifold-renderer/tests/gpu_proofs/matter_look.rs
crates/manifold-renderer/tests/gpu_proofs/matter_scene.rs
crates/manifold-renderer/tests/gpu_proofs/matter_solver_perf.rs
crates/manifold-renderer/tests/gpu_proofs/matter_transfer.rs
crates/manifold-renderer/tests/gpu_proofs/motion_blur_visibility.rs
crates/manifold-renderer/tests/gpu_proofs/node_error_status.rs
crates/manifold-renderer/tests/gpu_proofs/physics_boxes.rs
crates/manifold-renderer/tests/gpu_proofs/physics_solids.rs
crates/manifold-renderer/tests/gpu_proofs/physics_takes.rs
crates/manifold-renderer/tests/gpu_proofs/render_legacy_parity.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_ao_mask.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_exposure.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_fog.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_glass.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_ibl.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_instances.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_lights.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_map_set.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_material_upgrade.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_object_visibility.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_pbr_fidelity.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_pcss.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_punctual_fidelity.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_shadow_cache.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_shadows.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_subsurface.rs
crates/manifold-renderer/tests/gpu_proofs/render_scene_uv1_preservation.rs
crates/manifold-renderer/tests/gpu_proofs/rt_6caster_shadow.rs
crates/manifold-renderer/tests/gpu_proofs/rt_bug17r3_lightless_gi.rs
crates/manifold-renderer/tests/gpu_proofs/rt_bug318_import_toggle.rs
crates/manifold-renderer/tests/gpu_proofs/rt_bug326_fix_gate.rs
crates/manifold-renderer/tests/gpu_proofs/rt_bug88m_blend_specular_gate.rs
crates/manifold-renderer/tests/gpu_proofs/rt_bugmajv_kernel_toggle.rs
crates/manifold-renderer/tests/gpu_proofs/rt_dynamic_catalog.rs
crates/manifold-renderer/tests/gpu_proofs/rt_dynamic_current_frame.rs
crates/manifold-renderer/tests/gpu_proofs/rt_dynamic_fusion.rs
crates/manifold-renderer/tests/gpu_proofs/rt_dynamic_perf.rs
crates/manifold-renderer/tests/gpu_proofs/rt_dynamic_shading.rs
crates/manifold-renderer/tests/gpu_proofs/rt_edc_enclosure.rs
crates/manifold-renderer/tests/gpu_proofs/rt_emissive_direct.rs
crates/manifold-renderer/tests/gpu_proofs/rt_emissive_instancing.rs
crates/manifold-renderer/tests/gpu_proofs/rt_furnace_oracle.rs
crates/manifold-renderer/tests/gpu_proofs/rt_gesture_response.rs
crates/manifold-renderer/tests/gpu_proofs/rt_instancing.rs
crates/manifold-renderer/tests/gpu_proofs/rt_multi_caster_shadow.rs
crates/manifold-renderer/tests/gpu_proofs/rt_normal_tangent_mirror.rs
crates/manifold-renderer/tests/gpu_proofs/rt_object_cast_shadows.rs
crates/manifold-renderer/tests/gpu_proofs/rt_object_motion_shadow.rs
crates/manifold-renderer/tests/gpu_proofs/rt_p1_region_probe.rs
crates/manifold-renderer/tests/gpu_proofs/rt_p2_soft_ao_temporal.rs
crates/manifold-renderer/tests/gpu_proofs/rt_p3_emissive_gi.rs
crates/manifold-renderer/tests/gpu_proofs/rt_p3_emissive_texture.rs
crates/manifold-renderer/tests/gpu_proofs/rt_p4_metalfx_temporal.rs
crates/manifold-renderer/tests/gpu_proofs/rt_r1_reflection.rs
crates/manifold-renderer/tests/gpu_proofs/rt_r2_accumulation.rs
crates/manifold-renderer/tests/gpu_proofs/rt_r3_heldout_gltf.rs
crates/manifold-renderer/tests/gpu_proofs/rt_t2b_temporal_wiring.rs
crates/manifold-renderer/tests/gpu_proofs/rt_t38_multibounce.rs
crates/manifold-renderer/tests/gpu_proofs/rt_w0_gbuffer.rs
crates/manifold-renderer/tests/gpu_proofs/scene_modifier_legacy.rs
crates/manifold-renderer/tests/gpu_proofs/scene_object_migration_round_trip.rs
crates/manifold-renderer/tests/gpu_proofs/scene_viewport_navigate.rs
crates/manifold-renderer/tests/gpu_proofs/scene_viewport_session.rs
crates/manifold-renderer/tests/gpu_proofs/smoke.rs
crates/manifold-renderer/tests/gpu_proofs/substeps.rs
crates/manifold-renderer/tests/gpu_proofs/water_basin.rs
crates/manifold-renderer/tests/gpu_proofs/water_basin/authored_coupling.rs
crates/manifold-renderer/tests/gpu_proofs/water_basin/explicit_authoring.rs
crates/manifold-renderer/tests/led_preset_value_tests.rs
crates/manifold-renderer/tests/led_utility_value_tests.rs
crates/manifold-renderer/tests/mosh_presets.rs
crates/manifold-renderer/tests/param_wrap_smoke.rs
crates/manifold-renderer/tests/particle_pipeline_integration.rs
crates/manifold-renderer/tests/photoscan_modifier_plans.rs
crates/manifold-renderer/tests/physics_scene.rs
crates/manifold-renderer/tests/project_preset_overlay.rs
crates/manifold-renderer/tests/scene_force_presets.rs
crates/manifold-renderer/tests/scene_loop_e2e_import.rs
crates/manifold-renderer/tests/scene_loop_probe.rs
crates/manifold-renderer/tests/scene_loop_roundtrip_gate.rs
crates/manifold-renderer/tests/scene_loop_wrap_parity.rs
crates/manifold-renderer/tests/scene_modifier_file_authoring.rs
crates/manifold-renderer/tests/scene_modifier_fragment_masks.rs
crates/manifold-renderer/tests/scene_modifier_inv_gate.rs
crates/manifold-renderer/tests/scene_modifier_legacy_migration.rs
crates/manifold-renderer/tests/scene_modifier_photoscan_migration.rs
crates/manifold-renderer/tests/scene_modifier_stock.rs
crates/manifold-renderer/tests/structured_modifier_echo.rs
crates/manifold-renderer/tests/text_clip_to_node_bounds.rs
crates/manifold-renderer/tests/trigger_shadow_class_guard.rs
crates/manifold-renderer/tests/ui_cell_arc_repro.rs
crates/manifold-renderer/tests/ui_color_swatches.rs
crates/manifold-renderer/tests/uniform_layout_extended.rs
crates/manifold-renderer/tests/uniform_layout_proof.rs
crates/manifold-renderer/tests/wgsl_validation.rs
docs/RENDERER_CRATE_SPLIT_DESIGN.md
```
