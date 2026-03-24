// Compute variant of mirror.wgsl — same math, no TBDR tile overhead.
// 0=Horizontal, 1=Vertical, 2=Both. Amount blends between original and mirrored.

struct Uniforms {
    amount: f32,
    mode: u32,
    _pad0: f32,
    _pad1: f32,
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

    var mirror_uv = uv;
    if uniforms.mode == 0u || uniforms.mode == 2u {
        mirror_uv.x = 0.5 - abs(uv.x - 0.5);
    }
    if uniforms.mode == 1u || uniforms.mode == 2u {
        mirror_uv.y = 0.5 - abs(uv.y - 0.5);
    }

    let mirrored = textureSampleLevel(source_tex, tex_sampler, mirror_uv, 0.0);

    let result = mix(src.rgb, mirrored.rgb, uniforms.amount);
    let alpha = mix(src.a, mirrored.a, uniforms.amount);

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, alpha));
}
