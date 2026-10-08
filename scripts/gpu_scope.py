#!/usr/bin/env python3
"""GPU-proofs scope selection: touched paths -> the focused set of GPU tests.

Single source for scripts/gpu_proofs_gate.py (default mode, dev and landing),
scripts/landing_gate.py and scripts/codex_checks.py. Rules:

- Every touched GPU path maps to test filters, plus the fixed SMOKE set.
- A path maps to the proofs of the thing it changes: NARROW_ROWS (clock, fields,
  domain nodes) beat the broad solver rows, and timing reporters (REPORTER_SKIPS)
  run only when their own file is touched or nightly.
- A GPU path with no mapping is a hard failure naming the path; the author adds
  a rule here. There is no run-everything fallback. Everything runs only with
  `gpu_proofs_gate.py --all` (nightly trunk_health.py).
- Scoped runs defer tests measured over SLOW_THRESHOLD_S, except exact-name
  selections (including changed test bodies); there is no hand-kept list.
- glb_conformance (the ~16-minute glTF sample sweep) runs only when glTF import
  paths are touched, and is exempt from the time budget.
- manifold-gpu core, shared WGSL and the proof harness map to BROAD, a bounded
  set (named below), never to everything.

Filters are libtest substring filters applied to the renderer lib binary
(primitive `gpu_tests`, freeze, graph runtime) and the `gpu_proofs` binary.

Obsolete when: the GPU test suite is fast enough to run whole at every landing.
"""

import json
import math
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

RENDERER_SRC = "crates/manifold-renderer/src/"
ENGINE_SRC = "crates/manifold-node-engine/src/"
CONTRACT_TESTS_DIR = RENDERER_SRC + "engine_contract_tests/"
UI_PAINT_DIR = "crates/manifold-ui-paint/"
UI_PAINT_FILTERS = ["clip_content_gpu::tests::gpu::", "ui_renderer::tests::"]
PROOFS_DIR = "crates/manifold-renderer/tests/gpu_proofs/"
CPU_FLIP_FIXTURES_DIR = "crates/manifold-renderer/tests/fixtures/cpu-flip/"
CPU_FLIP_REFERENCE_FILTERS = [
    "liquid_conformance::",
    "water_basin::",
    "fluid_surface_perf::",
    "node_graph::primitives::whitewater_scene_tests::",
    "node_graph::primitives::gpu_flip_render_smoke_tests::",
    "water::primitives::gpu_flip_preset::",
    "load::expand::acceleration::",
    "water::runtime::physics_carry::",
    "water::runtime::physics_sampling::",
    "water::runtime::physics_impulses::tests::coupled_playback_tests::",
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

# Tests measured slower than this are skipped by scoped runs (nightly --all runs
# them). The measurements live in scripts/gpu_test_times.json, written by
# `gpu_proofs_gate.py --all --record-times PATH` (nightly trunk_health does this
# into /tmp). Successful gate-driven runs retain measurements in the Git common
# directory, shared by slots. Missing tests run once to establish their cost.
SLOW_THRESHOLD_S = 60
TIMES_PATH = Path(__file__).resolve().parent / "gpu_test_times.json"
# The glTF sweep has its own unbudgeted run (glb_conformance). Its measured time
# only sizes the hang watchdog's allowance; it never makes the sweep "slow".
GLB_TESTS = frozenset({"glb_conformance_sweep"})


def learned_times_path():
    repo = TIMES_PATH.parent.parent
    try:
        result = subprocess.run(["git", "-C", str(repo), "rev-parse", "--git-common-dir"],
                                capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError) as error:
        print(f"[WARN] cannot locate shared GPU timings: {error}", file=sys.stderr)
        return None
    if result.returncode:
        return None
    return (repo / result.stdout.strip()).resolve() / "gpu-test-times.json"


def read_times(path):
    """{test name: seconds} from the measured-times file; {} if absent."""
    path = Path(path)
    if not path.exists():
        return {}
    times = json.loads(path.read_text())["tests"]
    if not isinstance(times, dict):
        raise ValueError(f"invalid GPU measurements in {path}")
    times = {n: v["s"] if isinstance(v, dict) else v for n, v in times.items()}
    if not isinstance(times, dict) or any(
            not isinstance(n, str) or not isinstance(s, (int, float))
            or isinstance(s, bool) or not math.isfinite(s) or s < 0
            for n, s in times.items()):
        raise ValueError(f"invalid GPU measurements in {path}")
    return times


def merge_times(*tables):
    merged = {}
    for times in tables:
        merged.update(times)
    return merged


def load_times(path=None):
    if path is not None:
        return read_times(path)
    times = read_times(TIMES_PATH)
    learned = learned_times_path()
    if learned is not None:
        try:
            times = merge_times(times, read_times(learned))
        except (OSError, ValueError, KeyError, TypeError) as error:
            print(f"[WARN] GPU timing cache unreadable; using committed timings: {error}",
                  file=sys.stderr)
    return times


def slow_tests(times=None):
    """[(name, seconds)] measured over SLOW_THRESHOLD_S, slowest first."""
    times = load_times() if times is None else times
    return sorted(((n, s) for n, s in times.items()
                   if s > SLOW_THRESHOLD_S and n not in GLB_TESTS),
                  key=lambda t: -t[1])


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
# Real-clock proofs (liquid_coupled_live_frame_rate, ~4 minutes) are never
# named here: a name selects a slow test past the measured-time deferral.
# They run nightly and when their own body changes.
LIQUID_FORCE_FILTERS = [
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
    ((ENGINE_SRC + "water/primitives/gpu_flip_extension_tests.rs",),
     (["gpu_flip_step_order_", "gpu_flip_extend_faces_"], [])),
    ((ENGINE_SRC + "water/liquid/lattice.rs",
      ENGINE_SRC + "water/primitives/liquid_frame.rs",
      ENGINE_SRC + "water/primitives/liquid_solid_distance.rs",
      ENGINE_SRC + "water/primitives/particle_volume.rs",
      ENGINE_SRC + "water/primitives/shaders/particle_volume_body.wgsl",
      ENGINE_SRC + "water/primitives/shaders/liquid_solid_distance_body.wgsl"),
     (["fluid_mesh_grid_native_", "liquid_frame::gpu_tests::",
       "gpu_flip_narrow_band_mesher_values",
       "mesh_contact_oblique_wall_and_thin_plate_match_cpu_reference",
       "fluid_clamp_scheduled_boundary_renders_like_unfrozen"], [])),
    ((ENGINE_SRC + "water/primitives/particle_identity",
      ENGINE_SRC + "water/primitives/particle_publication",
      RENDERER_SRC + "node_graph/primitives/particle_frame_blend_tests",
      RENDERER_SRC + "node_graph/primitives/interpolate_particle_frames",
      ENGINE_SRC + "water/primitives/push_out_of_solid",
      RENDERER_SRC + "node_graph/primitives/mix_arrays",
      ENGINE_SRC + "water/primitives/liquid_frame",
      ENGINE_SRC + "water/liquid/frame_ring",
      ENGINE_SRC + "water/liquid/frame_history",
      ENGINE_SRC + "water/primitives/shaders/particle_identity",
      ENGINE_SRC + "water/primitives/shaders/particle_publication",
      RENDERER_SRC + "node_graph/primitives/shaders/interpolate_particle_frames",
      ENGINE_SRC + "water/primitives/shaders/push_out_of_solid",
      RENDERER_SRC + "node_graph/primitives/shaders/mix_arrays",
      ENGINE_SRC + "water/primitives/shaders/liquid_frame_faces.wgsl"),
     (["particle_publication_gpu_tests::", "particle_frame_blend_tests::gpu_tests::",
       "interpolate_particle_frames::gpu_tests::", "push_out_of_solid::gpu_tests::",
       "mix_arrays::gpu_tests::", "gpu_flip_inflow_emits_at_empty_sites_into_free_slots",
       "gpu_flip_narrow_band_publication_repeats_failed_ticks",
       "liquid_frame::gpu_tests::"], [])),
    # The sheeting stage; its step wiring is proven by gpu_flip_step's own filters.
    ((ENGINE_SRC + "water/primitives/gpu_flip_sheeting",
      ENGINE_SRC + "water/primitives/shaders/gpu_flip_sheeting.wgsl"),
     (["gpu_flip_sheeting_tests::"], [])),
    ((ENGINE_SRC + "water/primitives/gpu_flip_clock.rs",
      ENGINE_SRC + "water/primitives/shaders/gpu_flip_clock.wgsl"),
     (["gpu_flip_clock::gpu_tests::"], [])),
    ((ENGINE_SRC + "water/primitives/emission_count.rs",
      ENGINE_SRC + "water/primitives/spawn_whitewater.rs",
      ENGINE_SRC + "water/primitives/shaders/emission_count_body.wgsl",
      ENGINE_SRC + "water/primitives/shaders/spawn_whitewater_body.wgsl"),
     (["whitewater_particle_tests::"], [])),
    ((ENGINE_SRC + "water/primitives/gpu_flip_narrow_band_tests.rs",
      ENGINE_SRC + "water/primitives/gpu_flip_narrow_band.rs",
      ENGINE_SRC + "water/primitives/shaders/gpu_flip_narrow_band.wgsl"),
     (["narrow_band", "face_grid_demo_gpu_flip_and_matter_side_by_side"], [])),
    ((ENGINE_SRC + "water/liquid/clock.rs",
      ENGINE_SRC + "water/liquid/fields.rs",
      ENGINE_SRC + "water/liquid/fields/"),
     (LIQUID_FORCE_FILTERS, REPORTER_SKIPS)),
    ((ENGINE_SRC + "water/primitives/gpu_flip_domain.rs",),
     (LIQUID_DOMAIN_FILTERS + ["fluid_mesh_grid_native_"], REPORTER_SKIPS)),
    ((ENGINE_SRC + "water/primitives/matter_domain.rs",),
     (MATTER_DOMAIN_FILTERS, REPORTER_SKIPS)),
]

# Explicit rows: (path substrings, (filters, skips)). `rt_` skips particletext:
# the freeze proof `particletext_*` hangs the GPU on main (BUG-i6eo).
EXPLICIT_ROWS = [
    # Blob bounds controls the sparse reach and dense particle field together.
    ((RENDERER_SRC + "node_graph/primitives/blob_bounds.rs",
      RENDERER_SRC + "node_graph/primitives/shaders/blob_bounds.wgsl"),
     (["node_graph::primitives::blob_bounds::",
       "liquid_surface_tests::", "liquid_bricks::tests::gpu_tests::"], [])),
    ((ENGINE_SRC + "water/primitives/offset_lattice",
      ENGINE_SRC + "water/primitives/redistance_lattice",
      ENGINE_SRC + "water/primitives/lattice_closing",
      ENGINE_SRC + "water/primitives/shaders/offset_lattice",
      ENGINE_SRC + "water/primitives/shaders/redistance_lattice"),
     (["fluid_fill_pits"], [])),
    (("crates/manifold-gpu/src/metal/raytrace.rs",
      RENDERER_SRC + "node_graph/primitives/render_scene.rs",
      RENDERER_SRC + "node_graph/primitives/shaders/render_scene.wgsl",
      PROOFS_DIR + "rt_"),
     (["rt_"], ["particletext"])),
    ((ENGINE_SRC + "freeze/",), (["freeze::"], [])),
    # Live Matter (GPU_MPM_SOLVER_DESIGN.md) and the substep regions it runs in.
    ((ENGINE_SRC + "water/matter.rs",
      ENGINE_SRC + "water/matter/",
      ENGINE_SRC + "exec/substeps.rs",
      ENGINE_SRC + "exec/execution/substep_region.rs",
      ENGINE_SRC + "water/primitives/matter_",
      RENDERER_SRC + "node_graph/primitives/grid_to_matter",
      RENDERER_SRC + "node_graph/primitives/zero_array",
      ENGINE_SRC + "water/primitives/shaders/matter_",
      ENGINE_SRC + "water/primitives/shaders/grid_to_matter",
      RENDERER_SRC + "node_graph/primitives/shaders/zero_array",
      PROOFS_DIR + "matter_",
      PROOFS_DIR + "substeps"),
     (["matter_", "substeps_"], REPORTER_SKIPS)),
    # GPU FLIP water (GPU_FLIP_PRESSURE_SOLVE.md): the step's proofs are scene
    # proofs in other files (still pool, free fall, whitewater, resize), so a
    # module filter alone would miss them.
    ((ENGINE_SRC + "water/liquid/",
      ENGINE_SRC + "water/primitives/gpu_flip_",
      ENGINE_SRC + "water/primitives/liquid_state",
      ENGINE_SRC + "water/primitives/liquid_fill",
      ENGINE_SRC + "water/primitives/face_sample_component",
      ENGINE_SRC + "water/primitives/shaders/gpu_flip_",
      ENGINE_SRC + "water/primitives/shaders/liquid_fill",
      ENGINE_SRC + "water/primitives/shaders/face_sample_component"),
     (["gpu_flip_", "face_grid_tests::"], REPORTER_SKIPS)),
    # The GPU FLIP step runs its sort, scans and coarse inverse gated on the
    # clock's slot plan; only the inactive-slot proof runs them gated.
    ((ENGINE_SRC + "water/primitives/sort_particles_into_cells",
      ENGINE_SRC + "water/primitives/prefix_scan",
      ENGINE_SRC + "water/primitives/shaders/sort_particles_into_cells",
      ENGINE_SRC + "water/primitives/shaders/prefix_scan",
      ENGINE_SRC + "water/primitives/shaders/coarse_inverse"),
     (["gpu_flip_inactive_slots_match_the_ungated_step"], [])),
    # The counting sort word for word against its CPU oracle, and the proofs
    # that drive the sorter directly: the node, and the step's crowding cap.
    ((ENGINE_SRC + "water/primitives/sort_particles_into_cells",
      ENGINE_SRC + "water/primitives/prefix_scan",
      ENGINE_SRC + "water/primitives/shaders/sort_particles_into_cells",
      ENGINE_SRC + "water/primitives/shaders/prefix_scan"),
     (["sort_particles_into_cells::gpu_tests::", "fluid_sort_particles_into_cells_",
       "gpu_flip_step_order_cell_cap_compacts_preserving_ids"], [])),
    # Shared marching-cubes topology: ownership, solid-contact CPU value parity
    # (volume_surface_mesh::gpu_tests::mesh_contact_*), and raster parity.
    ((ENGINE_SRC + "water/primitives/count_surface_edges",
      ENGINE_SRC + "water/primitives/volume_surface_mesh",
      ENGINE_SRC + "water/primitives/relax_surface_mesh",
      RENDERER_SRC + "node_graph/primitives/smooth_surface_mesh",
      RENDERER_SRC + "node_graph/primitives/surface_mesh_normals",
      ENGINE_SRC + "water/primitives/surface_mesh_parity",
      RENDERER_SRC + "node_graph/primitives/surface_mesh_freeze_tests",
      ENGINE_SRC + "water/primitives/shaders/count_surface_edges",
      ENGINE_SRC + "water/primitives/shaders/surface_edge_",
      ENGINE_SRC + "water/primitives/shaders/volume_surface_mesh",
      ENGINE_SRC + "water/primitives/shaders/relax_surface_mesh",
      ENGINE_SRC + "water/primitives/shaders/surface_mesh_",
      RENDERER_SRC + "node_graph/primitives/shaders/surface_mesh_",
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
    ENGINE_SRC + "water/primitives/mod.rs",
    RENDERER_SRC + "node_graph/primitives/mod.rs",
    RENDERER_SRC + "node_graph/mod.rs",
    RENDERER_SRC + "lib.rs",
    PROOFS_DIR + "harness.rs",
    PROOFS_DIR + "main.rs",
)

GLTF_PATHS = (
    "crates/manifold-renderer/tests/glb_conformance.rs",
    "tests/fixtures/gltf/",
    RENDERER_SRC + "node_graph/gltf_",
    RENDERER_SRC + "node_graph/primitives/gltf_",
)

DOC_SUFFIXES = (".md", ".txt")

# Renderer files outside node_graph/ whose lib tests include GPU proofs.
# preset_runtime/ drives every graph; layer_skin.rs's end-to-end proofs live
# in preset_runtime's tests, so its row names both modules.
PRESET_RUNTIME_DIR = ENGINE_SRC + "runtime/"
LIB_PROOF_ROWS = {
    ENGINE_SRC + "runtime/layer_skin.rs": ["runtime::layer_skin::", "runtime::layer_skin_tests::"],
    # The whitewater step's proofs (across frames, the pool passes, the
    # handoff, the golden fingerprints that prove its output unchanged) live
    # in sibling `_tests` modules the path filter alone misses.
    ENGINE_SRC + "water/primitives/whitewater_step.rs": [
        "water::primitives::whitewater_step::",
        "water::primitives::whitewater_step_tests::",
        "water::primitives::whitewater_pool_tests::",
        "water::primitives::whitewater_handoff_tests::",
        "water::primitives::whitewater_engine_gpu_tests::",
        "water::primitives::whitewater_golden_tests::",
    ],
}

# The solver adapter publishes the accepted schedule and MAC history.
# The broad gpu_flip_ row above still supplies its existing solver proofs.
LIB_PROOF_ROWS[ENGINE_SRC + "water/primitives/gpu_flip_step.rs"] = [
    "water::primitives::gpu_flip_step::",
    "water::primitives::whitewater_engine_gpu_tests::",
]

# BUG-imy3.1: per-element emitters share CPU-reference and fused value proofs.
for _whitewater_atom in (
    "turbulence_field", "inside_turbulence_potential", "turbulence_emission_count",
    "whitewater_emitter_velocity", "whitewater_obstacle_source", "whitewater_influence",
    "dust_potential", "whitewater_emitter_dispatch", "whitewater_emitter_cpu",
    "whitewater_emitter_gpu_tests",
):
    LIB_PROOF_ROWS[ENGINE_SRC + f"water/primitives/{_whitewater_atom}.rs"] = [
        "water::primitives::whitewater_emitter_gpu_tests::",
        "water::primitives::whitewater_step_tests::",
    ]
del _whitewater_atom

# BUG-g75v.7: engine distance, accepted MAC history, force and drain proofs.
for _engine_path in (
    "primitives/upwind_distance.rs", "primitives/whitewater_distance.rs",
    "primitives/whitewater_engine_cpu.rs", "primitives/whitewater_engine_gpu_tests.rs",
    "primitives/advect_whitewater.rs", "primitives/keep_whitewater.rs",
    "liquid/substep_history.rs",
):
    LIB_PROOF_ROWS[ENGINE_SRC + "water/" + _engine_path] = [
        "water::primitives::whitewater_engine_gpu_tests::",
        "water::primitives::whitewater_pool_tests::",
        "water::primitives::whitewater_step_tests::",
    ]
del _engine_path

LIB_PROOF_ROWS[RENDERER_SRC + "reference_fixtures.rs"] = CPU_FLIP_REFERENCE_FILTERS
for _pressure_fixture in ("dambreak_pressure_problems.bin.zst", "deep_pool_pressure_problems.bin.zst",
                          "deep_pool_density_problems.bin.zst", "gpu_flip_pressure_golden.txt"):
    LIB_PROOF_ROWS["crates/manifold-node-engine/tests/fixtures/" + _pressure_fixture] = [
        "water::primitives::gpu_flip_pressure_tests::",
    ]

PATH_ATTR_MOD = re.compile(r'#\[path\s*=\s*"tests/([\w.]+)"\]\s*mod\s+(\w+)\s*;')


def is_gpu_path(path):
    """Paths that trigger the GPU-proofs leg (mirrors the context-nudge triggers)."""
    if path.endswith(".wgsl"):
        return True
    if path.startswith(("crates/manifold-gpu/", UI_PAINT_DIR, ENGINE_SRC, CONTRACT_TESTS_DIR, RENDERER_SRC + "node_graph/")):
        return True
    if "shaders/" in path or "gpu::gpu_encoder" in path:
        return True
    if path.startswith((PRESET_RUNTIME_DIR, CPU_FLIP_FIXTURES_DIR)) or path in LIB_PROOF_ROWS:
        return True
    return "tests/gpu_proofs/" in path or is_gltf_path(path)


def is_gltf_path(path):
    return any(path.startswith(p) for p in GLTF_PATHS)


@dataclass
class Plan:
    paths: list = field(default_factory=list)       # GPU paths considered
    filters: set = field(default_factory=set)
    skips: set = field(default_factory=set)
    ui_paint: bool = False
    glb: bool = False
    broad: list = field(default_factory=list)        # (path, reason) that mapped to BROAD
    unmapped: list = field(default_factory=list)     # (path, why)
    notes: list = field(default_factory=list)

    @property
    def active(self):
        return bool(self.paths)

    def final_filters(self):
        return sorted(set(SMOKE_FILTERS) | self.filters)

    def final_skips(self):
        # Exact test selections beat reporter and measured-time skips.
        skips = {s for s in self.skips if not any(s in f for f in self.filters)}
        return sorted(skips | {n for n, _ in slow_tests() if n not in self.final_filters()})

    def deferred(self):
        filters = self.final_filters()
        return [(n, s) for n, s in slow_tests()
                if n not in filters and any(f in n for f in filters)]

    def runs(self):
        """[{targets, lib, filters, skips, budgeted}] cargo invocations to make."""
        if not self.active:
            return []
        runs = [{"targets": ["gpu_proofs"], "lib": True, "filters": self.final_filters(),
                 "skips": self.final_skips(), "budgeted": True}]
        runs.append({"package": "manifold-node-engine", "targets": [], "lib": True,
                     "filters": self.final_filters(), "skips": self.final_skips(),
                     "budgeted": True})
        if self.ui_paint:
            runs.append({"package": "manifold-ui-paint", "targets": [], "lib": True,
                         "filters": UI_PAINT_FILTERS, "skips": self.final_skips(),
                         "budgeted": True})
        if self.glb:
            runs.append({"targets": ["glb_conformance"], "lib": False, "filters": [],
                         "skips": [], "budgeted": False})
        return runs

    def describe(self):
        lines = [f"{len(self.paths)} GPU path(s) touched; smoke + mapped filters"]
        lines.append(f"  filters: {', '.join(self.final_filters())}")
        skips = [s for s in self.final_skips()
                 if any(f in s or s in f for f in self.final_filters())]
        if skips:
            lines.append(f"  skips: {', '.join(skips)}")
        if self.deferred():
            lines.append("GPU-PROOFS DEFERRED: " + ", ".join(
                f"{name} ({secs:.0f}s)" for name, secs in self.deferred()))
        if self.ui_paint:
            lines.append("  manifold-ui-paint --lib: " + ", ".join(UI_PAINT_FILTERS))
        if self.broad:
            lines.append("  broad set (runtime + lighting) because: " +
                         "; ".join(f"{p} ({why})" for p, why in self.broad))
        if self.glb:
            lines.append("  glb_conformance: glTF import paths touched (exempt from time budget)")
        return "\n".join(lines)


def module_filters(path):
    """Lib/proof test-path filters for a renderer source file, or None for root."""
    root = ENGINE_SRC if path.startswith(ENGINE_SRC) else RENDERER_SRC
    parts = path[len(root):].split("/")
    name = parts[-1]
    if not name.endswith(".rs"):
        return []
    stem, dirs = name[:-3], parts[:-1]
    if stem in ("mod", "lib", "main"):
        mods = [dirs]
    else:
        mods = [dirs + [stem]]
        for suffix in ("_gpu_tests", "_tests"):
            if stem.endswith(suffix) and len(stem) > len(suffix):
                mods.append(dirs + [stem[: -len(suffix)]])
        if stem in ("tests", "gpu_tests"):
            mods.append(dirs)
    return ["::".join(m) + "::" for m in mods if m]


def contract_module_filters(path, repo):
    """Resolve relocated contracts through their real Rust module mounts."""
    from crate_move_replay import module_items
    repo = Path(repo)
    target = (repo / path).resolve()
    found = set()

    def walk(source, prefix, ancestors):
        source = source.resolve()
        if source in ancestors or not source.is_file():
            return
        if source == target:
            found.add("::".join(prefix) + "::")
            return
        text = source.read_text()
        for start, end, head, scope in module_items(text):
            declaration = re.match(r"(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;", text[head:end])
            if not declaration:
                continue
            name = declaration[1]
            attrs = re.findall(r'#\[path\s*=\s*"([^"\n]+)"\]', text[start:head])
            if attrs:
                child = source.parent.joinpath(*scope, attrs[-1])
            else:
                base = source.parent if source.stem in ("lib", "mod") else source.with_suffix("")
                child = base.joinpath(*scope, name + ".rs")
                if not child.is_file():
                    child = base.joinpath(*scope, name, "mod.rs")
            if "engine_contract_tests" in child.parts:
                walk(child, prefix + scope + (name,), ancestors | {source})

    walk(repo / RENDERER_SRC / "lib.rs", (), set())
    return sorted(found)


def path_attr_filters(path, repo):
    """Filters for a `<dir>/tests/<file>.rs` pulled in by `#[path] mod x;` in `<dir>/mod.rs`.

    The test module is named by that declaration, not by the file path, so the
    path-derived filter would select nothing. Unresolvable preset_runtime test
    files fall back to the whole preset_runtime module rather than to nothing.
    """
    if path.startswith(CONTRACT_TESTS_DIR):
        return contract_module_filters(path, repo)
    root = ENGINE_SRC if path.startswith(ENGINE_SRC) else RENDERER_SRC
    parts = path[len(root):].split("/")
    if len(parts) < 3 or parts[-2] != "tests":
        return None
    dirs = parts[:-2]
    try:
        text = (Path(repo) / root / "/".join(dirs) / "mod.rs").read_text()
    except OSError:
        text = ""
    for file_name, module in PATH_ATTR_MOD.findall(text):
        if file_name == parts[-1]:
            return ["::".join(dirs + [module]) + "::"]
    if path.startswith(PRESET_RUNTIME_DIR):
        return ["runtime::"]
    return None


def default_shader_users(repo, wgsl_path, depth=3):
    """Rust files that (transitively through other .wgsl) include `wgsl_path`."""
    found, frontier, seen = set(), [wgsl_path], {wgsl_path}
    for _ in range(depth):
        nxt = set()
        for current in frontier:
            out = subprocess.run(
                ["rg", "-l", "-F", Path(current).name, "--glob", "*.rs", "--glob", "*.wgsl",
                 str(Path(repo) / "crates")],
                capture_output=True, text=True).stdout
            for line in out.splitlines():
                rel = Path(line).resolve().relative_to(Path(repo).resolve()).as_posix()
                if rel in seen:
                    continue
                seen.add(rel)
                (nxt if rel.endswith(".wgsl") else found).add(rel)
        frontier = nxt
    return sorted(found)


def changed_test_filters(path, repo, base):
    """Promote changed test bodies; shared-helper edits retain module scope."""
    # Only renderer lib and proof paths have a derivable test-name prefix.
    if not path.startswith((RENDERER_SRC, ENGINE_SRC, PROOFS_DIR)):
        return set()
    source = Path(repo) / path
    if source.suffix != ".rs" or not source.exists():
        return set()
    text = source.read_text()
    if "#[test]" not in text:
        return set()
    diff = subprocess.run(["git", "-C", str(repo), "diff", "--no-ext-diff",
                           "--no-textconv", "-U0", "--merge-base", base, "--", path],
                          capture_output=True, text=True)
    if diff.returncode:
        raise RuntimeError(f"cannot scope changed test bodies: {diff.stderr.strip()}")
    hunks = [(int(m[1]), max(1, int(m[2] or 1))) for m in re.finditer(
        r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", diff.stdout, re.M)]
    if path.startswith(PROOFS_DIR):
        parts = list(Path(path[len(PROOFS_DIR):]).with_suffix("").parts)
        prefix = "::".join(parts[:-1] if parts[-1] == "mod" else parts) + "::"
    else:
        prefix = (path_attr_filters(path, repo) or module_filters(path))[0]
    selected = set()
    for match in re.finditer(r"#\[test\]\s*(?:#\[[^\n]+\]\s*)*"
                             r"fn (?P<name>\w+)\([^)]*\)[^{;]*\{", text):
        start = text.count("\n", 0, match.end()) + 1
        # Rustfmt puts a function's closing brace at the fn's indentation.
        fn_line = text.rfind("\n", 0, text.index("fn ", match.start())) + 1
        indent = re.match(r"[ \t]*", text[fn_line:])[0]
        end = re.search(r"^" + indent + r"\}", text[match.end():], re.M)
        stop = start + text[match.end():match.end() + end.end()].count("\n") if end else start
        if not any(row <= stop and row + count - 1 >= start for row, count in hunks):
            continue
        modules = []
        for mod in re.finditer(r"^([ \t]*)mod (\w+) \{", text[:match.start()], re.M):
            close = re.search(r"^" + mod[1] + r"\}", text[mod.end():], re.M)
            if close is None or mod.end() + close.start() > match.start():
                modules.append(mod[2])
        selected.add(prefix + "::".join(modules + [match["name"]]))
    return selected


def plan_for_paths(paths, repo, shader_users=None, base="origin/main"):
    """Map touched `paths` to a Plan. Never returns an implicit 'everything'."""
    shader_users = shader_users or (lambda p: default_shader_users(repo, p))
    plan = Plan()
    for path in sorted(set(paths)):
        if not is_gpu_path(path):
            continue
        plan.paths.append(path)
        if path.startswith(UI_PAINT_DIR):
            plan.ui_paint = True
            continue
        if path.startswith(CONTRACT_TESTS_DIR):
            mounted = contract_module_filters(path, repo)
            if not mounted:
                plan.unmapped.append((path, "contract test has no resolvable Rust module mount"))
                continue
            plan.filters.update(mounted)
        plan.filters.update(changed_test_filters(path, repo, base))
        if is_gltf_path(path):
            plan.glb = True
        # A path that several features own maps to every one of their rows.
        narrow = [(("",), row) for pats, row in NARROW_ROWS
                  if any(pat in path for pat in pats)]
        for patterns, (filters, skips) in (narrow or EXPLICIT_ROWS):
            if any(pat in path for pat in patterns):
                plan.filters.update(filters)
                plan.skips.update(skips)
        if path in BROAD_PATHS:
            plan.filters.update(BROAD_FILTERS)
            plan.broad.append((path, "affects every proof"))
            continue
        if path.startswith("crates/manifold-gpu/"):
            plan.ui_paint = True
            if path.endswith("raytrace.rs") or "/vulkan/" in path:
                continue  # rt row above / Vulkan not built here: smoke only
            plan.filters.update(BROAD_FILTERS)
            plan.broad.append((path, "manifold-gpu core"))
            continue
        if path in LIB_PROOF_ROWS:
            plan.filters.update(LIB_PROOF_ROWS[path])
            continue
        if path.endswith(DOC_SUFFIXES):
            continue
        if path.endswith(".wgsl"):
            _map_wgsl(plan, path, repo, shader_users)
            continue
        if path.startswith(PROOFS_DIR) and path.endswith(".rs"):
            rel = path[len(PROOFS_DIR):].split("/")
            plan.filters.add(rel[0][:-3] + "::" if len(rel) == 1 else rel[0] + "::")
            continue
        if path.startswith(CPU_FLIP_FIXTURES_DIR):
            plan.filters.update(CPU_FLIP_REFERENCE_FILTERS)
            continue
        if path.startswith((RENDERER_SRC, ENGINE_SRC)) and path.endswith(".rs"):
            plan.filters.update(path_attr_filters(path, repo) or module_filters(path))
            continue
        if is_gltf_path(path):
            continue
        if not path.startswith(("crates/manifold-renderer/", "crates/manifold-node-engine/", "crates/manifold-gpu/")):
            plan.notes.append(f"{path}: outside renderer/gpu crates, smoke only")
            continue
        plan.unmapped.append((path, "no GPU test mapping rule for this file type"))
    return plan


def _map_wgsl(plan, path, repo, shader_users):
    if not (Path(repo) / path).exists():
        plan.notes.append(f"{path}: deleted shader, smoke only")
        return
    if path.startswith(("crates/manifold-led/", "crates/manifold-recording/",
                        "crates/manifold-spectral/")):
        plan.notes.append(f"{path}: other crate's shader, smoke only")
        return
    if path.startswith(ENGINE_SRC + "freeze/shaders/"):
        plan.filters.add("freeze::")
        return
    users = shader_users(path)
    if not users:
        plan.unmapped.append((path, "no Rust file includes this shader; cannot find its proofs"))
        return
    if len(users) > SHARED_WGSL_USERS:
        plan.filters.update(BROAD_FILTERS)
        plan.broad.append((path, f"shared WGSL, {len(users)} users"))
        return
    for user in users:
        if user.startswith((RENDERER_SRC, ENGINE_SRC)):
            plan.filters.update(LIB_PROOF_ROWS.get(user, module_filters(user)))
        else:
            plan.notes.append(f"{path}: user {user} outside renderer")


def unmapped_message(plan):
    lines = ["GPU-PROOFS SCOPE: FAIL - touched GPU path(s) with no test mapping:"]
    lines += [f"  - {p}: {why}" for p, why in plan.unmapped]
    lines.append("Add a mapping rule in scripts/gpu_scope.py (EXPLICIT_ROWS or plan_for_paths) "
                 "and a case in scripts/test_gpu_scope.py. There is no run-everything fallback.")
    return "\n".join(lines)


if __name__ == "__main__":
    import sys
    p = plan_for_paths(sys.argv[1:], Path.cwd())
    print(p.describe())
    if p.unmapped:
        print(unmapped_message(p))
