// Port of WireframeDepthEffect.shader — 12 passes (removed heuristic_depth, update_history, semantic_mask).
// Unity source: Assets/Shaders/WireframeDepthEffect.shader
// Same math, same variable names, same constants, same pass order.

// Must match Rust WireUniforms exactly (20 × f32 = 80 bytes = 5 × vec4).
struct Uniforms {
    amount: f32,                // _Amount
    grid_density: f32,          // _GridDensity
    line_width: f32,            // _LineWidth
    depth_scale: f32,           // _DepthScale

    temporal_smooth: f32,       // _TemporalSmooth
    flow_lock_strength: f32,    // _FlowLockStrength
    mesh_regularize: f32,       // _MeshRegularize
    cell_affine_strength: f32,  // _CellAffineStrength

    edge_follow_strength: f32,  // _EdgeFollowStrength (was _FaceWarpStrength)
    surface_persistence: f32,   // _SurfacePersistence
    wire_taa: f32,              // _WireTaa
    subject_isolation: f32,     // _SubjectIsolation

    blend_mode: f32,            // _BlendMode
    texel_x: f32,               // _MainTex_TexelSize.x = 1/w
    texel_y: f32,               // _MainTex_TexelSize.y = 1/h
    depth_texel_x: f32,         // _DepthTex_TexelSize.x

    depth_texel_y: f32,         // _DepthTex_TexelSize.y
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var main_tex: texture_2d<f32>;        // _MainTex
@group(0) @binding(2) var prev_analysis_tex: texture_2d<f32>; // _PrevAnalysisTex
@group(0) @binding(3) var prev_depth_tex: texture_2d<f32>;  // _PrevDepthTex
@group(0) @binding(4) var depth_tex: texture_2d<f32>;       // _DepthTex
@group(0) @binding(5) var history_tex: texture_2d<f32>;     // _HistoryTex
@group(0) @binding(6) var flow_tex: texture_2d<f32>;        // _FlowTex
@group(0) @binding(7) var mesh_coord_tex: texture_2d<f32>;  // _MeshCoordTex
@group(0) @binding(8) var prev_mesh_coord_tex: texture_2d<f32>; // _PrevMeshCoordTex
@group(0) @binding(9) var semantic_tex: texture_2d<f32>;    // _SemanticTex
@group(0) @binding(10) var surface_cache_tex: texture_2d<f32>;  // _SurfaceCacheTex
@group(0) @binding(11) var prev_surface_cache_tex: texture_2d<f32>; // _PrevSurfaceCacheTex
@group(0) @binding(12) var subject_mask_tex: texture_2d<f32>; // _SubjectMaskTex
@group(0) @binding(13) var samp: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// Shared helpers — same as CGINCLUDE block in Unity shader.
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
// Pass 0: Analysis — downsample input and extract luminance.
// Unity: fragAnalysis
// ---------------------------------------------------------------------------
@fragment
fn fs_analysis(in: VertexOutput) -> @location(0) vec4<f32> {
    let l = luminance(textureSample(main_tex, samp, in.uv).rgb);
    return vec4<f32>(l, l, l, 1.0);
}

// (Pass 1: HeuristicDepth removed — DNN depth always used)

// ---------------------------------------------------------------------------
// Pass 1: WireMask — displaced wireframe mask from pseudo-depth.
// Unity: fragWireMask
// _DepthTex_TexelSize maps to u.depth_texel_x, u.depth_texel_y
// ---------------------------------------------------------------------------
@fragment
fn fs_wire_mask(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let depth_sample = textureSample(depth_tex, samp, uv);
    var depth = depth_sample.r;
    let confidence = depth_sample.a;

    // Foreground mask.
    let base_mask = smoothstep(0.28, 0.72, confidence);
    let sem = textureSample(semantic_tex, samp, uv).rgb;
    let body_mask = sem.r;
    let face_mask = sem.g;
    let boundary_mask = sem.b;

    let center_delta = (uv - vec2<f32>(0.5)) * vec2<f32>(1.0, 1.35);
    let center_bias = 1.0 - smoothstep(0.28, 1.08, length(center_delta));
    let sem_foreground = clamp(max(body_mask * (0.28 + center_bias * 0.95) + boundary_mask * 0.24, face_mask * 1.28), 0.0, 1.0);

    // Unity UV has v=0 at bottom; wgpu has uv.y=0 at top.
    // Flip Y of these hardcoded positions so they sample the same screen regions
    // (slightly above center, face/body area) as the Unity shader.
    let d_c0 = textureSample(depth_tex, samp, vec2<f32>(0.50, 0.48)).r;
    let d_c1 = textureSample(depth_tex, samp, vec2<f32>(0.38, 0.46)).r;
    let d_c2 = textureSample(depth_tex, samp, vec2<f32>(0.62, 0.46)).r;
    let d_c3 = textureSample(depth_tex, samp, vec2<f32>(0.50, 0.60)).r;
    let d_c4 = textureSample(depth_tex, samp, vec2<f32>(0.50, 0.34)).r;
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

    let subject_dnn_sample = textureSample(subject_mask_tex, samp, uv);
    let subject_dnn_avail = subject_dnn_sample.a;
    if subject_dnn_avail > 0.001 {
        let t = vec2<f32>(u.depth_texel_x, u.depth_texel_y);
        var subject_dnn = subject_dnn_sample.r;
        subject_dnn = max(subject_dnn, textureSample(subject_mask_tex, samp, uv + vec2<f32>(t.x, 0.0)).r);
        subject_dnn = max(subject_dnn, textureSample(subject_mask_tex, samp, uv - vec2<f32>(t.x, 0.0)).r);
        subject_dnn = max(subject_dnn, textureSample(subject_mask_tex, samp, uv + vec2<f32>(0.0, t.y)).r);
        subject_dnn = max(subject_dnn, textureSample(subject_mask_tex, samp, uv - vec2<f32>(0.0, t.y)).r);

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

    let mesh_data = textureSample(mesh_coord_tex, samp, uv);
    var mesh_uv = mesh_data.rg;
    let has_mesh = step(0.5, mesh_data.a);
    mesh_uv = mix(uv, mesh_uv, has_mesh);
    let mesh_uv_raw = mesh_uv;
    let depth_raw = depth;

    // Density-aware mesh decimation proxy.
    let density = max(u.grid_density, 1.0);
    let decimate_strength = smoothstep(170.0, 34.0, density);
    let boundary = boundary_mask;
    let silhouette_protect = smoothstep(0.06, 0.30, boundary);
    let local_decimate = decimate_strength * (1.0 - silhouette_protect * 0.85);

    let decimate_cells = clamp(density * mix(1.0, 0.38, local_decimate), 8.0, 320.0);
    let snapped_mesh_uv = (floor(mesh_uv * decimate_cells) + vec2<f32>(0.5)) / decimate_cells;
    let snapped_depth = textureSample(depth_tex, samp, snapped_mesh_uv).r;

    mesh_uv = mix(mesh_uv, snapped_mesh_uv, local_decimate);
    depth = mix(depth, snapped_depth, local_decimate * 0.92);

    // Pseudo perspective warp around frame center.
    let p = (mesh_uv - vec2<f32>(0.5)) * 2.0;
    let z = depth * u.depth_scale;
    let persp = 1.0 / (1.0 + z * 1.6);
    var warped = p * persp;
    // Unity: float2(z*0.12, -z*0.08) — shifts right and down in Y-up UV.
    // wgpu Y-down: positive Y is down, so negate the Y offset.
    warped = warped + vec2<f32>(z * 0.12, z * 0.08);

    // Derive AA from unsnapped coordinates.
    let p_raw = (mesh_uv_raw - vec2<f32>(0.5)) * 2.0;
    let z_raw = depth_raw * u.depth_scale;
    let persp_raw = 1.0 / (1.0 + z_raw * 1.6);
    var warped_raw = p_raw * persp_raw;
    warped_raw = warped_raw + vec2<f32>(z_raw * 0.12, z_raw * 0.08);

    // Quad lattice for DCC-style wireframe feel.
    let grid_coord = warped * density * 0.50;
    let grid_coord_aa = warped_raw * density * 0.50;
    let quad_cell = abs(fract(grid_coord) - vec2<f32>(0.5));

    let width = (u.line_width * 0.020) + 0.004;
    let aa_raw = abs(dpdx(grid_coord_aa.x)) + abs(dpdy(grid_coord_aa.y));
    let aa = clamp(max(aa_raw, 1e-4), 0.0008, width * 2.2 + 0.015);

    let line_x = 1.0 - smoothstep(width, width + aa * 1.35, quad_cell.x);
    let line_y = 1.0 - smoothstep(width, width + aa * 1.35, quad_cell.y);
    let mesh_line = max(line_x, line_y);

    let d_l = textureSample(depth_tex, samp, uv - vec2<f32>(u.depth_texel_x, 0.0)).r;
    let d_r = textureSample(depth_tex, samp, uv + vec2<f32>(u.depth_texel_x, 0.0)).r;
    let d_b = textureSample(depth_tex, samp, uv - vec2<f32>(0.0, u.depth_texel_y)).r;
    let d_t = textureSample(depth_tex, samp, uv + vec2<f32>(0.0, u.depth_texel_y)).r;
    let curvature = abs(d_l + d_r + d_b + d_t - depth * 4.0);
    let curve_boost = 1.0 + smoothstep(0.012, 0.090, curvature) * 0.48;

    let surface_age = clamp(textureSample(surface_cache_tex, samp, uv).b, 0.0, 1.0);
    let depth_fade = mix(1.0, 0.45, depth);
    let stable_boost = mix(0.82, 1.22, surface_age);
    var wire = mesh_line * object_mask * depth_fade * stable_boost * curve_boost;
    wire = smoothstep(0.15, 0.88, wire);
    return vec4<f32>(wire, wire, wire, 1.0);
}

// (Pass 3: UpdateHistory removed — persistence/history no longer used)

// ---------------------------------------------------------------------------
// Pass 2: Composite — wire mask over source.
// Unity: fragComposite
// _MainTex = source frame, _HistoryTex binding = lineMask (direct, no history)
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

@fragment
fn fs_composite(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(main_tex, samp, in.uv);
    let line_value = textureSample(history_tex, samp, in.uv).r;
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
    return vec4<f32>(result, src.a);
}

// ---------------------------------------------------------------------------
// Pass 3: DnnDepthPost — post-process DNN depth into internal depth format.
// Unity: fragDnnDepthPost
// _MainTex = dnnDepthTexture (uploaded CPU texture)
// ---------------------------------------------------------------------------
@fragment
fn fs_dnn_depth_post(in: VertexOutput) -> @location(0) vec4<f32> {
    let depth_current = textureSample(main_tex, samp, in.uv).r;
    let depth_prev = textureSample(prev_depth_tex, samp, in.uv).r;
    let depth_smoothed = mix(depth_current, depth_prev, u.temporal_smooth * 0.85);
    let depth_value = clamp(depth_smoothed, 0.0, 1.0);
    return vec4<f32>(depth_value, depth_value, depth_value, 1.0);
}

// ---------------------------------------------------------------------------
// Pass 4: FlowEstimate — optical flow from prev analysis to current.
// Unity: fragFlowEstimate
// _MainTex = analysis, _PrevAnalysisTex = previousAnalysisTex
// ---------------------------------------------------------------------------
@fragment
fn fs_flow_estimate(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let cur  = textureSample(main_tex, samp, uv).r;
    let prev = textureSample(prev_analysis_tex, samp, uv).r;

    let cur_l = textureSample(main_tex, samp, uv - vec2<f32>(texel.x, 0.0)).r;
    let cur_r = textureSample(main_tex, samp, uv + vec2<f32>(texel.x, 0.0)).r;
    let cur_b = textureSample(main_tex, samp, uv - vec2<f32>(0.0, texel.y)).r;
    let cur_t = textureSample(main_tex, samp, uv + vec2<f32>(0.0, texel.y)).r;

    let ix = (cur_r - cur_l) * 0.5;
    let iy = (cur_t - cur_b) * 0.5;
    let it = cur - prev;

    let denom = ix * ix + iy * iy + 0.0008;
    // Backward flow (current -> previous) for stable coordinate advection.
    let flow_pix = (it / denom) * vec2<f32>(ix, iy);
    let flow_pix_clamped = clamp(flow_pix, vec2<f32>(-2.0), vec2<f32>(2.0));
    let flow_uv = flow_pix_clamped * texel;

    let confidence = clamp((ix * ix + iy * iy) * 14.0, 0.0, 1.0);
    return vec4<f32>(flow_uv.x, flow_uv.y, confidence, 1.0);
}

// ---------------------------------------------------------------------------
// Pass 5: FlowAdvectCoord — advect mesh coordinates by flow.
// Unity: fragFlowAdvectCoord
// _MainTex = analysis, _FlowTex, _PrevMeshCoordTex, _PrevAnalysisTex
// ---------------------------------------------------------------------------
@fragment
fn fs_flow_advect_coord(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let flow_sample = textureSample(flow_tex, samp, uv);
    let flow_uv = flow_sample.rg;
    let confidence = flow_sample.b;
    let valid = flow_sample.a;

    let texel = vec2<f32>(u.texel_x, u.texel_y);
    let flow_l = textureSample(flow_tex, samp, uv - vec2<f32>(texel.x, 0.0)).rg;
    let flow_r = textureSample(flow_tex, samp, uv + vec2<f32>(texel.x, 0.0)).rg;
    let flow_b = textureSample(flow_tex, samp, uv - vec2<f32>(0.0, texel.y)).rg;
    let flow_t = textureSample(flow_tex, samp, uv + vec2<f32>(0.0, texel.y)).rg;
    let div = abs(flow_r.x - flow_l.x) + abs(flow_t.y - flow_b.y);
    let disocclusion = clamp((div - 0.0016) * 85.0, 0.0, 1.0);

    let sample_uv = clamp(uv + flow_uv, vec2<f32>(0.0), vec2<f32>(1.0));
    let prev_data = textureSample(prev_mesh_coord_tex, samp, sample_uv);
    var prev_coord = prev_data.rg;
    let prev_trust = prev_data.b;
    let has_prev = step(0.05, prev_data.a);
    let prev_cooldown = decode_cooldown(prev_data.a);
    prev_coord = mix(uv, prev_coord, has_prev);

    let cur_lum = textureSample(main_tex, samp, uv).r;
    let prev_lum_warp = textureSample(prev_analysis_tex, samp, sample_uv).r;
    let photo_err = abs(cur_lum - prev_lum_warp);
    let photo_mismatch = smoothstep(0.05, 0.22, photo_err);

    let c_l = textureSample(main_tex, samp, uv - vec2<f32>(texel.x, 0.0)).r;
    let c_r = textureSample(main_tex, samp, uv + vec2<f32>(texel.x, 0.0)).r;
    let c_b = textureSample(main_tex, samp, uv - vec2<f32>(0.0, texel.y)).r;
    let c_t = textureSample(main_tex, samp, uv + vec2<f32>(0.0, texel.y)).r;
    let grad_mag = sqrt((c_r - c_l) * (c_r - c_l) + (c_t - c_b) * (c_t - c_b));

    let flow_quality = clamp(confidence * valid, 0.0, 1.0);
    let occlusion = 1.0 - clamp(valid, 0.0, 1.0);
    let neigh_valid = min(
        min(textureSample(flow_tex, samp, uv - vec2<f32>(texel.x, 0.0)).a,
            textureSample(flow_tex, samp, uv + vec2<f32>(texel.x, 0.0)).a),
        min(textureSample(flow_tex, samp, uv - vec2<f32>(0.0, texel.y)).a,
            textureSample(flow_tex, samp, uv + vec2<f32>(0.0, texel.y)).a));
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

    // Temporal clamped update.
    let delta = coord - prev_coord;
    let d_len = length(delta);
    var max_step = mix(0.0038, 0.030, flow_quality);
    max_step = mix(max_step * 0.55, max_step, clamp(confidence * valid, 0.0, 1.0));
    if d_len > max_step {
        coord = prev_coord + delta * (max_step / max(d_len, 1e-5));
    }

    // Velocity-aware inertia.
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

    // Occlusion-aware re-seed.
    let low_conf = 1.0 - clamp(confidence, 0.0, 1.0);
    let reseed = clamp(
        occlusion * 0.85 +
        disocclusion_dilated * 0.95 +
        photo_mismatch * 0.65 +
        low_conf * 0.22 +
        prev_cooldown * 0.35,
        0.0, 1.0);
    let reseed_soft = smoothstep(0.22, 0.88, reseed);
    let reseed_hard = smoothstep(0.58, 0.97, reseed);
    coord = mix(coord, uv, clamp(reseed_soft * 0.82 + reseed_hard * 0.33, 0.0, 1.0));
    var trust_out = mix(trust, flow_quality * (1.0 - occlusion), reseed_soft);
    let cooldown = max(prev_cooldown * 0.70, reseed_soft * 0.75 + reseed_hard * 0.35);
    trust_out = trust_out * (1.0 - reseed_hard * 0.65);
    trust_out = trust_out * (1.0 - cooldown * 0.45);
    return vec4<f32>(coord, clamp(trust_out, 0.0, 1.0), encode_cooldown(cooldown));
}

// ---------------------------------------------------------------------------
// Pass 6: InitMeshCoord — initialize mesh coordinate map to identity UV.
// Unity: fragInitMeshCoord
// ---------------------------------------------------------------------------
@fragment
fn fs_init_mesh_coord(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(in.uv, 1.0, encode_cooldown(0.0));
}

// ---------------------------------------------------------------------------
// Pass 7: MeshRegularize — ARAP-lite regularization.
// Unity: fragMeshRegularize
// _MainTex = coordNext or coordAffine, _PrevMeshCoordTex, _FlowTex
// ---------------------------------------------------------------------------
@fragment
fn fs_mesh_regularize(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let c0 = textureSample(main_tex, samp, uv);
    let c = c0.rg;
    let trust = c0.b;

    let ce = textureSample(main_tex, samp, uv + vec2<f32>(texel.x, 0.0)).rg;
    let cw = textureSample(main_tex, samp, uv - vec2<f32>(texel.x, 0.0)).rg;
    let cn = textureSample(main_tex, samp, uv + vec2<f32>(0.0, texel.y)).rg;
    let cs_v = textureSample(main_tex, samp, uv - vec2<f32>(0.0, texel.y)).rg;
    let lap = (ce + cw + cn + cs_v) * 0.25;

    let pe = textureSample(prev_mesh_coord_tex, samp, uv + vec2<f32>(texel.x, 0.0)).rg;
    let pw = textureSample(prev_mesh_coord_tex, samp, uv - vec2<f32>(texel.x, 0.0)).rg;
    let pn = textureSample(prev_mesh_coord_tex, samp, uv + vec2<f32>(0.0, texel.y)).rg;
    let ps = textureSample(prev_mesh_coord_tex, samp, uv - vec2<f32>(0.0, texel.y)).rg;

    let p_ex = pe - pw;
    let p_ey = pn - ps;
    let c_ex = ce - cw;

    let prev_angle = atan2(p_ex.y, p_ex.x);
    let cur_angle  = atan2(c_ex.y, c_ex.x);
    let d = cur_angle - prev_angle;
    let s_val = sin(d);
    let cc = cos(d);
    // float2x2 R = float2x2(cc, -s, s, cc)  applied as mul(R, v) = (cc*v.x + (-s)*v.y, s*v.x + cc*v.y)
    let target_ex = vec2<f32>(cc * p_ex.x + (-s_val) * p_ex.y, s_val * p_ex.x + cc * p_ex.y);
    let target_ey = vec2<f32>(cc * p_ey.x + (-s_val) * p_ey.y, s_val * p_ey.x + cc * p_ey.y);
    let rigid_center =
        0.25 *
        ((ce - target_ex * 0.5) +
         (cw + target_ex * 0.5) +
         (cn - target_ey * 0.5) +
         (cs_v + target_ey * 0.5));

    let flow = textureSample(flow_tex, samp, uv);
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

    // Temporal anchor in low-motion regions.
    let prev_center = textureSample(prev_mesh_coord_tex, samp, uv).rg;
    let motion_proxy = length(c - prev_center);
    let temporal_anchor_raw = clamp(u.temporal_smooth * (0.08 + trust * 0.30 + (1.0 - flow_conf) * 0.34), 0.0, 1.0);
    let temporal_anchor = temporal_anchor_raw * (1.0 - smoothstep(0.0015, 0.018, motion_proxy));
    coord = mix(coord, prev_center, temporal_anchor);
    coord = clamp(coord, vec2<f32>(0.0), vec2<f32>(1.0));

    let trust_out = clamp(max(trust * 0.985, flow_conf * 0.90), 0.0, 1.0);
    let cooldown = decode_cooldown(c0.a) * 0.93;
    return vec4<f32>(coord, trust_out, encode_cooldown(cooldown));
}

// ---------------------------------------------------------------------------
// Pass 8: MeshCellAffine — per-cell affine deformation from local flow Jacobian.
// Unity: fragMeshCellAffine
// _MainTex = coordNext, _FlowTex
// ---------------------------------------------------------------------------
@fragment
fn fs_mesh_cell_affine(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    let c0 = textureSample(main_tex, samp, uv);
    var coord = c0.rg;
    let trust = c0.b;

    let cell_count = clamp(u.grid_density * 0.22, 12.0, 72.0);
    let cell_center_uv = (floor(uv * cell_count) + vec2<f32>(0.5)) / cell_count;

    let center_data = textureSample(main_tex, samp, cell_center_uv);
    let center_coord = center_data.rg;
    let du = uv - cell_center_uv;

    let step_uv = texel * 2.0;
    let flow_c = textureSample(flow_tex, samp, cell_center_uv);
    let flow_l = textureSample(flow_tex, samp, cell_center_uv - vec2<f32>(step_uv.x, 0.0)).rg;
    let flow_r = textureSample(flow_tex, samp, cell_center_uv + vec2<f32>(step_uv.x, 0.0)).rg;
    let flow_b = textureSample(flow_tex, samp, cell_center_uv - vec2<f32>(0.0, step_uv.y)).rg;
    let flow_t = textureSample(flow_tex, samp, cell_center_uv + vec2<f32>(0.0, step_uv.y)).rg;

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
    let div = abs(d_fdx.x) + abs(d_fdy.y);
    let disocclusion = clamp((div - 0.0012) * 70.0, 0.0, 1.0);

    var strength = clamp(u.cell_affine_strength, 0.0, 1.0);
    strength = strength * flow_conf;
    strength = strength * (0.35 + trust * 0.65);
    strength = strength * (1.0 - disocclusion * 0.75);
    strength = strength * sane;

    coord = mix(coord, affine_coord, strength);
    let trust_out = clamp(max(trust * 0.992, flow_conf * 0.92), 0.0, 1.0);
    return vec4<f32>(coord, trust_out, c0.a);
}

// (Pass 11: SemanticMask removed — DNN subject mask used directly instead of GPU heuristic)

// ---------------------------------------------------------------------------
// Pass 9: MeshEdgeFollow — DNN-driven non-rigid warp prior to regularization.
// Reads DNN subject mask (via semantic_tex binding) instead of GPU heuristic.
// _MainTex = coordAffine, _SemanticTex = dnn_subject_texture, _FlowTex
// ---------------------------------------------------------------------------
@fragment
fn fs_mesh_edge_follow(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let texel = vec2<f32>(u.texel_x, u.texel_y);
    let c0 = textureSample(main_tex, samp, uv);
    var coord = c0.rg;
    let trust = c0.b;

    let sem = textureSample(semantic_tex, samp, uv).rgb;
    let face = sem.g;
    let boundary = sem.b;
    if face < 0.02 {
        return c0;
    }

    let flow_c = textureSample(flow_tex, samp, uv).rg;
    let flow_l = textureSample(flow_tex, samp, uv - vec2<f32>(texel.x, 0.0)).rg;
    let flow_r = textureSample(flow_tex, samp, uv + vec2<f32>(texel.x, 0.0)).rg;
    let flow_b = textureSample(flow_tex, samp, uv - vec2<f32>(0.0, texel.y)).rg;
    let flow_t = textureSample(flow_tex, samp, uv + vec2<f32>(0.0, texel.y)).rg;

    let face_grad = vec2<f32>(
        textureSample(semantic_tex, samp, uv + vec2<f32>(texel.x, 0.0)).g - textureSample(semantic_tex, samp, uv - vec2<f32>(texel.x, 0.0)).g,
        textureSample(semantic_tex, samp, uv + vec2<f32>(0.0, texel.y)).g - textureSample(semantic_tex, samp, uv - vec2<f32>(0.0, texel.y)).g) * 0.5;

    let d_fdx = (flow_r - flow_l) / max(2.0 * texel.x, 1e-5);
    let d_fdy = (flow_t - flow_b) / max(2.0 * texel.y, 1e-5);
    var warp_vec = vec2<f32>(d_fdy.x - d_fdx.y, d_fdx.x + d_fdy.y);
    warp_vec = warp_vec * 0.55 + flow_c * 0.35 + face_grad * 0.20;

    var strength = clamp(u.edge_follow_strength, 0.0, 1.0);
    strength = strength * face;
    strength = strength * (1.0 - boundary * 0.65);
    strength = strength * clamp(0.45 + trust * 0.55, 0.0, 1.0);

    coord = clamp(coord + warp_vec * strength * 0.18, vec2<f32>(0.0), vec2<f32>(1.0));
    let trust_out = clamp(max(trust * 0.99, face * 0.86), 0.0, 1.0);
    return vec4<f32>(coord, trust_out, c0.a);
}

// ---------------------------------------------------------------------------
// Pass 10: SurfaceCacheUpdate — persistent surface cache (stable IDs/age).
// Unity: fragSurfaceCacheUpdate
// _MainTex = meshCoordTex (after regularize), _PrevSurfaceCacheTex, _FlowTex
// ---------------------------------------------------------------------------
@fragment
fn fs_surface_cache_update(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let mesh = textureSample(main_tex, samp, uv);
    let curr_coord = mesh.rg;
    let trust = mesh.b;

    let prev = textureSample(prev_surface_cache_tex, samp, curr_coord);
    let prev_id = prev.rg;
    let prev_age = prev.b;
    let prev_valid = prev.a;

    let dist = distance(prev_id, curr_coord);
    let stable = (1.0 - smoothstep(0.010, 0.080, dist)) * prev_valid;
    let id = mix(curr_coord, prev_id, stable);

    let flow_sample = textureSample(flow_tex, samp, uv);
    let flow_quality = clamp(flow_sample.b * flow_sample.a, 0.0, 1.0);
    let carry = prev_age * mix(0.88, 0.996, clamp(u.surface_persistence, 0.0, 1.0));
    var age = mix(0.10, carry + 0.030, stable);
    age = max(age, stable * (0.20 + prev_age * 0.80));
    age = age * (0.80 + flow_quality * 0.20);
    age = clamp(age, 0.0, 1.0);

    let valid = clamp((0.42 + trust * 0.58) * (0.55 + flow_quality * 0.45), 0.0, 1.0);
    return vec4<f32>(clamp(id, vec2<f32>(0.0), vec2<f32>(1.0)), age, valid);
}

// ---------------------------------------------------------------------------
// Pass 11: FlowHygiene — confidence-gated flow smoothing + hole fill.
// Unity: fragFlowHygiene
// _MainTex = flow source (native or PASS_FLOW_ESTIMATE output)
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

@fragment
fn fs_flow_hygiene(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let texel = vec2<f32>(u.texel_x, u.texel_y);
    let c0 = textureSample(main_tex, samp, uv);
    let f0 = c0.rg;
    let conf0 = clamp(c0.b, 0.0, 1.0);
    let valid0 = clamp(c0.a, 0.0, 1.0);
    let q0 = conf0 * valid0;

    let s_l  = textureSample(main_tex, samp, uv - vec2<f32>(texel.x, 0.0));
    let s_r  = textureSample(main_tex, samp, uv + vec2<f32>(texel.x, 0.0));
    let s_b  = textureSample(main_tex, samp, uv - vec2<f32>(0.0, texel.y));
    let s_t  = textureSample(main_tex, samp, uv + vec2<f32>(0.0, texel.y));
    let s_lb = textureSample(main_tex, samp, uv - texel);
    let s_rb = textureSample(main_tex, samp, uv + vec2<f32>(texel.x, -texel.y));
    let s_lt = textureSample(main_tex, samp, uv + vec2<f32>(-texel.x, texel.y));
    let s_rt = textureSample(main_tex, samp, uv + texel);

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

    // Hole fill.
    let neigh_valid = (
        clamp(s_l.a, 0.0, 1.0) + clamp(s_r.a, 0.0, 1.0) +
        clamp(s_b.a, 0.0, 1.0) + clamp(s_t.a, 0.0, 1.0) +
        clamp(s_lb.a, 0.0, 1.0) + clamp(s_rb.a, 0.0, 1.0) +
        clamp(s_lt.a, 0.0, 1.0) + clamp(s_rt.a, 0.0, 1.0)) / 8.0;
    let fill = smoothstep(0.02, 0.25, 1.0 - q0) * smoothstep(0.18, 0.70, neigh_valid);
    f_out = mix(f_out, (s_l.rg + s_r.rg + s_b.rg + s_t.rg) * 0.25, fill);
    conf_out = mix(conf_out, max(conf_out, neigh_valid * 0.75), fill);
    valid_out = mix(valid_out, max(valid_out, neigh_valid * 0.90), fill);

    // Preserve sharp high-confidence motion.
    let preserve = smoothstep(0.55, 0.92, q0);
    f_out = mix(f_out, f0, preserve * 0.78);
    conf_out = mix(conf_out, conf0, preserve * 0.78);
    valid_out = mix(valid_out, valid0, preserve * 0.78);

    return vec4<f32>(f_out, clamp(conf_out, 0.0, 1.0), clamp(valid_out, 0.0, 1.0));
}
