// node.lic_integrate — Line Integral Convolution.
//
// For each pixel, walk N steps forward AND N steps backward along the
// normalised velocity field, accumulating `source.r` at each step with
// a triangular weight (1 - i/N). Final output = weighted_sum /
// total_weight, written to R; GBA = (0, 0, 1).
//
// Classic flow-visualisation technique. Two common source choices:
//   - Hash noise (`node.noise` Random) → streamline patterns
//     (the oily-fluid Lines mode).
//   - A scalar derived from the velocity / state itself → flow-aligned
//     intensity (the oily-fluid Flow Field mode samples
//     `length(color.rg)` as height-style source).
//
// Steps is loop-bounded — capped at 64 in-shader because WGSL compilers
// reject unbounded loops on most backends.
//
// `dt` is in pixels-per-step (resolution-independent; shader divides
// by dims).
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) tex_source
//   @binding(2) tex_velocity
//   @binding(3) tex_sampler
//   @binding(4) output_tex (rgba16float storage)

struct Uniforms {
    steps: u32,
    dt: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_source: texture_2d<f32>;
@group(0) @binding(2) var tex_velocity: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

const EPS_LEN: f32 = 1e-4;
const MAX_STEPS: u32 = 64u;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let inv = vec2<f32>(1.0) / vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + 0.5) * inv;
    let step_uv = uniforms.dt * inv;
    let steps = min(uniforms.steps, MAX_STEPS);

    var sum: f32 = textureSampleLevel(tex_source, tex_sampler, uv, 0.0).r;
    var w_total: f32 = 1.0;
    let inv_steps = 1.0 / f32(max(steps, 1u));

    var walker = uv;
    for (var i: u32 = 1u; i <= steps; i = i + 1u) {
        let v = textureSampleLevel(tex_velocity, tex_sampler, walker, 0.0).rg;
        let vn = v / max(length(v), EPS_LEN);
        walker = walker + vn * step_uv;
        let w = 1.0 - f32(i) * inv_steps;
        sum = sum + textureSampleLevel(tex_source, tex_sampler, walker, 0.0).r * w;
        w_total = w_total + w;
    }

    walker = uv;
    for (var i: u32 = 1u; i <= steps; i = i + 1u) {
        let v = textureSampleLevel(tex_velocity, tex_sampler, walker, 0.0).rg;
        let vn = v / max(length(v), EPS_LEN);
        walker = walker - vn * step_uv;
        let w = 1.0 - f32(i) * inv_steps;
        sum = sum + textureSampleLevel(tex_source, tex_sampler, walker, 0.0).r * w;
        w_total = w_total + w;
    }

    let acc = sum / max(w_total, EPS_LEN);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(acc, 0.0, 0.0, 1.0));
}
