// Metallic Glass — Pass 2/3: Separable Gaussian blur.
//
// Replicates TD Blur TOP: Filter Size = 4, Pre-Shrink = 1.
// Filter Size 4 → radius 4 pixels → 9-tap separable Gaussian, sigma ~2.0.
// Applied as two passes (H then V) to the feedback buffer each frame.

struct Uniforms {
    direction: f32,   // 0.0 = horizontal, 1.0 = vertical
    width: f32,
    height: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var dst_tex: texture_storage_2d<rgba16float, write>;

// 9-tap Gaussian weights (sigma = 2.0, radius 4, pre-normalized)
const W: array<f32, 5> = array<f32, 5>(
    0.20236,   // center (offset 0)
    0.17820,   // ±1
    0.12162,   // ±2
    0.06433,   // ±3
    0.02637,   // ±4
);

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = i32(u.width);
    let h = i32(u.height);
    let pos = vec2<i32>(gid.xy);
    if pos.x >= w || pos.y >= h { return; }

    let dir = select(vec2<i32>(1, 0), vec2<i32>(0, 1), u.direction > 0.5);
    let bounds = vec2(w - 1, h - 1);

    var color = textureLoad(src_tex, pos, 0) * W[0];

    for (var i = 1; i <= 4; i++) {
        let pa = clamp(pos + dir * i, vec2(0), bounds);
        let pb = clamp(pos - dir * i, vec2(0), bounds);
        color += (textureLoad(src_tex, pa, 0) + textureLoad(src_tex, pb, 0)) * W[i];
    }

    textureStore(dst_tex, pos, color);
}
