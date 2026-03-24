// Compute variant of fx_quad_mirror.wgsl — same math, no TBDR tile overhead.
// Mirrors UVs around center in both axes, crossfades with original.

struct Uniforms {
    amount: f32,
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
    let original = src.rgb;

    // Mirror UVs around center in both axes
    var mirror_uv: vec2<f32>;
    mirror_uv.x = 0.5 - abs(uv.x - 0.5);
    mirror_uv.y = 0.5 - abs(uv.y - 0.5);

    // Scale back to full range (0-0.5 -> 0-1)
    mirror_uv = mirror_uv * 2.0;

    let mirror_sample = textureSampleLevel(source_tex, tex_sampler, mirror_uv, 0.0);
    let mirrored = mirror_sample.rgb;

    // 0.0->0.5: mirrors fade in, original stays at 100%
    // 0.5->1.0: original fades out, mirrors stay at 100%
    let orig_alpha = clamp((1.0 - uniforms.amount) * 2.0, 0.0, 1.0);
    let mir_alpha = clamp(uniforms.amount * 2.0, 0.0, 1.0);
    let result = clamp(original * orig_alpha + mirrored * mir_alpha, vec3<f32>(0.0), vec3<f32>(1.0));
    let a = clamp(src.a * orig_alpha + mirror_sample.a * mir_alpha, 0.0, 1.0);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, a));
}
