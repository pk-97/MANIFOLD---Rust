// node.whitewater_type — fusable BUFFER body, COINCIDENT spawns, GATHER
// distance, cells and tiles. FLIP's _getDiffuseParticleType
// (diffuseparticlesimulation.cpp:2056) for a fresh particle, with FLIP's
// defaults:
//   spray (2) when the particle's cell lies in no stored tile (the step's
//   world tiles have no box edge to inset from);
//   else by the distance at the particle, read trilinearly at cell centres:
//   foam (1) within a cell of the surface, bubble (0) deeper, spray (2)
//   higher;
//   foam or spray whose cell borders no air (26 neighbours) becomes bubble.
// The fields are read through the step's world-tile table (`tiles`,
// `pool_tiles` tiles): a cell in an absent tile reads the canonical element
// (distance 3h, air). Slots with lifetime 0 pass whole. The lifecycle types
// every particle by the same rule on each step, so the two must agree.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's _maxFoamToSurfaceDistance and _foamLayerOffset, cells.
const WT_FOAM_DEPTH: f32 = 1.0;
const WT_FOAM_OFFSET: f32 = 0.0;

var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }
fn wt_distance(c: vec3<i32>) -> f32 {
    return buf_distance[ts_cell_at(c)];
}

fn wt_borders_air(c: vec3<i32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                if buf_cells[ts_cell_at(c + vec3<i32>(dx, dy, dz))] == 0u {
                    return true;
                }
            }
        }
    }
    return false;
}

fn wt_finish(p: Element, idx: u32, speed: f32, seed: f32, epoch: f32) -> Element {
    var out = p;
    if out.kind == 2u {
        out.velocity *= 1.0 + (speed - 1.0) * ww_random(idx, bitcast<u32>(seed), u32(max(round(epoch), 0.0)), 11u);
    }
    return out;
}

fn body(
    idx: u32,
    count: u32,
    e_spawns: Element,
    spray_speed: f32, seed: f32, epoch: f32, dust: f32,
    cell_size: f32,
    pool_tiles: f32,
) -> Element {
    ww_pool_tiles = u32(pool_tiles);
    var spawn = e_spawns;
    if !(spawn.position_lifetime.w > 0.0) {
        return spawn;
    }
    if dust > 0.5 { spawn.kind = 4u; return spawn; }
    let h = cell_size;
    let q = ww_cell_position(spawn.position_lifetime.xyz, h);
    let cell = vec3<i32>(floor(q));
    ts_home(ts_cell_at(cell));
    if !ts_cell_stored(cell) {
        spawn.kind = 2u;
        return wt_finish(spawn, idx, spray_speed, seed, epoch);
    }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * wt_distance(base + vec3<i32>(ww_corner(corner)));
    }
    let depth = WT_FOAM_DEPTH * h;
    let offset = WT_FOAM_OFFSET * h;
    var kind = 2u;
    if d > -depth + offset && d < depth + offset {
        kind = 1u;
    } else if d < -depth + offset {
        kind = 0u;
    }
    if kind != 0u && !wt_borders_air(cell) {
        kind = 0u;
    }
    spawn.kind = kind;
    return wt_finish(spawn, idx, spray_speed, seed, epoch);
}
