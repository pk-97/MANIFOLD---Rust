// HDR Boost — sharp highlight extraction + gain, no blur.
// Smoothstep threshold selects bright areas; knee controls transition width.
// Pushes highlights into HDR range cleanly without halation.

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

    // Highlight selection: smoothstep from (threshold - knee) to (threshold + knee).
    // Knee=0 gives a hard cutoff, knee=1 gives a wide gradual ramp.
    let lum = max(c.r, max(c.g, c.b));
    let half_knee = uniforms.knee * 0.5;
    let lo = uniforms.threshold - half_knee;
    let hi = uniforms.threshold + half_knee;
    let response = smoothstep(lo, hi + 1e-5, lum);

    // Boost highlights and add back to source.
    let boosted = c + c * response * uniforms.gain;

    let result = mix(c, boosted, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(max(result, vec3<f32>(0.0)), src.a));
}
