// node.mirror_axis — sample input at UVs mirrored across a line through
// the center at `angle` radians. Single-axis 2-fold symmetry.
//
// Math: rotate -angle → fold Y (|y|) → rotate +angle → fract(+0.5).
// Matches the legacy MetallicGlass "Mirror TOP at angle" semantics.
//
// Bindings:
//   @binding(0) uniforms (16 bytes — angle + 3 pad)
//   @binding(1) tex_source
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    angle: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_source: texture_2d<f32>;
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
    let ca = cos(uniforms.angle);
    let sa = sin(uniforms.angle);

    // Rotate so the mirror axis aligns with the X axis.
    let rotated = vec2<f32>(
        centered.x * ca - centered.y * sa,
        centered.x * sa + centered.y * ca,
    );

    // Fold Y only (mirror across the rotated X axis).
    let folded = vec2<f32>(rotated.x, abs(rotated.y));

    // Rotate back to the original frame.
    let unrotated = vec2<f32>(
        folded.x * ca + folded.y * sa,
        -folded.x * sa + folded.y * ca,
    );

    let mirrored_uv = fract(unrotated + vec2<f32>(0.5));
    let color = textureSampleLevel(tex_source, tex_sampler, mirrored_uv, 0.0);
    textureStore(output_tex, vec2<i32>(id.xy), color);
}
