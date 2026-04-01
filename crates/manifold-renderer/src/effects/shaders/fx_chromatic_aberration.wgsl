// ChromaticAberration effect — radial or linear RGB channel separation.

struct Uniforms {
    amount: f32,
    mode: u32,       // 0=Radial, 1=Linear
    angle: f32,
    falloff: f32,
    offset: f32,
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

    let center = vec2<f32>(0.5, 0.5);
    let delta = uv - center;

    var dir: vec2<f32>;
    if uniforms.mode == 0u {
        // Radial mode: offset along direction from center
        let dist = length(delta);
        var radial_mask = smoothstep(0.0, 0.707, dist);
        radial_mask = mix(radial_mask, 1.0, 1.0 - uniforms.falloff);
        if dist > 1e-5 {
            dir = normalize(delta) * radial_mask;
        } else {
            dir = vec2<f32>(1.0, 0.0);
        }
    } else {
        // Linear mode: offset along fixed angle
        let rad = uniforms.angle * 0.01745329;
        dir = vec2<f32>(cos(rad), sin(rad));
    }

    let offset_r = dir * uniforms.offset;
    let offset_b = -dir * uniforms.offset;

    let r = textureSampleLevel(source_tex, tex_sampler, uv + offset_r, 0.0).r;
    let g = textureSampleLevel(source_tex, tex_sampler, uv, 0.0).g;
    let b = textureSampleLevel(source_tex, tex_sampler, uv + offset_b, 0.0).b;
    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    let effected = vec3<f32>(r, g, b);
    let result = mix(src.rgb, effected, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, src.a));
}
