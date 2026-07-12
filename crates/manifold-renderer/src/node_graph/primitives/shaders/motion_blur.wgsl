// node.motion_blur — hand parity oracle for the generated standalone
// kernel (docs/CINEMATIC_POST_DESIGN.md D4). Same velocity-directed gather
// formula as motion_blur_body.wgsl — kept independent (not sharing Rust
// source) so the gpu_tests parity check is a real cross-check, not a
// tautology.
//
// Bindings match the generated MultiInputCoincident layout (`in` Gather,
// `velocity` CoincidentTexel, in INPUTS declaration order): uniform(0),
// in_tex(1, sampled), velocity_tex(2, textureLoad — no sampler), samp(3),
// output_tex(4).

struct Uniforms {
    max_blur_px: f32,
    shutter_angle: f32,
    _pad0: f32,
    _pad1: f32,
}

const MOTION_BLUR_SAMPLES: u32 = 8u;

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var in_tex: texture_2d<f32>;
@group(0) @binding(2) var velocity_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }

    let dims_f = vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + vec2<f32>(0.5, 0.5)) / dims_f;

    let c_velocity = textureLoad(velocity_tex, vec2<i32>(id.xy), 0);
    let velocity_ndc = c_velocity.rg;
    let smear_px_raw = velocity_ndc * 0.5 * dims_f * (u.shutter_angle / 360.0);
    let smear_px = clamp(smear_px_raw, vec2<f32>(-u.max_blur_px), vec2<f32>(u.max_blur_px));
    let smear_uv = smear_px / dims_f;

    var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    for (var i: u32 = 0u; i < MOTION_BLUR_SAMPLES; i = i + 1u) {
        let t = f32(i) / f32(MOTION_BLUR_SAMPLES - 1u) - 0.5;
        acc = acc + textureSampleLevel(in_tex, samp, uv + smear_uv * t, 0.0);
    }
    let result = acc / f32(MOTION_BLUR_SAMPLES);

    textureStore(output_tex, vec2<i32>(id.xy), result);
}
