#!/usr/bin/env python3
"""Semantic gate ownership and assets that Cargo metadata cannot describe."""
from pathlib import Path
import re

def is_inert_plan_path(path):
    """Committed crate-move plans are replay data, never live build inputs."""
    root = ".claude/orchestration/crate-split"
    return path == root or path.startswith(root + "/")


SHARED_ASSETS = ['crates/manifold-foundation/assets/fonts']
GPU_DEFAULT_CPU_ONLY = {
    "manifold-nodes-water": "Moved default water tests are CPU contracts; device proofs require gpu-proofs",
    "manifold-nodes-scene": "Device proofs require gpu-proofs; ungated imported-graph validation lives in the catalog",
    "manifold-compositor": "GPU device proofs require gpu-proofs; default tests are CPU contracts",
    "manifold-nodes-image": "GPU device proofs require gpu-proofs; default tests are CPU contracts",
    'manifold-editing': 'GPU graph construction is gated by gpu-proofs',
    'manifold-spectral': 'spectrogram device tests require gpu-proofs',
}
NEXTTEST_GPU_FILTER = '''
    package(manifold-gpu)
  | (binary_id(manifold-media) & test(/^decode_scheduler::tests::|^image_renderer::tests::prewarm_layer_decodes_image_clips$/))
  | (binary_id(manifold-nodes::main) & test(/^contracts::node_graph::(catalog_tests::gltf_import::corrupted_assembler_output_fails_validation_naming_the_node|catalog_tests::validate::every_bundled_preset_validates_clean)$/))
  | (binary_id(manifold-node-engine) & test(/^(exec::execution::tests::aliased_output_assertion_fires_on_silent_primitive|load::graph_loader::tests::(audit_fires_on_unbound_array_resource|pre_allocate_resources_accepts_fully_bound_plan))$/))
  | (binary_id(manifold-ui-paint::main) & test(/^contracts::(ableton_picker_scroll_proof|dropdown_clip_proof|text_clip_to_node_bounds|ui_cell_arc_repro)::/))
  | (binary_id(manifold-nodes::main) & test(/^scene_loop_probe::/))
  | (binary_id(manifold-nodes::main) & test(/^scene_loop_wrap_parity::/))
  | (binary_id(manifold-app::renderer_contracts) & test(/^ui_color_swatches::/))
  | binary_id(manifold-app::led_edge_identity)
  | (binary_id(manifold-app::bin/manifold) & test(/^(content_thread::tests::paused_|gap_start_probe::|mute_visibility_probe::|viewport_p5c_demo::|viewport_p6_demo::|ui_bridge::project::tests::sdr_controls_route_pointer_gestures_to_content_and_undo$)/))
'''
GPU_BACKEND_ROOT = 'crates/manifold-gpu/'
OTHER_SHADER_ROOTS = ('crates/manifold-led/', 'crates/manifold-recording/', 'crates/manifold-spectral/')
CATALOG_PACKAGE = 'manifold-nodes'
PRIMITIVE_PATHS = ('crates/manifold-nodes-scene/src/node_graph/primitives/',
                   'crates/manifold-nodes-image/src/node_graph/primitives/',
                   'crates/manifold-node-engine/src/primitives/',
                   'crates/manifold-nodes-water/src/primitives/')
CATALOG_PATHS = (*PRIMITIVE_PATHS,
                 'crates/manifold-nodes/src/catalog_gen.rs',
                 'crates/manifold-node-engine/src/descriptor.rs',
                 'crates/manifold-nodes/src/registry.rs',
                 'docs/node_catalog')

RENDERER_SRC = "crates/manifold-nodes/src/"
ENGINE_SRC = "crates/manifold-node-engine/src/"
WATER_SRC = "crates/manifold-nodes-water/src/"
CONTRACT_TESTS_DIR = ("crates/manifold-nodes/tests/contracts/",
                      "crates/manifold-app/tests/contracts/")
GPU_CONTRACT_TARGETS = {"manifold-nodes": "main", "manifold-app": "renderer_contracts"}
# These app surfaces translate project snapshots into UI state. Their CPU
# contracts and UI flows own coverage; renderer proof harnesses do not mount
# them. Keep content commands, render loops and GPU paint on their GPU routes.
UI_PROJECTION_PATHS = (
    "crates/manifold-app/src/ui_bridge/projection/",
    "crates/manifold-app/src/ui_bridge/state_sync.rs",
    "crates/manifold-app/src/ui_translate.rs",
)
UI_PAINT_DIR = "crates/manifold-ui-paint/"
UI_PAINT_FILTERS = ["clip_content_gpu::tests::gpu::", "ui_renderer::tests::", "contracts::"]
PROOFS_DIR = "crates/manifold-nodes/tests/gpu_proofs/"
CPU_FLIP_FIXTURES_DIR = "crates/manifold-nodes/tests/fixtures/cpu-flip/"
CPU_FLIP_REFERENCE_FILTERS = [
    "liquid_conformance::",
    "water_basin::",
    "fluid_surface_perf::",
    "contracts::node_graph::catalog_tests::whitewater_scene::",
    "primitives::gpu_flip_preset::",
    "load::expand::acceleration::",
    "runtime::physics_carry::",
    "runtime::physics_sampling::",
    "runtime::physics_impulses::tests::coupled_playback_tests::",
]

# Landing warning budget for the scoped (non-glb) GPU step, seconds of test time.
LANDING_BUDGET_S = 360

# Fixed end-to-end smoke: always runs when any GPU path is touched. Four proofs
# that cover the effect chain + alpha contract, command-buffer replay, the
# camera/scene render, and the G-buffer. Must stay under ~2 minutes in total;
# the 25-slowest timing report is how that is re-checked.
SMOKE_FILTERS = [
    "alpha_contract::effects_preserve_transparency",
    "encode_replay::encode_replay_parity",
    "camera_conformance::render_scene_matches_project_to_pixel_oracle",
    "gbuffer_depth::gbuffer_depth_conformance",
]

# Exact filters whose Cargo owner has been audited.  Plan.runs may prune a
# filtered harness only when every selected filter is in this table and none
# belongs to that harness; unknown filters conservatively keep the old run.
GPU_FILTER_TARGETS = {
    "alpha_contract::effects_preserve_transparency": ("manifold-nodes", "gpu_proofs"),
    "encode_replay::encode_replay_parity": ("manifold-nodes", "gpu_proofs"),
    "gbuffer_depth::gbuffer_depth_conformance": ("manifold-nodes", "gpu_proofs"),
    "camera_conformance::render_scene_matches_project_to_pixel_oracle":
        ("manifold-nodes-scene", "gpu_proofs"),
    "bundled_presets::": ("manifold-nodes", "lib"),
    "bundled_generator_presets::": ("manifold-nodes", "lib"),
}

# Graph runtime + freeze compiler.
RUNTIME_FILTERS = [
    "freeze::",
    "exec::execution",
    "exec::resource_allocation",
    "exec::metal_backend",
    "bindings",
    "load::graph_loader",
    "runtime::",
]

# manifold-gpu core, shared WGSL, proof harness: runtime set + lighting proofs,
# plus the generated water mesher, whose Metal compile is the shader compiler's
# known hard case (constant struct arrays, BUG-jro0j).
BROAD_FILTERS = RUNTIME_FILTERS + ["render_scene_lights", "volume_surface_mesh::gpu_tests::mesh_contact_"]

# Proofs over this duration require a reviewed measurement before landing.
# The measurements live in scripts/gpu_test_times.json, written by
# `gpu_proofs_gate.py --all --record-times PATH` (nightly trunk_health does this
# into /tmp). Successful gate-driven runs retain measurements in the Git common
# directory, shared by slots, for information only; duration never drops coverage.
SLOW_THRESHOLD_S = 60
TIMES_PATH = Path(__file__).resolve().parent / "gpu_test_times.json"
# The glTF sweep has its own unbudgeted run (glb_conformance). Its measured time
# only sizes the hang watchdog's allowance; it never makes the sweep "slow".
GLB_TESTS = frozenset({"glb_conformance_sweep"})


# A shader included by more primitives than this is "shared WGSL" -> BROAD.
SHARED_WGSL_USERS = 12

# Reporters print timings and assert nothing about behaviour, so they prove no
# change; they run when their own file is touched (the skip drops out then, see
# Plan.final_skips) and nightly under --all. Filters name the test fn.
REPORTER_SKIPS = [
    "matter_cost_probe",
    "matter_solver_perf",
    "gpu_flip_frame_perf",
    "gpu_flip_cost_probe",
    "gpu_flip_speed_measure",
]

# Liquid paths whose change is narrower than the whole solver: the tick clock,
# the scene force fields and the domain nodes feed forces and pacing, not the
# body, step or pressure kernels. They get the force/clock proofs only, never
# the body engine side-by-side or the sparse-vs-dense solver proofs. Body, step
# and pressure paths stay on the broad `gpu_flip_` row below.
# Filters, not skips: a skip is global and would hide body proofs that another
# touched path selected.
# The live frame-rate proof owns pacing too; its duration never removes it.
LIQUID_FORCE_FILTERS = [
    "liquid_conformance::liquid_coupled_live_frame_rate",
    "liquid_conformance::liquid_coupled_world_steps",
    "liquid_conformance::liquid_free_flight",
    "liquid_conformance::liquid_pause_",
    "liquid_conformance::liquid_export_",
    "liquid_conformance::liquid_nonfinite",
    "liquid_conformance::liquid_overflow",
    "liquid_conformance::liquid_live_frames",
    "liquid_conformance::liquid_half_speed",
    "liquid_conformance::liquid_reset",
    "gpu_flip_face_gravity",
]
LIQUID_DOMAIN_FILTERS = LIQUID_FORCE_FILTERS + [
    "gpu_flip_domain_",
    "gpu_flip_preset::",
    "gpu_flip_resolution_card",
    "gpu_flip_still_pool",
    "gpu_flip_free_fall",
]
MATTER_DOMAIN_FILTERS = ["matter_scene::", "matter_coupling::", "matter_look::",
                         "matter_transfer::", "substeps_"]

# Narrow rows win over EXPLICIT_ROWS: a path matching any gets only the narrow
# rows it matches.
NARROW_ROWS = [
    ((WATER_SRC + "primitives/gpu_flip_extension_tests.rs",),
     (["gpu_flip_step_order_", "gpu_flip_extend_faces_"], [])),
    ((WATER_SRC + "liquid/lattice.rs",
      WATER_SRC + "primitives/liquid_frame.rs",
      WATER_SRC + "primitives/liquid_solid_distance.rs",
      WATER_SRC + "primitives/particle_volume.rs",
      WATER_SRC + "primitives/shaders/particle_volume_body.wgsl",
      WATER_SRC + "primitives/shaders/liquid_solid_distance_body.wgsl"),
     (["fluid_mesh_grid_native_", "liquid_frame::gpu_tests::",
       "gpu_flip_narrow_band_mesher_values",
       "mesh_contact_oblique_wall_and_thin_plate_match_cpu_reference",
       "fluid_clamp_scheduled_boundary_renders_like_unfrozen"], [])),
    ((WATER_SRC + "primitives/particle_identity",
      WATER_SRC + "primitives/particle_publication",
      RENDERER_SRC + "node_graph/primitives/particle_frame_blend_tests",
      "crates/manifold-nodes-image/src/node_graph/primitives/interpolate_particle_frames",
      WATER_SRC + "primitives/push_out_of_solid",
      "crates/manifold-nodes-image/src/node_graph/primitives/mix_arrays",
      WATER_SRC + "primitives/liquid_frame",
      WATER_SRC + "liquid/frame_ring",
      WATER_SRC + "liquid/frame_history",
      WATER_SRC + "primitives/shaders/particle_identity",
      WATER_SRC + "primitives/shaders/particle_publication",
      "crates/manifold-nodes-image/src/node_graph/primitives/shaders/interpolate_particle_frames",
      WATER_SRC + "primitives/shaders/push_out_of_solid",
      "crates/manifold-nodes-image/src/node_graph/primitives/shaders/mix_arrays",
      WATER_SRC + "primitives/shaders/liquid_frame_faces.wgsl"),
     (["particle_publication_gpu_tests::", "particle_frame_blend_tests::gpu_tests::",
       "interpolate_particle_frames::gpu_tests::", "push_out_of_solid::gpu_tests::",
       "mix_arrays::gpu_tests::", "gpu_flip_inflow_emits_at_empty_sites_into_free_slots",
       "gpu_flip_narrow_band_publication_repeats_failed_ticks",
       "liquid_frame::gpu_tests::"], [])),
    # The sheeting stage; its step wiring is proven by gpu_flip_step's own filters.
    ((WATER_SRC + "primitives/gpu_flip_sheeting",
      WATER_SRC + "primitives/shaders/gpu_flip_sheeting.wgsl"),
     (["gpu_flip_sheeting_tests::"], [])),
    ((WATER_SRC + "primitives/gpu_flip_clock.rs",
      WATER_SRC + "primitives/shaders/gpu_flip_clock.wgsl"),
     (["gpu_flip_clock::gpu_tests::"], [])),
    ((WATER_SRC + "primitives/emission_count.rs",
      WATER_SRC + "primitives/spawn_whitewater.rs",
      WATER_SRC + "primitives/shaders/emission_count_body.wgsl",
      WATER_SRC + "primitives/shaders/spawn_whitewater_body.wgsl"),
     (["whitewater_particle_tests::"], [])),
    ((WATER_SRC + "primitives/gpu_flip_narrow_band_tests.rs",
      WATER_SRC + "primitives/gpu_flip_narrow_band.rs",
      WATER_SRC + "primitives/shaders/gpu_flip_narrow_band.wgsl"),
     (["narrow_band", "face_grid_demo_gpu_flip_and_matter_side_by_side"], [])),
    ((WATER_SRC + "liquid/clock.rs",
      WATER_SRC + "liquid/fields.rs",
      WATER_SRC + "liquid/fields/"),
     (LIQUID_FORCE_FILTERS, REPORTER_SKIPS)),
    ((WATER_SRC + "primitives/gpu_flip_domain.rs",),
     (LIQUID_DOMAIN_FILTERS + ["fluid_mesh_grid_native_"], REPORTER_SKIPS)),
    ((WATER_SRC + "primitives/matter_domain.rs",),
     (MATTER_DOMAIN_FILTERS, REPORTER_SKIPS)),
]

# Explicit rows: (path substrings, (filters, reporter-only skips)).
EXPLICIT_ROWS = [
    # The readback helper now serves scene and retained catalog proofs.
    (("crates/manifold-nodes-scene/src/testkit/gpu_harness.rs",),
     (["rt_t2b_temporal_wiring::", "rt_bug318_import_toggle::",
       "rt_bugmajv_kernel_toggle::"], [])),

    # Imported graph tails need image registrations, so these proofs stay catalog-side.
    (("crates/manifold-nodes-scene/src/node_graph/gltf_import/",
      "crates/manifold-nodes-scene/src/node_graph/gltf_load.rs",
      "crates/manifold-nodes-scene/src/node_graph/primitives/render_scene"),
     (["render_scene_material_upgrade::", "rt_bug318_import_toggle::",
       "rt_bug326_fix_gate::", "rt_bugmajv_kernel_toggle::",
       "rt_normal_tangent_mirror::", "rt_r3_heldout_gltf::"], [])),

    # Blob bounds controls the sparse reach and dense particle field together.
    ((WATER_SRC + "primitives/blob_bounds.rs",
      WATER_SRC + "primitives/shaders/blob_bounds.wgsl"),
     (["primitives::blob_bounds::",
       "liquid_surface_tests::", "liquid_bricks::tests::gpu_tests::"], [])),
    ((WATER_SRC + "primitives/offset_lattice",
      WATER_SRC + "primitives/redistance_lattice",
      WATER_SRC + "primitives/lattice_closing",
      WATER_SRC + "primitives/shaders/offset_lattice",
      WATER_SRC + "primitives/shaders/redistance_lattice"),
     (["fluid_fill_pits"], [])),
    (("crates/manifold-gpu/src/metal/raytrace.rs",
      "crates/manifold-nodes-scene/src/node_graph/primitives/render_scene.rs",
      "crates/manifold-nodes-scene/src/node_graph/primitives/shaders/render_scene.wgsl",
      PROOFS_DIR + "rt_"),
     (["rt_"], [])),
    ((ENGINE_SRC + "freeze/",), (["freeze::"], [])),
    # Live Matter (GPU_MPM_SOLVER_DESIGN.md) and the substep regions it runs in.
    ((WATER_SRC + "matter.rs",
      WATER_SRC + "matter/",
      ENGINE_SRC + "exec/substeps.rs",
      ENGINE_SRC + "exec/execution/substep_region.rs",
      WATER_SRC + "primitives/matter_",
      RENDERER_SRC + "node_graph/primitives/grid_to_matter",
      "crates/manifold-nodes-image/src/node_graph/primitives/zero_array",
      WATER_SRC + "primitives/shaders/matter_",
      WATER_SRC + "primitives/shaders/grid_to_matter",
      "crates/manifold-nodes-image/src/node_graph/primitives/shaders/zero_array",
      PROOFS_DIR + "matter_",
      PROOFS_DIR + "substeps"),
     (["matter_", "substeps_"], REPORTER_SKIPS)),
    # GPU FLIP water (GPU_FLIP_PRESSURE_SOLVE.md): the step's proofs are scene
    # proofs in other files (still pool, free fall, whitewater, resize), so a
    # module filter alone would miss them.
    ((WATER_SRC + "liquid/",
      WATER_SRC + "primitives/gpu_flip_",
      WATER_SRC + "primitives/liquid_state",
      WATER_SRC + "primitives/liquid_fill",
      WATER_SRC + "primitives/face_sample_component",
      WATER_SRC + "primitives/shaders/gpu_flip_",
      WATER_SRC + "primitives/shaders/liquid_fill",
      WATER_SRC + "primitives/shaders/face_sample_component"),
     (["gpu_flip_", "face_grid_tests::"], REPORTER_SKIPS)),
    # The GPU FLIP step runs its sort, scans and coarse inverse gated on the
    # clock's slot plan; only the inactive-slot proof runs them gated.
    ((WATER_SRC + "primitives/sort_particles_into_cells",
      WATER_SRC + "primitives/prefix_scan",
      WATER_SRC + "primitives/shaders/sort_particles_into_cells",
      WATER_SRC + "primitives/shaders/prefix_scan",
      WATER_SRC + "primitives/shaders/coarse_inverse"),
     (["gpu_flip_inactive_slots_match_the_ungated_step"], [])),
    # The counting sort word for word against its CPU oracle, and the proofs
    # that drive the sorter directly: the node, and the step's crowding cap.
    ((WATER_SRC + "primitives/sort_particles_into_cells",
      WATER_SRC + "primitives/prefix_scan",
      WATER_SRC + "primitives/shaders/sort_particles_into_cells",
      WATER_SRC + "primitives/shaders/prefix_scan"),
     (["sort_particles_into_cells::gpu_tests::", "fluid_sort_particles_into_cells_",
       "gpu_flip_step_order_cell_cap_compacts_preserving_ids"], [])),
    # Shared marching-cubes topology: ownership, solid-contact CPU value parity
    # (volume_surface_mesh::gpu_tests::mesh_contact_*), and raster parity.
    ((WATER_SRC + "primitives/count_surface_edges",
      WATER_SRC + "primitives/volume_surface_mesh",
      WATER_SRC + "primitives/relax_surface_mesh",
      "crates/manifold-nodes-water/src/primitives/smooth_surface_mesh",
      "crates/manifold-nodes-water/src/primitives/surface_mesh_normals",
      WATER_SRC + "primitives/surface_mesh_parity",
      RENDERER_SRC + "node_graph/primitives/surface_mesh_freeze_tests",
      WATER_SRC + "primitives/shaders/count_surface_edges",
      WATER_SRC + "primitives/shaders/surface_edge_",
      WATER_SRC + "primitives/shaders/volume_surface_mesh",
      WATER_SRC + "primitives/shaders/relax_surface_mesh",
      WATER_SRC + "primitives/shaders/surface_mesh_",
      PROOFS_DIR + "liquid_indexed.rs"),
     (["count_surface_edges::gpu_tests::", "volume_surface_mesh::gpu_tests::", "surface_mesh_normals::gpu_tests::", "surface_mesh_freeze_tests::gpu_tests::", "fluid_indexed_", "liquid_indexed::"], [])),
    # Graph runtime.
    ((ENGINE_SRC + "exec/execution",
      ENGINE_SRC + "exec/resource_allocation",
      ENGINE_SRC + "exec/metal_backend",
      ENGINE_SRC + "exec/backend.rs",
      ENGINE_SRC + "exec/bound_graph.rs",
      ENGINE_SRC + "graph.rs",
      ENGINE_SRC + "load/graph_loader.rs",
      ENGINE_SRC + "bindings",
      ENGINE_SRC + "exec/effect_node.rs",
      ENGINE_SRC + "primitive.rs",
      ENGINE_SRC + "gpu/gpu_encoder.rs"),
     (RUNTIME_FILTERS, [])),
]

# Paths whose change affects every proof: BROAD.
BROAD_PATHS = (
    ENGINE_SRC + "lib.rs",
    ENGINE_SRC + "primitives/mod.rs",
    WATER_SRC + "primitives/mod.rs",
    RENDERER_SRC + "node_graph/primitives/mod.rs",
    RENDERER_SRC + "node_graph/mod.rs",
    RENDERER_SRC + "lib.rs",
    "crates/manifold-nodes-scene/src/testkit/gpu_harness.rs",
    PROOFS_DIR + "main.rs",
)

GLTF_PATHS = (
    "crates/manifold-nodes/tests/gpu_proofs/glb_conformance.rs",
    "tests/fixtures/gltf/",
    "crates/manifold-nodes-scene/src/node_graph/gltf_",
    "crates/manifold-nodes-scene/src/node_graph/primitives/gltf_",
)

DOC_SUFFIXES = (".md", ".txt")

# Renderer files outside node_graph/ whose lib tests include GPU proofs.
# preset_runtime/ drives every graph; layer_skin.rs's end-to-end proofs live
# in preset_runtime's tests, so its row names both modules.
PRESET_RUNTIME_DIR = ENGINE_SRC + "runtime/"
LIB_PROOF_ROWS = {
    # Retained legacy shader; the owning primitive holds its proof coverage.
    "crates/manifold-nodes-image/src/node_graph/primitives/shaders/heightfield_shadow.wgsl": [
        "node_graph::primitives::heightfield_shadow::",
    ],
    ENGINE_SRC + "runtime/layer_skin.rs": ["runtime::layer_skin::", "runtime::layer_skin_tests::"],
    # The whitewater step's proofs (across frames, the pool passes, the
    # handoff, the golden fingerprints that prove its output unchanged) live
    # in sibling `_tests` modules the path filter alone misses.
    WATER_SRC + "primitives/whitewater_step.rs": [
        "primitives::whitewater_step::",
        "contracts::water::primitives::whitewater_step::",
        "primitives::whitewater_step_tests::",
        "primitives::whitewater_pool_tests::",
        "primitives::whitewater_handoff_tests::",
        "primitives::whitewater_engine_gpu_tests::",
        "contracts::water::primitives::whitewater_golden_tests::",
    ],
}

# The solver adapter publishes the accepted schedule and MAC history.
# The broad gpu_flip_ row above still supplies its existing solver proofs.
LIB_PROOF_ROWS[WATER_SRC + "primitives/gpu_flip_step.rs"] = [
    "primitives::gpu_flip_step::",
    "primitives::whitewater_engine_gpu_tests::",
]

# BUG-imy3.1: per-element emitters share CPU-reference and fused value proofs.
for _whitewater_atom in (
    "turbulence_field", "inside_turbulence_potential", "turbulence_emission_count",
    "whitewater_emitter_velocity", "whitewater_obstacle_source", "whitewater_influence",
    "dust_potential", "whitewater_emitter_dispatch", "whitewater_emitter_cpu",
    "whitewater_emitter_gpu_tests",
):
    LIB_PROOF_ROWS[WATER_SRC + f"primitives/{_whitewater_atom}.rs"] = [
        "primitives::whitewater_emitter_gpu_tests::",
        "primitives::whitewater_step_tests::",
    ]
del _whitewater_atom

# BUG-g75v.7: engine distance, accepted MAC history, force and drain proofs.
for _engine_path in (
    "primitives/upwind_distance.rs", "primitives/whitewater_distance.rs",
    "primitives/whitewater_engine_cpu.rs", "primitives/whitewater_engine_gpu_tests.rs",
    "primitives/advect_whitewater.rs", "primitives/keep_whitewater.rs",
    "liquid/substep_history.rs",
):
    LIB_PROOF_ROWS[WATER_SRC + _engine_path] = [
        "primitives::whitewater_engine_gpu_tests::",
        "primitives::whitewater_pool_tests::",
        "primitives::whitewater_step_tests::",
    ]
del _engine_path

LIB_PROOF_ROWS[RENDERER_SRC + "testkit/reference_fixtures.rs"] = CPU_FLIP_REFERENCE_FILTERS
for _pressure_fixture in ("dambreak_pressure_problems.bin.zst", "deep_pool_pressure_problems.bin.zst",
                          "deep_pool_density_problems.bin.zst", "gpu_flip_pressure_golden.txt"):
    LIB_PROOF_ROWS["crates/manifold-nodes-water/tests/fixtures/" + _pressure_fixture] = [
        "primitives::gpu_flip_pressure_tests::",
    ]


def godfile_paths():
    """Read the source-of-truth CEILINGS table; reject unparsed entries."""
    source = Path(__file__).resolve().parent.parent / "crates/manifold-app/tests/godfile_regrowth.rs"
    text = re.sub(r"//[^\n]*", "", source.read_text())
    table = re.search(r"const CEILINGS\b[^=]*=\s*&\[(.*?)\];", text, re.S)
    entry = re.compile(r'\(\s*"([^"]+)"\s*,\s*\d[\d_]*\s*,?\s*\)\s*,?')
    if table is None or not entry.search(table[1]) or entry.sub("", table[1]).strip():
        raise ValueError(f"cannot parse CEILINGS in {source}")
    return entry.findall(table[1])


# Cross-file contracts: path -> (owning package, integration binaries).
INTEGRATION_ROWS = {
    "Cargo.toml": ("manifold-app", ["crate_layering"]),
    "crates/manifold-nodes/tests/contracts/primitive_registry.rs": ("manifold-nodes", ["main"]),
    "crates/manifold-nodes-image/src/node_graph/primitives/mod.rs": ("manifold-nodes", ["main"]),
    "crates/manifold-nodes-scene/src/node_graph/primitives/mod.rs": ("manifold-nodes", ["main"]),
    "crates/manifold-nodes-water/src/fluid.rs": ("manifold-nodes", ["gpu_proofs"]),
}


def integration_rows():
    # Parse during readiness, not module import, so other tooling errors can
    # still be reported when the ownership inventory itself is broken.
    return {**INTEGRATION_ROWS,
            **{path: ('manifold-app', ['godfile_regrowth']) for path in godfile_paths()}}
# Contracts over every file under a prefix, Rust or not:
# (prefix, suffix, package, test modules, integration binaries).
PREFIX_ROWS = [
    ("crates/", "/Cargo.toml", "manifold-app", [], ["crate_layering"]),
    (WATER_SRC + "primitives/euler_step_particles.rs", ".rs", CATALOG_PACKAGE,
     ["contracts::particle_pipeline_integration"], []),
    # Scene-panel manifest rows are guarded by the existing INV-8 integration
    # test; keep it in the scoped CPU plan for every panel change.
    ("crates/manifold-ui/src/panels/", ".rs", "manifold-ui", [],
     ["no_bespoke_row_infra"]),
    # Bundled preset JSON is compiled into the renderer.
    ("crates/manifold-nodes/assets/", ".json", "manifold-nodes",
     ["bundled_presets"], []),
    ("crates/manifold-node-engine/src/", ".wgsl", "manifold-nodes",
     ["uniform_layout_extended", "wgsl_validation"], []),
    # wgsl_validation parses every shader in the crate.
    ("crates/manifold-nodes/src/", ".wgsl", "manifold-nodes", ["wgsl_validation"], []),
]

# Both catalog-side layout proofs cover primitive Rust mirrors and WGSL.
PREFIX_ROWS += [(prefix, suffix, CATALOG_PACKAGE,
                 ["uniform_layout_proof", "uniform_layout_extended"], [])
                for prefix in PRIMITIVE_PATHS for suffix in (".rs", ".wgsl")]

# manifold-nodes-image owns these source trees; ABI and WGSL contracts stay catalog-side.
PREFIX_ROWS += [
    ('crates/manifold-nodes-image/src/', '.wgsl', "manifold-nodes", ['uniform_layout_extended', 'wgsl_validation'], []),
]

# manifold-nodes-scene owns these source trees; ABI and WGSL contracts stay catalog-side.
PREFIX_ROWS += [
    ('crates/manifold-nodes-scene/src/', '.wgsl', "manifold-nodes", ['uniform_layout_extended', 'wgsl_validation'], []),
]

PREFIX_ROWS += [(WATER_SRC, ".wgsl", "manifold-nodes", ["uniform_layout_extended", "wgsl_validation"], [])]

PREFIX_ROWS += [('crates/manifold-compositor/src/', ".wgsl", "manifold-nodes", ["wgsl_validation"], [])]

# P2 extractions retain catalog ownership when their production source changes.
# (source prefix, catalog module, has default-config CPU tests).
CATALOG_TEST_ROWS = [
    ("crates/manifold-compositor/src/layer_compositor", "layer_compositor", True),
    ("crates/manifold-compositor/src/preset_thumbnail", "preset_thumbnail", True),
    ("crates/manifold-compositor/src/generator_renderer", "generator_renderer_tests", False),
    ("crates/manifold-compositor/src/generator_renderer", "generator_renderer_warmup_tests", False),
    ("crates/manifold-nodes-scene/src/node_graph/scene_modifier_legacy_migration/loop_upgrade", "loop_upgrade", True),
    ("crates/manifold-nodes-scene/src/node_graph/gltf_import/", "gltf_import", True),
    ("crates/manifold-nodes-scene/src/node_graph/gltf_import/", "gltf_card_precedence", True),
    ("crates/manifold-nodes-scene/src/node_graph/gltf_import/", "gltf_upgrade", True),
    ("crates/manifold-nodes-scene/src/node_graph/gltf_import/", "gltf_upgrade_project", True),
    ("crates/manifold-nodes-scene/src/node_graph/gltf_load", "gltf_import", True),
    ("crates/manifold-nodes-scene/src/node_graph/relight", "relight", True),
    ("crates/manifold-nodes-scene/src/node_graph/scene_vm", "scene_vm", True),
    ("crates/manifold-nodes-scene/src/node_graph/scene_exposure", "scene_exposure", True),
    ("crates/manifold-nodes-scene/src/node_graph/scene_exposure", "fluid_objects", True),
    ("crates/manifold-nodes-scene/src/node_graph/primitives/gltf_animation_source", "gltf_animation_source", True),
    ("crates/manifold-nodes-water/src/primitives/surface_mesh_normals", "surface_mesh_normals", True),
    ("crates/manifold-nodes-scene/src/node_graph/primitives/copy_positions", "copy_positions", False),
    ("crates/manifold-nodes-image/src/node_graph/primitives/wave_field_3d", "copy_positions", False),
    ("crates/manifold-nodes-scene/src/node_graph/primitives/nested_cubes_geometry", "nested_cubes_geometry", False),
    ("crates/manifold-nodes-scene/src/node_graph/primitives/lerp_instance_fields", "image_fused", True),
    ("crates/manifold-nodes-image/src/node_graph/primitives/neighbor_smooth", "image_fused", True),
    ("crates/manifold-nodes-image/src/node_graph/primitives/bokeh_gather", "bokeh_gather", False),
    ("crates/manifold-nodes-image/src/node_graph/primitives/seed_particles_from_texture", "seed_particles_from_texture", True),
    ("crates/manifold-nodes-water/src/primitives/blob_bounds", "blob_bounds", True),
    ("crates/manifold-nodes-water/src/primitives/face_grid_", "face_grid_scene_tests", False),
    ("crates/manifold-nodes-image/src/node_graph/primitives/interpolate_particle_frames", "particle_frame_blend_tests", True),
    ("crates/manifold-nodes-scene/src/node_graph/primitives/particles_to_copies", "particle_frame_blend_tests", True),
    ("crates/manifold-nodes-water/src/primitives/push_out_of_solid", "particle_frame_blend_tests", True),
    ("crates/manifold-nodes-water/src/primitives/particle_publication", "particle_publication_gpu_tests", False),
]
APP_CATALOG_MODULES = {"layer_compositor", "preset_thumbnail", "generator_renderer_tests",
                       "generator_renderer_warmup_tests", "fluid_objects", "bokeh_gather"}
PREFIX_ROWS += [(prefix, ".rs", "manifold-app" if module in APP_CATALOG_MODULES else CATALOG_PACKAGE,
                 ["contracts::node_graph::catalog_tests::" + module] if module in APP_CATALOG_MODULES
                 else ["contracts::node_graph::catalog_tests::" + module], [])
                for prefix, module, cpu in CATALOG_TEST_ROWS if cpu]
