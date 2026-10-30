// Per-pixel optical depth of the mist field along the camera ray. Adds, for the
// water's field, `(tau, m)` to `mist_out`: `tau` is the integral of the
// extinction coefficient (per metre) along the ray from the camera to the
// nearest of the opaque scene, the water and the far side of the field's
// extent, and `m` its first moment, `tau`'s integrand times the world distance
// from the ray origin. The shading divides them for the mist's mean position
// along the ray. Starts after `water_field_common.wgsl` and
// `water_march_core.wgsl`, which hold bindings 0 and 1, the field sampling and
// the ray set-up; the mist is a pooled field under the same world-tile table,
// and `tile_flags` the field's per-slot flags (`water_tile_flags.wgsl`).

@group(0) @binding(3) var opaque_depth: texture_depth_2d;
@group(0) @binding(4) var<storage, read> hits: array<FieldHit>;
@group(0) @binding(7) var<storage, read> mist: array<f32>;
@group(0) @binding(8) var<storage, read_write> mist_out: array<vec2<f32>>;
@group(0) @binding(72) var<storage, read> tile_flags: array<u32>;

fn field_tile_dry(slot: u32) -> bool {
    return (tile_flags[slot] & TILE_WATER) == 0u;
}

// One sample per cell: the field is trilinear between nodes, so finer steps add
// nothing a midpoint sample misses on a smooth density.
const MIST_STEP_CELLS: f32 = 1.0;
// Safety cap so a broken field cannot hang the GPU.
const MIST_MAX_STEPS: u32 = 512u;

fn mist_level(c: FieldCorners) -> f32 {
    return field_mix(
        mist[c.c000], mist[c.c100], mist[c.c010], mist[c.c110],
        mist[c.c001], mist[c.c101], mist[c.c011], mist[c.c111],
        c.f,
    );
}

// The trilinear of the node extinctions at an object-space point.
fn sample_mist(p: vec3<f32>) -> f32 {
    let q = field_node_coord(p, u.cell.x);
    field_enter(q);
    return mist_level(field_corners(q));
}

@compute @workgroup_size(8, 8)
fn cs_mist(@builtin(global_invocation_id) gid: vec3<u32>) {
    let width = u32(u.target_size.x);
    let height = u32(u.target_size.y);
    if gid.x >= width || gid.y >= height { return; }
    let pix = vec2<i32>(gid.xy);
    let idx = gid.y * width + gid.x;
    field_pool = u.tiles.x;

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(u.target_size.xy);
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let origin_w = unproject(ndc, 1.0);
    let dir_w = normalize(unproject(ndc, 0.5) - origin_w);

    var t_stop = 3.4e38;
    let od = textureLoad(opaque_depth, pix, 0);
    if od > 0.0 {
        t_stop = dot(unproject(ndc, od) - origin_w, dir_w);
    }
    let hit_depth = hits[idx].depth;
    if hit_depth > 0.0 {
        t_stop = min(t_stop, dot(unproject(ndc, hit_depth) - origin_w, dir_w));
    }

    let ray = local_ray(origin_w, dir_w);
    let o = ray.o;
    let d = ray.d;
    let s = ray.s;

    let span = lattice_span(o, d);
    let t0 = span.x;
    let t1 = min(span.y, t_stop * s);
    if t0 >= t1 { return; }

    let cell = u.cell.x;
    let step = MIST_STEP_CELLS * cell;
    var sum_sigma = 0.0;
    var sum_moment = 0.0;
    // Midpoint samples a cell apart. A tile the table does not hold has no
    // mist (its canonical element is 0), nor has a held one without
    // TILE_MIST in any of its cells, so the ray steps past them at once.
    var t = t0;
    for (var k = 0u; k < MIST_MAX_STEPS && t < t1; k++) {
        let q = field_node_coord(o + d * t, cell);
        if !field_enter(q) || (tile_flags[ts_home_slot] & TILE_MIST) == 0u {
            t += tile_exit(q, d);
            continue;
        }
        let len = min(step, t1 - t);
        let mid = t + 0.5 * len;
        let sigma = sample_mist(o + d * mid);
        sum_sigma += len * sigma;
        sum_moment += len * sigma * mid;
        t += len;
    }
    // Local lengths are world lengths times `s`.
    let tau = sum_sigma / s;
    let moment = sum_moment / s / s;
    mist_out[idx] += vec2<f32>(tau, moment);
}
