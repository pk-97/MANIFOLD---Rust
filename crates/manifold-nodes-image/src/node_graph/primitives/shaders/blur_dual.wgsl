// node.blur, Smooth mode — one Dual Kawase pyramid operation.
//
// At an even 2:1 ratio the down kernel is the five-tap Dual Kawase equivalent
// of a normalized 2x2 box; odd ratios use exact area overlaps. The up kernel
// has eight equally weighted taps. Both kernels are explicitly normalized and
// operate on vec4<f32>, so HDR values and alpha take the same path without a
// hidden clamp or divide.
// The CPU dispatches this shader for each fixed pyramid level, then uses the
// blend operation to interpolate adjacent reconstructions as radius changes.

struct Uniforms {
    /// 0 = exact-area downsample, 1 = upsample 8, 2 = copy, 3 = lerp two images.
    operation: u32,
    _pad0: u32,
    blend: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_source: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var tex_source_b: texture_2d<f32>;

fn sample_at(tex: texture_2d<f32>, uv: vec2<f32>, offset: vec2<f32>) -> vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(tex));
    return textureSampleLevel(tex, tex_sampler, uv + offset * texel, 0.0);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    var value = vec4<f32>(0.0);

    if uniforms.operation == 0u {
        // Exact area downsample. At an even 2:1 ratio this is the same
        // normalized 2x2 box as the five-tap Dual Kawase kernel. For odd
        // dimensions the footprint is fractional, so up to 3x3 integer
        // texels are weighted by their actual intersection area; normalized
        // UV bilinear taps otherwise lose impulse mass at 65->32, etc.
        let source_dims = vec2<f32>(textureDimensions(tex_source));
        let output_dims = vec2<f32>(dims);
        let area_min = vec2<f32>(id.xy) * source_dims / output_dims;
        let area_max = vec2<f32>(id.xy + vec2<u32>(1u, 1u)) * source_dims / output_dims;
        let first = vec2<i32>(floor(area_min));
        let last = vec2<i32>(ceil(area_max)) - vec2<i32>(1);
        let source_dims_i = vec2<i32>(textureDimensions(tex_source));
        var weight_sum = 0.0;
        for (var y: i32 = 0; y < 3; y = y + 1) {
            for (var x: i32 = 0; x < 3; x = x + 1) {
                let px = first.x + x;
                let py = first.y + y;
                if px <= last.x && py <= last.y && px >= 0 && py >= 0
                    && px < source_dims_i.x && py < source_dims_i.y {
                    let overlap_x = min(area_max.x, f32(px + 1)) - max(area_min.x, f32(px));
                    let overlap_y = min(area_max.y, f32(py + 1)) - max(area_min.y, f32(py));
                    let weight = max(overlap_x, 0.0) * max(overlap_y, 0.0);
                    value = value + textureLoad(tex_source, vec2<i32>(px, py), 0) * weight;
                    weight_sum = weight_sum + weight;
                }
            }
        }
        value = value / max(weight_sum, 1e-6);
    } else if uniforms.operation == 1u {
        // Eight taps, normalized. The half-texel offsets avoid a directional
        // bias while the clamp sampler handles 1-pixel and odd dimensions.
        value = sample_at(tex_source, uv, vec2<f32>(-0.5, -1.5));
        value = value + sample_at(tex_source, uv, vec2<f32>(0.5, -1.5));
        value = value + sample_at(tex_source, uv, vec2<f32>(-1.5, -0.5));
        value = value + sample_at(tex_source, uv, vec2<f32>(1.5, -0.5));
        value = value + sample_at(tex_source, uv, vec2<f32>(-1.5, 0.5));
        value = value + sample_at(tex_source, uv, vec2<f32>(1.5, 0.5));
        value = value + sample_at(tex_source, uv, vec2<f32>(-0.5, 1.5));
        value = value + sample_at(tex_source, uv, vec2<f32>(0.5, 1.5));
        value = value / 8.0;
    } else if uniforms.operation == 3u {
        let a = textureSampleLevel(tex_source, tex_sampler, uv, 0.0);
        let b = textureSampleLevel(tex_source_b, tex_sampler, uv, 0.0);
        value = mix(a, b, clamp(uniforms.blend, 0.0, 1.0));
    } else {
        // Copy is used for radius zero and preserves all four channels.
        value = textureSampleLevel(tex_source, tex_sampler, uv, 0.0);
    }

    textureStore(output_tex, vec2<i32>(id.xy), value);
}
