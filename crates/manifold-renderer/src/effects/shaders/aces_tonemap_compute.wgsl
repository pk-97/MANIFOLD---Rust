// Tonemapping — compute dispatch variant.
// Supports multiple curves (Narkowicz ACES, Hill ACES, AgX) and multiple
// output modes (SDR, PQ, EDR, EDR passthrough).

struct Uniforms {
    exposure: f32,
    paper_white: f32,
    max_nits: f32,
    mode: u32,  // 0 = SDR, 1 = PQ, 2 = EDR curve, 3 = EDR shoulder, 4 = scene-linear
    curve: u32, // 0 = Narkowicz, 1 = Hill, 2 = AgX, 3 = Khronos PBR Neutral
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var s_source: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

// ── Main ───────────────────────────────────────────────────────────────

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let src = textureSampleLevel(t_source, s_source, uv, 0.0);
    let hdr = src.rgb * u.exposure;

    var result: vec3<f32>;

    if (u.mode == 1u) {
        // PQ output (export pipeline).
        let mapped = tonemap_raw(hdr, u.curve);
        let nits = clamp(mapped * u.paper_white, vec3<f32>(0.0), vec3<f32>(u.max_nits));
        result = linear_to_pq(nits / 10000.0);
    } else if (u.mode == 2u) {
        // HDR display-linear output (macOS EDR).
        let mapped = tonemap_raw(hdr, u.curve);
        let nits = clamp(mapped * u.paper_white, vec3<f32>(0.0), vec3<f32>(u.max_nits));
        result = nits / max(u.paper_white, 1.0);
    } else if (u.mode == 3u) {
        // Existing EDR output path: preserve linear values through the
        // display shoulder using the configured display peak.
        result = edr_soft_shoulder(hdr, u.max_nits / max(u.paper_white, 1.0));
    } else if (u.mode == 4u) {
        // Scene-linear output is the compositor interchange surface. Display
        // headroom and the soft shoulder are applied by presentation.wgsl.
        result = hdr;
    } else {
        // SDR output (default).
        result = tonemap_sdr(hdr, u.curve);
    }

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(result, src.a));
}
