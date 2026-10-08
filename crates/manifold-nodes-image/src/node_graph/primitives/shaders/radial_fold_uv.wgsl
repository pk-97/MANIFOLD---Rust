// node.radial_fold_uv — kaleidoscope coordinate generator. Folds the plane
// into `segments` mirrored wedges around (cx, cy) and emits the per-pixel
// sample UV. Verbatim port of the legacy node.kaleidoscope fold math
// (only the sample + crossfade are split off into node.remap + node.mix).
//
// R = folded_u, G = folded_v, B = 0, A = 1.

struct Uniforms {
    segments: f32,
    cx: f32,
    cy: f32,
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

const TAU: f32 = 6.28318530717958647692;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let centered = uv - vec2<f32>(u.cx, u.cy);

    let angle = atan2(centered.y, centered.x);
    let radius = length(centered);

    let seg = max(u.segments, 2.0);
    let segment_angle = TAU / seg;
    let slice_index = floor(angle / segment_angle);
    var local_angle = angle - slice_index * segment_angle;
    if (abs(slice_index) % 2.0) > 0.5 {
        local_angle = segment_angle - local_angle;
    }

    let kx = cos(local_angle) * radius + u.cx;
    let ky = sin(local_angle) * radius + u.cy;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(kx, ky, 0.0, 1.0));
}
