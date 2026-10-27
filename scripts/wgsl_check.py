#!/usr/bin/env python3
"""Validate the hand-composed water shaders with naga in a few seconds.

Each stack is stitched from source files the way the Rust `shader()` /
`step_source()` functions stitch it, then handed to `naga` (install with
`cargo install naga-cli`). No cargo build, no GPU. Run with no args for every
stack, or name stacks: `scripts/wgsl_check.py step sheeting`.

Keep a stack's file list in step with its Rust composition when that changes.
"""

import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
C = ROOT / "crates"
LIQ = C / "manifold-water-liquid/src/primitives/shaders"
FLIP = C / "manifold-water-gpu-flip/src/primitives/shaders"
WW = C / "manifold-water-whitewater/src/primitives/shaders"
SURF = C / "manifold-water-surface/src/primitives/shaders"
SURFACE = C / "manifold-water-surface/src/primitives/shaders"

SCENE = C / "manifold-nodes-scene/src/node_graph/primitives/shaders"

WORLD_READ = C / "manifold-node-engine/src/scene/world_tiles_read.wgsl"
# The read side's includer contract, as a hand shader defines it.
WORLD_READ_STUB = """@group(0) @binding(0) var<storage, read> ts_table: array<u32>;
fn ts_pool_tiles() -> u32 { return 3u; }
fn ts_table_entries() -> u32 { return 16u; }
fn ts_table_word(i: u32) -> u32 { return ts_table[i]; }
"""
# The key sources `sort_particles_into_cells.rs` composes before the sort body
# (DENSE_KEYS, POOLED_KEYS).
SORT_DENSE_KEYS = """fn sort_key_of(p: vec3<f32>) -> u32 {
    let last = vec3<i32>(params.bins) - vec3<i32>(1);
    let b = clamp(vec3<i32>(floor((p - params.bin_min) * params.inv_cell)), vec3<i32>(0), last);
    return u32(b.x) + params.bins.x * (u32(b.y) + params.bins.y * u32(b.z));
}
"""
SORT_POOLED_KEYS = """@group(0) @binding(9) var<storage, read> ts_table: array<u32>;
fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { return params.table_entries; }
fn ts_table_word(i: u32) -> u32 { return ts_table[i]; }
fn sort_key_of(p: vec3<f32>) -> u32 { return ts_cell_at(ts_world_cell(p, params.bin_min, params.inv_cell)); }
"""
# What the codegen wraps around `face_sample_component_body.wgsl`: its uniform,
# its gathered buffers and an entry point calling `body`.
FACE_COMPONENT_STUB = """struct Params {
    axis: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, pool_tiles: f32,
    cell_origin_x: i32, cell_origin_y: i32, cell_origin_z: i32, dispatch_count: u32,
}
struct FaceSample { face_velocity: vec4<f32>, face_weight: vec4<f32> }
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_faces: array<FaceSample>;
@group(0) @binding(2) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(3) var<storage, read_write> buf_out: array<f32>;
"""
FACE_COMPONENT_MAIN = """@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    buf_out[gid.x] = body(gid.x, params.dispatch_count, params.axis, params.nodes_x, params.nodes_y,
        params.nodes_z, params.pool_tiles, params.cell_origin_x, params.cell_origin_y, params.cell_origin_z);
}
"""
# What the codegen wraps around the pooled lattice atoms' bodies
# (`redistance_lattice_body.wgsl`, `smooth_lattice_body.wgsl`,
# `field_smooth_blend_body.wgsl`): the uniform, the gathered buffers and an
# entry point calling `body`.
REDISTANCE_STUB = """struct Params { cell_size: f32, band_cells: f32, pool_tiles: f32, dispatch_count: u32 }
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_levelset: array<f32>;
@group(0) @binding(2) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(3) var<storage, read_write> buf_out: array<f32>;
"""
REDISTANCE_MAIN = """@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    buf_out[gid.x] = body(gid.x, params.dispatch_count, params.cell_size, params.band_cells, params.pool_tiles);
}
"""
SMOOTH_STUB = """struct Params {
    cell_size: f32, passes: f32, axis: i32, pool_tiles: f32,
    dispatch_count: u32, pad0: u32, pad1: u32, pad2: u32,
}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_levelset: array<f32>;
@group(0) @binding(2) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(3) var<storage, read_write> buf_out: array<f32>;
"""
SMOOTH_MAIN = """@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    buf_out[gid.x] = body(gid.x, params.dispatch_count, params.cell_size, params.passes, params.axis, params.pool_tiles);
}
"""
BLEND_STUB = """struct Params {
    cell_size: f32, smoothness: f32, band_cells: f32, pool_tiles: f32,
    dispatch_count: u32, pad0: u32, pad1: u32, pad2: u32,
}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_smoothed: array<f32>;
@group(0) @binding(2) var<storage, read> buf_prior: array<f32>;
@group(0) @binding(3) var<storage, read> buf_weights: array<f32>;
@group(0) @binding(4) var<storage, read> buf_reference: array<f32>;
@group(0) @binding(5) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(6) var<storage, read_write> buf_out: array<f32>;
"""
BLEND_MAIN = """@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let e_smoothed = buf_smoothed[gid.x];
    buf_out[gid.x] = body(gid.x, params.dispatch_count, e_smoothed, params.cell_size, params.smoothness,
        params.band_cells, params.pool_tiles);
}
"""
# What the codegen wraps around `particle_volume_body.wgsl`: the uniform, the
# gathered buffers and an entry point calling `body`.
VOLUME_STUB = """struct Params { cell_size: f32, particle_scale: f32, sim_cell_size: f32, particle_radius: f32, pool_tiles: f32, dispatch_count: u32 }
struct CellRange { start: u32, count: u32 }
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_blobs: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> buf_cell_ranges: array<CellRange>;
@group(0) @binding(3) var<storage, read> buf_solid: array<f32>;
@group(0) @binding(4) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(5) var<storage, read_write> buf_out: array<f32>;
"""
VOLUME_MAIN = """@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    buf_out[gid.x] = body(gid.x, params.dispatch_count, params.cell_size, params.particle_scale, params.sim_cell_size, params.particle_radius, params.pool_tiles);
}
"""

# What the codegen wraps around `push_out_of_solid_body.wgsl`: its uniform, its
# gathered buffers and an entry point calling `body`.
PUSH_OUT_STUB = """struct Element { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32 }
struct Params { cell_size: f32, pool_tiles: f32, dispatch_count: u32, _pad0: u32 }
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_particles: array<Element>;
@group(0) @binding(2) var<storage, read> buf_solid: array<f32>;
@group(0) @binding(3) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(4) var<storage, read_write> buf_out: array<Element>;
"""
PUSH_OUT_MAIN = """@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.dispatch_count { return; }
    let e_particles = buf_particles[idx];
    buf_out[idx] = body(idx, params.dispatch_count, e_particles, params.cell_size, params.pool_tiles);
}
"""
# The same for `liquid_solid_distance_body.wgsl`; its clock plan binds at 6.
SOLID_DISTANCE_STUB = """struct LiquidBody {
    position_inv_mass: vec4<f32>, rotation: vec4<f32>, linear_velocity: vec4<f32>,
    angular_velocity: vec4<f32>, inv_inertia_x: vec4<f32>, inv_inertia_y: vec4<f32>,
    inv_inertia_z: vec4<f32>, accel_shape: vec4<f32>,
}
struct LiquidShape {
    origin_spacing: vec4<f32>, dims_x: u32, dims_y: u32, dims_z: u32, atlas_offset: u32,
    scale_min: vec4<f32>,
}
struct Params {
    cell_size: f32, closed_faces: i32,
    bounds_lo_x: f32, bounds_lo_y: f32, bounds_lo_z: f32,
    bounds_hi_x: f32, bounds_hi_y: f32, bounds_hi_z: f32,
    body_count: i32, rows: i32, tick_seconds: f32, pool_tiles: f32,
    dispatch_count: u32, _pad0: u32, _pad1: u32, _pad2: u32,
}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_bodies: array<LiquidBody>;
@group(0) @binding(2) var<storage, read> buf_shapes: array<LiquidShape>;
@group(0) @binding(3) var<storage, read> buf_atlas: array<u32>;
@group(0) @binding(4) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(5) var<storage, read_write> buf_solid: array<f32>;
"""
SOLID_DISTANCE_MAIN = """@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.dispatch_count { return; }
    buf_solid[idx] = body(idx, params.dispatch_count, params.cell_size, params.closed_faces,
        params.bounds_lo_x, params.bounds_lo_y, params.bounds_lo_z,
        params.bounds_hi_x, params.bounds_hi_y, params.bounds_hi_z,
        params.body_count, params.rows, params.tick_seconds, params.pool_tiles);
}
"""
# The wet look's probe kernel, as `wetness_gpu_tests.rs` appends it.
WET_PROBE_KERNEL = """@group(0) @binding(91) var<storage, read> probe_in: array<vec4<f32>>;
@group(0) @binding(90) var<storage, read_write> probe_out: array<vec4<f32>>;
@compute @workgroup_size(64)
fn cs_probe(@builtin(global_invocation_id) id: vec3<u32>) {
    let surface = look_wet_surface(probe_in[id.x].xyz, vec3<f32>(0.2, 0.5, 0.8), 0.6);
    probe_out[id.x] = vec4<f32>(surface.albedo, surface.roughness);
}
"""
STEP_PARTS = [
    "common", "transfer", "extend", "forces", "solids", "pocket_bindings",
    "tile_bindings", "pockets", "divergence", "particle_distance",
    "projection", "density", "move", "emit", "count", "tiles", "marker_removal",
]


def stats_layout() -> str:
    """The `with_stats_layout` const lines, values read from liquid_stats.rs."""
    src = (C / "manifold-water-liquid/src/primitives/liquid_stats.rs").read_text()
    values: dict[str, int] = {}
    for name, expr in re.findall(r"pub const (\w+): u32 = ([^;]+);", src):
        # Definition order: each constant only names ones defined above it.
        values[name] = int(eval(expr, {}, dict(values)))  # noqa: S307 - our own integer consts

    names = ["SOLVER_STATS_START", "SOLVER_WORDS", "INFLOW_BIRTHS_TAIL",
             "INFLOW_REFUSED_TAIL", "SHEET_REFUSED_TAIL", "TILE_REMOVED_TAIL"]
    return "".join(f"const {n}: u32 = {values[n]}u;\n" for n in names) + "\n"


STACKS = {
    "step": (True, [LIQ / "liquid_pose.wgsl", LIQ / "liquid_collider.wgsl",
                    C / "manifold-nodes-physics/src/shaders/force_lattice.wgsl",
                    LIQ / "tile_grid.wgsl", WORLD_READ,
                    *[FLIP / "gpu_flip_step" / f"{p}.wgsl" for p in STEP_PARTS]]),
    "pressure": (False, [WORLD_READ, LIQ / "tile_grid.wgsl", FLIP / "gpu_flip_pressure.wgsl"]),
    "bodies": (False, [WORLD_READ, FLIP / "gpu_flip_bodies.wgsl"]),
    "lentine": (False, [LIQ / "tile_grid.wgsl", FLIP / "gpu_flip_lentine.wgsl"]),
    "clock": (False, [FLIP / "gpu_flip_clock.wgsl"]),
    "sheeting": (True, [WORLD_READ, FLIP / "gpu_flip_sheeting.wgsl"]),
    "sheet_distance": (False, [WORLD_READ, FLIP / "gpu_flip_sheet_distance.wgsl"]),
    "publication": (True, [LIQ / "particle_publication.wgsl"]),
    "stats": (True, [LIQ / "liquid_stats.wgsl"]),
    "state_records": (False, [FLIP / "liquid_state_records.wgsl"]),
    "sort_dense": (False, [SORT_DENSE_KEYS, LIQ / "sort_particles_into_cells.wgsl"]),
    "sort_pooled": (False, [WORLD_READ, SORT_POOLED_KEYS, LIQ / "sort_particles_into_cells.wgsl"]),
    "face_component": (False, [WORLD_READ, FACE_COMPONENT_STUB, LIQ / "face_sample_component_body.wgsl",
                               FACE_COMPONENT_MAIN]),
    "redistance": (False, [WORLD_READ, LIQ / "marching_cubes_common.wgsl", REDISTANCE_STUB,
                           LIQ / "redistance_lattice_body.wgsl", REDISTANCE_MAIN]),
    "smooth": (False, [WORLD_READ, SMOOTH_STUB, LIQ / "smooth_lattice_body.wgsl", SMOOTH_MAIN]),
    "blend": (False, [WORLD_READ, BLEND_STUB, LIQ / "field_smooth_blend_body.wgsl", BLEND_MAIN]),
    "particle_volume": (False, [WORLD_READ, VOLUME_STUB, SURFACE / "particle_volume_body.wgsl", VOLUME_MAIN]),
    "surface_weights": (False, [WORLD_READ, SURFACE / "surface_weights.wgsl"]),
    "carry_liquid": (False, [WORLD_READ, SURFACE / "carry_liquid_into_solids.wgsl"]),
    "ww_step": (False, [WW / "whitewater_step.wgsl"]),
    "ww_fused": (False, [WORLD_READ, LIQ / "whitewater_common.wgsl",
                         LIQ / "liquid_faces.wgsl",
                         C / "manifold-nodes-physics/src/shaders/force_lattice.wgsl",
                         WW / "whitewater_fused.wgsl"]),
    "world_tiles_read": (False, [WORLD_READ, WORLD_READ_STUB]),
    "world_tiles": (False, [WORLD_READ, LIQ / "world_tiles_liquid.wgsl", LIQ / "world_tiles.wgsl"]),
    "world_tiles_parents": (False, [WORLD_READ, LIQ / "world_tiles_parents.wgsl",
                                    LIQ / "world_tiles.wgsl"]),
    "surface_tiles": (False, [WORLD_READ, SURF / "surface_tiles.wgsl"]),
    "surface_wetness": (False, [WORLD_READ, SURF / "surface_wetness.wgsl"]),
    "foam_cover": (False, [WORLD_READ, WW / "particle_splat.wgsl", WW / "foam_cover.wgsl"]),
    "spray_mist": (False, [WORLD_READ, WW / "particle_splat.wgsl", WW / "spray_mist.wgsl"]),
    "push_out_of_solid": (False, [PUSH_OUT_STUB, WORLD_READ, FLIP / "push_out_of_solid_body.wgsl",
                                  PUSH_OUT_MAIN]),
    "liquid_solid_distance": (False, [SOLID_DISTANCE_STUB, WORLD_READ, LIQ / "liquid_pose.wgsl",
                                      LIQ / "liquid_collider.wgsl",
                                      FLIP / "liquid_solid_distance_body.wgsl",
                                      SOLID_DISTANCE_MAIN]),
    # The scene renderer's field reads: the stacks `water_march.rs`,
    # `water_cover.rs`, `water_shade.rs` and `render_scene.rs` compose.
    "scene_march": (False, [WORLD_READ, SCENE / "field_tiles.wgsl", SCENE / "water_field_common.wgsl",
                            SCENE / "water_march_core.wgsl", SCENE / "water_march.wgsl"]),
    "scene_mist": (False, [WORLD_READ, SCENE / "field_tiles.wgsl", SCENE / "water_field_common.wgsl",
                           SCENE / "water_march_core.wgsl", SCENE / "water_mist.wgsl"]),
    "scene_cover": (False, [WORLD_READ, SCENE / "field_tiles.wgsl", SCENE / "water_march_core.wgsl",
                            SCENE / "water_cover.wgsl"]),
    "scene_water_shade": (False, [WORLD_READ, SCENE / "field_tiles.wgsl", SCENE / "scene_shading.wgsl",
                                  SCENE / "look_ripple_detail.wgsl", SCENE / "foam_field.wgsl",
                                  SCENE / "look_foam.wgsl", SCENE / "look_mist.wgsl",
                                  SCENE / "look_thin_water_light.wgsl", SCENE / "look_wet_surface.wgsl",
                                  SCENE / "water_field_common.wgsl", SCENE / "water_shade.wgsl"]),
    "scene_mesh": (False, [WORLD_READ, SCENE / "field_tiles.wgsl", SCENE / "instance_transform.wgsl",
                           SCENE / "scene_shading.wgsl", SCENE / "look_ripple_detail.wgsl",
                           SCENE / "foam_field.wgsl", SCENE / "look_foam.wgsl", SCENE / "look_mist.wgsl",
                           SCENE / "look_thin_water_light.wgsl", SCENE / "look_wet_surface.wgsl",
                           SCENE / "render_scene.wgsl"]),
    "scene_wet": (False, [WORLD_READ, SCENE / "field_tiles.wgsl", SCENE / "look_wet_surface.wgsl",
                          WET_PROBE_KERNEL]),
}


def check(name: str, stats: bool, files: list[Path | str], tmp: Path) -> bool:
    # A str part is inline source (an includer stub), a Path a file.
    source = "\n".join(f if isinstance(f, str) else f.read_text() for f in files)
    if stats:
        source = stats_layout() + source
    out = tmp / f"{name}.wgsl"
    out.write_text(source)
    run = subprocess.run(["naga", str(out)], capture_output=True, text=True)
    if run.returncode == 0:
        print(f"ok    {name}")
        return True
    print(f"FAIL  {name}  (composed source: {out})")
    print((run.stderr or run.stdout).rstrip())
    return False


def main() -> int:
    wanted = sys.argv[1:] or list(STACKS)
    unknown = [w for w in wanted if w not in STACKS]
    if unknown:
        print(f"unknown stack(s): {unknown}; known: {list(STACKS)}")
        return 2
    tmp = Path(tempfile.mkdtemp(prefix="wgsl_check_"))
    results = [check(n, *STACKS[n], tmp) for n in wanted]
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
