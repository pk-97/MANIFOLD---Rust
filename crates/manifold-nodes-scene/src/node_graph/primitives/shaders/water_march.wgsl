// Per-pixel ray march of a water level set. Writes the nearest field hit per
// pixel into the G-buffer `hits`, plus optional depth and motion vectors.
// Starts after `water_march_core.wgsl`, which holds bindings 0 and 1, the
// march constants, the field sampling and the hit search. The velocity is a
// pooled field under the same world-tile table as the level set, and
// `tile_flags` the field's per-slot flags (`water_tile_flags.wgsl`). With
// `u.flags.z` set, `first_hit` is the water meshes' nearest depth over the
// opaque scene: a ray meets water only where a mesh is in front, and the
// search runs only within MESH_WINDOW_CELLS of it.

@group(0) @binding(2) var<storage, read> velocity: array<f32>;
@group(0) @binding(3) var opaque_depth: texture_depth_2d;
@group(0) @binding(4) var<storage, read_write> hits: array<FieldHit>;
@group(0) @binding(5) var depth_out: texture_storage_2d<r32float, write>;
@group(0) @binding(6) var velocity_out: texture_storage_2d<rg16float, write>;
@group(0) @binding(8) var first_hit: texture_depth_2d;
@group(0) @binding(72) var<storage, read> tile_flags: array<u32>;

// How far the field's zero surface can lie from its marching-cubes mesh along
// a ray, in cells: the mesh's vertices are on the field's edge crossings, and
// inside a cell the two part by under a cell diagonal.
const MESH_WINDOW_CELLS: f32 = 2.0;
// The even step that brackets the crossing round the mesh, in cells: a step
// sized by the field's value could pass a sheet thinner than its error.
const MESH_STEP_CELLS: f32 = 0.25;

// The first crossing in [t0, t1] by even steps, refined on its bracket; -1.0
// when there is none.
fn march_window(o: vec3<f32>, d: vec3<f32>, t0: f32, t1: f32) -> f32 {
    let step = MESH_STEP_CELLS * u.cell.x;
    var a = t0;
    var fa = sample_field(o + d * a);
    if fa <= 0.0 { return a; }
    while a < t1 {
        let b = min(a + step, t1);
        let fb = sample_field(o + d * b);
        if fb <= 0.0 {
            return refine_crossing(o, d, a, fa, b, fb);
        }
        a = b;
        fa = fb;
    }
    return -1.0;
}

fn field_tile_dry(slot: u32) -> bool {
    return (tile_flags[slot] & TILE_WATER) == 0u;
}

fn velocity_at_element(i: u32) -> vec3<f32> {
    return vec3<f32>(velocity[3u * i], velocity[3u * i + 1u], velocity[3u * i + 2u]);
}

// The velocity at an object-space point: the trilinear of the node velocities.
fn sample_velocity(p: vec3<f32>) -> vec3<f32> {
    let q = field_node_coord(p, u.cell.x);
    field_enter(q);
    let c = field_corners(q);
    return field_mix3(
        velocity_at_element(c.c000), velocity_at_element(c.c100),
        velocity_at_element(c.c010), velocity_at_element(c.c110),
        velocity_at_element(c.c001), velocity_at_element(c.c101),
        velocity_at_element(c.c011), velocity_at_element(c.c111),
        c.f,
    );
}

fn bspline_w(t: f32) -> vec4<f32> {
    let m = 1.0 - t;
    return vec4<f32>(
        m * m * m / 6.0,
        (3.0 * t * t * t - 6.0 * t * t + 4.0) / 6.0,
        (-3.0 * t * t * t + 3.0 * t * t + 3.0 * t + 1.0) / 6.0,
        t * t * t / 6.0,
    );
}

fn bspline_dw(t: f32) -> vec4<f32> {
    let m = 1.0 - t;
    return vec4<f32>(
        -m * m / 2.0,
        1.5 * t * t - 2.0 * t,
        -1.5 * t * t + t + 0.5,
        0.5 * t * t,
    );
}

// Gradient of the cubic B-spline field (prototype `normal_cubic`,
// levelset_proto/src/main.rs:134-156), in field units per metre. The 4 x 4 x 4
// nodes lie in the home's tile or its neighbours, so the table is walked once.
fn field_gradient_cubic(p: vec3<f32>) -> vec3<f32> {
    let g = field_node_coord(p, u.cell.x);
    field_enter(g);
    let fl = floor(g);
    let t = g - fl;
    let base = vec3<i32>(fl) - vec3<i32>(1);
    let wx = bspline_w(t.x);
    let wy = bspline_w(t.y);
    let wz = bspline_w(t.z);
    let dx = bspline_dw(t.x);
    let dy = bspline_dw(t.y);
    let dz = bspline_dw(t.z);
    var gx = 0.0;
    var gy = 0.0;
    var gz = 0.0;
    for (var c = 0; c < 4; c++) {
        for (var b = 0; b < 4; b++) {
            for (var a = 0; a < 4; a++) {
                let v = node_value(base + vec3<i32>(a, b, c));
                gx += v * dx[a] * wy[b] * wz[c];
                gy += v * wx[a] * dy[b] * wz[c];
                gz += v * wx[a] * wy[b] * dz[c];
            }
        }
    }
    return vec3<f32>(gx, gy, gz) / u.cell.x;
}

@compute @workgroup_size(8, 8)
fn cs_march(@builtin(global_invocation_id) gid: vec3<u32>) {
    let width = u32(u.target_size.x);
    let height = u32(u.target_size.y);
    if gid.x >= width || gid.y >= height { return; }
    let pix = vec2<i32>(gid.xy);
    let idx = gid.y * width + gid.x;
    field_pool = u.tiles.xy;

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(u.target_size.xy);
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let origin_w = unproject(ndc, 1.0);
    let dir_w = normalize(unproject(ndc, 0.5) - origin_w);

    var t_stop = 3.4e38;
    let od = textureLoad(opaque_depth, pix, 0);
    if od > 0.0 {
        t_stop = dot(unproject(ndc, od) - origin_w, dir_w);
    }
    let prev = hits[idx].depth;
    if prev > 0.0 {
        t_stop = min(t_stop, dot(unproject(ndc, prev) - origin_w, dir_w));
    }

    let ray = local_ray(origin_w, dir_w);
    let o = ray.o;
    let d = ray.d;
    let s = ray.s;

    let span = lattice_span(o, d);
    // The ray's water ends at t1, which bounds the thickness walk.
    var t0 = span.x;
    let t1 = min(span.y, t_stop * s);
    if t0 >= t1 { return; }
    var t_hit = -1.0;
    if u.flags.z == 0u {
        t_hit = march_surface(o, d, t0, t1);
    } else {
        // Reversed depth: a mesh is in front where its depth is above the
        // opaque scene's.
        let md = textureLoad(first_hit, pix, 0);
        if md <= od { return; }
        let tm = dot(unproject(ndc, md) - origin_w, dir_w) * s;
        let window = MESH_WINDOW_CELLS * u.cell.x;
        t_hit = march_window(o, d, max(t0, tm - window), min(t1, tm + window));
    }
    if t_hit < 0.0 { return; }
    let cell = u.cell.x;

    let p = o + d * t_hit;
    let world = (u.local_to_world * vec4<f32>(p, 1.0)).xyz;
    let clip = u.view_proj * vec4<f32>(world, 1.0);
    let depth = clip.z / clip.w;
    let n_local = field_gradient_cubic(p);
    let n_world = normalize((transpose(u.world_to_local) * vec4<f32>(n_local, 0.0)).xyz);

    // Thickness along the view ray: step through the water until the field
    // turns positive. A held tile with no air in its cells is below zero all
    // through, so the walk crosses it in one step.
    var e = 0.05 * cell;
    for (var i = 0u; i < MARCH_MAX_STEPS; i++) {
        if t_hit + e >= t1 { break; }
        let q = field_node_coord(p + d * e, cell);
        if field_enter(q) && (tile_flags[ts_home_slot] & TILE_AIR) == 0u {
            e += tile_exit(q, d);
            continue;
        }
        let phi = level_at(field_corners(q));
        if phi >= 0.0 { break; }
        e += max(-phi, EXIT_MIN_STEP_CELLS * cell);
    }
    let thickness_world = min(e, t1 - t_hit) / s;

    hits[idx] = FieldHit(
        depth,
        pack2x16snorm(oct_encode(n_world)),
        pack2x16float(vec2<f32>(thickness_world, f32(u.tiles.z))),
    );

    if (u.flags.x & 1u) != 0u {
        textureStore(depth_out, pix, vec4<f32>(depth, 0.0, 0.0, 1.0));
    }
    if (u.flags.x & 2u) != 0u {
        var v = vec3<f32>(0.0);
        if u.target_size.z > 0.5 {
            v = (u.local_to_world * vec4<f32>(sample_velocity(p), 0.0)).xyz;
        }
        let prev_world = (u.prev_local_to_world * vec4<f32>(p, 1.0)).xyz - v * u.cell.y;
        let c0 = u.view_proj * vec4<f32>(world, 1.0);
        let c1 = u.prev_view_proj * vec4<f32>(prev_world, 1.0);
        let motion = c0.xy / c0.w - c1.xy / c1.w - (u.jitter.xy - u.jitter.zw);
        textureStore(velocity_out, pix, vec4<f32>(motion, 0.0, 0.0));
    }
}
