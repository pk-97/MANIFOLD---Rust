// node.whitewater_type — fusable BUFFER body, COINCIDENT spawns, GATHER
// distance and cells. FLIP's _getDiffuseParticleType
// (diffuseparticlesimulation.cpp:2056) for a fresh particle, with FLIP's
// defaults:
//   spray (2) outside the boundary box, which sits 1.625 cells inside the
//   grid (FLIP's box 3 cells smaller than the domain, then a quarter cell,
//   AABB::expand moving each side by half);
//   else by the distance at the particle, read trilinearly at cell centres
//   (a cell outside the grid reads 0): foam (1) within a cell of the
//   surface, bubble (0) deeper, spray (2) higher;
//   foam or spray whose cell borders no air (26 neighbours, outside the grid
//   counting as solid) becomes bubble.
// Slots with lifetime 0 pass whole. The lifecycle types every particle by
// the same rule on each step, so the two must agree.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's _maxFoamToSurfaceDistance and _foamLayerOffset, cells.
const WT_FOAM_DEPTH: f32 = 1.0;
const WT_FOAM_OFFSET: f32 = 0.0;
// FLIP's boundary box inset, cells, and its 1e-6 m epsilon halved.
const WT_BOX_INSET: f32 = 1.625;
const WT_BOX_EPSILON: f32 = 0.5e-6;

fn wt_distance(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_distance[ww_cell_index(vec3<u32>(c), cells)];
}

fn wt_borders_air(c: vec3<i32>, cells: vec3<u32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let n = c + vec3<i32>(dx, dy, dz);
                if (dx == 0 && dy == 0 && dz == 0) || !ww_in_grid(n, cells) {
                    continue;
                }
                if buf_cells[ww_cell_index(vec3<u32>(n), cells)] == 0u {
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
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    spray_speed: f32, seed: f32, epoch: f32, dust: f32,
) -> Element {
    var spawn = e_spawns;
    if !(spawn.position_lifetime.w > 0.0) {
        return spawn;
    }
    if dust > 0.5 { spawn.kind = 4u; return spawn; }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return spawn;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if total > arrayLength(&buf_distance) || total > arrayLength(&buf_cells) {
        return spawn;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let q = ww_grid_position(spawn.position_lifetime.xyz, vec3<f32>(center_x, center_y, center_z), size, cells);
    let lo = vec3<f32>(WT_BOX_INSET + WT_BOX_EPSILON / h);
    let hi = vec3<f32>(cells) - lo;
    if any(q < lo) || any(q >= hi) {
        spawn.kind = 2u;
        return wt_finish(spawn, idx, spray_speed, seed, epoch);
    }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * wt_distance(base + vec3<i32>(ww_corner(corner)), cells);
    }
    let depth = WT_FOAM_DEPTH * h;
    let offset = WT_FOAM_OFFSET * h;
    var kind = 2u;
    if d > -depth + offset && d < depth + offset {
        kind = 1u;
    } else if d < -depth + offset {
        kind = 0u;
    }
    if kind != 0u && !wt_borders_air(vec3<i32>(floor(q)), cells) {
        kind = 0u;
    }
    spawn.kind = kind;
    return wt_finish(spawn, idx, spray_speed, seed, epoch);
}
