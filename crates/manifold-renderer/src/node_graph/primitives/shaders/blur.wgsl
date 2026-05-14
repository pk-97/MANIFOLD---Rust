// node.blur — separable 1D blur (Gaussian / Box).
//
// Run twice per evaluate(): once horizontally (direction = (1, 0)),
// once vertically (direction = (0, 1)), with a scratch texture
// in between. Cost is 2 × (2r + 1) per pixel — linear in radius
// instead of the quadratic O(r²) of a single 2D pass.
//
// Modes:
//   0 = Gaussian — exp(-d² / (2σ²)), σ = radius / 2.
//   1 = Box      — uniform weight across the kernel.
//   2 = Radial   — V1 fallback to Gaussian; reserving the slot
//                  keeps the param enum stable.
//
// Bindings (canonical layout for one-texture-input primitives):
//   @binding(0) uniforms
//   @binding(1) tex_source
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    radius: f32,
    mode: u32,
    /// Sample direction in pixel-space: (1, 0) = horizontal pass,
    /// (0, 1) = vertical pass. Anything else compiles but reads
    /// off-axis and is not what you want.
    direction: vec2<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_source: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

const MAX_RADIUS: i32 = 32;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }

    let r_int = min(i32(uniforms.radius), MAX_RADIUS);
    let inv_dims = 1.0 / vec2<f32>(dims);
    let center_uv = (vec2<f32>(id.xy) + 0.5) * inv_dims;

    if r_int <= 0 {
        textureStore(
            output_tex,
            vec2<i32>(id.xy),
            textureSampleLevel(tex_source, tex_sampler, center_uv, 0.0),
        );
        return;
    }

    let sigma = max(uniforms.radius * 0.5, 0.5);
    let two_sigma_sq = 2.0 * sigma * sigma;
    let step_uv = uniforms.direction * inv_dims;

    var sum = vec4<f32>(0.0);
    var weight_sum = 0.0;

    for (var d: i32 = -r_int; d <= r_int; d = d + 1) {
        let uv = center_uv + step_uv * f32(d);
        let s = textureSampleLevel(tex_source, tex_sampler, uv, 0.0);

        var w: f32;
        if uniforms.mode == 1u {
            w = 1.0;
        } else {
            // Gaussian (and Radial fallback).
            let dist_sq = f32(d * d);
            w = exp(-dist_sq / two_sigma_sq);
        }
        sum = sum + s * w;
        weight_sum = weight_sum + w;
    }

    textureStore(output_tex, vec2<i32>(id.xy), sum / max(weight_sum, 1e-6));
}
