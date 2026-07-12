// node.coc_from_depth — hand parity oracle for the generated standalone
// kernel (docs/CINEMATIC_POST_DESIGN.md D1). Same thin-lens CoC formula as
// coc_from_depth_body.wgsl — kept independent (not sharing Rust source) so
// the gpu_tests parity check is a real cross-check, not a tautology.
//
// Bindings match the generated CoincidentTexel-only layout: uniform(0),
// depth_tex(1, textureLoad — no sampler), output_tex(2).

struct Uniforms {
    max_radius: f32,
    fov_y: f32,
    near: f32,
    far: f32,
    focus_distance: f32,
    f_stop: f32,
    _pad0: f32,
    _pad1: f32,
}

const SENSOR_H_MM: f32 = 24.0;
const WORLD_TO_MM: f32 = 1000.0;

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var depth_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

fn linearize_depth(raw: f32, near: f32, far: f32) -> f32 {
    let range = far / (near - far);
    return (range * near) / (raw + range);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }

    let raw_depth = textureLoad(depth_tex, vec2<i32>(id.xy), 0).r;

    let f_mm = SENSOR_H_MM / (2.0 * tan(u.fov_y * 0.5));
    let a_mm = f_mm / u.f_stop;
    let d_mm = linearize_depth(raw_depth, u.near, u.far) * WORLD_TO_MM;
    let s_mm = u.focus_distance * WORLD_TO_MM;
    let coc_mm = a_mm * f_mm * abs(d_mm - s_mm) / (d_mm * max(s_mm - f_mm, 1.0));
    let coc_px = clamp(coc_mm / SENSOR_H_MM * f32(dims.y), 0.0, u.max_radius);
    let normalized = coc_px / u.max_radius;

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(normalized, normalized, normalized, 1.0));
}
