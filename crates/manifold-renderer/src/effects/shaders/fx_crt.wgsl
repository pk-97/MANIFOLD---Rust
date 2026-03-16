// CRT effect — barrel distortion, scanlines, RGB phosphor mask, glow, vignette.
// Single-pass approximation of the Unity 3-pass version.

struct Uniforms {
    amount: f32,
    scanlines: f32,
    glow: f32,
    curvature: f32,
    style: f32,
    resolution_x: f32,
    resolution_y: f32,
    _pad: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

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

fn barrel_distort(uv: vec2<f32>, curvature: f32) -> vec2<f32> {
    var centered = uv * 2.0 - 1.0;
    let k = curvature * 0.25;
    // Pre-scale so corners land exactly at edge after distortion
    let corner_scale = 1.0 / (1.0 + k * 2.0);
    centered = centered * corner_scale;
    let r2 = dot(centered, centered);
    centered = centered * (1.0 + k * r2);
    return centered * 0.5 + 0.5;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // 1. Barrel distortion
    let warped_uv = clamp(barrel_distort(in.uv, uniforms.curvature), vec2<f32>(0.0), vec2<f32>(1.0));

    // 2. Sample source at warped UVs
    let src = textureSample(source_tex, tex_sampler, warped_uv);
    var col = src.rgb;

    // 3. Approximate glow: 4-tap box blur at warped UVs
    let texel = vec2<f32>(1.0 / uniforms.resolution_x, 1.0 / uniforms.resolution_y);
    let glow_radius = 4.0;
    let t = texel * glow_radius;
    var glow_sample = textureSample(source_tex, tex_sampler, warped_uv + vec2<f32>(-t.x, -t.y)).rgb;
    glow_sample += textureSample(source_tex, tex_sampler, warped_uv + vec2<f32>( t.x, -t.y)).rgb;
    glow_sample += textureSample(source_tex, tex_sampler, warped_uv + vec2<f32>(-t.x,  t.y)).rgb;
    glow_sample += textureSample(source_tex, tex_sampler, warped_uv + vec2<f32>( t.x,  t.y)).rgb;
    glow_sample = glow_sample * 0.25;
    // Soft threshold for glow
    let glow_lum = luma(glow_sample);
    let glow_response = smoothstep(0.1, 0.4, glow_lum);
    let warm_tint = mix(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(1.0, 0.85, 0.6), uniforms.style * 0.4);
    glow_sample = glow_sample * glow_response * warm_tint;

    // 4. Scanlines — pow(sin) phosphor model
    let scan_count = max(mix(uniforms.resolution_y / 3.0, uniforms.resolution_y / 8.0, uniforms.style), 60.0);
    let scan_phase = fract(warped_uv.y * scan_count);
    let scan_exp = mix(1.0, 3.0, uniforms.style);
    let phosphor = pow(sin(scan_phase * 3.14159265), scan_exp);

    let scan_strength = uniforms.scanlines * mix(0.6, 0.9, uniforms.style);
    let lum_val = luma(col);
    let suppression = lum_val * 0.15;
    col = col * mix(1.0, phosphor, scan_strength * (1.0 - suppression));

    // Brightness compensation
    col = col * (1.0 + scan_strength * 0.3);

    // 5. RGB phosphor mask (repeating R,G,B column pattern, period = 3 pixels)
    let pixel_x = warped_uv.x * uniforms.resolution_x;
    let mask_phase = pixel_x % 3.0;

    let mask_sharpness = mix(0.8, 0.3, uniforms.style);
    var phosphor_mask: vec3<f32>;
    phosphor_mask.x = smoothstep(mask_sharpness, 1.0, 1.0 - abs(mask_phase - 0.5) / 1.5);
    phosphor_mask.y = smoothstep(mask_sharpness, 1.0, 1.0 - abs(mask_phase - 1.5) / 1.5);
    phosphor_mask.z = smoothstep(mask_sharpness, 1.0, 1.0 - abs(mask_phase - 2.5) / 1.5);

    let mask_opacity = mix(0.05, 0.3, uniforms.style);
    col = col * mix(vec3<f32>(1.0, 1.0, 1.0), phosphor_mask, mask_opacity);

    // 6. Phosphor glow composite
    let warm_glow = mix(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(1.0, 0.9, 0.7), uniforms.style * 0.5);
    col = col + glow_sample * uniforms.glow * warm_glow * 1.5;

    // 7. Vignette
    let vig = (warped_uv - 0.5) * 2.0;
    let vig_dist = dot(vig, vig);
    let vig_strength = mix(0.15, 0.6, uniforms.style) * uniforms.amount;
    col = col * (1.0 - vig_dist * vig_strength);

    // 8. Final lerp with original at un-warped UVs
    let original = textureSample(source_tex, tex_sampler, in.uv);
    let result = mix(original.rgb, col, uniforms.amount);
    return vec4<f32>(result, original.a);
}
