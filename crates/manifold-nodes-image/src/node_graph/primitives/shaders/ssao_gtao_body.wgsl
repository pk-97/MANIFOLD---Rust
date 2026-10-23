// node.ssao_gtao — fusable body, Pointwise + GatherTexel.
//
// Ground Truth Ambient Occlusion (Jimenez et al. 2016), sampled the way
// Intel's XeGTAO does: per pixel, `slices` screen directions; along each,
// `steps` depth taps per side; each side keeps its highest horizon; the
// visible arc between the two horizons, projected on the normal, is the
// visibility. `ssao_gtao.rs`'s `cpu_reference` is the plain-Rust twin of
// this file and the module doc there carries the derivation.
//
// Three things keep the result free of fixed patterns:
//   - Falloff: a tap's horizon fades to the open horizon over the outer
//     GTAO_FALLOFF_RANGE of `radius` (XeGTAO's falloff), so occlusion
//     ends smoothly instead of drawing an outline at the radius.
//   - Step jitter: each pixel offsets its taps along the slice by its own
//     fraction of a step, so equal tap distances never line up into bands
//     parallel to an edge.
//   - The slice angle and the step offset come from XeGTAO's spatio-temporal
//     noise: a Hilbert-curve index over a 64x64 tile walked by the R2
//     sequence. With `noise_per_frame` on, `noise_frame` (288 per frame,
//     from the frame time) moves the pattern every frame for a temporal
//     filter to average. Off, the pattern holds still.
//
// The Hilbert index, the R2 noise and the falloff are ported from XeGTAO
// (Intel, MIT license; see THIRD_PARTY_NOTICES.md).
//
// `depth` is raw [0,1] reversed-Z depth, read texel-exact (integer
// textureLoad, manual ClampToEdge). `camera` reaches the kernel only through
// the derived fov_y/near/far uniforms. Rounding uses a half-away-from-zero
// helper instead of WGSL's banker's `round()` so the CPU twin matches ties.

const GTAO_HALF_PI: f32 = 1.5707963267948966;
const GTAO_TAU: f32 = 6.283185307179586;
// XeGTAO's DEFAULT_FALLOFF_RANGE: the fraction of the radius over which a
// tap's weight falls from 1 to 0.
const GTAO_FALLOFF_RANGE: f32 = 0.615;
// XeGTAO's pixel-too-close threshold: the first tap sits at least this
// far out, where the finite-difference normal no longer misreads the
// surface itself as an occluder.
const GTAO_MIN_TAP_PX: f32 = 1.3;
// XeGTAO's DEFAULT_RADIUS_MULTIPLIER: taps reach this far past `radius`, so
// with the falloff, occlusion is about half faded at `radius` itself.
const GTAO_RADIUS_MULTIPLIER: f32 = 1.457;

const GTAO_HILBERT_WIDTH: u32 = 64u;

// XeGTAO's HilbertIndex: the pixel's position along a Hilbert curve that
// fills a 64x64 tile, so neighbouring pixels take distant noise values.
fn gtao_hilbert_index(x_in: u32, y_in: u32) -> u32 {
    var x = x_in;
    var y = y_in;
    var index = 0u;
    for (var level = GTAO_HILBERT_WIDTH / 2u; level > 0u; level = level / 2u) {
        let rx = select(0u, 1u, (x & level) > 0u);
        let ry = select(0u, 1u, (y & level) > 0u);
        index = index + level * level * ((3u * rx) ^ ry);
        if ry == 0u {
            if rx == 1u {
                x = GTAO_HILBERT_WIDTH - 1u - x;
                y = GTAO_HILBERT_WIDTH - 1u - y;
            }
            let t = x;
            x = y;
            y = t;
        }
    }
    return index;
}

// XeGTAO's SpatioTemporalNoise: (slice, step) in [0,1) from the R2
// sequence at the pixel's Hilbert index plus the frame's offset.
fn gtao_noise(c: vec2<i32>, frame_offset: u32) -> vec2<f32> {
    let cc = vec2<u32>(c) % vec2<u32>(GTAO_HILBERT_WIDTH, GTAO_HILBERT_WIDTH);
    let index = f32(gtao_hilbert_index(cc.x, cc.y) + frame_offset);
    return fract(vec2<f32>(0.5, 0.5) + index * vec2<f32>(0.7548776662466927, 0.5698402909980532));
}

fn gtao_round(x: f32) -> f32 {
    if x >= 0.0 {
        return floor(x + 0.5);
    }
    return -floor(-x + 0.5);
}

fn gtao_view_pos(
    depth: texture_2d<f32>,
    c: vec2<i32>,
    dims_i: vec2<i32>,
    tan_half_fov: f32,
    aspect: f32,
    near: f32,
    far: f32,
) -> vec3<f32> {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    // Reversed-Z uses raw=0 for the far/background sentinel; the stable
    // inverse maps it to `far`, so background remains outside nearby samples.
    let raw = textureLoad(depth, cc, 0).r;
    let view_z = linearize_depth(raw, near, far);
    let uv = (vec2<f32>(cc) + vec2<f32>(0.5, 0.5)) / vec2<f32>(dims_i);
    let ndc_x = uv.x * 2.0 - 1.0;
    let ndc_y = 1.0 - uv.y * 2.0;
    let view_x = ndc_x * tan_half_fov * aspect * view_z;
    let view_y = ndc_y * tan_half_fov * view_z;
    return vec3<f32>(view_x, view_y, view_z);
}

// The arc integral a(h) for one side of a slice.
fn gtao_integrate_arc(h: f32, n: f32) -> f32 {
    return 0.25 * (-cos(2.0 * h - n) + cos(n) + 2.0 * h * sin(n));
}

// a(h1) + a(h2) with both horizons fully open: what an unoccluded slice
// gives. It is not constant in `n`, so visibility divides by it instead of
// by the slice count, or flat ground darkens at grazing angles (BUG-y5w7).
fn gtao_full_ref(n: f32) -> f32 {
    return cos(n) + n * sin(n);
}

// Heightfield-mode position: orthographic frame, `depth` read as a raw
// height map, x/y in uv units (x aspect-corrected), z = (1 - raw) * relief.
fn gtao_height_pos(
    depth: texture_2d<f32>,
    c: vec2<i32>,
    dims_i: vec2<i32>,
    aspect: f32,
    relief: f32,
) -> vec3<f32> {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    let raw = textureLoad(depth, cc, 0).r;
    let uv = (vec2<f32>(cc) + vec2<f32>(0.5, 0.5)) / vec2<f32>(dims_i);
    return vec3<f32>(uv.x * aspect, 1.0 - uv.y, (1.0 - raw) * relief);
}

fn gtao_pos(
    depth: texture_2d<f32>,
    c: vec2<i32>,
    dims_i: vec2<i32>,
    tan_half_fov: f32,
    aspect: f32,
    near: f32,
    far: f32,
    hf: f32,
    relief: f32,
) -> vec3<f32> {
    if hf >= 0.5 {
        return gtao_height_pos(depth, c, dims_i, aspect, relief);
    }
    return gtao_view_pos(depth, c, dims_i, tan_half_fov, aspect, near, far);
}

// One side's horizon cosine after a tap: the tap's cosine faded toward the
// open horizon `low_cos` by the falloff weight, then the running maximum.
fn gtao_horizon(hcos: f32, d: vec3<f32>, view_vec: vec3<f32>, radius: f32, low_cos: f32) -> f32 {
    let len = length(d);
    if len <= 1e-5 {
        return hcos;
    }
    let weight = clamp((radius - len) / (GTAO_FALLOFF_RANGE * radius), 0.0, 1.0);
    return max(hcos, mix(low_cos, dot(d / len, view_vec), weight));
}

fn body(
    depth: texture_2d<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    radius: f32,
    intensity: f32,
    slices: f32,
    steps: f32,
    projection: u32,
    relief: f32,
    noise_per_frame: u32,
    fov_y: f32,
    near: f32,
    far: f32,
    noise_frame: u32,
) -> vec4<f32> {
    let reach = radius * GTAO_RADIUS_MULTIPLIER;
    let n_slices = max(1u, u32(gtao_round(slices)));
    let n_steps = max(1u, u32(gtao_round(steps)));
    let hf = select(0.0, 1.0, projection == 1u);
    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(uv * dims);
    let tan_half_fov = tan(fov_y * 0.5);
    let aspect = dims.x / dims.y;

    let p_c = gtao_pos(depth, c, dims_i, tan_half_fov, aspect, near, far, hf, relief);

    // Normal from ±1-texel central differences of the reconstructed positions.
    let p_xp = gtao_pos(depth, c + vec2<i32>(1, 0), dims_i, tan_half_fov, aspect, near, far, hf, relief);
    let p_xm = gtao_pos(depth, c - vec2<i32>(1, 0), dims_i, tan_half_fov, aspect, near, far, hf, relief);
    let p_yp = gtao_pos(depth, c + vec2<i32>(0, 1), dims_i, tan_half_fov, aspect, near, far, hf, relief);
    let p_ym = gtao_pos(depth, c - vec2<i32>(0, 1), dims_i, tan_half_fov, aspect, near, far, hf, relief);
    let ddx = p_xp - p_xm;
    let ddy = p_yp - p_ym;
    var normal = cross(ddx, ddy);
    let normal_len = length(normal);
    if normal_len > 1e-8 {
        normal = normal / normal_len;
    } else {
        normal = vec3<f32>(0.0, 0.0, -1.0);
    }

    // Heightfield mode: fixed ortho view direction and a uv-unit radius.
    var view_vec = vec3<f32>(0.0, 0.0, -1.0);
    var radius_px = reach * dims.y;
    if hf < 0.5 {
        view_vec = normalize(-p_c);
        let center_vz = max(p_c.z, 1e-4);
        radius_px = (reach / (tan_half_fov * center_vz)) * (dims.y * 0.5);
    }

    let noise = gtao_noise(c, select(0u, noise_frame, noise_per_frame != 0u));
    let rot = noise.x * GTAO_TAU;
    let step_offset = noise.y;

    var visibility_sum = 0.0;
    var baseline_sum = 0.0;
    for (var s: u32 = 0u; s < n_slices; s = s + 1u) {
        // Slices spread over the half circle, pi/N apart.
        let phi = rot * 0.5 + f32(s) * (2.0 * GTAO_HALF_PI / f32(n_slices));
        let dir2 = vec2<f32>(cos(phi), sin(phi));
        // The view-space direction of a +dir2 pixel step: Y flips because
        // the position reconstruction flips screen-down to view-up (BUG-y5w7).
        let dir3 = vec3<f32>(dir2.x, -dir2.y, 0.0);

        // The normal projected into the slice plane: its length and its
        // signed angle from the view vector.
        let axis = cross(dir3, view_vec);
        let axis_len = length(axis);
        var proj_len = 0.0;
        var n_signed = 0.0;
        if axis_len > 1e-6 {
            let axis_n = axis / axis_len;
            let proj_normal = normal - axis_n * dot(normal, axis_n);
            proj_len = length(proj_normal);
            if proj_len > 1e-6 {
                let cos_n = clamp(dot(proj_normal, view_vec) / proj_len, -1.0, 1.0);
                let ortho = dir3 - dot(dir3, view_vec) * view_vec;
                let sign_n = select(-1.0, 1.0, dot(ortho, proj_normal) >= 0.0);
                n_signed = sign_n * acos(cos_n);
            }
        }

        // The open horizon on each side, where a faded tap ends up.
        let low_plus = cos(n_signed + GTAO_HALF_PI);
        let low_minus = cos(n_signed - GTAO_HALF_PI);
        var hcos_minus = -1.0;
        var hcos_plus = -1.0;
        for (var j: u32 = 0u; j < n_steps; j = j + 1u) {
            let r_j = radius_px * (f32(j) + step_offset) / f32(n_steps) + GTAO_MIN_TAP_PX;

            let off_plus = vec2<i32>(vec2<f32>(gtao_round(dir2.x * r_j), gtao_round(dir2.y * r_j)));
            let c_plus = clamp(c + off_plus, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
            let s_plus = gtao_pos(depth, c_plus, dims_i, tan_half_fov, aspect, near, far, hf, relief);
            hcos_plus = gtao_horizon(hcos_plus, s_plus - p_c, view_vec, reach, low_plus);

            let off_minus = vec2<i32>(vec2<f32>(gtao_round(-dir2.x * r_j), gtao_round(-dir2.y * r_j)));
            let c_minus = clamp(c + off_minus, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
            let s_minus = gtao_pos(depth, c_minus, dims_i, tan_half_fov, aspect, near, far, hf, relief);
            hcos_minus = gtao_horizon(hcos_minus, s_minus - p_c, view_vec, reach, low_minus);
        }

        var h1 = -acos(clamp(hcos_minus, -1.0, 1.0));
        var h2 = acos(clamp(hcos_plus, -1.0, 1.0));
        h1 = n_signed + max(h1 - n_signed, -GTAO_HALF_PI);
        h2 = n_signed + min(h2 - n_signed, GTAO_HALF_PI);

        let arc = gtao_integrate_arc(h1, n_signed) + gtao_integrate_arc(h2, n_signed);
        visibility_sum = visibility_sum + proj_len * arc;
        baseline_sum = baseline_sum + proj_len * gtao_full_ref(n_signed);
    }

    let visibility = clamp(visibility_sum / max(baseline_sum, 1e-4), 0.0, 1.0);
    let ao = clamp(1.0 - intensity * (1.0 - visibility), 0.0, 1.0);
    return vec4<f32>(ao, ao, ao, 1.0);
}
