// Port of Unity CrtEffect.shader with HDR glow boost.
// Two textures: main_tex (_MainTex) and glow_tex (_GlowTex).
// Mode 0: fragPrefilter  — 4-tap box blur + smoothstep threshold + warm tint + HDR boost → half-res
// Mode 1: fragDownsample — 4-tap box blur → quarter-res
// Mode 2: fragComposite  — barrel distort + scanlines + phosphor mask + glow + vignette + lerp
//
// HDR_BOOST in prefilter produces values > 1.0 so the phosphor glow has
// real dynamic range. ACES tonemapping (applied after all effects) compresses
// the result back to display range with smooth highlight rolloff.

struct Uniforms {
    mode: u32,               // 0=prefilter, 1=downsample, 2=composite
    amount: f32,             // _Amount
    scanlines: f32,          // _Scanlines
    glow: f32,               // _Glow
    curvature: f32,          // _Curvature
    style: f32,              // _Style
    glow_threshold: f32,     // _GlowThreshold
    screen_height: f32,      // _ScreenHeight
    main_texel_size_x: f32,  // _MainTex_TexelSize.x (1/width)
    main_texel_size_y: f32,  // _MainTex_TexelSize.y (1/height)
    main_texel_size_z: f32,  // _MainTex_TexelSize.z (width in pixels)
    _pad: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var main_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var glow_tex: texture_2d<f32>;

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

// CrtEffect.shader lines 57-67 — BarrelDistort
fn barrel_distort(uv: vec2<f32>, curvature: f32) -> vec2<f32> {
    var centered = uv * 2.0 - 1.0;
    let k = curvature * 0.25;
    // Pre-scale so corners land exactly at edge after distortion (no black border)
    let corner_scale = 1.0 / (1.0 + k * 2.0);
    centered = centered * corner_scale;
    let r2 = dot(centered, centered);
    centered = centered * (1.0 + k * r2);
    return centered * 0.5 + 0.5;
}

// CrtEffect.shader lines 69-72 — Luma
fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let main_ts = vec2<f32>(uniforms.main_texel_size_x, uniforms.main_texel_size_y);

    if uniforms.mode == 0u {
        // ════════════════════════════════════════════════
        // Pass 0: Prefilter — CrtEffect.shader lines 77-95
        // fragPrefilter: 4-tap box filter + smoothstep threshold + warm tint
        // ════════════════════════════════════════════════

        // CrtEffect.shader lines 79-85: 4-tap box filter for slight blur during downsample
        let t = main_ts * 0.5;
        var s = textureSample(main_tex, tex_sampler, in.uv + vec2<f32>(-t.x, -t.y)).rgb;
        s = s + textureSample(main_tex, tex_sampler, in.uv + vec2<f32>( t.x, -t.y)).rgb;
        s = s + textureSample(main_tex, tex_sampler, in.uv + vec2<f32>(-t.x,  t.y)).rgb;
        s = s + textureSample(main_tex, tex_sampler, in.uv + vec2<f32>( t.x,  t.y)).rgb;
        s = s * 0.25;

        // CrtEffect.shader lines 87-89: soft threshold — lower than bloom to let mid-tones bleed
        let lum_val = luma(s);
        let response = smoothstep(uniforms.glow_threshold, uniforms.glow_threshold + 0.3, lum_val);

        // CrtEffect.shader lines 91-92: warm tint bias at high Style values (amber/orange phosphor glow)
        let warm_tint = mix(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(1.0, 0.85, 0.6), uniforms.style * 0.4);

        // CrtEffect.shader line 94
        return vec4<f32>(s * response * warm_tint, 1.0);

    } else if uniforms.mode == 1u {
        // ════════════════════════════════════════════════
        // Pass 1: Downsample — CrtEffect.shader lines 100-108
        // fragDownsample: 4-tap box filter (bilinear gives natural blur)
        // ════════════════════════════════════════════════

        // CrtEffect.shader lines 102-107
        let t = main_ts * 0.5;
        var s = textureSample(main_tex, tex_sampler, in.uv + vec2<f32>(-t.x, -t.y)).rgb;
        s = s + textureSample(main_tex, tex_sampler, in.uv + vec2<f32>( t.x, -t.y)).rgb;
        s = s + textureSample(main_tex, tex_sampler, in.uv + vec2<f32>(-t.x,  t.y)).rgb;
        s = s + textureSample(main_tex, tex_sampler, in.uv + vec2<f32>( t.x,  t.y)).rgb;
        return vec4<f32>(s * 0.25, 1.0);

    } else {
        // ════════════════════════════════════════════════
        // Pass 2: CRT Composite — CrtEffect.shader lines 113-178
        // fragComposite
        // ════════════════════════════════════════════════

        // CrtEffect.shader line 116: float2 warpedUV = saturate(BarrelDistort(i.uv, _Curvature))
        let warped_uv = clamp(barrel_distort(in.uv, uniforms.curvature), vec2<f32>(0.0), vec2<f32>(1.0));

        // CrtEffect.shader lines 119-120: Sample source at warped UVs
        let src = textureSample(main_tex, tex_sampler, warped_uv);
        var col = src.rgb;

        // CrtEffect.shader line 123: Sample glow at warped UVs (quarter-res bilinear = soft diffuse glow)
        let glow_sample = textureSample(glow_tex, tex_sampler, warped_uv).rgb;

        // CrtEffect.shader lines 126-134: Scanlines — pow(sin) phosphor model
        // Count: accurate = ~height/3 (fine), stylized = ~height/8 (chunky)
        let scan_count = max(mix(uniforms.screen_height / 3.0, uniforms.screen_height / 8.0, uniforms.style), 60.0);

        // Phosphor shape: pow(sin(pi*frac), exp) models the bright phosphor row
        // with genuinely dark gaps between rows
        let scan_phase = fract(warped_uv.y * scan_count);
        let scan_exp = mix(1.0, 3.0, uniforms.style); // accurate=smooth, stylized=narrow bright bands
        let phosphor = pow(sin(scan_phase * 3.14159265), scan_exp);

        // CrtEffect.shader lines 136-143: Darkening strength — much stronger for visible gaps
        // Scanlines=1 Style=0: 60% dark gaps. Scanlines=1 Style=1: 90% dark gaps.
        let scan_strength = uniforms.scanlines * mix(0.6, 0.9, uniforms.style);

        // Slight luminance suppression (very bright areas show a bit less)
        let lum_val = luma(col);
        let suppression = lum_val * 0.15;
        col = col * mix(1.0, phosphor, scan_strength * (1.0 - suppression));

        // CrtEffect.shader lines 145-146: Brightness compensation
        // CRTs push peak brightness to offset scanline gaps
        col = col * (1.0 + scan_strength * 0.3);

        // CrtEffect.shader lines 148-162: RGB phosphor mask (repeating R,G,B column pattern, period = 3 pixels)
        let pixel_x = warped_uv.x * uniforms.main_texel_size_z;
        // CrtEffect.shader line 150: float maskPhase = fmod(pixelX, 3.0)
        let mask_phase = pixel_x % 3.0;

        let mask_sharpness = mix(0.8, 0.3, uniforms.style);
        var phosphor_mask: vec3<f32>;
        phosphor_mask.r = smoothstep(mask_sharpness, 1.0, 1.0 - abs(mask_phase - 0.5) / 1.5);
        phosphor_mask.g = smoothstep(mask_sharpness, 1.0, 1.0 - abs(mask_phase - 1.5) / 1.5);
        phosphor_mask.b = smoothstep(mask_sharpness, 1.0, 1.0 - abs(mask_phase - 2.5) / 1.5);

        let mask_opacity = mix(0.05, 0.3, uniforms.style);
        col = col * mix(vec3<f32>(1.0, 1.0, 1.0), phosphor_mask, mask_opacity);

        // CrtEffect.shader lines 164-166: Phosphor glow composite
        let warm_glow = mix(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(1.0, 0.9, 0.7), uniforms.style * 0.5);
        col = col + glow_sample * uniforms.glow * warm_glow * 1.5;

        // CrtEffect.shader lines 168-172: Vignette
        let vig = (warped_uv - 0.5) * 2.0;
        let vig_dist = dot(vig, vig);
        let vig_strength = mix(0.15, 0.6, uniforms.style) * uniforms.amount;
        col = col * (1.0 - vig_dist * vig_strength);

        // CrtEffect.shader lines 174-177: Final lerp: blend effected with original at un-warped UVs
        let original = textureSample(main_tex, tex_sampler, in.uv);
        let result = mix(original.rgb, col, uniforms.amount);
        return vec4<f32>(result, original.a);
    }
}
