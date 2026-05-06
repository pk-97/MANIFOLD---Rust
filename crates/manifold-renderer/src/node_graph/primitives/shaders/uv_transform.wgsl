// primitive.uv_transform — translate / scale / rotate / mirror the input.
//
// Single shader covering all 6 modes:
//   0 = Identity     — no UV change beyond translate/scale/rotation.
//   1 = Mirror       — flip horizontally.
//   2 = MirrorX      — alias of Mirror.
//   3 = MirrorY      — flip vertically.
//   4 = FlipY        — alias of MirrorY.
//   5 = QuadMirror   — fold the image into one corner, mirror across both
//                      axes (visually: 4 mirrored copies in a 2×2 grid).
//
// Bindings (canonical layout for one-texture-input primitives):
//   @binding(0) uniforms
//   @binding(1) tex_source
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    translate: vec2<f32>,    // 8 bytes — UV-space offset.
    scale: vec2<f32>,        // 8 bytes — UV-space scale (per-axis).
    rotation: f32,           // radians, applied around (0.5, 0.5).
    mode: u32,               // 0..5 — see top-of-file table.
    _pad0: f32,
    _pad1: f32,
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
    var uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    // 1. Mirror modes — operate in [0, 1] UV space, fold across the
    //    relevant axis. Applied first so translate/scale/rotation
    //    compose on top.
    if uniforms.mode == 1u || uniforms.mode == 2u {
        // Mirror / MirrorX
        uv.x = 1.0 - uv.x;
    } else if uniforms.mode == 3u || uniforms.mode == 4u {
        // MirrorY / FlipY
        uv.y = 1.0 - uv.y;
    } else if uniforms.mode == 5u {
        // QuadMirror — fold both axes onto [0, 0.5], then double back so
        // the result reads as 4 mirrored quadrants.
        let folded = abs(uv - 0.5) * 2.0;  // [0, 1] in each axis
        uv = folded * 0.5 + 0.25;          // pull to [0.25, 0.75]
    }

    // 2. Center for rotation/scale.
    let centered = uv - vec2<f32>(0.5);

    // 3. Rotation.
    let cs = cos(uniforms.rotation);
    let sn = sin(uniforms.rotation);
    let rotated = vec2<f32>(
        centered.x * cs - centered.y * sn,
        centered.x * sn + centered.y * cs,
    );

    // 4. Scale (per-axis).
    let sx = select(uniforms.scale.x, 1.0, uniforms.scale.x == 0.0);
    let sy = select(uniforms.scale.y, 1.0, uniforms.scale.y == 0.0);
    let scaled = rotated / vec2<f32>(sx, sy);

    // 5. Translate + recenter.
    let final_uv = scaled + vec2<f32>(0.5) + uniforms.translate;

    let color = textureSampleLevel(tex_source, tex_sampler, final_uv, 0.0);
    textureStore(output_tex, vec2<i32>(id.xy), color);
}
