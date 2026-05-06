// primitive.blur — single-pass 2D blur (Gaussian / Box).
//
// V1 takes the simple route: one dispatch sampling an N×N kernel
// centered on the output pixel. Cost is O(r²) per pixel which is
// acceptable for the V1 radius range (0..32) on typical layer
// resolutions. A separable two-pass variant lands when fusion does.
//
// Modes:
//   0 = Gaussian — exp(-(dx² + dy²) / (2σ²)), σ = radius / 2.
//   1 = Box      — uniform weight across the kernel.
//   2 = Radial   — V1 falls back to Gaussian. A real radial/zoom
//                  blur lands later; reserving the slot keeps the
//                  param enum stable.
//
// Bindings (canonical layout for one-texture-input primitives):
//   @binding(0) uniforms
//   @binding(1) tex_source
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    radius: f32,
    mode: u32,
    _pad0: f32,
    _pad1: f32,
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
    if r_int <= 0 {
        // Pass-through — no blur, just sample center.
        let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
        textureStore(
            output_tex,
            vec2<i32>(id.xy),
            textureSampleLevel(tex_source, tex_sampler, uv, 0.0),
        );
        return;
    }

    let inv_dims = 1.0 / vec2<f32>(dims);
    let center_uv = (vec2<f32>(id.xy) + 0.5) * inv_dims;

    // sigma chosen so the Gaussian falls to ~13% of peak at the kernel
    // edge. Hardcoded to radius/2 — fine for a soft, even falloff.
    let sigma = max(uniforms.radius * 0.5, 0.5);
    let two_sigma_sq = 2.0 * sigma * sigma;

    var sum = vec4<f32>(0.0);
    var weight_sum = 0.0;

    for (var dy: i32 = -r_int; dy <= r_int; dy = dy + 1) {
        for (var dx: i32 = -r_int; dx <= r_int; dx = dx + 1) {
            let offset = vec2<f32>(f32(dx), f32(dy)) * inv_dims;
            let uv = center_uv + offset;
            let s = textureSampleLevel(tex_source, tex_sampler, uv, 0.0);

            var w: f32;
            if uniforms.mode == 1u {
                // Box — uniform weight.
                w = 1.0;
            } else {
                // Gaussian (and Radial fallback for V1).
                let dist_sq = f32(dx * dx + dy * dy);
                w = exp(-dist_sq / two_sigma_sq);
            }
            sum = sum + s * w;
            weight_sum = weight_sum + w;
        }
    }

    let result = sum / max(weight_sum, 1e-6);
    textureStore(output_tex, vec2<i32>(id.xy), result);
}
