// Galactic Rock — Phase 4: Luma Blur post-process.
//
// Simulates macro depth-of-field with a horizontal luma ramp mask:
//   - Center (horizontal): sharp (0px blur)
//   - Edges (left/right): 10px Gaussian blur
//
// Two-pass separable Gaussian: this shader handles one axis per dispatch.
// The uniform selects horizontal (pass 0) or vertical (pass 1).

struct BlurUniforms {
    max_radius: f32,   // max blur radius in pixels (default 10)
    direction: f32,    // 0 = horizontal, 1 = vertical
    width: f32,
    height: f32,
};

@group(0) @binding(0) var<uniform> u: BlurUniforms;
@group(0) @binding(1) var input_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

// Horizontal luma ramp: 0 at edges, 1 at center.
fn luma_ramp(uv_x: f32) -> f32 {
    // Symmetric ramp: 0→1→0 across horizontal axis
    let center_dist = abs(uv_x - 0.5) * 2.0; // 0 at center, 1 at edges
    return 1.0 - center_dist;
}

// Gaussian weight (σ derived from radius)
fn gaussian(x: f32, sigma: f32) -> f32 {
    return exp(-(x * x) / (2.0 * sigma * sigma));
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(i32(u.width), i32(u.height));
    let coord = vec2<i32>(gid.xy);
    if coord.x >= dims.x || coord.y >= dims.y { return; }

    let uv_x = (f32(coord.x) + 0.5) / u.width;

    // Luma ramp determines blur radius: 1.0 at center (no blur), 0.0 at edges (max blur)
    let sharpness = luma_ramp(uv_x);
    let radius = u.max_radius * (1.0 - sharpness);

    // No blur needed — pass through
    if radius < 0.5 {
        let color = textureLoad(input_tex, coord, 0);
        textureStore(output_tex, coord, color);
        return;
    }

    let sigma = radius / 3.0; // 3-sigma rule
    let kernel_half = i32(ceil(radius));

    // Direction: horizontal (1,0) or vertical (0,1)
    let dir = select(vec2<i32>(1, 0), vec2<i32>(0, 1), u.direction > 0.5);

    var color_sum = vec4<f32>(0.0);
    var weight_sum = 0.0;

    for (var i = -kernel_half; i <= kernel_half; i++) {
        let sample_coord = coord + dir * i;
        // Clamp to texture bounds
        let sc = clamp(sample_coord, vec2<i32>(0), dims - 1);
        let w = gaussian(f32(i), sigma);
        color_sum += textureLoad(input_tex, sc, 0) * w;
        weight_sum += w;
    }

    textureStore(output_tex, coord, color_sum / weight_sum);
}
