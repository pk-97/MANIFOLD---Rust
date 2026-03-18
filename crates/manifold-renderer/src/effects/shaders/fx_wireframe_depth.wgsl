// Mechanical port of WireframeDepthEffect.shader — all 15 passes.
// Unity source: Assets/Shaders/WireframeDepthEffect.shader
//
// Each pass is a separate fragment entry point; the Rust side creates 15
// separate render pipelines (one per pass) with bind groups that match
// each pass's texture requirements. Passes that don't use a texture receive
// a 1x1 dummy texture view in the corresponding slot.
//
// Bind group layout (shared across all passes):
//   binding 0  : Uniforms (see WireframeDepthUniforms)
//   binding 1  : _MainTex           (source / pass-specific primary input)
//   binding 2  : _PrevAnalysisTex
//   binding 3  : _PrevDepthTex
//   binding 4  : _DepthTex
//   binding 5  : _HistoryTex
//   binding 6  : _FlowTex
//   binding 7  : _MeshCoordTex
//   binding 8  : _PrevMeshCoordTex
//   binding 9  : _SemanticTex
//   binding 10 : _SurfaceCacheTex
//   binding 11 : _PrevSurfaceCacheTex
//   binding 12 : _SubjectMaskTex
//   binding 13 : sampler

// ---------------------------------------------------------------------------
// Uniforms — all passes share one struct; each pass reads only what it needs.
// 16-byte aligned: 18 f32 fields → pad to 20 f32 (80 bytes = 5 × vec4).
// ---------------------------------------------------------------------------
struct Uniforms {
    // vec4 at offset 0
    amount:            f32,   // _Amount
    grid_density:      f32,   // _GridDensity
    line_width:        f32,   // _LineWidth
    depth_scale:       f32,   // _DepthScale
    // vec4 at offset 16
    temporal_smooth:   f32,   // _TemporalSmooth
    persistence:       f32,   // _Persistence
    flow_lock_strength: f32,  // _FlowLockStrength
    mesh_regularize:   f32,   // _MeshRegularize
    // vec4 at offset 32
    cell_affine_strength: f32, // _CellAffineStrength
    face_warp_strength: f32,  // _FaceWarpStrength
    surface_persistence: f32, // _SurfacePersistence
    wire_taa:          f32,   // _WireTaa
    // vec4 at offset 48
    subject_isolation: f32,   // _SubjectIsolation
    blend_mode:        f32,   // _BlendMode
    main_texel_x:      f32,   // _MainTex_TexelSize.x  = 1/width
    main_texel_y:      f32,   // _MainTex_TexelSize.y  = 1/height
    // vec4 at offset 64
    depth_texel_x:     f32,   // _DepthTex_TexelSize.x = 1/analysis_width
    depth_texel_y:     f32,   // _DepthTex_TexelSize.y = 1/analysis_height
    _pad0:             f32,
    _pad1:             f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;

@group(0) @binding(1)  var main_tex:              texture_2d<f32>;
@group(0) @binding(2)  var prev_analysis_tex:     texture_2d<f32>;
@group(0) @binding(3)  var prev_depth_tex:        texture_2d<f32>;
@group(0) @binding(4)  var depth_tex:             texture_2d<f32>;
@group(0) @binding(5)  var history_tex:           texture_2d<f32>;
@group(0) @binding(6)  var flow_tex:              texture_2d<f32>;
@group(0) @binding(7)  var mesh_coord_tex:        texture_2d<f32>;
@group(0) @binding(8)  var prev_mesh_coord_tex:   texture_2d<f32>;
@group(0) @binding(9)  var semantic_tex:          texture_2d<f32>;
@group(0) @binding(10) var surface_cache_tex:     texture_2d<f32>;
@group(0) @binding(11) var prev_surface_cache_tex: texture_2d<f32>;
@group(0) @binding(12) var subject_mask_tex:      texture_2d<f32>;
@group(0) @binding(13) var tex_sampler:           sampler;

// ---------------------------------------------------------------------------
// Shared vertex shader — fullscreen triangle (same for all passes)
// ---------------------------------------------------------------------------
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// ---------------------------------------------------------------------------
// Shared helper functions
// ---------------------------------------------------------------------------
fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn decode_cooldown(encoded: f32) -> f32 {
    return saturate((encoded - 0.55) / 0.45);
}

fn encode_cooldown(cooldown: f32) -> f32 {
    return 0.55 + saturate(cooldown) * 0.45;
}

// ---------------------------------------------------------------------------
// Pass 0: Analysis — downsample input and extract luminance.
// Unity: fragAnalysis — reads _MainTex, writes luminance to all channels.
// ---------------------------------------------------------------------------
@fragment
fn fs_pass0(in: VertexOutput) -> @location(0) vec4<f32> {
    let l = luminance(textureSample(main_tex, tex_sampler, in.uv).rgb);
    return vec4<f32>(l, l, l, 1.0);
}

// ---------------------------------------------------------------------------
// Pass 1: HeuristicDepth — pseudo-depth from edges + frame delta + temporal.
// Unity: fragHeuristicDepth — reads _MainTex (analysis), _PrevAnalysisTex,
//        _PrevDepthTex. Texel size is from _MainTex_TexelSize (analysis size).
// ---------------------------------------------------------------------------
@fragment
fn fs_pass1(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = vec2<f32>(u.main_texel_x, u.main_texel_y);
    let uv = in.uv;

    let c   = textureSample(main_tex, tex_sampler, uv).r;
    let tl  = textureSample(main_tex, tex_sampler, uv + texel * vec2<f32>(-1.0, -1.0)).r;
    let tc  = textureSample(main_tex, tex_sampler, uv + texel * vec2<f32>( 0.0, -1.0)).r;
    let tr  = textureSample(main_tex, tex_sampler, uv + texel * vec2<f32>( 1.0, -1.0)).r;
    let ml  = textureSample(main_tex, tex_sampler, uv + texel * vec2<f32>(-1.0,  0.0)).r;
    let mr  = textureSample(main_tex, tex_sampler, uv + texel * vec2<f32>( 1.0,  0.0)).r;
    let bl  = textureSample(main_tex, tex_sampler, uv + texel * vec2<f32>(-1.0,  1.0)).r;
    let bc  = textureSample(main_tex, tex_sampler, uv + texel * vec2<f32>( 0.0,  1.0)).r;
    let br  = textureSample(main_tex, tex_sampler, uv + texel * vec2<f32>( 1.0,  1.0)).r;

    let gx = -tl - 2.0 * ml - bl + tr + 2.0 * mr + br;
    let gy = -tl - 2.0 * tc - tr + bl + 2.0 * bc + br;
    let edge = saturate(sqrt(gx * gx + gy * gy) * 0.18);

    let prev_luma    = textureSample(prev_analysis_tex, tex_sampler, uv).r;
    let motion       = saturate(abs(c - prev_luma) * 2.0);
    let luma_depth   = 1.0 - c;
    let neighborhood_mean = (tl + tc + tr + ml + c + mr + bl + bc + br) / 9.0;
    let local_contrast = saturate(abs(c - neighborhood_mean) * 2.0);
    let structure    = saturate(edge * 0.9 + local_contrast * 0.6);

    let raw_depth    = saturate(luma_depth * 0.78 + structure * 0.20 + motion * 0.10);
    let prev_depth   = textureSample(prev_depth_tex, tex_sampler, uv).r;
    let prev_depth_l = textureSample(prev_depth_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0)).r;
    let prev_depth_r = textureSample(prev_depth_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0)).r;
    let prev_depth_b = textureSample(prev_depth_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y)).r;
    let prev_depth_t = textureSample(prev_depth_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y)).r;
    let prev_depth_blur = (prev_depth * 2.0 + prev_depth_l + prev_depth_r + prev_depth_b + prev_depth_t) / 6.0;
    let smooth_depth = mix(raw_depth, prev_depth_blur, u.temporal_smooth);

    let confidence_raw = saturate(luma_depth * 0.60 + structure * 0.30 + motion * 0.10);
    let confidence     = smoothstep(0.35, 0.75, confidence_raw);
    return vec4<f32>(smooth_depth, smooth_depth, smooth_depth, confidence);
}

// ---------------------------------------------------------------------------
// Pass 2: WireMask — render displaced wireframe mask from pseudo-depth.
// Unity: fragWireMask — reads _DepthTex, _MeshCoordTex, _SemanticTex,
//        _SurfaceCacheTex, _SubjectMaskTex.
// Texel size is from _DepthTex_TexelSize.
// ---------------------------------------------------------------------------
@fragment
fn fs_pass2(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let depth_sample = textureSample(depth_tex, tex_sampler, uv);
    var depth        = depth_sample.r;
    let confidence   = depth_sample.a;

    let base_mask    = smoothstep(0.28, 0.72, confidence);
    let sem          = textureSample(semantic_tex, tex_sampler, uv).rgb;
    let body_mask    = sem.r;
    let face_mask    = sem.g;
    let boundary_mask = sem.b;

    let center_delta  = (uv - 0.5) * vec2<f32>(1.0, 1.35);
    let center_bias   = 1.0 - smoothstep(0.28, 1.08, length(center_delta));
    let sem_foreground = saturate(max(body_mask * (0.28 + center_bias * 0.95) + boundary_mask * 0.24, face_mask * 1.28));

    let d_c0 = textureSample(depth_tex, tex_sampler, vec2<f32>(0.50, 0.52)).r;
    let d_c1 = textureSample(depth_tex, tex_sampler, vec2<f32>(0.38, 0.54)).r;
    let d_c2 = textureSample(depth_tex, tex_sampler, vec2<f32>(0.62, 0.54)).r;
    let d_c3 = textureSample(depth_tex, tex_sampler, vec2<f32>(0.50, 0.40)).r;
    let d_c4 = textureSample(depth_tex, tex_sampler, vec2<f32>(0.50, 0.66)).r;
    let near_ref = min(min(d_c0, d_c1), min(min(d_c2, d_c3), d_c4));

    let isolation  = saturate(u.subject_isolation);
    let near_band  = 1.0 - smoothstep(
        near_ref + mix(0.62, 0.16, isolation),
        near_ref + mix(0.92, 0.28, isolation),
        depth);
    let depth_foreground = smoothstep(0.20, 0.80, 1.0 - depth);
    let subject_core = max(face_mask * 1.25, body_mask * (0.25 + center_bias * 0.98));
    let boundary_fill = smoothstep(0.03, 0.26, boundary_mask) * (0.35 + subject_core * 0.65);
    let subject_evidence = saturate(max(max(sem_foreground, subject_core), max(boundary_fill, depth_foreground * (0.22 + subject_core * 0.92))));
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

    let t                = vec2<f32>(u.depth_texel_x, u.depth_texel_y);
    let subject_dnn_sample = textureSample(subject_mask_tex, tex_sampler, uv);
    let subject_dnn_avail  = subject_dnn_sample.a;
    if subject_dnn_avail > 0.001 {
        var subject_dnn = subject_dnn_sample.r;
        subject_dnn = max(subject_dnn, textureSample(subject_mask_tex, tex_sampler, uv + vec2<f32>(t.x, 0.0)).r);
        subject_dnn = max(subject_dnn, textureSample(subject_mask_tex, tex_sampler, uv - vec2<f32>(t.x, 0.0)).r);
        subject_dnn = max(subject_dnn, textureSample(subject_mask_tex, tex_sampler, uv + vec2<f32>(0.0, t.y)).r);
        subject_dnn = max(subject_dnn, textureSample(subject_mask_tex, tex_sampler, uv - vec2<f32>(0.0, t.y)).r);

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

    let mesh_data = textureSample(mesh_coord_tex, tex_sampler, uv);
    var mesh_uv   = mesh_data.rg;
    let has_mesh  = step(0.5, mesh_data.a);
    mesh_uv       = mix(uv, mesh_uv, has_mesh);
    let mesh_uv_raw = mesh_uv;
    let depth_raw   = depth;

    let density_val      = max(u.grid_density, 1.0);
    let decimate_strength = smoothstep(170.0, 34.0, density_val);
    let boundary         = boundary_mask;
    let silhouette_protect = smoothstep(0.06, 0.30, boundary);
    let local_decimate   = decimate_strength * (1.0 - silhouette_protect * 0.85);

    let decimate_cells   = clamp(density_val * mix(1.0, 0.38, local_decimate), 8.0, 320.0);
    let snapped_mesh_uv  = (floor(mesh_uv * decimate_cells) + 0.5) / decimate_cells;
    let snapped_depth    = textureSample(depth_tex, tex_sampler, snapped_mesh_uv).r;

    mesh_uv = mix(mesh_uv, snapped_mesh_uv, local_decimate);
    depth   = mix(depth,   snapped_depth,   local_decimate * 0.92);

    let p    = (mesh_uv - 0.5) * 2.0;
    let z    = depth * u.depth_scale;
    let persp = 1.0 / (1.0 + z * 1.6);
    var warped = p * persp;
    warped += vec2<f32>(z * 0.12, -z * 0.08);

    let p_raw    = (mesh_uv_raw - 0.5) * 2.0;
    let z_raw    = depth_raw * u.depth_scale;
    let persp_raw = 1.0 / (1.0 + z_raw * 1.6);
    var warped_raw = p_raw * persp_raw;
    warped_raw += vec2<f32>(z_raw * 0.12, -z_raw * 0.08);

    let grid_coord    = warped * density_val * 0.50;
    let grid_coord_aa = warped_raw * density_val * 0.50;
    let quad_cell     = abs(fract(grid_coord) - 0.5);

    let width_val = (u.line_width * 0.020) + 0.004;
    let aa_raw    = fwidthFine(grid_coord_aa.x) + fwidthFine(grid_coord_aa.y);
    let aa        = clamp(max(aa_raw, 1e-4), 0.0008, width_val * 2.2 + 0.015);

    let line_x = 1.0 - smoothstep(width_val, width_val + aa * 1.35, quad_cell.x);
    let line_y = 1.0 - smoothstep(width_val, width_val + aa * 1.35, quad_cell.y);
    let mesh_line = max(line_x, line_y);

    let d_l = textureSample(depth_tex, tex_sampler, uv - vec2<f32>(u.depth_texel_x, 0.0)).r;
    let d_r = textureSample(depth_tex, tex_sampler, uv + vec2<f32>(u.depth_texel_x, 0.0)).r;
    let d_b = textureSample(depth_tex, tex_sampler, uv - vec2<f32>(0.0, u.depth_texel_y)).r;
    let d_t = textureSample(depth_tex, tex_sampler, uv + vec2<f32>(0.0, u.depth_texel_y)).r;
    let curvature  = abs(d_l + d_r + d_b + d_t - depth * 4.0);
    let curve_boost = 1.0 + smoothstep(0.012, 0.090, curvature) * 0.48;

    let surface_age  = saturate(textureSample(surface_cache_tex, tex_sampler, uv).b);
    let depth_fade   = mix(1.0, 0.45, depth);
    let stable_boost = mix(0.82, 1.22, surface_age);
    var wire = mesh_line * object_mask * depth_fade * stable_boost * curve_boost;
    wire     = smoothstep(0.15, 0.88, wire);
    return vec4<f32>(wire, wire, wire, 1.0);
}

// ---------------------------------------------------------------------------
// Pass 3: UpdateHistory — temporal persistence of line history.
// Unity: fragUpdateHistory — reads _MainTex (lineMask), _HistoryTex,
//        _SurfaceCacheTex. Texel from _MainTex_TexelSize (wire size).
// ---------------------------------------------------------------------------
@fragment
fn fs_pass3(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel    = vec2<f32>(u.main_texel_x, u.main_texel_y);
    let line_now  = textureSample(main_tex, tex_sampler, in.uv).r;
    let history_prev = textureSample(history_tex, tex_sampler, in.uv).r;
    let persist_t = saturate(u.persistence);
    let decay     = mix(0.55, 0.9985, persist_t);
    let stability = saturate(textureSample(surface_cache_tex, tex_sampler, in.uv).b);
    let taa       = saturate(u.wire_taa) * (0.22 + stability * 0.72);
    let reprojected = history_prev * decay;
    let n_l = textureSample(main_tex, tex_sampler, in.uv - vec2<f32>(texel.x, 0.0)).r;
    let n_r = textureSample(main_tex, tex_sampler, in.uv + vec2<f32>(texel.x, 0.0)).r;
    let n_b = textureSample(main_tex, tex_sampler, in.uv - vec2<f32>(0.0, texel.y)).r;
    let n_t = textureSample(main_tex, tex_sampler, in.uv + vec2<f32>(0.0, texel.y)).r;
    let local_min  = min(line_now, min(min(n_l, n_r), min(n_b, n_t)));
    let local_max  = max(line_now, max(max(n_l, n_r), max(n_b, n_t)));
    let clamp_pad  = 0.05 + (1.0 - stability) * 0.03;
    let reproj_clamped = clamp(reprojected, local_min - clamp_pad, local_max + clamp_pad);
    let support    = max(line_now, max(max(n_l, n_r), max(n_b, n_t)));
    let support_gate = smoothstep(0.025, 0.14, support);
    let reproj_gated = reproj_clamped * support_gate;
    let taa_gated    = taa * support_gate;
    let blended    = mix(line_now, reproj_gated, taa_gated);
    let line_value = max(blended, line_now * (0.72 + stability * 0.20));
    return vec4<f32>(line_value, line_value, line_value, 1.0);
}

// ---------------------------------------------------------------------------
// Pass 4: Composite — composite line history over source.
// Unity: fragComposite — reads _MainTex (source), _HistoryTex.
// ---------------------------------------------------------------------------
fn blend_add(base_col: vec3<f32>, blend_col: vec3<f32>) -> vec3<f32> {
    return saturate(base_col + blend_col);
}

fn blend_multiply(base_col: vec3<f32>, blend_col: vec3<f32>) -> vec3<f32> {
    return base_col * blend_col;
}

fn blend_screen(base_col: vec3<f32>, blend_col: vec3<f32>) -> vec3<f32> {
    return 1.0 - (1.0 - base_col) * (1.0 - blend_col);
}

fn blend_overlay(base_col: vec3<f32>, blend_col: vec3<f32>) -> vec3<f32> {
    let lo = 2.0 * base_col * blend_col;
    let hi = 1.0 - 2.0 * (1.0 - base_col) * (1.0 - blend_col);
    return select(hi, lo, base_col < vec3<f32>(0.5, 0.5, 0.5));
}

@fragment
fn fs_pass4(in: VertexOutput) -> @location(0) vec4<f32> {
    let src        = textureSample(main_tex, tex_sampler, in.uv);
    let line_value = textureSample(history_tex, tex_sampler, in.uv).r;
    let wire       = saturate(vec3<f32>(line_value * 1.1));
    let mask       = saturate(line_value);
    var mixed      = wire;
    let mode       = floor(u.blend_mode + 0.5);

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
// Pass 5: DnnDepthPost — post-process DNN depth into internal depth format.
// Unity: fragDnnDepthPost — reads _MainTex (dnnDepthTexture), _PrevDepthTex.
// ---------------------------------------------------------------------------
@fragment
fn fs_pass5(in: VertexOutput) -> @location(0) vec4<f32> {
    let depth_current = textureSample(main_tex, tex_sampler, in.uv).r;
    let depth_prev    = textureSample(prev_depth_tex, tex_sampler, in.uv).r;
    let depth_smoothed = mix(depth_current, depth_prev, u.temporal_smooth * 0.85);
    let depth_value    = saturate(depth_smoothed);
    return vec4<f32>(depth_value, depth_value, depth_value, 1.0);
}

// ---------------------------------------------------------------------------
// Pass 6: FlowEstimate — estimate optical flow from prev analysis to current.
// Unity: fragFlowEstimate — reads _MainTex (analysis), _PrevAnalysisTex.
// Texel from _MainTex_TexelSize (analysis size).
// ---------------------------------------------------------------------------
@fragment
fn fs_pass6(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv    = in.uv;
    let texel = vec2<f32>(u.main_texel_x, u.main_texel_y);

    let cur  = textureSample(main_tex, tex_sampler, uv).r;
    let prev = textureSample(prev_analysis_tex, tex_sampler, uv).r;

    let cur_l = textureSample(main_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0)).r;
    let cur_r = textureSample(main_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0)).r;
    let cur_b = textureSample(main_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y)).r;
    let cur_t = textureSample(main_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y)).r;

    let ix    = (cur_r - cur_l) * 0.5;
    let iy    = (cur_t - cur_b) * 0.5;
    let it    = cur - prev;

    let denom     = ix * ix + iy * iy + 0.0008;
    var flow_pix  = (it / denom) * vec2<f32>(ix, iy);
    flow_pix      = clamp(flow_pix, vec2<f32>(-2.0), vec2<f32>(2.0));
    let flow_uv   = flow_pix * texel;

    let confidence = saturate((ix * ix + iy * iy) * 14.0);
    return vec4<f32>(flow_uv.x, flow_uv.y, confidence, 1.0);
}

// ---------------------------------------------------------------------------
// Pass 7: FlowAdvectCoord — advect mesh coordinates by flow.
// Unity: fragFlowAdvectCoord — reads _MainTex (analysis), _FlowTex,
//        _PrevMeshCoordTex, _PrevAnalysisTex.
// Texel from _MainTex_TexelSize (analysis size).
// ---------------------------------------------------------------------------
@fragment
fn fs_pass7(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv   = in.uv;
    let flow_sample = textureSample(flow_tex, tex_sampler, uv);
    let flow_uv     = flow_sample.rg;
    let confidence  = flow_sample.b;
    let valid       = flow_sample.a;

    let texel = vec2<f32>(u.main_texel_x, u.main_texel_y);
    let flow_l = textureSample(flow_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0)).rg;
    let flow_r = textureSample(flow_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0)).rg;
    let flow_b = textureSample(flow_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y)).rg;
    let flow_t = textureSample(flow_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y)).rg;
    let div = abs(flow_r.x - flow_l.x) + abs(flow_t.y - flow_b.y);
    let disocclusion = saturate((div - 0.0016) * 85.0);

    let sample_uv = saturate(uv + flow_uv);
    let prev_data  = textureSample(prev_mesh_coord_tex, tex_sampler, sample_uv);
    var prev_coord  = prev_data.rg;
    let prev_trust  = prev_data.b;
    let has_prev    = step(0.05, prev_data.a);
    let prev_cooldown = decode_cooldown(prev_data.a);
    prev_coord      = mix(uv, prev_coord, has_prev);

    let cur_lum      = textureSample(main_tex, tex_sampler, uv).r;
    let prev_lum_warp = textureSample(prev_analysis_tex, tex_sampler, sample_uv).r;
    let photo_err    = abs(cur_lum - prev_lum_warp);
    let photo_mismatch = smoothstep(0.05, 0.22, photo_err);

    let c_l = textureSample(main_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0)).r;
    let c_r = textureSample(main_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0)).r;
    let c_b = textureSample(main_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y)).r;
    let c_t = textureSample(main_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y)).r;
    let grad_mag = sqrt((c_r - c_l) * (c_r - c_l) + (c_t - c_b) * (c_t - c_b));

    let flow_quality = saturate(confidence * valid);
    let occlusion    = 1.0 - saturate(valid);
    let neigh_valid  = min(
        min(textureSample(flow_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0)).a,
            textureSample(flow_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0)).a),
        min(textureSample(flow_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y)).a,
            textureSample(flow_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y)).a));
    let disocclusion_dilated = saturate(max(disocclusion, (1.0 - neigh_valid) * 0.88));
    let anchor      = smoothstep(0.03, 0.16, grad_mag) * valid;
    let trust_carry = max(prev_trust * 0.975, flow_quality);
    var trust       = saturate(max(trust_carry, anchor * 0.85));
    trust           = trust * (1.0 - disocclusion_dilated * 0.68);
    trust           = trust * (1.0 - photo_mismatch * 0.80);
    trust           = trust * (1.0 - prev_cooldown * 0.55);

    let settle         = (1.0 - trust) * 0.015 + disocclusion_dilated * 0.060 + photo_mismatch * 0.032;
    let relaxed_coord  = mix(prev_coord, uv, settle);
    let lock_base      = saturate(0.40 + u.flow_lock_strength * 0.60);
    let lock           = saturate(lock_base * trust + 0.08 - disocclusion_dilated * 0.25 - photo_mismatch * 0.25);
    var coord          = mix(uv, relaxed_coord, lock);

    let delta   = coord - prev_coord;
    let d_len   = length(delta);
    var max_step = mix(0.0038, 0.030, flow_quality);
    max_step     = mix(max_step * 0.55, max_step, saturate(confidence * valid));
    if d_len > max_step {
        coord = prev_coord + delta * (max_step / max(d_len, 1e-5));
    }

    let flow_pix   = vec2<f32>(
        flow_uv.x / max(texel.x, 1e-5),
        flow_uv.y / max(texel.y, 1e-5));
    let motion_px  = length(flow_pix);
    let motion_norm = saturate(motion_px / 2.2);
    var inertia    = saturate(u.temporal_smooth * (0.24 + trust * 0.44 + (1.0 - flow_quality) * 0.20));
    inertia        = inertia * (1.0 - disocclusion_dilated * 0.55);
    inertia        = inertia * (1.0 - photo_mismatch * 0.50);
    inertia        = inertia * (1.0 - motion_norm * 0.70);
    coord          = mix(coord, prev_coord, inertia);

    let low_conf   = 1.0 - saturate(confidence);
    let reseed     = saturate(
        occlusion * 0.85 +
        disocclusion_dilated * 0.95 +
        photo_mismatch * 0.65 +
        low_conf * 0.22 +
        prev_cooldown * 0.35);
    let reseed_soft = smoothstep(0.22, 0.88, reseed);
    let reseed_hard = smoothstep(0.58, 0.97, reseed);
    coord           = mix(coord, uv, saturate(reseed_soft * 0.82 + reseed_hard * 0.33));
    let trust_out   = mix(trust, flow_quality * (1.0 - occlusion), reseed_soft);
    let cooldown    = max(prev_cooldown * 0.70, reseed_soft * 0.75 + reseed_hard * 0.35);
    let trust_final = trust_out * (1.0 - reseed_hard * 0.65) * (1.0 - cooldown * 0.45);
    return vec4<f32>(coord, saturate(trust_final), encode_cooldown(cooldown));
}

// ---------------------------------------------------------------------------
// Pass 8: InitMeshCoord — initialize mesh coordinate map to identity UV.
// Unity: fragInitMeshCoord — no texture reads.
// ---------------------------------------------------------------------------
@fragment
fn fs_pass8(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(in.uv, 1.0, encode_cooldown(0.0));
}

// ---------------------------------------------------------------------------
// Pass 9: MeshRegularize — ARAP-lite regularization for non-rigid deformation.
// Unity: fragMeshRegularize — reads _MainTex (coordNext/coordAffine),
//        _PrevMeshCoordTex, _FlowTex. Texel from _MainTex_TexelSize.
// ---------------------------------------------------------------------------
@fragment
fn fs_pass9(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv    = in.uv;
    let texel = vec2<f32>(u.main_texel_x, u.main_texel_y);

    let c0    = textureSample(main_tex, tex_sampler, uv);
    let c     = c0.rg;
    let trust = c0.b;

    let ce = textureSample(main_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0)).rg;
    let cw = textureSample(main_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0)).rg;
    let cn = textureSample(main_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y)).rg;
    let cs = textureSample(main_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y)).rg;
    let lap = (ce + cw + cn + cs) * 0.25;

    let pe = textureSample(prev_mesh_coord_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0)).rg;
    let pw = textureSample(prev_mesh_coord_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0)).rg;
    let pn = textureSample(prev_mesh_coord_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y)).rg;
    let ps = textureSample(prev_mesh_coord_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y)).rg;

    let p_ex = pe - pw;
    let p_ey = pn - ps;
    let c_ex = ce - cw;
    let c_ey = cn - cs;

    let prev_angle = atan2(p_ex.y, p_ex.x);
    let cur_angle  = atan2(c_ex.y, c_ex.x);
    let d  = cur_angle - prev_angle;
    let s  = sin(d);
    let cc = cos(d);
    // 2x2 rotation matrix applied manually (WGSL has no float2x2 mul shorthand):
    let target_ex = vec2<f32>(cc * p_ex.x - s * p_ex.y, s * p_ex.x + cc * p_ex.y);
    let target_ey = vec2<f32>(cc * p_ey.x - s * p_ey.y, s * p_ey.x + cc * p_ey.y);
    let rigid_center =
        0.25 *
        ((ce - target_ex * 0.5) +
         (cw + target_ex * 0.5) +
         (cn - target_ey * 0.5) +
         (cs + target_ey * 0.5));

    let flow      = textureSample(flow_tex, tex_sampler, uv);
    let flow_conf = saturate(flow.b * flow.a);
    let reg       = saturate(u.mesh_regularize);

    let keep_w  = saturate(0.45 + trust * 0.40 + flow_conf * 0.25) * (1.0 - reg * 0.55);
    let smooth_w = reg * (0.28 + (1.0 - flow_conf) * 0.22);
    let rigid_w  = reg * (0.40 + flow_conf * 0.45);

    let w_sum   = max(keep_w + smooth_w + rigid_w, 1e-4);
    var coord   = (c * keep_w + lap * smooth_w + rigid_center * rigid_w) / w_sum;

    let relax_to_uv = uv - coord;
    let relax_w     = (0.0025 + (1.0 - flow_conf) * 0.0035) * mix(1.0, 0.62, u.temporal_smooth);
    coord           = coord + relax_to_uv * relax_w;

    let prev_center      = textureSample(prev_mesh_coord_tex, tex_sampler, uv).rg;
    let motion_proxy     = length(c - prev_center);
    var temporal_anchor  = saturate(u.temporal_smooth * (0.08 + trust * 0.30 + (1.0 - flow_conf) * 0.34));
    temporal_anchor      = temporal_anchor * (1.0 - smoothstep(0.0015, 0.018, motion_proxy));
    coord                = mix(coord, prev_center, temporal_anchor);
    coord                = saturate(coord);

    let trust_out = saturate(max(trust * 0.985, flow_conf * 0.90));
    let cooldown  = decode_cooldown(c0.a) * 0.93;
    return vec4<f32>(coord, trust_out, encode_cooldown(cooldown));
}

// ---------------------------------------------------------------------------
// Pass 10: MeshCellAffine — per-cell affine deformation from local flow Jacobian.
// Unity: fragMeshCellAffine — reads _MainTex (coordNext), _FlowTex.
// Texel from _MainTex_TexelSize. GridDensity used for cell size.
// ---------------------------------------------------------------------------
@fragment
fn fs_pass10(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv    = in.uv;
    let texel = vec2<f32>(u.main_texel_x, u.main_texel_y);

    let c0    = textureSample(main_tex, tex_sampler, uv);
    let coord = c0.rg;
    let trust = c0.b;

    let cell_count     = clamp(u.grid_density * 0.22, 12.0, 72.0);
    let cell_center_uv = (floor(uv * cell_count) + 0.5) / cell_count;

    let center_data  = textureSample(main_tex, tex_sampler, cell_center_uv);
    let center_coord = center_data.rg;
    let du           = uv - cell_center_uv;

    let step_uv  = texel * 2.0;
    let flow_c   = textureSample(flow_tex, tex_sampler, cell_center_uv);
    let flow_l   = textureSample(flow_tex, tex_sampler, cell_center_uv - vec2<f32>(step_uv.x, 0.0)).rg;
    let flow_r   = textureSample(flow_tex, tex_sampler, cell_center_uv + vec2<f32>(step_uv.x, 0.0)).rg;
    let flow_b   = textureSample(flow_tex, tex_sampler, cell_center_uv - vec2<f32>(0.0, step_uv.y)).rg;
    let flow_t   = textureSample(flow_tex, tex_sampler, cell_center_uv + vec2<f32>(0.0, step_uv.y)).rg;

    let d_fdx = (flow_r - flow_l) / max(2.0 * step_uv.x, 1e-5);
    let d_fdy = (flow_t - flow_b) / max(2.0 * step_uv.y, 1e-5);

    let j00  = clamp(1.0 + d_fdx.x, 0.55, 1.65);
    let j01  = clamp(d_fdy.x,       -0.60, 0.60);
    let j10  = clamp(d_fdx.y,       -0.60, 0.60);
    let j11  = clamp(1.0 + d_fdy.y,  0.55, 1.65);
    let det  = j00 * j11 - j01 * j10;
    let sane = step(0.22, det) * step(det, 2.60);

    let affine_coord = saturate(center_coord + vec2<f32>(
        j00 * du.x + j01 * du.y,
        j10 * du.x + j11 * du.y));

    let flow_conf  = saturate(flow_c.b * flow_c.a);
    let div        = abs(d_fdx.x) + abs(d_fdy.y);
    let disocclusion = saturate((div - 0.0012) * 70.0);

    var strength = saturate(u.cell_affine_strength);
    strength     = strength * flow_conf;
    strength     = strength * (0.35 + trust * 0.65);
    strength     = strength * (1.0 - disocclusion * 0.75);
    strength     = strength * sane;

    let coord_out  = mix(coord, affine_coord, strength);
    let trust_out  = saturate(max(trust * 0.992, flow_conf * 0.92));
    return vec4<f32>(coord_out, trust_out, c0.a);
}

// ---------------------------------------------------------------------------
// Pass 11: SemanticMask — lightweight semantic proxy (body/face/boundary).
// Unity: fragSemanticMask — reads _MainTex (analysis), _DepthTex, _FlowTex.
// Texel from _MainTex_TexelSize (analysis size).
// ---------------------------------------------------------------------------
@fragment
fn fs_pass11(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv    = in.uv;
    let texel = vec2<f32>(u.main_texel_x, u.main_texel_y);
    let lum   = textureSample(main_tex, tex_sampler, uv).r;
    let depth  = textureSample(depth_tex, tex_sampler, uv).r;
    let flow   = textureSample(flow_tex, tex_sampler, uv);
    let flow_conf = saturate(flow.b * flow.a);

    let l_l = textureSample(main_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0)).r;
    let l_r = textureSample(main_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0)).r;
    let l_b = textureSample(main_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y)).r;
    let l_t = textureSample(main_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y)).r;
    let grad = sqrt((l_r - l_l) * (l_r - l_l) + (l_t - l_b) * (l_t - l_b));

    var body = smoothstep(0.14, 0.64, flow_conf * 0.92 + (1.0 - depth) * 0.16 + grad * 0.20);
    body     = body * smoothstep(0.05, 0.92, 1.0 - abs(lum - 0.5) * 1.4);

    let p            = (uv - 0.5) * vec2<f32>(1.20, 1.55);
    let center_bias  = 1.0 - smoothstep(0.32, 0.98, length(p));
    let face         = body * center_bias * smoothstep(0.10, 0.70, (1.0 - depth) * 0.35 + flow_conf * 0.65);

    var boundary = smoothstep(0.07, 0.28, grad) * body;
    boundary     = saturate(boundary * (0.6 + (1.0 - face) * 0.4));

    return vec4<f32>(saturate(body), saturate(face), saturate(boundary), 1.0);
}

// ---------------------------------------------------------------------------
// Pass 12: MeshFaceWarp — face-region non-rigid warp before regularization.
// Unity: fragMeshFaceWarp — reads _MainTex (coordAffine), _SemanticTex,
//        _FlowTex. Texel from _MainTex_TexelSize (analysis size).
// ---------------------------------------------------------------------------
@fragment
fn fs_pass12(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv    = in.uv;
    let texel = vec2<f32>(u.main_texel_x, u.main_texel_y);
    let c0    = textureSample(main_tex, tex_sampler, uv);
    let coord = c0.rg;
    let trust = c0.b;

    let sem      = textureSample(semantic_tex, tex_sampler, uv).rgb;
    let face     = sem.g;
    let boundary = sem.b;
    if face < 0.02 {
        return c0;
    }

    let flow_c = textureSample(flow_tex, tex_sampler, uv).rg;
    let flow_l = textureSample(flow_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0)).rg;
    let flow_r = textureSample(flow_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0)).rg;
    let flow_b = textureSample(flow_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y)).rg;
    let flow_t = textureSample(flow_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y)).rg;

    let face_grad = vec2<f32>(
        textureSample(semantic_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0)).g -
        textureSample(semantic_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0)).g,
        textureSample(semantic_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y)).g -
        textureSample(semantic_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y)).g) * 0.5;

    let d_fdx  = (flow_r - flow_l) / max(2.0 * texel.x, 1e-5);
    let d_fdy  = (flow_t - flow_b) / max(2.0 * texel.y, 1e-5);
    let warp_vec = vec2<f32>(d_fdy.x - d_fdx.y, d_fdx.x + d_fdy.y);
    let warp_combined = warp_vec * 0.55 + flow_c * 0.35 + face_grad * 0.20;

    var strength = saturate(u.face_warp_strength);
    strength     = strength * face;
    strength     = strength * (1.0 - boundary * 0.65);
    strength     = strength * saturate(0.45 + trust * 0.55);

    let coord_out = saturate(coord + warp_combined * strength * 0.18);
    let trust_out = saturate(max(trust * 0.99, face * 0.86));
    return vec4<f32>(coord_out, trust_out, c0.a);
}

// ---------------------------------------------------------------------------
// Pass 13: SurfaceCacheUpdate — persistent surface-cache update.
// Unity: fragSurfaceCacheUpdate — reads _MainTex (meshCoordTex),
//        _PrevSurfaceCacheTex, _FlowTex.
// ---------------------------------------------------------------------------
@fragment
fn fs_pass13(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv         = in.uv;
    let mesh       = textureSample(main_tex, tex_sampler, uv);
    let curr_coord = mesh.rg;
    let trust      = mesh.b;

    let prev       = textureSample(prev_surface_cache_tex, tex_sampler, curr_coord);
    let prev_id    = prev.rg;
    let prev_age   = prev.b;
    let prev_valid = prev.a;

    let dist       = distance(prev_id, curr_coord);
    let stable     = (1.0 - smoothstep(0.010, 0.080, dist)) * prev_valid;
    let id         = mix(curr_coord, prev_id, stable);

    let flow_sample  = textureSample(flow_tex, tex_sampler, uv);
    let flow_quality = saturate(flow_sample.b * flow_sample.a);
    let carry        = prev_age * mix(0.88, 0.996, saturate(u.surface_persistence));
    var age          = mix(0.10, carry + 0.030, stable);
    age              = max(age, stable * (0.20 + prev_age * 0.80));
    age              = age * (0.80 + flow_quality * 0.20);
    age              = saturate(age);

    let valid = saturate((0.42 + trust * 0.58) * (0.55 + flow_quality * 0.45));
    return vec4<f32>(saturate(id), age, valid);
}

// ---------------------------------------------------------------------------
// Pass 14: FlowHygiene — confidence-gated smoothing + hole fill.
// Unity: fragFlowHygiene — reads _MainTex (flowInput — native or flowTex).
// Texel from _MainTex_TexelSize (analysis size).
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
    let conf_n  = saturate(s.b);
    let valid_n = saturate(s.a);
    let q_n     = conf_n * valid_n;
    let dist    = length(fn_ - f0);
    let sim     = 1.0 - smoothstep(0.002, 0.060, dist);
    let w       = q_n * (0.25 + sim * 0.75);
    *acc_f     += fn_ * w;
    *acc_w     += w;
    *acc_conf  += conf_n * w;
    *acc_valid += valid_n * w;
}

@fragment
fn fs_pass14(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv    = in.uv;
    let texel = vec2<f32>(u.main_texel_x, u.main_texel_y);
    let c0    = textureSample(main_tex, tex_sampler, uv);
    let f0    = c0.rg;
    let conf0  = saturate(c0.b);
    let valid0 = saturate(c0.a);
    let q0     = conf0 * valid0;

    let s_l  = textureSample(main_tex, tex_sampler, uv - vec2<f32>(texel.x, 0.0));
    let s_r  = textureSample(main_tex, tex_sampler, uv + vec2<f32>(texel.x, 0.0));
    let s_b  = textureSample(main_tex, tex_sampler, uv - vec2<f32>(0.0, texel.y));
    let s_t  = textureSample(main_tex, tex_sampler, uv + vec2<f32>(0.0, texel.y));
    let s_lb = textureSample(main_tex, tex_sampler, uv - texel);
    let s_rb = textureSample(main_tex, tex_sampler, uv + vec2<f32>(texel.x, -texel.y));
    let s_lt = textureSample(main_tex, tex_sampler, uv + vec2<f32>(-texel.x, texel.y));
    let s_rt = textureSample(main_tex, tex_sampler, uv + texel);

    var acc_f:     vec2<f32> = vec2<f32>(0.0, 0.0);
    var acc_w:     f32       = 0.0;
    var acc_conf:  f32       = 0.0;
    var acc_valid: f32       = 0.0;

    accumulate_flow_sample(c0,  f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_l, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_r, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_b, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_t, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_lb, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_rb, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_lt, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);
    accumulate_flow_sample(s_rt, f0, &acc_f, &acc_w, &acc_conf, &acc_valid);

    var f_out:     vec2<f32>;
    var conf_out:  f32;
    var valid_out: f32;
    if acc_w > 1e-5 {
        f_out     = acc_f / acc_w;
        conf_out  = acc_conf / acc_w;
        valid_out = acc_valid / acc_w;
    } else {
        f_out     = f0;
        conf_out  = conf0;
        valid_out = valid0;
    }

    let neigh_valid = (
        saturate(s_l.a) + saturate(s_r.a) + saturate(s_b.a) + saturate(s_t.a) +
        saturate(s_lb.a) + saturate(s_rb.a) + saturate(s_lt.a) + saturate(s_rt.a)) / 8.0;
    let fill = smoothstep(0.02, 0.25, 1.0 - q0) * smoothstep(0.18, 0.70, neigh_valid);
    f_out     = mix(f_out, (s_l.rg + s_r.rg + s_b.rg + s_t.rg) * 0.25, fill);
    conf_out  = mix(conf_out,  max(conf_out,  neigh_valid * 0.75), fill);
    valid_out = mix(valid_out, max(valid_out, neigh_valid * 0.90), fill);

    let preserve = smoothstep(0.55, 0.92, q0);
    f_out     = mix(f_out, f0, preserve * 0.78);
    conf_out  = mix(conf_out, conf0, preserve * 0.78);
    valid_out = mix(valid_out, valid0, preserve * 0.78);

    return vec4<f32>(f_out, saturate(conf_out), saturate(valid_out));
}
