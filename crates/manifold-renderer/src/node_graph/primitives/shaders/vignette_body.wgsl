// node.vignette — fusable body (freeze §12). The first POSITIONAL atom: it
// reads only its own texel but modulates that texel by the pixel's POSITION.
// `uv` (normalized center-of-texel) drives the shape distance; `dims` recovers
// aspect = dims.x/dims.y for the aspect-correct circle (aspect is no longer a
// uniform — it's derived from the canvas, which is exactly what run() did with
// width/height). Pure; matches vignette.wgsl exactly. PARAMS order:
// [shape, size, softness, strength].
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, shape: u32, size: f32, softness: f32, strength: f32) -> vec4<f32> {
    let aspect = dims.x / dims.y;
    let centered = uv - vec2<f32>(0.5);

    var d: f32;
    if shape == 0u {
        // Circle: aspect-correct so iso-distance rings are visually circular.
        let aspect_corrected = vec2<f32>(centered.x * aspect, centered.y);
        d = length(aspect_corrected) * 2.0;
    } else if shape == 1u {
        // Ellipse: raw UV space — stretches with canvas aspect.
        d = length(centered) * 2.0;
    } else {
        // Rectangle: chebyshev distance to nearest edge (per-edge fade).
        let edge_dist = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
        d = 1.0 - edge_dist * 2.0;
    }

    let size_inner = size - softness * 0.5;
    let size_outer = size + softness * 0.5;
    let raw_mask = 1.0 - smoothstep(size_inner, size_outer, d);
    let final_mask = mix(1.0, raw_mask, strength);

    return vec4<f32>(c.rgb * final_mask, c.a * final_mask);
}
