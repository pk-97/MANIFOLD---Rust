// node.mirror_fold_uv — mirror / fold coordinate generator. Emits the
// per-pixel sample UV produced by the legacy node.transform mirror/fold
// mode table (axis flips + kaleidoscope-style folds). The affine half of
// node.transform (translate / scale / rotate) is left to
// node.affine_transform — this atom is purely the mirror/fold UV rewrite.
//
// Pair with node.remap (Clamp) to resample a source at these coordinates,
// then node.mix (Lerp) to crossfade — the TD coordinate → remap → blend
// shape that replaces the fused node.transform mirror modes.
//
// R = folded_u, G = folded_v, B = 0, A = 1. Verbatim port of
// uv_transform.wgsl steps before the affine pass.
//
// Modes:
//   0 Identity   — no UV change.
//   1 Mirror     — flip horizontally (alias 2 MirrorX).
//   3 MirrorY    — flip vertically (alias 4 FlipY).
//   5 QuadMirror — fold both axes onto one corner (2×2 mirrored grid).
//   6 FoldX      — fold across the vertical center (triangle wave on u).
//   7 FoldY      — fold across the horizontal center (triangle wave on v).
//   8 FoldBoth   — FoldX combined with FoldY.

struct Uniforms {
    mode: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    var uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    if u.mode == 1u || u.mode == 2u {
        // Mirror / MirrorX
        uv.x = 1.0 - uv.x;
    } else if u.mode == 3u || u.mode == 4u {
        // MirrorY / FlipY
        uv.y = 1.0 - uv.y;
    } else if u.mode == 5u {
        // QuadMirror — fold both axes onto [0, 0.5], pull to [0.25, 0.75].
        let folded = abs(uv - 0.5) * 2.0;
        uv = folded * 0.5 + 0.25;
    } else if u.mode == 6u || u.mode == 8u {
        // FoldX / FoldBoth — fold horizontally across center.
        uv.x = 0.5 - abs(uv.x - 0.5);
    }
    if u.mode == 7u || u.mode == 8u {
        // FoldY / FoldBoth — fold vertically across center.
        uv.y = 0.5 - abs(uv.y - 0.5);
    }

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(uv.x, uv.y, 0.0, 1.0));
}
