// node.posterize — quantize each RGB channel to `levels` discrete steps,
// rounding to the nearest level with endpoints included. Alpha passes
// through unchanged.

struct Uniforms {
    levels: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    let n = max(floor(uniforms.levels), 2.0);
    let steps = n - 1.0;
    // round to nearest level, endpoints preserved.
    let q = round(clamp(src.rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * steps) / steps;

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(q, src.a));
}
