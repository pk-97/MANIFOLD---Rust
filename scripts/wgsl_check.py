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
    "ww_gather": (False, [WORLD_READ, WW / "gather_distance.wgsl"]),
    "sort_dense": (False, [SORT_DENSE_KEYS, LIQ / "sort_particles_into_cells.wgsl"]),
    "sort_pooled": (False, [WORLD_READ, SORT_POOLED_KEYS, LIQ / "sort_particles_into_cells.wgsl"]),
    "face_component": (False, [WORLD_READ, FACE_COMPONENT_STUB, LIQ / "face_sample_component_body.wgsl",
                               FACE_COMPONENT_MAIN]),
    "ww_step": (False, [WW / "whitewater_step.wgsl"]),
    "ww_fused": (False, [LIQ / "tile_grid.wgsl", LIQ / "whitewater_common.wgsl",
                         LIQ / "liquid_faces.wgsl",
                         C / "manifold-nodes-physics/src/shaders/force_lattice.wgsl",
                         WW / "whitewater_fused.wgsl"]),
    "world_tiles_read": (False, [WORLD_READ, WORLD_READ_STUB]),
    "world_tiles": (False, [WORLD_READ, LIQ / "world_tiles_liquid.wgsl", LIQ / "world_tiles.wgsl"]),
    "world_tiles_parents": (False, [WORLD_READ, LIQ / "world_tiles_parents.wgsl",
                                    LIQ / "world_tiles.wgsl"]),
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
