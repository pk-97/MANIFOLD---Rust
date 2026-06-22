// node.color_ramp — map input luminance to a two-stop gradient
// (color_a at luma 0 → color_b at luma 1). The gradient-map atom
// (Blender ColorRamp / TD Lookup with two stops). For richer multi-stop
// palettes (thermal, etc.) use node.lut1d with a supplied LUT texture.

struct Uniforms {
    color_a: vec4<f32>,
    color_b: vec4<f32>,
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
    // Input is premultiplied alpha — unpremultiply to read the true colour
    // for the ramp index. A transparent pixel has no defined colour, so it
    // maps to luma 0 (and is masked back out below).
    let straight_rgb = select(vec3<f32>(0.0), src.rgb / max(src.a, 1e-4), src.a > 1e-4);
    let luma = clamp(dot(straight_rgb, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
    let ramp = mix(uniforms.color_a, uniforms.color_b, luma);
    // Preserve input coverage: a transparent input pixel stays transparent so
    // the gradient map keys over the layer below instead of painting color_a
    // as an opaque box. Output premultiplied (rgb * a). For an opaque input
    // with opaque stops this reduces to the old `mix(color_a, color_b, luma)`,
    // and for data fields (which carry alpha = 1) it is a no-op.
    let out_a = src.a * ramp.a;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(ramp.rgb * out_a, out_a));
}
