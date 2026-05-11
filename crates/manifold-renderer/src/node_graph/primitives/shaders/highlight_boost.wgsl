// primitive.highlight_boost — pixel-exact replacement for legacy
// `effects/shaders/hdr_boost_compute.wgsl`. Excess-above-threshold
// boost with EV-stop gain, color-ratio-preserving. Bindings, math,
// and dispatch shape preserved verbatim.

struct Uniforms {
    amount: f32,
    gain: f32,
    threshold: f32,
    knee: f32,
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
    let c = src.rgb;

    let lum = max(c.r, max(c.g, c.b));
    let half_knee = uniforms.knee * 0.5;
    let lo = uniforms.threshold - half_knee;
    let hi = uniforms.threshold + half_knee;

    let soft = smoothstep(lo, hi + 1e-5, lum);
    let excess = soft * max(lum - uniforms.threshold, 0.0);

    let boost = excess * (pow(2.0, uniforms.gain) - 1.0);
    let scale = 1.0 + boost / max(lum, 1e-5);
    let boosted = c * scale;

    let result = mix(c, boosted, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(max(result, vec3<f32>(0.0)), src.a));
}
