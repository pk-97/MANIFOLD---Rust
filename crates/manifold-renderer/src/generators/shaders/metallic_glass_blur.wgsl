// Metallic Glass — Pass 2/3: Separable Gaussian blur.
//
// Two-pass separable blur (H then V) applied to the feedback buffer.
// Radius 4 pixels (9-tap kernel). Matches TD Blur TOP with filter_size=4.

struct Uniforms {
    direction: f32,   // 0.0 = horizontal, 1.0 = vertical
    width: f32,
    height: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var dst_tex: texture_storage_2d<rgba16float, write>;

// 9-tap Gaussian weights (sigma ~2.0, radius 4)
const KERNEL_RADIUS: i32 = 4;
const W0: f32 = 0.1964825501511404;
const W1: f32 = 0.2969069646728344;
const W2: f32 = 0.09447039785044732;
const W3: f32 = 0.01038159685599673;
const W4: f32 = 0.00039400560964572126;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = i32(u.width);
    let h = i32(u.height);
    let pos = vec2<i32>(gid.xy);
    if pos.x >= w || pos.y >= h { return; }

    // Direction vector
    let dir = select(vec2<i32>(1, 0), vec2<i32>(0, 1), u.direction > 0.5);

    // Accumulate weighted samples
    var color = textureLoad(src_tex, pos, 0) * W0;

    let p1a = clamp(pos + dir * 1, vec2(0), vec2(w - 1, h - 1));
    let p1b = clamp(pos - dir * 1, vec2(0), vec2(w - 1, h - 1));
    color += (textureLoad(src_tex, p1a, 0) + textureLoad(src_tex, p1b, 0)) * W1;

    let p2a = clamp(pos + dir * 2, vec2(0), vec2(w - 1, h - 1));
    let p2b = clamp(pos - dir * 2, vec2(0), vec2(w - 1, h - 1));
    color += (textureLoad(src_tex, p2a, 0) + textureLoad(src_tex, p2b, 0)) * W2;

    let p3a = clamp(pos + dir * 3, vec2(0), vec2(w - 1, h - 1));
    let p3b = clamp(pos - dir * 3, vec2(0), vec2(w - 1, h - 1));
    color += (textureLoad(src_tex, p3a, 0) + textureLoad(src_tex, p3b, 0)) * W3;

    let p4a = clamp(pos + dir * 4, vec2(0), vec2(w - 1, h - 1));
    let p4b = clamp(pos - dir * 4, vec2(0), vec2(w - 1, h - 1));
    color += (textureLoad(src_tex, p4a, 0) + textureLoad(src_tex, p4b, 0)) * W4;

    textureStore(dst_tex, pos, color);
}
