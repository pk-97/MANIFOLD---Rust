// HDR Boost — sharp highlight extraction + gain, no blur.
// Same soft-knee threshold as bloom's bright_prefilter, but applied per-pixel
// without any blur passes. Pushes bright areas into HDR range cleanly.

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

    // Soft-knee highlight extraction — identical math to bloom's bright_prefilter.
    let lum = max(c.r, max(c.g, c.b));
    let soft_start = uniforms.threshold - uniforms.knee;
    var t = clamp((lum - soft_start) / max(2.0 * uniforms.knee, 1e-5), 0.0, 1.0);
    t = t * t * (3.0 - 2.0 * t);  // smoothstep
    let hard = clamp((lum - uniforms.threshold) / max(1.0 - uniforms.threshold, 1e-5), 0.0, 1.0);
    let response = max(t * 0.78, hard);

    // Extract highlights, boost by gain, add back to source.
    let highlights = c * response;
    let boosted = c + highlights * uniforms.gain;

    let result = mix(c, boosted, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(max(result, vec3<f32>(0.0)), src.a));
}
