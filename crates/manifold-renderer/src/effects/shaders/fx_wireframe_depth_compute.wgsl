// Compute variant of fx_wireframe_depth.wgsl — all 15 passes as compute dispatches.
// Eliminates TBDR tile load/store overhead (15 passes × ~290μs = ~4.35ms savings at 4K).
// textureSample → textureSampleLevel, dpdx/dpdy → finite-difference approximation,
// fragment output → textureStore.

struct Uniforms {
    amount: f32,
    grid_density: f32,
    line_width: f32,
    depth_scale: f32,

    temporal_smooth: f32,
    persistence: f32,
    flow_lock_strength: f32,
    mesh_regularize: f32,

    cell_affine_strength: f32,
    face_warp_strength: f32,
    surface_persistence: f32,
    wire_taa: f32,

    subject_isolation: f32,
    blend_mode: f32,
    texel_x: f32,
    texel_y: f32,

    depth_texel_x: f32,
    depth_texel_y: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var main_tex: texture_2d<f32>;
@group(0) @binding(2) var prev_analysis_tex: texture_2d<f32>;
@group(0) @binding(3) var prev_depth_tex: texture_2d<f32>;
@group(0) @binding(4) var depth_tex: texture_2d<f32>;
@group(0) @binding(5) var history_tex: texture_2d<f32>;
@group(0) @binding(6) var flow_tex: texture_2d<f32>;
@group(0) @binding(7) var mesh_coord_tex: texture_2d<f32>;
@group(0) @binding(8) var prev_mesh_coord_tex: texture_2d<f32>;
@group(0) @binding(9) var semantic_tex: texture_2d<f32>;
@group(0) @binding(10) var surface_cache_tex: texture_2d<f32>;
@group(0) @binding(11) var prev_surface_cache_tex: texture_2d<f32>;
@group(0) @binding(12) var subject_mask_tex: texture_2d<f32>;
@group(0) @binding(13) var samp: sampler;
@group(0) @binding(14) var output_tex: texture_storage_2d<rgba16float, write>;

// Shared helpers
fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn decode_cooldown(encoded: f32) -> f32 {
    return clamp((encoded - 0.55) / 0.45, 0.0, 1.0);
}

fn encode_cooldown(cooldown: f32) -> f32 {
    return 0.55 + clamp(cooldown, 0.0, 1.0) * 0.45;
}

// ---------------------------------------------------------------------------
// Pass 0: Analysis
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_analysis(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let l = luminance(textureSampleLevel(main_tex, samp, uv, 0.0).rgb);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(l, l, l, 1.0));
}

// ---------------------------------------------------------------------------
// Pass 1: HeuristicDepth
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_heuristic_depth(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let c  = textureSampleLevel(main_tex, samp, uv, 0.0).r;
    let tl = textureSampleLevel(main_tex, samp, uv + texel * vec2<f32>(-1.0, -1.0), 0.0).r;
    let tc = textureSampleLevel(main_tex, samp, uv + texel * vec2<f32>( 0.0, -1.0), 0.0).r;
    let tr = textureSampleLevel(main_tex, samp, uv + texel * vec2<f32>( 1.0, -1.0), 0.0).r;
    let ml = textureSampleLevel(main_tex, samp, uv + texel * vec2<f32>(-1.0,  0.0), 0.0).r;
    let mr = textureSampleLevel(main_tex, samp, uv + texel * vec2<f32>( 1.0,  0.0), 0.0).r;
    let bl = textureSampleLevel(main_tex, samp, uv + texel * vec2<f32>(-1.0,  1.0), 0.0).r;
    let bc = textureSampleLevel(main_tex, samp, uv + texel * vec2<f32>( 0.0,  1.0), 0.0).r;
    let br = textureSampleLevel(main_tex, samp, uv + texel * vec2<f32>( 1.0,  1.0), 0.0).r;

    let gx = -tl - 2.0 * ml - bl + tr + 2.0 * mr + br;
    let gy = -tl - 2.0 * tc - tr + bl + 2.0 * bc + br;
    let edge = clamp(sqrt(gx * gx + gy * gy) * 0.18, 0.0, 1.0);

    let prev_luma = textureSampleLevel(prev_analysis_tex, samp, uv, 0.0).r;
    let motion = clamp(abs(c - prev_luma) * 2.0, 0.0, 1.0);
    let luma_depth = 1.0 - c;
    let neighborhood_mean = (tl + tc + tr + ml + c + mr + bl + bc + br) / 9.0;
    let local_contrast = clamp(abs(c - neighborhood_mean) * 2.0, 0.0, 1.0);
    let structure = clamp(edge * 0.9 + local_contrast * 0.6, 0.0, 1.0);

    let raw_depth = clamp(luma_depth * 0.78 + structure * 0.20 + motion * 0.10, 0.0, 1.0);
    let prev_depth   = textureSampleLevel(prev_depth_tex, samp, uv, 0.0).r;
    let prev_depth_l = textureSampleLevel(prev_depth_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).r;
    let prev_depth_r = textureSampleLevel(prev_depth_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).r;
    let prev_depth_b = textureSampleLevel(prev_depth_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).r;
    let prev_depth_t = textureSampleLevel(prev_depth_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).r;
    let prev_depth_blur = (prev_depth * 2.0 + prev_depth_l + prev_depth_r + prev_depth_b + prev_depth_t) / 6.0;
    let smooth_depth = mix(raw_depth, prev_depth_blur, u.temporal_smooth);

    let confidence_raw = clamp(luma_depth * 0.60 + structure * 0.30 + motion * 0.10, 0.0, 1.0);
    let confidence = smoothstep(0.35, 0.75, confidence_raw);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(smooth_depth, smooth_depth, smooth_depth, confidence));
}

// ---------------------------------------------------------------------------
// Pass 2: WireMask — compute dpdx/dpdy via finite differences
// ---------------------------------------------------------------------------

// Catmull-Rom bicubic sampling — C1-continuous interpolation from the low-res
// analysis textures (360px).  Eliminates the slope discontinuities at texel
// boundaries that cause visible stair-stepping in the grid lines at 4K.
// 4 bilinear taps (hardware-filtered) per bicubic sample.
fn bicubic_sample(tex: texture_2d<f32>, s: sampler, uv: vec2<f32>, tex_size: vec2<f32>) -> vec4<f32> {
    let inv_size = 1.0 / tex_size;
    let tc = uv * tex_size - 0.5;
    let f = fract(tc);
    let tc0 = (floor(tc) + 0.5) * inv_size;

    // Catmull-Rom weights for x and y
    let w0x = f.x * (-0.5 + f.x * (1.0 - 0.5 * f.x));
    let w1x = 1.0 + f.x * f.x * (-2.5 + 1.5 * f.x);
    let w2x = f.x * (0.5 + f.x * (2.0 - 1.5 * f.x));
    let w3x = f.x * f.x * (-0.5 + 0.5 * f.x);

    let w0y = f.y * (-0.5 + f.y * (1.0 - 0.5 * f.y));
    let w1y = 1.0 + f.y * f.y * (-2.5 + 1.5 * f.y);
    let w2y = f.y * (0.5 + f.y * (2.0 - 1.5 * f.y));
    let w3y = f.y * f.y * (-0.5 + 0.5 * f.y);

    // Collapse 4 taps per row into 2 via weight grouping (4×4 → 4 bilinear taps)
    let s12x = w1x + w2x;
    let s12y = w1y + w2y;
    let ox = w2x / s12x;
    let oy = w2y / s12y;

    let tc12 = tc0 + vec2<f32>(ox, oy) * inv_size;
    let tc0x = tc0.x - inv_size.x;
    let tc3x = tc0.x + 2.0 * inv_size.x;
    let tc0y = tc0.y - inv_size.y;
    let tc3y = tc0.y + 2.0 * inv_size.y;

    let row0 = w0x * textureSampleLevel(tex, s, vec2<f32>(tc0x, tc0y), 0.0)
             + s12x * textureSampleLevel(tex, s, vec2<f32>(tc12.x, tc0y), 0.0)
             + w3x * textureSampleLevel(tex, s, vec2<f32>(tc3x, tc0y), 0.0);
    let row12 = w0x * textureSampleLevel(tex, s, vec2<f32>(tc0x, tc12.y), 0.0)
              + s12x * textureSampleLevel(tex, s, vec2<f32>(tc12.x, tc12.y), 0.0)
              + w3x * textureSampleLevel(tex, s, vec2<f32>(tc3x, tc12.y), 0.0);
    let row3 = w0x * textureSampleLevel(tex, s, vec2<f32>(tc0x, tc3y), 0.0)
             + s12x * textureSampleLevel(tex, s, vec2<f32>(tc12.x, tc3y), 0.0)
             + w3x * textureSampleLevel(tex, s, vec2<f32>(tc3x, tc3y), 0.0);

    return w0y * row0 + s12y * row12 + w3y * row3;
}

// Helper: compute grid_coord_aa (both components) for derivative approximation.
// Returns warped_raw * density * 0.5 as vec2 — same math the main body uses for
// grid_coord_aa, factored out so we can evaluate at neighbor pixels.
fn compute_grid_coord_aa(
    mesh_uv_raw: vec2<f32>,
    depth_raw_val: f32,
) -> vec2<f32> {
    let p_raw = (mesh_uv_raw - vec2<f32>(0.5)) * 2.0;
    let z_raw = depth_raw_val * u.depth_scale;
    let persp_raw = 1.0 / (1.0 + z_raw * 1.6);
    var warped_raw = p_raw * persp_raw;
    warped_raw = warped_raw + vec2<f32>(z_raw * 0.12, z_raw * 0.08);
    return warped_raw * u.grid_density * 0.50;
}

@compute @workgroup_size(16, 16)
fn cs_wire_mask(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    // Analysis texture size for bicubic sampling (derived from uniform texel sizes)
    let atex_size = vec2<f32>(1.0 / u.depth_texel_x, 1.0 / u.depth_texel_y);

    let depth_sample = bicubic_sample(depth_tex, samp, uv, atex_size);
    var depth = depth_sample.r;
    let confidence = depth_sample.a;

    let base_mask = smoothstep(0.28, 0.72, confidence);
    let sem = textureSampleLevel(semantic_tex, samp, uv, 0.0).rgb;
    let body_mask = sem.r;
    let face_mask = sem.g;
    let boundary_mask = sem.b;

    let center_delta = (uv - vec2<f32>(0.5)) * vec2<f32>(1.0, 1.35);
    let center_bias = 1.0 - smoothstep(0.28, 1.08, length(center_delta));
    let sem_foreground = clamp(max(body_mask * (0.28 + center_bias * 0.95) + boundary_mask * 0.24, face_mask * 1.28), 0.0, 1.0);

    let d_c0 = textureSampleLevel(depth_tex, samp, vec2<f32>(0.50, 0.48), 0.0).r;
    let d_c1 = textureSampleLevel(depth_tex, samp, vec2<f32>(0.38, 0.46), 0.0).r;
    let d_c2 = textureSampleLevel(depth_tex, samp, vec2<f32>(0.62, 0.46), 0.0).r;
    let d_c3 = textureSampleLevel(depth_tex, samp, vec2<f32>(0.50, 0.60), 0.0).r;
    let d_c4 = textureSampleLevel(depth_tex, samp, vec2<f32>(0.50, 0.34), 0.0).r;
    let near_ref = min(min(d_c0, d_c1), min(min(d_c2, d_c3), d_c4));

    let isolation = clamp(u.subject_isolation, 0.0, 1.0);
    let near_band = 1.0 - smoothstep(
        near_ref + mix(0.62, 0.16, isolation),
        near_ref + mix(0.92, 0.28, isolation),
        depth);
    let depth_foreground = smoothstep(0.20, 0.80, 1.0 - depth);
    let subject_core = max(face_mask * 1.25, body_mask * (0.25 + center_bias * 0.98));
    let boundary_fill = smoothstep(0.03, 0.26, boundary_mask) * (0.35 + subject_core * 0.65);
    let subject_evidence = clamp(max(max(sem_foreground, subject_core), max(boundary_fill, depth_foreground * (0.22 + subject_core * 0.92))), 0.0, 1.0);
    let isolate_mask = smoothstep(
        mix(0.04, 0.45, isolation),
        mix(0.30, 0.80, isolation),
        subject_evidence);
    var object_mask = mix(base_mask, isolate_mask * near_band, isolation);
    let hardened = smoothstep(
        mix(0.05, 0.52, isolation),
        mix(0.45, 0.90, isolation),
        object_mask);
    object_mask = mix(object_mask, hardened, isolation);

    let subject_dnn_sample = textureSampleLevel(subject_mask_tex, samp, uv, 0.0);
    let subject_dnn_avail = subject_dnn_sample.a;
    if subject_dnn_avail > 0.001 {
        let t = vec2<f32>(u.depth_texel_x, u.depth_texel_y);
        var subject_dnn = subject_dnn_sample.r;
        subject_dnn = max(subject_dnn, textureSampleLevel(subject_mask_tex, samp, uv + vec2<f32>(t.x, 0.0), 0.0).r);
        subject_dnn = max(subject_dnn, textureSampleLevel(subject_mask_tex, samp, uv - vec2<f32>(t.x, 0.0), 0.0).r);
        subject_dnn = max(subject_dnn, textureSampleLevel(subject_mask_tex, samp, uv + vec2<f32>(0.0, t.y), 0.0).r);
        subject_dnn = max(subject_dnn, textureSampleLevel(subject_mask_tex, samp, uv - vec2<f32>(0.0, t.y), 0.0).r);

        let subject_soft = smoothstep(
            mix(0.10, 0.48, isolation),
            mix(0.38, 0.84, isolation),
            subject_dnn);
        let subject_hard = smoothstep(
            mix(0.22, 0.60, isolation),
            mix(0.58, 0.92, isolation),
            subject_dnn);
        let subject_gate = mix(subject_soft, subject_hard, isolation);
        object_mask = max(object_mask, subject_soft * isolation);
        object_mask = object_mask * mix(1.0, subject_gate, subject_dnn_avail * isolation);
    }

    let mesh_data = bicubic_sample(mesh_coord_tex, samp, uv, atex_size);
    var mesh_uv = mesh_data.rg;
    let has_mesh = step(0.5, mesh_data.a);
    mesh_uv = mix(uv, mesh_uv, has_mesh);
    let mesh_uv_raw = mesh_uv;
    let depth_raw = depth;

    let density = max(u.grid_density, 1.0);
    let decimate_strength = smoothstep(170.0, 34.0, density);
    let boundary = boundary_mask;
    let silhouette_protect = smoothstep(0.06, 0.30, boundary);
    let local_decimate = decimate_strength * (1.0 - silhouette_protect * 0.85);

    let decimate_cells = clamp(density * mix(1.0, 0.38, local_decimate), 8.0, 320.0);
    let snapped_mesh_uv = (floor(mesh_uv * decimate_cells) + vec2<f32>(0.5)) / decimate_cells;
    let snapped_depth = bicubic_sample(depth_tex, samp, snapped_mesh_uv, atex_size).r;

    mesh_uv = mix(mesh_uv, snapped_mesh_uv, local_decimate);
    depth = mix(depth, snapped_depth, local_decimate * 0.92);

    let p = (mesh_uv - vec2<f32>(0.5)) * 2.0;
    let z = depth * u.depth_scale;
    let persp = 1.0 / (1.0 + z * 1.6);
    var warped = p * persp;
    warped = warped + vec2<f32>(z * 0.12, z * 0.08);

    let p_raw = (mesh_uv_raw - vec2<f32>(0.5)) * 2.0;
    let z_raw = depth_raw * u.depth_scale;
    let persp_raw = 1.0 / (1.0 + z_raw * 1.6);
    var warped_raw = p_raw * persp_raw;
    warped_raw = warped_raw + vec2<f32>(z_raw * 0.12, z_raw * 0.08);

    let grid_coord = warped * density * 0.50;
    let grid_coord_aa = warped_raw * density * 0.50;

    let width = (u.line_width * 0.020) + 0.004;

    // Compute signed screen-space derivatives of grid_coord_aa for sub-pixel
    // interpolation.  We sample mesh_coord + depth at one-pixel-right and
    // one-pixel-down neighbors (already needed for AA), then derive dg/dx, dg/dy.
    let pixel_step = vec2<f32>(u.texel_x, u.texel_y);

    let uv_dx = uv + vec2<f32>(pixel_step.x, 0.0);
    let mesh_data_dx = bicubic_sample(mesh_coord_tex, samp, uv_dx, atex_size);
    var mesh_uv_dx = mesh_data_dx.rg;
    let has_mesh_dx = step(0.5, mesh_data_dx.a);
    mesh_uv_dx = mix(uv_dx, mesh_uv_dx, has_mesh_dx);
    let depth_dx = bicubic_sample(depth_tex, samp, uv_dx, atex_size).r;
    let gc_at_dx = compute_grid_coord_aa(mesh_uv_dx, depth_dx);

    let uv_dy = uv + vec2<f32>(0.0, pixel_step.y);
    let mesh_data_dy = bicubic_sample(mesh_coord_tex, samp, uv_dy, atex_size);
    var mesh_uv_dy = mesh_data_dy.rg;
    let has_mesh_dy = step(0.5, mesh_data_dy.a);
    mesh_uv_dy = mix(uv_dy, mesh_uv_dy, has_mesh_dy);
    let depth_dy = bicubic_sample(depth_tex, samp, uv_dy, atex_size).r;
    let gc_at_dy = compute_grid_coord_aa(mesh_uv_dy, depth_dy);

    // Signed per-pixel derivatives of grid_coord_aa
    let dg_dx = gc_at_dx - grid_coord_aa;
    let dg_dy = gc_at_dy - grid_coord_aa;

    // 4x MSAA with rotated grid pattern — hard step edges, coverage from averaging.
    // Offsets in pixel units (standard 4xMSAA rotated grid).
    let qc0 = abs(fract(grid_coord + vec2<f32>(-0.125) * dg_dx + vec2<f32>(-0.375) * dg_dy) - vec2<f32>(0.5));
    let qc1 = abs(fract(grid_coord + vec2<f32>( 0.375) * dg_dx + vec2<f32>(-0.125) * dg_dy) - vec2<f32>(0.5));
    let qc2 = abs(fract(grid_coord + vec2<f32>(-0.375) * dg_dx + vec2<f32>( 0.125) * dg_dy) - vec2<f32>(0.5));
    let qc3 = abs(fract(grid_coord + vec2<f32>( 0.125) * dg_dx + vec2<f32>( 0.375) * dg_dy) - vec2<f32>(0.5));

    let mesh_line = (
        max(1.0 - step(width, qc0.x), 1.0 - step(width, qc0.y)) +
        max(1.0 - step(width, qc1.x), 1.0 - step(width, qc1.y)) +
        max(1.0 - step(width, qc2.x), 1.0 - step(width, qc2.y)) +
        max(1.0 - step(width, qc3.x), 1.0 - step(width, qc3.y))
    ) * 0.25;

    let d_l = bicubic_sample(depth_tex, samp, uv - vec2<f32>(u.depth_texel_x, 0.0), atex_size).r;
    let d_r = bicubic_sample(depth_tex, samp, uv + vec2<f32>(u.depth_texel_x, 0.0), atex_size).r;
    let d_b = bicubic_sample(depth_tex, samp, uv - vec2<f32>(0.0, u.depth_texel_y), atex_size).r;
    let d_t = bicubic_sample(depth_tex, samp, uv + vec2<f32>(0.0, u.depth_texel_y), atex_size).r;
    let curvature = abs(d_l + d_r + d_b + d_t - depth * 4.0);
    let curve_boost = 1.0 + smoothstep(0.012, 0.090, curvature) * 0.48;

    let surface_age = clamp(textureSampleLevel(surface_cache_tex, samp, uv, 0.0).b, 0.0, 1.0);
    let depth_fade = mix(1.0, 0.45, depth);
    let stable_boost = mix(0.82, 1.22, surface_age);
    var wire = mesh_line * object_mask * depth_fade * stable_boost * curve_boost;
    wire = smoothstep(0.15, 0.88, wire);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(wire, wire, wire, 1.0));
}

// ---------------------------------------------------------------------------
// Pass 3: UpdateHistory
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_update_history(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let line_now = textureSampleLevel(main_tex, samp, uv, 0.0).r;
    let history_prev = textureSampleLevel(history_tex, samp, uv, 0.0).r;
    let persist_t = clamp(u.persistence, 0.0, 1.0);
    let decay = mix(0.55, 0.9985, persist_t);
    let stability = clamp(textureSampleLevel(surface_cache_tex, samp, uv, 0.0).b, 0.0, 1.0);
    let taa_base = clamp(u.wire_taa, 0.0, 1.0) * (0.22 + stability * 0.72);
    let reprojected = history_prev * decay;
    let n_l = textureSampleLevel(main_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).r;
    let n_r = textureSampleLevel(main_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).r;
    let n_b = textureSampleLevel(main_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).r;
    let n_t = textureSampleLevel(main_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).r;
    let local_min = min(line_now, min(min(n_l, n_r), min(n_b, n_t)));
    let local_max = max(line_now, max(max(n_l, n_r), max(n_b, n_t)));
    let clamp_pad = 0.05 + (1.0 - stability) * 0.03;
    let reproj_clamped_raw = clamp(reprojected, local_min - clamp_pad, local_max + clamp_pad);
    let support = max(line_now, max(max(n_l, n_r), max(n_b, n_t)));
    let support_gate = smoothstep(0.025, 0.14, support);
    let reproj_clamped = reproj_clamped_raw * support_gate;
    let taa = taa_base * support_gate;
    let blended = mix(line_now, reproj_clamped, taa);
    let line_value = max(blended, line_now * (0.72 + stability * 0.20));
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(line_value, line_value, line_value, 1.0));
}

// ---------------------------------------------------------------------------
// Pass 4: Composite
// ---------------------------------------------------------------------------
fn blend_add(base_col: vec3<f32>, blend_col: vec3<f32>) -> vec3<f32> {
    return clamp(base_col + blend_col, vec3<f32>(0.0), vec3<f32>(1.0));
}
fn blend_multiply(base_col: vec3<f32>, blend_col: vec3<f32>) -> vec3<f32> {
    return base_col * blend_col;
}
fn blend_screen(base_col: vec3<f32>, blend_col: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(1.0) - (vec3<f32>(1.0) - base_col) * (vec3<f32>(1.0) - blend_col);
}
fn blend_overlay(base_col: vec3<f32>, blend_col: vec3<f32>) -> vec3<f32> {
    let lo = 2.0 * base_col * blend_col;
    let hi = vec3<f32>(1.0) - 2.0 * (vec3<f32>(1.0) - base_col) * (vec3<f32>(1.0) - blend_col);
    return vec3<f32>(
        select(hi.r, lo.r, base_col.r < 0.5),
        select(hi.g, lo.g, base_col.g < 0.5),
        select(hi.b, lo.b, base_col.b < 0.5));
}

@compute @workgroup_size(16, 16)
fn cs_composite(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(main_tex, samp, uv, 0.0);
    let line_value = textureSampleLevel(history_tex, samp, uv, 0.0).r;
    let wire = clamp(vec3<f32>(line_value) * 1.1, vec3<f32>(0.0), vec3<f32>(1.0));
    let mask = clamp(line_value, 0.0, 1.0);
    var mixed = wire;
    let mode = floor(u.blend_mode + 0.5);

    if mode > 0.5 && mode < 1.5 {
        mixed = mix(src.rgb, blend_add(src.rgb, wire), mask);
    } else if mode > 1.5 && mode < 2.5 {
        mixed = mix(src.rgb, blend_multiply(src.rgb, wire), mask);
    } else if mode > 2.5 && mode < 3.5 {
        mixed = mix(src.rgb, blend_screen(src.rgb, wire), mask);
    } else if mode > 3.5 && mode < 4.5 {
        mixed = mix(src.rgb, blend_overlay(src.rgb, wire), mask);
    } else if mode > 4.5 && mode < 5.5 {
        mixed = src.rgb * mask;
    } else if mode > 5.5 {
        mixed = wire;
    }

    let result = mix(src.rgb, mixed, u.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, src.a));
}

// ---------------------------------------------------------------------------
// Pass 5: DnnDepthPost
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_dnn_depth_post(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let depth_current = textureSampleLevel(main_tex, samp, uv, 0.0).r;
    let depth_prev = textureSampleLevel(prev_depth_tex, samp, uv, 0.0).r;
    let depth_smoothed = mix(depth_current, depth_prev, u.temporal_smooth * 0.85);
    let depth_value = clamp(depth_smoothed, 0.0, 1.0);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(depth_value, depth_value, depth_value, 1.0));
}

// ---------------------------------------------------------------------------
// Pass 6: FlowEstimate
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_flow_estimate(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let cur  = textureSampleLevel(main_tex, samp, uv, 0.0).r;
    let prev = textureSampleLevel(prev_analysis_tex, samp, uv, 0.0).r;

    let cur_l = textureSampleLevel(main_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).r;
    let cur_r = textureSampleLevel(main_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).r;
    let cur_b = textureSampleLevel(main_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).r;
    let cur_t = textureSampleLevel(main_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).r;

    let ix = (cur_r - cur_l) * 0.5;
    let iy = (cur_t - cur_b) * 0.5;
    let it = cur - prev;

    let denom = ix * ix + iy * iy + 0.0008;
    let flow_pix = (it / denom) * vec2<f32>(ix, iy);
    let flow_pix_clamped = clamp(flow_pix, vec2<f32>(-2.0), vec2<f32>(2.0));
    let flow_uv = flow_pix_clamped * texel;

    let confidence = clamp((ix * ix + iy * iy) * 14.0, 0.0, 1.0);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(flow_uv.x, flow_uv.y, confidence, 1.0));
}

// ---------------------------------------------------------------------------
// Pass 7: FlowAdvectCoord
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_flow_advect_coord(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let flow_sample = textureSampleLevel(flow_tex, samp, uv, 0.0);
    let flow_uv = flow_sample.rg;
    let confidence = flow_sample.b;
    let valid = flow_sample.a;

    let texel = vec2<f32>(u.texel_x, u.texel_y);
    let flow_l = textureSampleLevel(flow_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).rg;
    let flow_r = textureSampleLevel(flow_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).rg;
    let flow_b = textureSampleLevel(flow_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).rg;
    let flow_t = textureSampleLevel(flow_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).rg;
    let div = abs(flow_r.x - flow_l.x) + abs(flow_t.y - flow_b.y);
    let disocclusion = clamp((div - 0.0016) * 85.0, 0.0, 1.0);

    let sample_uv = clamp(uv + flow_uv, vec2<f32>(0.0), vec2<f32>(1.0));
    let prev_data = textureSampleLevel(prev_mesh_coord_tex, samp, sample_uv, 0.0);
    var prev_coord = prev_data.rg;
    let prev_trust = prev_data.b;
    let has_prev = step(0.05, prev_data.a);
    let prev_cooldown = decode_cooldown(prev_data.a);
    prev_coord = mix(uv, prev_coord, has_prev);

    let cur_lum = textureSampleLevel(main_tex, samp, uv, 0.0).r;
    let prev_lum_warp = textureSampleLevel(prev_analysis_tex, samp, sample_uv, 0.0).r;
    let photo_err = abs(cur_lum - prev_lum_warp);
    let photo_mismatch = smoothstep(0.05, 0.22, photo_err);

    let c_l = textureSampleLevel(main_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).r;
    let c_r = textureSampleLevel(main_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).r;
    let c_b = textureSampleLevel(main_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).r;
    let c_t = textureSampleLevel(main_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).r;
    let grad_mag = sqrt((c_r - c_l) * (c_r - c_l) + (c_t - c_b) * (c_t - c_b));

    let flow_quality = clamp(confidence * valid, 0.0, 1.0);
    let occlusion = 1.0 - clamp(valid, 0.0, 1.0);
    let neigh_valid = min(
        min(textureSampleLevel(flow_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).a,
            textureSampleLevel(flow_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).a),
        min(textureSampleLevel(flow_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).a,
            textureSampleLevel(flow_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).a));
    let disocclusion_dilated = clamp(max(disocclusion, (1.0 - neigh_valid) * 0.88), 0.0, 1.0);
    let anchor = smoothstep(0.03, 0.16, grad_mag) * valid;
    let trust_carry = max(prev_trust * 0.975, flow_quality);
    var trust = clamp(max(trust_carry, anchor * 0.85), 0.0, 1.0);
    trust = trust * (1.0 - disocclusion_dilated * 0.68);
    trust = trust * (1.0 - photo_mismatch * 0.80);
    trust = trust * (1.0 - prev_cooldown * 0.55);

    let settle = (1.0 - trust) * 0.015 + disocclusion_dilated * 0.060 + photo_mismatch * 0.032;
    let relaxed_coord = mix(prev_coord, uv, settle);
    let lock_base = clamp(0.40 + u.flow_lock_strength * 0.60, 0.0, 1.0);
    let lock = clamp(lock_base * trust + 0.08 - disocclusion_dilated * 0.25 - photo_mismatch * 0.25, 0.0, 1.0);
    var coord = mix(uv, relaxed_coord, lock);

    let delta = coord - prev_coord;
    let d_len = length(delta);
    var max_step = mix(0.0038, 0.030, flow_quality);
    max_step = mix(max_step * 0.55, max_step, clamp(confidence * valid, 0.0, 1.0));
    if d_len > max_step {
        coord = prev_coord + delta * (max_step / max(d_len, 1e-5));
    }

    let flow_pix = vec2<f32>(
        flow_uv.x / max(texel.x, 1e-5),
        flow_uv.y / max(texel.y, 1e-5));
    let motion_px = length(flow_pix);
    let motion_norm = clamp(motion_px / 2.2, 0.0, 1.0);
    var inertia = clamp(u.temporal_smooth * (0.24 + trust * 0.44 + (1.0 - flow_quality) * 0.20), 0.0, 1.0);
    inertia = inertia * (1.0 - disocclusion_dilated * 0.55);
    inertia = inertia * (1.0 - photo_mismatch * 0.50);
    inertia = inertia * (1.0 - motion_norm * 0.70);
    coord = mix(coord, prev_coord, inertia);

    let low_conf = 1.0 - clamp(confidence, 0.0, 1.0);
    let reseed = clamp(
        occlusion * 0.85 + disocclusion_dilated * 0.95 + photo_mismatch * 0.65 +
        low_conf * 0.22 + prev_cooldown * 0.35, 0.0, 1.0);
    let reseed_soft = smoothstep(0.22, 0.88, reseed);
    let reseed_hard = smoothstep(0.58, 0.97, reseed);
    coord = mix(coord, uv, clamp(reseed_soft * 0.82 + reseed_hard * 0.33, 0.0, 1.0));
    var trust_out = mix(trust, flow_quality * (1.0 - occlusion), reseed_soft);
    let cooldown = max(prev_cooldown * 0.70, reseed_soft * 0.75 + reseed_hard * 0.35);
    trust_out = trust_out * (1.0 - reseed_hard * 0.65);
    trust_out = trust_out * (1.0 - cooldown * 0.45);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(coord, clamp(trust_out, 0.0, 1.0), encode_cooldown(cooldown)));
}

// ---------------------------------------------------------------------------
// Pass 8: InitMeshCoord
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_init_mesh_coord(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(uv, 1.0, encode_cooldown(0.0)));
}

// ---------------------------------------------------------------------------
// Pass 9: MeshRegularize
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_mesh_regularize(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let c0 = textureSampleLevel(main_tex, samp, uv, 0.0);
    let c = c0.rg;
    let trust = c0.b;

    let ce = textureSampleLevel(main_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).rg;
    let cw = textureSampleLevel(main_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).rg;
    let cn = textureSampleLevel(main_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).rg;
    let cs_v = textureSampleLevel(main_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).rg;
    let lap = (ce + cw + cn + cs_v) * 0.25;

    let pe = textureSampleLevel(prev_mesh_coord_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).rg;
    let pw = textureSampleLevel(prev_mesh_coord_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).rg;
    let pn = textureSampleLevel(prev_mesh_coord_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).rg;
    let ps = textureSampleLevel(prev_mesh_coord_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).rg;

    let p_ex = pe - pw;
    let p_ey = pn - ps;
    let c_ex = ce - cw;

    let prev_angle = atan2(p_ex.y, p_ex.x);
    let cur_angle  = atan2(c_ex.y, c_ex.x);
    let d = cur_angle - prev_angle;
    let s_val = sin(d);
    let cc = cos(d);
    let target_ex = vec2<f32>(cc * p_ex.x + (-s_val) * p_ex.y, s_val * p_ex.x + cc * p_ex.y);
    let target_ey = vec2<f32>(cc * p_ey.x + (-s_val) * p_ey.y, s_val * p_ey.x + cc * p_ey.y);
    let rigid_center =
        0.25 * ((ce - target_ex * 0.5) + (cw + target_ex * 0.5) +
                (cn - target_ey * 0.5) + (cs_v + target_ey * 0.5));

    let flow = textureSampleLevel(flow_tex, samp, uv, 0.0);
    let flow_conf = clamp(flow.b * flow.a, 0.0, 1.0);
    let reg = clamp(u.mesh_regularize, 0.0, 1.0);

    let keep_w_raw = clamp(0.45 + trust * 0.40 + flow_conf * 0.25, 0.0, 1.0);
    let keep_w = keep_w_raw * (1.0 - reg * 0.55);
    let smooth_w = reg * (0.28 + (1.0 - flow_conf) * 0.22);
    let rigid_w = reg * (0.40 + flow_conf * 0.45);

    let w_sum = max(keep_w + smooth_w + rigid_w, 1e-4);
    var coord = (c * keep_w + lap * smooth_w + rigid_center * rigid_w) / w_sum;

    let relax_to_uv = uv - coord;
    let relax_w = (0.0025 + (1.0 - flow_conf) * 0.0035) * mix(1.0, 0.62, u.temporal_smooth);
    coord = coord + relax_to_uv * relax_w;

    let prev_center = textureSampleLevel(prev_mesh_coord_tex, samp, uv, 0.0).rg;
    let motion_proxy = length(c - prev_center);
    let temporal_anchor_raw = clamp(u.temporal_smooth * (0.08 + trust * 0.30 + (1.0 - flow_conf) * 0.34), 0.0, 1.0);
    let temporal_anchor = temporal_anchor_raw * (1.0 - smoothstep(0.0015, 0.018, motion_proxy));
    coord = mix(coord, prev_center, temporal_anchor);
    coord = clamp(coord, vec2<f32>(0.0), vec2<f32>(1.0));

    let trust_out = clamp(max(trust * 0.985, flow_conf * 0.90), 0.0, 1.0);
    let cooldown = decode_cooldown(c0.a) * 0.93;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(coord, trust_out, encode_cooldown(cooldown)));
}

// ---------------------------------------------------------------------------
// Pass 10: MeshCellAffine
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_mesh_cell_affine(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let c0 = textureSampleLevel(main_tex, samp, uv, 0.0);
    var coord = c0.rg;
    let trust = c0.b;

    let cell_count = clamp(u.grid_density * 0.22, 12.0, 72.0);
    let cell_center_uv = (floor(uv * cell_count) + vec2<f32>(0.5)) / cell_count;

    let center_data = textureSampleLevel(main_tex, samp, cell_center_uv, 0.0);
    let center_coord = center_data.rg;
    let du = uv - cell_center_uv;

    let step_uv = texel * 2.0;
    let flow_c = textureSampleLevel(flow_tex, samp, cell_center_uv, 0.0);
    let flow_l = textureSampleLevel(flow_tex, samp, cell_center_uv - vec2<f32>(step_uv.x, 0.0), 0.0).rg;
    let flow_r = textureSampleLevel(flow_tex, samp, cell_center_uv + vec2<f32>(step_uv.x, 0.0), 0.0).rg;
    let flow_b = textureSampleLevel(flow_tex, samp, cell_center_uv - vec2<f32>(0.0, step_uv.y), 0.0).rg;
    let flow_t = textureSampleLevel(flow_tex, samp, cell_center_uv + vec2<f32>(0.0, step_uv.y), 0.0).rg;

    let d_fdx = (flow_r - flow_l) / max(2.0 * step_uv.x, 1e-5);
    let d_fdy = (flow_t - flow_b) / max(2.0 * step_uv.y, 1e-5);

    let j00 = clamp(1.0 + d_fdx.x, 0.55, 1.65);
    let j01 = clamp(d_fdy.x, -0.60, 0.60);
    let j10 = clamp(d_fdx.y, -0.60, 0.60);
    let j11 = clamp(1.0 + d_fdy.y, 0.55, 1.65);
    let det = j00 * j11 - j01 * j10;
    let sane = step(0.22, det) * step(det, 2.60);

    let affine_coord = clamp(center_coord + vec2<f32>(
        j00 * du.x + j01 * du.y,
        j10 * du.x + j11 * du.y),
        vec2<f32>(0.0), vec2<f32>(1.0));

    let flow_conf = clamp(flow_c.b * flow_c.a, 0.0, 1.0);
    let div_val = abs(d_fdx.x) + abs(d_fdy.y);
    let disocclusion = clamp((div_val - 0.0012) * 70.0, 0.0, 1.0);

    var strength = clamp(u.cell_affine_strength, 0.0, 1.0);
    strength = strength * flow_conf;
    strength = strength * (0.35 + trust * 0.65);
    strength = strength * (1.0 - disocclusion * 0.75);
    strength = strength * sane;

    coord = mix(coord, affine_coord, strength);
    let trust_out = clamp(max(trust * 0.992, flow_conf * 0.92), 0.0, 1.0);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(coord, trust_out, c0.a));
}

// ---------------------------------------------------------------------------
// Pass 11: SemanticMask
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_semantic_mask(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let lum = textureSampleLevel(main_tex, samp, uv, 0.0).r;
    let depth_val = textureSampleLevel(depth_tex, samp, uv, 0.0).r;
    let flow = textureSampleLevel(flow_tex, samp, uv, 0.0);
    let flow_conf = clamp(flow.b * flow.a, 0.0, 1.0);

    let l_l = textureSampleLevel(main_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).r;
    let l_r = textureSampleLevel(main_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).r;
    let l_b = textureSampleLevel(main_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).r;
    let l_t = textureSampleLevel(main_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).r;
    let grad = sqrt((l_r - l_l) * (l_r - l_l) + (l_t - l_b) * (l_t - l_b));

    var body = smoothstep(0.14, 0.64, flow_conf * 0.92 + (1.0 - depth_val) * 0.16 + grad * 0.20);
    body = body * smoothstep(0.05, 0.92, 1.0 - abs(lum - 0.5) * 1.4);

    let p_val = (uv - vec2<f32>(0.5)) * vec2<f32>(1.20, 1.55);
    let center_bias = 1.0 - smoothstep(0.32, 0.98, length(p_val));
    let face = body * center_bias * smoothstep(0.10, 0.70, (1.0 - depth_val) * 0.35 + flow_conf * 0.65);

    var boundary_val = smoothstep(0.07, 0.28, grad) * body;
    boundary_val = clamp(boundary_val * (0.6 + (1.0 - face) * 0.4), 0.0, 1.0);

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(clamp(body, 0.0, 1.0), clamp(face, 0.0, 1.0), clamp(boundary_val, 0.0, 1.0), 1.0));
}

// ---------------------------------------------------------------------------
// Pass 12: MeshFaceWarp
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_mesh_face_warp(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let c0 = textureSampleLevel(main_tex, samp, uv, 0.0);
    var coord = c0.rg;
    let trust = c0.b;

    let sem = textureSampleLevel(semantic_tex, samp, uv, 0.0).rgb;
    let face = sem.g;
    let boundary_val = sem.b;
    if face < 0.02 {
        textureStore(output_tex, vec2<i32>(id.xy), c0);
        return;
    }

    let flow_c = textureSampleLevel(flow_tex, samp, uv, 0.0).rg;
    let flow_l = textureSampleLevel(flow_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).rg;
    let flow_r = textureSampleLevel(flow_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).rg;
    let flow_b = textureSampleLevel(flow_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).rg;
    let flow_t = textureSampleLevel(flow_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).rg;

    let face_grad = vec2<f32>(
        textureSampleLevel(semantic_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0).g -
        textureSampleLevel(semantic_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0).g,
        textureSampleLevel(semantic_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0).g -
        textureSampleLevel(semantic_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0).g) * 0.5;

    let d_fdx = (flow_r - flow_l) / max(2.0 * texel.x, 1e-5);
    let d_fdy = (flow_t - flow_b) / max(2.0 * texel.y, 1e-5);
    var warp_vec = vec2<f32>(d_fdy.x - d_fdx.y, d_fdx.x + d_fdy.y);
    warp_vec = warp_vec * 0.55 + flow_c * 0.35 + face_grad * 0.20;

    var strength = clamp(u.face_warp_strength, 0.0, 1.0);
    strength = strength * face;
    strength = strength * (1.0 - boundary_val * 0.65);
    strength = strength * clamp(0.45 + trust * 0.55, 0.0, 1.0);

    coord = clamp(coord + warp_vec * strength * 0.18, vec2<f32>(0.0), vec2<f32>(1.0));
    let trust_out = clamp(max(trust * 0.99, face * 0.86), 0.0, 1.0);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(coord, trust_out, c0.a));
}

// ---------------------------------------------------------------------------
// Pass 13: SurfaceCacheUpdate
// ---------------------------------------------------------------------------
@compute @workgroup_size(16, 16)
fn cs_surface_cache_update(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let mesh = textureSampleLevel(main_tex, samp, uv, 0.0);
    let curr_coord = mesh.rg;
    let trust = mesh.b;

    let prev = textureSampleLevel(prev_surface_cache_tex, samp, curr_coord, 0.0);
    let prev_id = prev.rg;
    let prev_age = prev.b;
    let prev_valid = prev.a;

    let dist = distance(prev_id, curr_coord);
    let stable = (1.0 - smoothstep(0.010, 0.080, dist)) * prev_valid;
    let id_val = mix(curr_coord, prev_id, stable);

    let flow_sample = textureSampleLevel(flow_tex, samp, uv, 0.0);
    let flow_quality = clamp(flow_sample.b * flow_sample.a, 0.0, 1.0);
    let carry = prev_age * mix(0.88, 0.996, clamp(u.surface_persistence, 0.0, 1.0));
    var age = mix(0.10, carry + 0.030, stable);
    age = max(age, stable * (0.20 + prev_age * 0.80));
    age = age * (0.80 + flow_quality * 0.20);
    age = clamp(age, 0.0, 1.0);

    let valid_val = clamp((0.42 + trust * 0.58) * (0.55 + flow_quality * 0.45), 0.0, 1.0);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(clamp(id_val, vec2<f32>(0.0), vec2<f32>(1.0)), age, valid_val));
}

// ---------------------------------------------------------------------------
// Pass 14: FlowHygiene
// ---------------------------------------------------------------------------
fn accumulate_flow_sample(
    s: vec4<f32>,
    f0: vec2<f32>,
    acc_f: ptr<function, vec2<f32>>,
    acc_w: ptr<function, f32>,
    acc_conf: ptr<function, f32>,
    acc_valid: ptr<function, f32>
) {
    let fn_ = s.rg;
    let conf_n = clamp(s.b, 0.0, 1.0);
    let valid_n = clamp(s.a, 0.0, 1.0);
    let q_n = conf_n * valid_n;
    let dist = length(fn_ - f0);
    let sim = 1.0 - smoothstep(0.002, 0.060, dist);
    let w = q_n * (0.25 + sim * 0.75);
    *acc_f = *acc_f + fn_ * w;
    *acc_w = *acc_w + w;
    *acc_conf = *acc_conf + conf_n * w;
    *acc_valid = *acc_valid + valid_n * w;
}

@compute @workgroup_size(16, 16)
fn cs_flow_hygiene(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let c0 = textureSampleLevel(main_tex, samp, uv, 0.0);
    let f0 = c0.rg;
    let conf0 = clamp(c0.b, 0.0, 1.0);
    let valid0 = clamp(c0.a, 0.0, 1.0);
    let q0 = conf0 * valid0;

    let s_l  = textureSampleLevel(main_tex, samp, uv - vec2<f32>(texel.x, 0.0), 0.0);
    let s_r  = textureSampleLevel(main_tex, samp, uv + vec2<f32>(texel.x, 0.0), 0.0);
    let s_b  = textureSampleLevel(main_tex, samp, uv - vec2<f32>(0.0, texel.y), 0.0);
    let s_t  = textureSampleLevel(main_tex, samp, uv + vec2<f32>(0.0, texel.y), 0.0);
    let s_lb = textureSampleLevel(main_tex, samp, uv - texel, 0.0);
    let s_rb = textureSampleLevel(main_tex, samp, uv + vec2<f32>(texel.x, -texel.y), 0.0);
    let s_lt = textureSampleLevel(main_tex, samp, uv + vec2<f32>(-texel.x, texel.y), 0.0);
    let s_rt = textureSampleLevel(main_tex, samp, uv + texel, 0.0);

    var acc_f: vec2<f32> = vec2<f32>(0.0);
    var acc_w: f32 = 0.0;
    var acc_conf: f32 = 0.0;
    var acc_valid: f32 = 0.0;

    accumulate_flow_sample(c0,   f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_l,  f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_r,  f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_b,  f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_t,  f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_lb, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_rb, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_lt, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_rt, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);

    var f_out = select(f0, acc_f / acc_w, acc_w > 1e-5);
    var conf_out = select(conf0, acc_conf / acc_w, acc_w > 1e-5);
    var valid_out = select(valid0, acc_valid / acc_w, acc_w > 1e-5);

    let neigh_valid = (
        clamp(s_l.a, 0.0, 1.0) + clamp(s_r.a, 0.0, 1.0) +
        clamp(s_b.a, 0.0, 1.0) + clamp(s_t.a, 0.0, 1.0) +
        clamp(s_lb.a, 0.0, 1.0) + clamp(s_rb.a, 0.0, 1.0) +
        clamp(s_lt.a, 0.0, 1.0) + clamp(s_rt.a, 0.0, 1.0)) / 8.0;
    let fill = smoothstep(0.02, 0.25, 1.0 - q0) * smoothstep(0.18, 0.70, neigh_valid);
    f_out = mix(f_out, (s_l.rg + s_r.rg + s_b.rg + s_t.rg) * 0.25, fill);
    conf_out = mix(conf_out, max(conf_out, neigh_valid * 0.75), fill);
    valid_out = mix(valid_out, max(valid_out, neigh_valid * 0.90), fill);

    let preserve = smoothstep(0.55, 0.92, q0);
    f_out = mix(f_out, f0, preserve * 0.78);
    conf_out = mix(conf_out, conf0, preserve * 0.78);
    valid_out = mix(valid_out, valid0, preserve * 0.78);

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(f_out, clamp(conf_out, 0.0, 1.0), clamp(valid_out, 0.0, 1.0)));
}
