// Metallic Glass — Pass 4: Edge + Mirror + Levels.
//
// Replicates TD post-processing chain:
//   1. Edge TOP: Strength 5.0, Sample Step 1,1 (3×3 Sobel)
//   2. Mirror TOP: Rotate 45°, Pivot 0.5,0.5
//   3. Level TOP 1 (Height): Brightness 1.2, Contrast 1.5, Gamma 0.8
//   4. Level TOP 2 (Metallic): Invert On, Gamma 1.5
//
// Output: R = height, G = metallic, B = edge, A = 1.0

struct Uniforms {
    edge_strength: f32,   // TD Edge Strength (default 5.0)
    mirror_angle: f32,    // TD Mirror Rotate in radians (default π/4 = 45°)
    width: f32,
    height: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var dst_tex: texture_storage_2d<rgba16float, write>;

// ─── Sobel edge detection (TD Edge TOP, Sample Step 1,1) ──────────

fn sample_luma(pos: vec2<i32>, w: i32, h: i32) -> f32 {
    let clamped = clamp(pos, vec2(0), vec2(w - 1, h - 1));
    let c = textureLoad(src_tex, clamped, 0);
    return dot(c.rgb, vec3<f32>(0.299, 0.587, 0.114));
}

fn sobel(pos: vec2<i32>, w: i32, h: i32) -> f32 {
    let tl = sample_luma(pos + vec2(-1, -1), w, h);
    let tc = sample_luma(pos + vec2( 0, -1), w, h);
    let tr = sample_luma(pos + vec2( 1, -1), w, h);
    let ml = sample_luma(pos + vec2(-1,  0), w, h);
    let mr = sample_luma(pos + vec2( 1,  0), w, h);
    let bl = sample_luma(pos + vec2(-1,  1), w, h);
    let bc = sample_luma(pos + vec2( 0,  1), w, h);
    let br = sample_luma(pos + vec2( 1,  1), w, h);

    let gx = -tl - 2.0 * ml - bl + tr + 2.0 * mr + br;
    let gy = -tl - 2.0 * tc - tr + bl + 2.0 * bc + br;

    return sqrt(gx * gx + gy * gy);
}

// ─── Mirror (TD Mirror TOP: Rotate 45°, Pivot 0.5,0.5) ───────────

fn mirror_uv(uv: vec2<f32>, angle: f32) -> vec2<f32> {
    let centered = uv - vec2(0.5);

    let ca = cos(angle);
    let sa = sin(angle);
    let rotated = vec2<f32>(
        centered.x * ca - centered.y * sa,
        centered.x * sa + centered.y * ca,
    );

    // Fold = mirror across both axes
    let folded = abs(rotated);

    // Rotate back
    let unrotated = vec2<f32>(
        folded.x * ca + folded.y * sa,
        -folded.x * sa + folded.y * ca,
    );

    return unrotated + vec2(0.5);
}

// ─── Levels (TD Level TOPs) ────────────────────────────────────────

fn levels_height(v: f32) -> f32 {
    // TD Level TOP 1: Brightness 1.2, Contrast 1.5, Gamma 0.8
    var val = v * 1.2;                     // brightness
    val = (val - 0.5) * 1.5 + 0.5;        // contrast
    val = clamp(val, 0.0, 1.0);
    val = pow(val, 0.8);                   // gamma
    return val;
}

fn levels_metallic(v: f32) -> f32 {
    // TD Level TOP 2: Invert On, Gamma 1.5
    var val = 1.0 - v;                     // invert
    val = clamp(val, 0.0, 1.0);
    val = pow(val, 1.5);                   // gamma
    return val;
}

// ─── Main ──────────────────────────────────────────────────────────

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = i32(u.width);
    let h = i32(u.height);
    let pos = vec2<i32>(gid.xy);
    if pos.x >= w || pos.y >= h { return; }

    let uv = vec2<f32>(f32(pos.x) / u.width, f32(pos.y) / u.height);

    // Step 1: Mirror UV (TD Mirror TOP)
    let mirrored_uv = mirror_uv(uv, u.mirror_angle);

    let mirrored_pos = vec2<i32>(
        clamp(i32(mirrored_uv.x * u.width), 0, w - 1),
        clamp(i32(mirrored_uv.y * u.height), 0, h - 1),
    );

    // Step 2: Edge detection (TD Edge TOP, Strength param)
    let edge = sobel(mirrored_pos, w, h) * u.edge_strength;
    let edge_clamped = clamp(edge, 0.0, 1.0);

    // Step 3: Apply levels
    let height_val = levels_height(edge_clamped);
    let metallic_val = levels_metallic(edge_clamped);

    // R = height, G = metallic, B = edge, A = 1
    textureStore(dst_tex, pos, vec4<f32>(height_val, metallic_val, edge_clamped, 1.0));
}
