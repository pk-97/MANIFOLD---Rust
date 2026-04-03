// Metallic Glass — Pass 4: Mirror + Height/Metallic map generation.
//
// Height map: Uses the mirrored FEEDBACK texture directly. The feedback
// contains smooth, continuous values everywhere — distinct regions with
// gradual transitions. This produces a smooth undulating surface.
//
// Metallic map: Uses Sobel edge detection on the mirrored feedback.
// Edges isolate the "veins" between feedback regions, creating material
// variation (shiny chrome between veins, different reflectivity at veins).
//
// TD chain replication:
//   Mirror TOP: Rotate 45°, Pivot 0.5,0.5
//   Level TOP 1 (Height): Brightness 1.2, Contrast 1.5, Gamma 0.8
//   Level TOP 2 (Metallic): Invert On, Gamma 1.5

struct Uniforms {
    edge_strength: f32,   // Sobel multiplier for metallic vein detail
    mirror_angle: f32,    // Mirror rotation in radians (default π/4 = 45°)
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

// ─── Mirror (TD Mirror TOP) ───────────────────────────────────────

fn mirror_uv(uv: vec2<f32>, angle: f32) -> vec2<f32> {
    let centered = uv - vec2(0.5);

    let ca = cos(angle);
    let sa = sin(angle);
    let rotated = vec2<f32>(
        centered.x * ca - centered.y * sa,
        centered.x * sa + centered.y * ca,
    );

    let folded = abs(rotated);

    let unrotated = vec2<f32>(
        folded.x * ca + folded.y * sa,
        -folded.x * sa + folded.y * ca,
    );

    return fract(unrotated + vec2(0.5));
}

// ─── Levels (TD Level TOPs) ────────────────────────────────────────

fn levels_height(v: f32) -> f32 {
    // TD Level TOP 1: Brightness 1.2, Contrast 1.5, Gamma 0.8
    var val = v * 1.2;
    val = (val - 0.5) * 1.5 + 0.5;
    val = clamp(val, 0.0, 1.0);
    val = pow(val, 0.8);
    return val;
}

fn levels_metallic(v: f32) -> f32 {
    // TD Level TOP 2: Invert On, Gamma 1.5
    var val = 1.0 - v;
    val = clamp(val, 0.0, 1.0);
    val = pow(val, 1.5);
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

    // Mirror UV
    let mirrored_uv = mirror_uv(uv, u.mirror_angle);

    let mirrored_pos = vec2<i32>(
        clamp(i32(mirrored_uv.x * u.width), 0, w - 1),
        clamp(i32(mirrored_uv.y * u.height), 0, h - 1),
    );

    // HEIGHT: Read feedback luma directly at mirrored position.
    // The feedback texture is smooth and continuous — every pixel has a
    // non-trivial value. This produces a smooth undulating surface.
    let feedback = textureLoad(src_tex, mirrored_pos, 0);
    let feedback_luma = dot(feedback.rgb, vec3<f32>(0.299, 0.587, 0.114));
    // Remap luma from [0,1] to [0.3,1.0] so the contrast adjustment in
    // levels_height never crushes values to zero. Prevents flat spots.
    let remapped = feedback_luma * 0.7 + 0.3;
    let height_val = levels_height(remapped);

    // METALLIC: Sobel edges at mirrored position.
    // Edges isolate the "veins" — boundaries between feedback regions.
    // These drive material variation, not geometry.
    let edge = sobel(mirrored_pos, w, h) * u.edge_strength;
    let edge_clamped = clamp(edge, 0.0, 1.0);
    let metallic_val = levels_metallic(edge_clamped);

    // R = height (from feedback, continuous), G = metallic (from edges, veins)
    textureStore(dst_tex, pos, vec4<f32>(height_val, metallic_val, edge_clamped, 1.0));
}
