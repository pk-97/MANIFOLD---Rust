// EdgeGlow effect — edge detection with soft glow.
// Translated line-by-line from EdgeGlowEffect.shader (MANIFOLD/EdgeGlowEffect).
// Modes: 0=Sobel, 1=Laplacian, 2=Frei-Chen.

struct Uniforms {
    amount: f32,        // EdgeGlowFX.cs:23 — _Amount   = GetParam(0)
    threshold: f32,     // EdgeGlowFX.cs:24 — _Threshold = GetParam(1)
    glow: f32,          // EdgeGlowFX.cs:25 — _Glow     = GetParam(2)
    mode: u32,          // EdgeGlowFX.cs:26 — _Mode     = round(GetParam(3))
    texel_size_x: f32,  // EdgeGlowEffect.shader:133 — _MainTex_TexelSize.x = 1/width
    texel_size_y: f32,  // EdgeGlowEffect.shader:133 — _MainTex_TexelSize.y = 1/height
    _pad: vec2<f32>,
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

// EdgeGlowEffect.shader:53-56 — luminance()
fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// EdgeGlowEffect.shader:59-62 — sampleLum()
fn sample_lum(uv: vec2<f32>, offset: vec2<f32>) -> f32 {
    let texel = vec2<f32>(uniforms.texel_size_x, uniforms.texel_size_y);
    return luminance(textureSample(source_tex, tex_sampler, uv + offset * texel).rgb);
}

// EdgeGlowEffect.shader:65-80 — edgeSobel()
fn edge_sobel(uv: vec2<f32>) -> f32 {
    let tl = sample_lum(uv, vec2<f32>(-1.0, -1.0));
    let tc = sample_lum(uv, vec2<f32>( 0.0, -1.0));
    let tr = sample_lum(uv, vec2<f32>( 1.0, -1.0));
    let ml = sample_lum(uv, vec2<f32>(-1.0,  0.0));
    let mr = sample_lum(uv, vec2<f32>( 1.0,  0.0));
    let bl = sample_lum(uv, vec2<f32>(-1.0,  1.0));
    let bc = sample_lum(uv, vec2<f32>( 0.0,  1.0));
    let br = sample_lum(uv, vec2<f32>( 1.0,  1.0));

    let gx = -tl - 2.0 * ml - bl + tr + 2.0 * mr + br;
    let gy = -tl - 2.0 * tc - tr + bl + 2.0 * bc + br;

    return sqrt(gx * gx + gy * gy);
}

// EdgeGlowEffect.shader:83-92 — edgeLaplacian()
fn edge_laplacian(uv: vec2<f32>) -> f32 {
    let c = sample_lum(uv, vec2<f32>( 0.0,  0.0));
    let t = sample_lum(uv, vec2<f32>( 0.0, -1.0));
    let b = sample_lum(uv, vec2<f32>( 0.0,  1.0));
    let l = sample_lum(uv, vec2<f32>(-1.0,  0.0));
    let r = sample_lum(uv, vec2<f32>( 1.0,  0.0));

    return abs(t + b + l + r - 4.0 * c);
}

// EdgeGlowEffect.shader:95-113 — edgeFreiChen()
fn edge_frei_chen(uv: vec2<f32>) -> f32 {
    let tl = sample_lum(uv, vec2<f32>(-1.0, -1.0));
    let tc = sample_lum(uv, vec2<f32>( 0.0, -1.0));
    let tr = sample_lum(uv, vec2<f32>( 1.0, -1.0));
    let ml = sample_lum(uv, vec2<f32>(-1.0,  0.0));
    let mr = sample_lum(uv, vec2<f32>( 1.0,  0.0));
    let bl = sample_lum(uv, vec2<f32>(-1.0,  1.0));
    let bc = sample_lum(uv, vec2<f32>( 0.0,  1.0));
    let br = sample_lum(uv, vec2<f32>( 1.0,  1.0));

    let s = 1.41421356; // EdgeGlowEffect.shader:107 — sqrt(2)
    let gx = (tr + s * mr + br) - (tl + s * ml + bl);
    let gy = (bl + s * bc + br) - (tl + s * tc + tr);
    let div = 2.0 + s; // EdgeGlowEffect.shader:110 — 2.0 + sqrt(2) = 3.41421356

    return sqrt(gx * gx + gy * gy) / div;
}

// EdgeGlowEffect.shader:120-124 — dispatch to mode-selected edge detector
fn detect_edge(uv: vec2<f32>) -> f32 {
    if uniforms.mode == 0u {
        return edge_sobel(uv);
    } else if uniforms.mode == 1u {
        return edge_laplacian(uv);
    } else {
        return edge_frei_chen(uv);
    }
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(source_tex, tex_sampler, in.uv);

    // EdgeGlowEffect.shader:120-124 — detect edges at current pixel
    var edge = detect_edge(in.uv);

    // EdgeGlowEffect.shader:127-128 — smooth threshold
    let thresh = uniforms.threshold;
    edge = smoothstep(thresh * 0.5, thresh * 1.5 + 0.01, edge);

    // EdgeGlowEffect.shader:131-132 — glow setup
    var glow = 0.0;
    let glow_radius = uniforms.glow * 4.0 + 0.5; // EdgeGlowEffect.shader:132
    let texel = vec2<f32>(uniforms.texel_size_x, uniforms.texel_size_y);

    // EdgeGlowEffect.shader:135-148 — inner ring: 8 taps at 1x radius
    var s = 0u;
    loop {
        if s >= 8u { break; }
        let angle = f32(s) * 0.785398; // EdgeGlowEffect.shader:138 — 2*PI/8
        let offset = vec2<f32>(cos(angle), sin(angle)) * texel * glow_radius;
        var e2 = detect_edge(in.uv + offset);
        e2 = smoothstep(thresh * 0.5, thresh * 1.5 + 0.01, e2);
        glow += e2;
        s += 1u;
    }

    // EdgeGlowEffect.shader:151-163 — outer ring: 8 taps at 2x radius, rotated pi/8, weighted 0.5
    var s2 = 0u;
    loop {
        if s2 >= 8u { break; }
        let angle2 = f32(s2) * 0.785398 + 0.3927; // EdgeGlowEffect.shader:153 — offset by half step
        let offset2 = vec2<f32>(cos(angle2), sin(angle2)) * texel * glow_radius * 2.0;
        var e3 = detect_edge(in.uv + offset2);
        e3 = smoothstep(thresh * 0.5, thresh * 1.5 + 0.01, e3);
        glow += e3 * 0.5; // EdgeGlowEffect.shader:162 — outer ring dimmer
        s2 += 1u;
    }

    glow = glow / 12.0; // EdgeGlowEffect.shader:165 — normalize (8 + 8*0.5 = 12)

    // EdgeGlowEffect.shader:168 — combine sharp edge + soft glow
    let final_edge = clamp(edge + glow * uniforms.glow, 0.0, 1.0); // saturate()

    // EdgeGlowEffect.shader:171-172 — mix: lerp(src.rgb, grayscale_edge, amount), preserve alpha
    let result = mix(src.rgb, vec3<f32>(final_edge), uniforms.amount);
    return vec4<f32>(result, src.a);
}
