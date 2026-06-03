// node.mirror_fold_uv — fusable body (freeze §12), SOURCE. Rewrites the fragment
// uv via an axis flip / kaleidoscope fold (mode table), emits it as R/G. The uv
// param is immutable so the working copy is `var p`. Matches mirror_fold_uv.wgsl.
// PARAMS: [mode] (Enum -> u32).
fn body(uv: vec2<f32>, dims: vec2<f32>, mode: u32) -> vec4<f32> {
    var p = uv;
    if mode == 1u || mode == 2u {
        p.x = 1.0 - p.x;
    } else if mode == 3u || mode == 4u {
        p.y = 1.0 - p.y;
    } else if mode == 5u {
        let folded = abs(uv - 0.5) * 2.0;
        p = folded * 0.5 + 0.25;
    } else if mode == 6u || mode == 8u {
        p.x = 0.5 - abs(p.x - 0.5);
    }
    if mode == 7u || mode == 8u {
        p.y = 0.5 - abs(p.y - 0.5);
    }
    return vec4<f32>(p.x, p.y, 0.0, 1.0);
}
