// node.vignette — soft fade-to-black border in three shapes.
//
// Shape modes (must match `VIGNETTE_SHAPES` in vignette.rs):
//   0 = Circle    — aspect-corrected, d=1 at the short-axis edge (true circle on any canvas)
//   1 = Ellipse   — raw UV space, d=1 at the top/bottom/left/right edges (fits canvas)
//   2 = Rectangle — chebyshev distance to nearest edge (per-edge fade; used to hide hard
//                   sampling cutoffs in displacement chains like feedback / mirror)
//
// `size` sets the inner boundary of the fade (full input below `size - softness/2`).
// `softness` sets the fade width. `strength` blends the result back against the
// untouched input — strength=0 is no-op, strength=1 is full vignette.
//
// Bindings:
//   @binding(0) uniforms (shape + size + softness + strength + aspect → 32 bytes)
//   @binding(1) tex_in
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    shape: u32,
    size: f32,
    softness: f32,
    strength: f32,
    aspect: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_in: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let centered = uv - vec2<f32>(0.5);

    var d: f32;
    if uniforms.shape == 0u {
        // Circle: aspect-correct so the iso-distance rings are visually
        // circular on any canvas. d=1 at the short-axis edge from center.
        let aspect_corrected = vec2<f32>(centered.x * uniforms.aspect, centered.y);
        d = length(aspect_corrected) * 2.0;
    } else if uniforms.shape == 1u {
        // Ellipse: raw UV space — d=1 at top/bottom/left/right edges,
        // d≈1.41 at corners. Stretches with canvas aspect.
        d = length(centered) * 2.0;
    } else {
        // Rectangle: chebyshev distance to nearest edge in UV space.
        // d=0 at center, d=1 at any edge. The per-edge fade is what hides
        // hard sampling cutoffs in feedback / mirror / displacement chains.
        let edge_dist = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
        d = 1.0 - edge_dist * 2.0;
    }

    let size_inner = uniforms.size - uniforms.softness * 0.5;
    let size_outer = uniforms.size + uniforms.softness * 0.5;
    let raw_mask = 1.0 - smoothstep(size_inner, size_outer, d);
    let final_mask = mix(1.0, raw_mask, uniforms.strength);

    let src = textureSampleLevel(tex_in, tex_sampler, uv, 0.0);
    textureStore(
        output_tex,
        vec2<i32>(id.xy),
        vec4<f32>(src.rgb * final_mask, src.a * final_mask),
    );
}
