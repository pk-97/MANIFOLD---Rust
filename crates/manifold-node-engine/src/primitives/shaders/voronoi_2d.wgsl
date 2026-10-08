// node.voronoi_2d — 2D Worley/Voronoi cellular noise.
//
// Each integer cell holds one randomly-jittered feature point. The
// shader visits the 9 neighboring cells around the query pixel and
// returns the smallest (F1) and second-smallest (F2) Euclidean
// distances. Output channels:
//
// `out` (binding 1):
//   R = F1                  (distance to nearest feature point)
//   G = F2                  (distance to second-nearest)
//   B = F2 - F1             (cell-edge factor — high at boundaries)
//   A = cell_hash           (per-cell stable random in [0, 1] —
//                            the hash of the F1-winning cell's
//                            coordinates. Constant across every
//                            pixel inside one cell; uncorrelated
//                            between neighboring cells. Foundation
//                            for per-cell variation: density
//                            thresholding, per-cell colour, per-cell
//                            timing/twinkle, per-cell size.)
// `cell_id` (binding 2):
//   RG = the F1-winning cell's integer coordinate (B=0, A=1) — the
//        raw cell coordinate, constant within a Voronoi region, for
//        seeding per-cell randoms (feed RG + a seed into
//        node.hash_field_by_seed for beat-reseeded cellular composites
//        like Voronoi Prism).
//
// Both outputs are independently optional: `write_out` / `write_cell_id`
// gate each store, so a consumer that reads only one output doesn't pay
// to allocate the other (the executor skips the unconsumed slot, and the
// run() binds the live one to both bindings as a harmless placeholder).
//
// F1/F2/(F2-F1) are scaled by `out_scale` so the user can remap into
// a useful range without an extra scale_offset_texture node. The
// cell_hash on A is always raw [0, 1] — out_scale does not apply.

struct Uniforms {
    scale:         f32,   // cell density (cells per UV-unit)
    offset_x:      f32,
    offset_y:      f32,
    jitter:        f32,   // 0..1 — 0 = grid points, 1 = full random offset
    out_scale:     f32,   // multiplier on F1/F2/(F2-F1)
    write_out:     u32,   // 1 = store the F1/F2/edge/cell_hash output
    write_cell_id: u32,   // 1 = store the cell-coordinate output
    _pad2:         f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var cell_id_tex: texture_storage_2d<rgba16float, write>;

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn cell_point(cell: vec2<i32>) -> vec2<f32> {
    // Two independent hashes for X and Y jitter.
    let hx = wang_hash(u32(cell.x + 10000) * 73856093u ^ u32(cell.y + 10000) * 19349663u);
    let hy = wang_hash(u32(cell.x + 10000) * 83492791u ^ u32(cell.y + 10000) * 28411627u);
    let jx = f32(hx & 0xFFFFu) / 65535.0;
    let jy = f32(hy & 0xFFFFu) / 65535.0;
    return vec2<f32>(cell) + vec2<f32>(0.5, 0.5) + (vec2<f32>(jx, jy) - 0.5) * u.jitter;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let p = uv * u.scale + vec2<f32>(u.offset_x, u.offset_y);
    let cell = vec2<i32>(floor(p));

    var f1 = 1e9;
    var f2 = 1e9;
    var f1_cell = cell;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let neighbor = cell + vec2<i32>(dx, dy);
            let fp = cell_point(neighbor);
            let d = length(p - fp);
            if d < f1 {
                f2 = f1;
                f1 = d;
                f1_cell = neighbor;
            } else if d < f2 {
                f2 = d;
            }
        }
    }

    // Per-cell stable random — independent hash mix from the jitter
    // hashes so cell_hash decorrelates from the per-cell jitter offset
    // (otherwise dense / sparse cells correlate with how-jittered-the-
    // point-is, which is a non-obvious coupling for downstream consumers).
    let cell_hash = f32(wang_hash(
        u32(f1_cell.x + 10000) * 12345701u ^ u32(f1_cell.y + 10000) * 39916801u,
    ) & 0xFFFFu) / 65535.0;

    let r = f1 * u.out_scale;
    let g = f2 * u.out_scale;
    let b = (f2 - f1) * u.out_scale;
    if u.write_out != 0u {
        textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(r, g, b, cell_hash));
    }
    if u.write_cell_id != 0u {
        textureStore(
            cell_id_tex,
            vec2<i32>(gid.xy),
            vec4<f32>(f32(f1_cell.x), f32(f1_cell.y), 0.0, 1.0),
        );
    }
}
