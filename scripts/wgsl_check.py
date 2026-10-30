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
WET_LAYOUT = C / "manifold-node-engine/src/scene/wet_map_layout.wgsl"
# The constants `wet_map::layout()` writes ahead of the layout's helpers.
WET_CONSTS = """const WD_HEADER_WORDS: u32 = 16u;
const WD_HASH_SLOTS: u32 = 262144u;
const WD_CAPACITY: u32 = 131072u;
const WD_DECAL_WORDS: u32 = 8u;
const WD_DRY_SECONDS: f32 = 15.0;
"""
# A second table's read side, as `surface_tiles.rs` and `interior_fill.rs`
# compose it: the same text renamed sim_ts_ / SIM_WT_.
SIM_READ = WORLD_READ.read_text().replace("ts_", "sim_ts_").replace("WT_", "SIM_WT_")
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
STEP_PARTS = [
    "common", "transfer", "extend", "forces", "solids", "pocket_bindings",
    "tile_bindings", "pockets", "divergence", "particle_distance",
    "projection", "density", "move", "emit", "count", "tiles", "marker_removal",
    "narrow_band",
]


def stats_layout() -> str:
    """The `with_stats_layout` const lines, values read from liquid_stats.rs."""
    src = (C / "manifold-water-liquid/src/primitives/liquid_stats.rs").read_text()
    values: dict[str, int] = {}
    for name, expr in re.findall(r"pub const (\w+): u32 = ([^;]+);", src):
        # Definition order: each constant only names ones defined above it.
        values[name] = int(eval(expr, {}, dict(values)))  # noqa: S307 - our own integer consts

    names = ["SOLVER_STATS_START", "SOLVER_WORDS", "INFLOW_BIRTHS_TAIL",
             "INFLOW_REFUSED_TAIL", "SHEET_REFUSED_TAIL", "TILE_REMOVED_TAIL",
             "RESEED_REFUSED_TAIL"]
    return "".join(f"const {n}: u32 = {values[n]}u;\n" for n in names) + "\n"


STACKS = {
    "step": (True, [LIQ / "liquid_pose.wgsl", LIQ / "liquid_collider.wgsl",
                    LIQ / "collider_bvh.wgsl",
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
    "blend": (False, [WORLD_READ, LIQ / "field_smooth_blend.wgsl"]),
    "particle_volume": (False, [WORLD_READ, SURFACE / "particle_volume.wgsl"]),
    "carry_liquid": (False, [WORLD_READ, SURFACE / "carry_liquid_into_solids.wgsl"]),
    "field_close": (False, [WORLD_READ, SURFACE / "field_close.wgsl"]),
    "ww_step": (False, [WW / "whitewater_step.wgsl"]),
    "ww_fused": (False, [WORLD_READ, LIQ / "whitewater_common.wgsl",
                         LIQ / "liquid_faces.wgsl",
                         C / "manifold-nodes-physics/src/shaders/force_lattice.wgsl",
                         WW / "whitewater_fused.wgsl"]),
    "world_tiles_read": (False, [WORLD_READ, WORLD_READ_STUB]),
    "world_tiles": (False, [WORLD_READ, LIQ / "world_tiles_liquid.wgsl", LIQ / "world_tiles.wgsl"]),
    "world_tiles_kept": (False, [WORLD_READ, LIQ / "world_tiles_kept.wgsl", LIQ / "world_tiles.wgsl"]),
    "world_tiles_parents": (False, [WORLD_READ, LIQ / "world_tiles_parents.wgsl",
                                    LIQ / "world_tiles.wgsl"]),
    "surface_tiles": (False, [WORLD_READ, SIM_READ, SURF / "surface_tiles.wgsl"]),
    "interior_fill": (False, [WORLD_READ, SIM_READ, SURF / "interior_fill.wgsl"]),
    "wet_map": (False, [WORLD_READ, WET_CONSTS, WET_LAYOUT, SURF / "wet_map.wgsl"]),
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
    "scene_caustics": (False, [WORLD_READ, SCENE / "field_tiles.wgsl", SCENE / "look_ripple_detail.wgsl",
                               SCENE / "caustics.wgsl"]),
    "scene_water_shade": (False, [WORLD_READ, SCENE / "field_tiles.wgsl", SCENE / "scene_shading.wgsl",
                                  SCENE / "look_ripple_detail.wgsl", SCENE / "foam_mask.wgsl",
                                  SCENE / "look_foam.wgsl", SCENE / "look_mist.wgsl",
                                  SCENE / "look_thin_water_light.wgsl",
                                  SCENE / "look_wet_surface.wgsl",
                                  SCENE / "water_field_common.wgsl", SCENE / "water_shade.wgsl"]),
    "scene_mesh": (False, [WORLD_READ, SCENE / "field_tiles.wgsl", SCENE / "instance_transform.wgsl",
                           SCENE / "scene_shading.wgsl", SCENE / "look_ripple_detail.wgsl",
                           SCENE / "foam_mask.wgsl", SCENE / "look_foam.wgsl", SCENE / "look_mist.wgsl",
                           SCENE / "look_thin_water_light.wgsl", SCENE / "look_wet_surface.wgsl",
                           SCENE / "cover_rule.wgsl", SCENE / "render_scene.wgsl",
                           SCENE / "cutout_depth.wgsl"]),
    "scene_scatter": (False, [SCENE / "cover_rule.wgsl", SCENE / "scatter_on_mesh.wgsl"]),
    "scene_tree": (False, [SCENE / "tree.wgsl"]),
    "scene_terrain": (False, [SCENE / "terrain.wgsl"]),
    "scene_daylight": (False, [SCENE / "daylight.wgsl", SCENE / "bake_daylight.wgsl"]),
    "scene_wet_pass": (False, [WET_CONSTS, WET_LAYOUT, SCENE / "wet_pass.wgsl"]),
    "scene_foam_pass": (False, [SCENE / "water_field_common.wgsl", SCENE / "foam_pass.wgsl"]),
    "scene_gas": (False, [SCENE / "scene_shading.wgsl",
                          "const BLACKBODY_MIN_KELVIN: f32 = 1000.0;\nconst BLACKBODY_MAX_KELVIN: f32 = 4000.0;\n"
                          "const BLACKBODY_ENTRIES: u32 = 64u;\nconst NOISE_LATTICE_CELLS: f32 = 32.0;\n",
                          SCENE / "gas_volume.wgsl"]),
    "scene_gas_compute": (False, [SCENE / "gas_light.wgsl"]),
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
