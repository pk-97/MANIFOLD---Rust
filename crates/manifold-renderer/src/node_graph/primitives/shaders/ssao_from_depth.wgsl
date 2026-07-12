// node.ssao_from_depth — hand parity oracle for the generated standalone
// kernel (docs/CINEMATIC_POST_DESIGN.md D3). Same SSAO algorithm as
// ssao_from_depth_body.wgsl — kept independent (not sharing WGSL source) so
// the gpu_tests parity check is a real cross-check, not a tautology.
//
// Bindings match the generated GatherTexel-only layout: uniform(0),
// depth_tex(1, textureLoad — no sampler), output_tex(2).

struct Uniforms {
    radius: f32,
    intensity: f32,
    bias: f32,
    fov_y: f32,
    near: f32,
    far: f32,
    _pad0: f32,
    _pad1: f32,
}

const SSAO_N: u32 = 16u;
const SSAO_GOLDEN_ANGLE: f32 = 2.399963;

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

    let p_c = view_pos(c, dims_i, tan_half_fov, aspect, u.near, u.far);
    let p_xp = view_pos(c + vec2<i32>(1, 0), dims_i, tan_half_fov, aspect, u.near, u.far);
    let p_xm = view_pos(c - vec2<i32>(1, 0), dims_i, tan_half_fov, aspect, u.near, u.far);
    let p_yp = view_pos(c + vec2<i32>(0, 1), dims_i, tan_half_fov, aspect, u.near, u.far);
    let p_ym = view_pos(c - vec2<i32>(0, 1), dims_i, tan_half_fov, aspect, u.near, u.far);

    let ddx = p_xp - p_xm;
    let ddy = p_yp - p_ym;
    var normal = cross(ddx, ddy);
    let normal_len = length(normal);
    if normal_len > 1e-8 {
        normal = normal / normal_len;
    } else {
        normal = vec3<f32>(0.0, 0.0, -1.0);
    }

    let up_ref = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(normal.y) > 0.999);
    let tangent = normalize(cross(up_ref, normal));
    let bitangent = cross(normal, tangent);

    let rot = hash_angle(vec2<f32>(c));

    var occlusion = 0.0;
    for (var i: u32 = 0u; i < SSAO_N; i = i + 1u) {
        let r = sqrt((f32(i) + 0.5) / f32(SSAO_N));
        let theta = f32(i) * SSAO_GOLDEN_ANGLE + rot;
        let disc_x = r * cos(theta);
        let disc_y = r * sin(theta);
        let disc_z = sqrt(max(0.0, 1.0 - r * r));
        let kernel_vec = tangent * disc_x + bitangent * disc_y + normal * disc_z;
        let sample_pos = p_c + kernel_vec * u.radius;

        let vz = max(sample_pos.z, 1e-4);
        let denom = tan_half_fov * vz;
        let sample_ndc_x = sample_pos.x / (aspect * denom);
        let sample_ndc_y = sample_pos.y / denom;
        let sample_uv = vec2<f32>(sample_ndc_x * 0.5 + 0.5, (1.0 - sample_ndc_y) * 0.5);
        let sample_c = clamp(vec2<i32>(sample_uv * vec2<f32>(dims_i)), vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));

        let scene_raw = textureLoad(depth_tex, sample_c, 0).r;
        let scene_view_z = linearize_depth(scene_raw, u.near, u.far);

        let occluded = select(0.0, 1.0, scene_view_z <= sample_pos.z - u.bias);
        let range_ok = select(0.0, 1.0, abs(p_c.z - scene_view_z) < u.radius);
        occlusion = occlusion + occluded * range_ok;
    }

    let ao = clamp(1.0 - u.intensity * occlusion / f32(SSAO_N), 0.0, 1.0);
    textureStore(output_tex, c, vec4<f32>(ao, ao, ao, 1.0));
}
