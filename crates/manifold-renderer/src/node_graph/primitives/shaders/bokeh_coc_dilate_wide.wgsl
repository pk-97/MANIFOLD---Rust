// node.bokeh_gather — internal far-field CoC dilation (BOKEH_LAYERED_DOF_DESIGN.md P2).
//
// Separable max-dilation of the signed CoC field so in-focus pixels near a
// defocused far-side region start gathering and the halo feathers outward.
// Reads the R channel ONLY where G == 0 (far side + in-focus). Near-field
// pixels (G == 1) are ignored, so near bokeh cannot leak into the far field.
// Two dispatches per enabled frame: H pass writes a temp; V pass reads it.
//
// The footprint is a full-radius window in the chosen axis, so a far CoC of
// radius `max_radius` can influence background pixels up to `max_radius` away
// — the structural rim fix. Output: R = dilated far CoC magnitude, G/B = 0,
// A = 1. The gather body only consumes R, so the other channels are padding.
//
// Bindings: uniform(0) [max_radius, direction], src(1), samp(2), dst(3, rgba16float write).

struct Uniforms {
    max_radius: f32,
    direction: u32, // 0 = horizontal, 1 = vertical
    decay: f32,      // distance-decay per half-res tap (px_per_tap / max_radius)
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims_i = textureDimensions(dst);
    if id.x >= u32(dims_i.x) || id.y >= u32(dims_i.y) {
        return;
    }

    let dims = vec2<f32>(f32(dims_i.x), f32(dims_i.y));
    let uv = (vec2<f32>(f32(id.x), f32(id.y)) + vec2<f32>(0.5)) / dims;
    let texel = 1.0 / dims;

    // Sample interval along the chosen axis (H: x, V: y).
    let step = select(vec2<f32>(0.0, texel.y), vec2<f32>(texel.x, 0.0), u.direction == 0u);

    var far_coc: f32 = 0.0;

    // Brute-force separable max over [-max_radius, +max_radius] px.
    // The gather's own disc falloff dominates the visible falloff, so the
    // square-vs-disc footprint error of separable max is accepted per D3.
    // Round to the nearest pixel count — the CPU reference rounds too, and
    // truncation would shrink the window by a tap at fractional max_radius.
    let radius_px = i32(u.max_radius + 0.5);
    for (var i: i32 = -radius_px; i <= radius_px; i = i + 1) {
        let tap_uv = uv + step * f32(i);
        let sample = textureSampleLevel(src, samp, tap_uv, 0.0);
        // R carries magnitude; G is the sign flag. Only far-side and in-focus
        // pixels (G == 0) contribute to the far field. The field is half-res,
        // so each tap step is 2 full-res px; decay is 2 / max_radius.
        if (sample.g == 0.0) {
            far_coc = max(far_coc, sample.r - f32(abs(i)) * u.decay);
        }
    }

    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(far_coc, 0.0, 0.0, 1.0));
}
