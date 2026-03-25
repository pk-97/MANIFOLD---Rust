// Compute variant of reaction_diffusion_display.wgsl.
// Identical math — only I/O mechanism changes:
//   - textureSampleLevel instead of textureSample
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment
//
// NOTE: The fragment shader uses dpdx/dpdy for edge detection. In the compute
// variant we use finite differences at 1-pixel spacing, which produces the
// same result as hardware derivatives (dpdx/dpdy = 1-pixel forward difference).

struct Uniforms {
    uv_scale: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var state_tex: texture_2d<f32>;
@group(0) @binding(2) var state_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv_raw = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // Center and apply scale
    let uv = (uv_raw - vec2<f32>(0.5)) * u.uv_scale + vec2<f32>(0.5);
    let c = textureSampleLevel(state_tex, state_sampler, uv, 0.0);
    let b_val = c.g;

    var lum = smoothstep(0.0, 0.4, b_val);

    // Edge highlight via finite differences (replaces fragment dpdx/dpdy).
    // Sample adjacent pixels to compute screen-space gradient — equivalent
    // to hardware derivatives at 1-pixel spacing.
    let pixel_step = 1.0 / vec2<f32>(dims);
    let b_right = textureSampleLevel(state_tex, state_sampler,
        (uv_raw + vec2<f32>(pixel_step.x, 0.0) - vec2<f32>(0.5)) * u.uv_scale + vec2<f32>(0.5), 0.0).g;
    let b_down = textureSampleLevel(state_tex, state_sampler,
        (uv_raw + vec2<f32>(0.0, pixel_step.y) - vec2<f32>(0.5)) * u.uv_scale + vec2<f32>(0.5), 0.0).g;
    let ddx_b = b_right - b_val;
    let ddy_b = b_down - b_val;
    let edge = length(vec2<f32>(ddx_b, ddy_b)) * 40.0;
    lum += edge * 0.2;

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(lum, lum, lum, lum));
}
