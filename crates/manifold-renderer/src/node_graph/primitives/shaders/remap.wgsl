// node.remap — resample source_tex at the absolute UV coordinates carried
// in uv_field_tex's R/G channels. out(p) = source(wrap(uv_field(p).rg)).
// The generic UV-warp / TD Remap TOP.

struct Uniforms {
    wrap: u32, // 0 = Clamp, 1 = Repeat, 2 = Mirror
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var uv_field_tex: texture_2d<f32>;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

fn wrap_coord(t: f32, mode: u32) -> f32 {
    if mode == 1u {
        // Repeat: fract keeps the fractional part in [0, 1).
        return fract(t);
    }
    if mode == 2u {
        // Mirror: triangle wave, period 2, peak 1.
        let m = fract(t * 0.5) * 2.0;
        return 1.0 - abs(1.0 - m);
    }
    // Clamp (default).
    return clamp(t, 0.0, 1.0);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let sample_uv = textureSampleLevel(uv_field_tex, tex_sampler, uv, 0.0).rg;
    let wrapped = vec2<f32>(
        wrap_coord(sample_uv.x, uniforms.wrap),
        wrap_coord(sample_uv.y, uniforms.wrap),
    );
    let result = textureSampleLevel(source_tex, tex_sampler, wrapped, 0.0);
    textureStore(output_tex, vec2<i32>(id.xy), result);
}
