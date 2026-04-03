// Metallic Glass — Pass 4: Edge detection + mirror + level adjustments.
//
// Reads the blurred feedback texture and produces a processed texture
// containing height (R) and metallic (G) maps for the render pass.
//
// Pipeline:
//   1. Sobel edge detection (strength 5.0)
//   2. 45° UV mirror/fold for Y2K kaleidoscopic symmetry
//   3. Height remap: brightness 1.2, contrast 1.5, gamma 0.8
//   4. Metallic remap: invert, gamma 1.5
//
// Output: R = height map, G = metallic map, B = raw edge, A = 1.0

struct Uniforms {
    edge_strength: f32,   // Sobel multiplier (default 5.0)
    mirror_angle: f32,    // Mirror rotation in radians (default π/4)
    width: f32,
    height: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var dst_tex: texture_storage_2d<rgba16float, write>;

// ─── Sobel edge detection ──────────────────────────────────────────

fn sample_luma(pos: vec2<i32>, w: i32, h: i32) -> f32 {
    let clamped = clamp(pos, vec2(0), vec2(w - 1, h - 1));
    let c = textureLoad(src_tex, clamped, 0);
    return dot(c.rgb, vec3<f32>(0.299, 0.587, 0.114));
}

fn sobel(pos: vec2<i32>, w: i32, h: i32) -> f32 {
    // 3×3 Sobel kernels
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

// ─── UV mirror with rotation ───────────────────────────────────────

fn mirror_uv(uv: vec2<f32>, angle: f32) -> vec2<f32> {
    // Center UV around 0.5
    let centered = uv - vec2(0.5);

    // Rotate
    let ca = cos(angle);
    let sa = sin(angle);
    let rotated = vec2<f32>(
        centered.x * ca - centered.y * sa,
        centered.x * sa + centered.y * ca,
    );

    // Fold (absolute value = mirror across both axes)
    let folded = abs(rotated);

    // Rotate back
    let unrotated = vec2<f32>(
        folded.x * ca + folded.y * sa,
        -folded.x * sa + folded.y * ca,
    );

    return unrotated + vec2(0.5);
}

// ─── Level adjustments ─────────────────────────────────────────────

fn apply_levels_height(v: f32) -> f32 {
    // Brightness 1.2, Contrast 1.5, Gamma 0.8
    var val = v;
    val = val * 1.2;                           // brightness
    val = (val - 0.5) * 1.5 + 0.5;            // contrast
    val = clamp(val, 0.0, 1.0);
    val = pow(val, 0.8);                        // gamma
    return val;
}

fn apply_levels_metallic(v: f32) -> f32 {
    // Invert, Gamma 1.5
    var val = 1.0 - v;                          // invert
    val = clamp(val, 0.0, 1.0);
    val = pow(val, 1.5);                        // gamma
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

    // Step 1: Mirror UV
    let mirrored_uv = mirror_uv(uv, u.mirror_angle);

    // Convert mirrored UV back to texel coordinates
    let mirrored_pos = vec2<i32>(
        clamp(i32(mirrored_uv.x * u.width), 0, w - 1),
        clamp(i32(mirrored_uv.y * u.height), 0, h - 1),
    );

    // Step 2: Sobel edge detection at the mirrored position.
    // This isolates the "veins" — boundaries between feedback regions.
    let edge = sobel(mirrored_pos, w, h) * u.edge_strength;
    let edge_clamped = clamp(edge, 0.0, 1.0);

    // Step 3: Apply level adjustments
    let height_val = apply_levels_height(edge_clamped);
    let metallic_val = apply_levels_metallic(edge_clamped);

    // Pack: R = height, G = metallic, B = edge, A = 1
    textureStore(dst_tex, pos, vec4<f32>(height_val, metallic_val, edge_clamped, 1.0));
}
