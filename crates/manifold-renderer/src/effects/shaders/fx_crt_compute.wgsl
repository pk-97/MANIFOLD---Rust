// Compute variant of fx_crt.wgsl for ComputeDualBlitHelper.
// Two source textures: source_tex_a (_MainTex) and source_tex_b (_GlowTex).
// Mode 0: fragPrefilter  — 4-tap box blur + smoothstep threshold + warm tint
// Mode 1: fragDownsample — 4-tap box blur
// Mode 2: fragComposite  — barrel distort + scanlines + phosphor mask + glow + vignette + lerp

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
@group(0) @binding(1) var source_tex_a: texture_2d<f32>;
@group(0) @binding(2) var source_tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

// CrtEffect.shader lines 57-67 — BarrelDistort
fn barrel_distort(uv: vec2<f32>, curvature: f32) -> vec2<f32> {
    var centered = uv * 2.0 - 1.0;
    let k = curvature * 0.25;
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

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    let main_ts = vec2<f32>(uniforms.main_texel_size_x, uniforms.main_texel_size_y);

    var color: vec4<f32>;

    if uniforms.mode == 0u {
        // Pass 0: Prefilter — 4-tap box filter + smoothstep threshold + warm tint
        let t = main_ts * 0.5;
        var s = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-t.x, -t.y), 0.0).rgb;
        s = s + textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( t.x, -t.y), 0.0).rgb;
        s = s + textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-t.x,  t.y), 0.0).rgb;
        s = s + textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( t.x,  t.y), 0.0).rgb;
        s = s * 0.25;

        let lum_val = luma(s);
        let response = smoothstep(uniforms.glow_threshold, uniforms.glow_threshold + 0.3, lum_val);

        let warm_tint = mix(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(1.0, 0.85, 0.6), uniforms.style * 0.4);

        color = vec4<f32>(s * response * warm_tint, 1.0);

    } else if uniforms.mode == 1u {
        // Pass 1: Downsample — 4-tap box filter
        let t = main_ts * 0.5;
        var s = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-t.x, -t.y), 0.0).rgb;
        s = s + textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( t.x, -t.y), 0.0).rgb;
        s = s + textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-t.x,  t.y), 0.0).rgb;
        s = s + textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( t.x,  t.y), 0.0).rgb;
        color = vec4<f32>(s * 0.25, 1.0);

    } else {
        // Pass 2: CRT Composite
        let warped_uv = clamp(barrel_distort(uv, uniforms.curvature), vec2<f32>(0.0), vec2<f32>(1.0));

        let src = textureSampleLevel(source_tex_a, tex_sampler, warped_uv, 0.0);
        var col = src.rgb;

        let glow_sample = textureSampleLevel(source_tex_b, tex_sampler, warped_uv, 0.0).rgb;

        let scan_count = max(mix(uniforms.screen_height / 3.0, uniforms.screen_height / 8.0, uniforms.style), 60.0);

        let scan_phase = fract(warped_uv.y * scan_count);
        let scan_exp = mix(1.0, 3.0, uniforms.style);
        let phosphor = pow(sin(scan_phase * 3.14159265), scan_exp);

        let scan_strength = uniforms.scanlines * mix(0.6, 0.9, uniforms.style);

        let lum_val = luma(col);
        let suppression = lum_val * 0.15;
        col = col * mix(1.0, phosphor, scan_strength * (1.0 - suppression));

        col = col * (1.0 + scan_strength * 0.3);

        let pixel_x = warped_uv.x * uniforms.main_texel_size_z;
        let mask_phase = pixel_x % 3.0;

        let mask_sharpness = mix(0.8, 0.3, uniforms.style);
        var phosphor_mask: vec3<f32>;
        phosphor_mask.r = smoothstep(mask_sharpness, 1.0, 1.0 - abs(mask_phase - 0.5) / 1.5);
        phosphor_mask.g = smoothstep(mask_sharpness, 1.0, 1.0 - abs(mask_phase - 1.5) / 1.5);
        phosphor_mask.b = smoothstep(mask_sharpness, 1.0, 1.0 - abs(mask_phase - 2.5) / 1.5);

        let mask_opacity = mix(0.05, 0.3, uniforms.style);
        col = col * mix(vec3<f32>(1.0, 1.0, 1.0), phosphor_mask, mask_opacity);

        let warm_glow = mix(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(1.0, 0.9, 0.7), uniforms.style * 0.5);
        col = col + glow_sample * uniforms.glow * warm_glow * 1.5;

        let vig = (warped_uv - 0.5) * 2.0;
        let vig_dist = dot(vig, vig);
        let vig_strength = mix(0.15, 0.6, uniforms.style) * uniforms.amount;
        col = col * (1.0 - vig_dist * vig_strength);

        let original = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
        let result = mix(original.rgb, col, uniforms.amount);
        color = vec4<f32>(result, original.a);
    }

    textureStore(output_tex, vec2<i32>(gid.xy), color);
}
