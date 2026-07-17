// node.ssao_gtao — hand parity oracle for the generated standalone kernel
// (docs/CINEMATIC_POST_DESIGN.md D9(a)). Same GTAO algorithm as
// ssao_gtao_body.wgsl — kept independent (not sharing WGSL source) so the
// gpu_tests parity check is a real cross-check, not a tautology.
//
// Bindings match the generated GatherTexel-only layout: uniform(0),
// depth_tex(1, textureLoad — no sampler), output_tex(2).

struct Uniforms {
    radius: f32,
    intensity: f32,
    slices: f32,
    steps: f32,
    projection: u32,
    relief: f32,
    fov_y: f32,
    near: f32,
    far: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

const GTAO_HALF_PI: f32 = 1.5707963267948966;

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var depth_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

fn linearize_depth(raw: f32, near: f32, far: f32) -> f32 {
    let range = far / (near - far);
    return (range * near) / (raw + range);
}

fn hash_angle(px: vec2<f32>) -> f32 {
    return fract(sin(dot(px, vec2<f32>(12.9898, 78.233))) * 43758.5453) * 6.283185307;
}

fn gtao_round(x: f32) -> f32 {
    if x >= 0.0 {
        return floor(x + 0.5);
    }
    return -floor(-x + 0.5);
}

fn view_pos(c: vec2<i32>, dims_i: vec2<i32>, tan_half_fov: f32, aspect: f32, near: f32, far: f32) -> vec3<f32> {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    let raw = textureLoad(depth_tex, cc, 0).r;
    let view_z = linearize_depth(raw, near, far);
    let uv = (vec2<f32>(cc) + vec2<f32>(0.5, 0.5)) / vec2<f32>(dims_i);
    let ndc_x = uv.x * 2.0 - 1.0;
    let ndc_y = 1.0 - uv.y * 2.0;
    let view_x = ndc_x * tan_half_fov * aspect * view_z;
    let view_y = ndc_y * tan_half_fov * view_z;
    return vec3<f32>(view_x, view_y, view_z);
}

fn integrate_arc(h: f32, n: f32) -> f32 {
    return 0.25 * (-cos(2.0 * h - n) + cos(n) + 2.0 * h * sin(n));
}

fn height_pos(c: vec2<i32>, dims_i: vec2<i32>, aspect: f32, relief: f32) -> vec3<f32> {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    let raw = textureLoad(depth_tex, cc, 0).r;
    let uv = (vec2<f32>(cc) + vec2<f32>(0.5, 0.5)) / vec2<f32>(dims_i);
    return vec3<f32>(uv.x * aspect, 1.0 - uv.y, (1.0 - raw) * relief);
}

fn pos_dispatch(c: vec2<i32>, dims_i: vec2<i32>, tan_half_fov: f32, aspect: f32, near: f32, far: f32, hf: f32, relief: f32) -> vec3<f32> {
    if hf >= 0.5 {
        return height_pos(c, dims_i, aspect, relief);
    }
    return view_pos(c, dims_i, tan_half_fov, aspect, near, far);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }
    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(id.xy);
    let tan_half_fov = tan(u.fov_y * 0.5);
    let aspect = f32(dims.x) / f32(dims.y);

    let hf = select(0.0, 1.0, u.projection == 1u);
    let p_c = pos_dispatch(c, dims_i, tan_half_fov, aspect, u.near, u.far, hf, u.relief);

    let p_xp = pos_dispatch(c + vec2<i32>(1, 0), dims_i, tan_half_fov, aspect, u.near, u.far, hf, u.relief);
    let p_xm = pos_dispatch(c - vec2<i32>(1, 0), dims_i, tan_half_fov, aspect, u.near, u.far, hf, u.relief);
    let p_yp = pos_dispatch(c + vec2<i32>(0, 1), dims_i, tan_half_fov, aspect, u.near, u.far, hf, u.relief);
    let p_ym = pos_dispatch(c - vec2<i32>(0, 1), dims_i, tan_half_fov, aspect, u.near, u.far, hf, u.relief);
    let ddx = p_xp - p_xm;
    let ddy = p_yp - p_ym;
    var normal = cross(ddx, ddy);
    let normal_len = length(normal);
    if normal_len > 1e-8 {
        normal = normal / normal_len;
    } else {
        normal = vec3<f32>(0.0, 0.0, -1.0);
    }

    var view_vec = vec3<f32>(0.0, 0.0, -1.0);
    var radius_px = u.radius * f32(dims.y);
    if hf < 0.5 {
        view_vec = normalize(-p_c);
        let center_vz = max(p_c.z, 1e-4);
        radius_px = (u.radius / (tan_half_fov * center_vz)) * (f32(dims.y) * 0.5);
    }

    let rot = hash_angle(vec2<f32>(c));

    let n_slices = max(1u, u32(gtao_round(u.slices)));
    let n_steps = max(1u, u32(gtao_round(u.steps)));

    var visibility_sum = 0.0;
    for (var s: u32 = 0u; s < n_slices; s = s + 1u) {
        let phi = rot * 0.5 + f32(s) * (2.0 * GTAO_HALF_PI / f32(n_slices));
        let dir2 = vec2<f32>(cos(phi), sin(phi));
        let dir3 = vec3<f32>(dir2, 0.0);

        let axis = cross(dir3, view_vec);
        let axis_len = length(axis);
        var proj_normal = normal;
        var proj_len = 0.0;
        var n_signed = 0.0;
        if axis_len > 1e-6 {
            let axis_n = axis / axis_len;
            proj_normal = normal - axis_n * dot(normal, axis_n);
            proj_len = length(proj_normal);
            if proj_len > 1e-6 {
                let cos_n = clamp(dot(proj_normal, view_vec) / proj_len, -1.0, 1.0);
                let ortho = dir3 - dot(dir3, view_vec) * view_vec;
                let sign_n = select(-1.0, 1.0, dot(ortho, proj_normal) >= 0.0);
                n_signed = sign_n * acos(cos_n);
            }
        }

        var hcos_minus = -1.0;
        var hcos_plus = -1.0;
        for (var j: u32 = 0u; j < n_steps; j = j + 1u) {
            let r_j = radius_px * (f32(j) + 1.0) / f32(n_steps);

            let off_plus = vec2<i32>(vec2<f32>(gtao_round(dir2.x * r_j), gtao_round(dir2.y * r_j)));
            let c_plus = clamp(c + off_plus, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
            let s_plus = pos_dispatch(c_plus, dims_i, tan_half_fov, aspect, u.near, u.far, hf, u.relief);
            let d_plus = s_plus - p_c;
            let len_plus = length(d_plus);
            if len_plus > 1e-5 && len_plus <= u.radius {
                hcos_plus = max(hcos_plus, dot(d_plus / len_plus, view_vec));
            }

            let off_minus = vec2<i32>(vec2<f32>(gtao_round(-dir2.x * r_j), gtao_round(-dir2.y * r_j)));
            let c_minus = clamp(c + off_minus, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
            let s_minus = pos_dispatch(c_minus, dims_i, tan_half_fov, aspect, u.near, u.far, hf, u.relief);
            let d_minus = s_minus - p_c;
            let len_minus = length(d_minus);
            if len_minus > 1e-5 && len_minus <= u.radius {
                hcos_minus = max(hcos_minus, dot(d_minus / len_minus, view_vec));
            }
        }

        var h1 = -acos(clamp(hcos_minus, -1.0, 1.0));
        var h2 = acos(clamp(hcos_plus, -1.0, 1.0));
        h1 = n_signed + max(h1 - n_signed, -GTAO_HALF_PI);
        h2 = n_signed + min(h2 - n_signed, GTAO_HALF_PI);

        let arc = integrate_arc(h1, n_signed) + integrate_arc(h2, n_signed);
        visibility_sum = visibility_sum + proj_len * arc;
    }

    let visibility = clamp(visibility_sum / f32(n_slices), 0.0, 1.0);
    let ao = clamp(1.0 - u.intensity * (1.0 - visibility), 0.0, 1.0);
    textureStore(output_tex, c, vec4<f32>(ao, ao, ao, 1.0));
}
