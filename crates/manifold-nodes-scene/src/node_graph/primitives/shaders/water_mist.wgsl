// Per-pixel optical depth of the mist field along the camera ray. Adds, for the
// water's lattice, `(tau, m)` to `mist_out`: `tau` is the integral of the
// extinction coefficient (per metre) along the ray from the camera to the
// nearest of the opaque scene, the water and the lattice's far side, and `m`
// its first moment, `tau`'s integrand times the world distance from the ray
// origin. The shading divides them for the mist's mean position along the ray.
// Starts after `water_field_common.wgsl` and `water_march_core.wgsl`, which hold
// bindings 0 and 1, the field sampling and the ray set-up.

@group(0) @binding(3) var opaque_depth: texture_depth_2d;
@group(0) @binding(4) var<storage, read> hits: array<FieldHit>;
@group(0) @binding(7) var<storage, read> mist: array<f32>;
@group(0) @binding(8) var<storage, read_write> mist_out: array<vec2<f32>>;

// One sample per cell: the field is trilinear between nodes, so finer steps add
// nothing a trapezoid misses on a smooth density.
const MIST_STEP_CELLS: f32 = 1.0;
// Safety cap so a broken field cannot hang the GPU.
const MIST_MAX_STEPS: u32 = 512u;

fn mist_node(i: vec3<i32>) -> f32 {
    let hi = vec3<i32>(u.nodes.xyz) - vec3<i32>(1);
    let c = clamp(i, vec3<i32>(0), hi);
    let n = u32(c.x) + u.nodes.x * (u32(c.y) + u.nodes.y * u32(c.z));
    return mist[n];
}

fn sample_mist(p: vec3<f32>) -> f32 {
    let g = lattice_coord(p);
    let b = min(vec3<i32>(floor(g)), vec3<i32>(u.nodes.xyz) - vec3<i32>(2));
    let f = g - vec3<f32>(b);
    let c000 = mist_node(b);
    let c100 = mist_node(b + vec3<i32>(1, 0, 0));
    let c010 = mist_node(b + vec3<i32>(0, 1, 0));
    let c110 = mist_node(b + vec3<i32>(1, 1, 0));
    let c001 = mist_node(b + vec3<i32>(0, 0, 1));
    let c101 = mist_node(b + vec3<i32>(1, 0, 1));
    let c011 = mist_node(b + vec3<i32>(0, 1, 1));
    let c111 = mist_node(b + vec3<i32>(1, 1, 1));
    let x00 = mix(c000, c100, f.x);
    let x10 = mix(c010, c110, f.x);
    let x01 = mix(c001, c101, f.x);
    let x11 = mix(c011, c111, f.x);
    return mix(mix(x00, x10, f.y), mix(x01, x11, f.y), f.z);
}

@compute @workgroup_size(8, 8)
fn cs_mist(@builtin(global_invocation_id) gid: vec3<u32>) {
    let width = u32(u.target_size.x);
    let height = u32(u.target_size.y);
    if gid.x >= width || gid.y >= height { return; }
    let pix = vec2<i32>(gid.xy);
    let idx = gid.y * width + gid.x;

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

    let cell = u.lattice_min.w;
    let n = clamp(u32(ceil((t1 - t0) / (MIST_STEP_CELLS * cell))), 1u, MIST_MAX_STEPS);
    let h = (t1 - t0) / f32(n);
    var sum_sigma = 0.0;
    var sum_moment = 0.0;
    // Trapezoid over the n + 1 samples, the end samples at half weight.
    for (var k = 0u; k <= n; k++) {
        let w = select(1.0, 0.5, k == 0u || k == n);
        let t = t0 + f32(k) * h;
        let sigma = sample_mist(o + d * t);
        sum_sigma += w * sigma;
        sum_moment += w * sigma * t;
    }
    // Local lengths are world lengths times `s`.
    let tau = h / s * sum_sigma;
    let moment = h / s * sum_moment / s;
    mist_out[idx] += vec2<f32>(tau, moment);
}
