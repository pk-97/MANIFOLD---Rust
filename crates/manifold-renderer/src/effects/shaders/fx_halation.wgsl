// Halation effect — separable Gaussian blur with threshold extraction.
// Improvement over Unity's 13-tap 2D cross kernel: separable 17-tap Gaussian
// produces smooth, gap-free glow with equivalent GPU cost at half-resolution.
//
// Pass 0 (mode 0): Threshold + Tint — extract bright pixels, apply tint color
// Pass 1 (mode 1): Horizontal Gaussian blur
// Pass 2 (mode 2): Vertical Gaussian blur
// Pass 3 (mode 3): Composite — source + blurred halo × amount

struct Uniforms {
    mode: u32,
    amount: f32,
    threshold: f32,
    spread: f32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    main_texel_size_x: f32,
    main_texel_size_y: f32,
    halo_texel_size_x: f32,
    halo_texel_size_y: f32,
    _pad: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var main_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var halo_tex: texture_2d<f32>;

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

// 17-tap normalized Gaussian kernel (sigma = 4.0 relative to tap indices).
// Weights sum to 1.0. Wider sigma than a 13-tap kernel produces a softer,
// more cinematic glow falloff.
const W0: f32 = 0.10315;
const W1: f32 = 0.09998;
const W2: f32 = 0.09103;
const W3: f32 = 0.07786;
const W4: f32 = 0.06257;
const W5: f32 = 0.04723;
const W6: f32 = 0.03350;
const W7: f32 = 0.02232;
const W8: f32 = 0.01396;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if uniforms.mode == 0u {
        // Pass 0: Threshold + Tint — extract bright pixels, apply tint color.
        // No blur in this pass; the separable H/V passes handle all blurring.
        let col = textureSample(main_tex, tex_sampler, in.uv).rgb;
        let lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        let mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        let tint = vec3<f32>(uniforms.tint_r, uniforms.tint_g, uniforms.tint_b);
        return vec4<f32>(col * mk * tint, 1.0);

    } else if uniforms.mode == 1u {
        // Pass 1: Horizontal Gaussian blur (17-tap separable)
        let step_size = uniforms.spread * 5.0 + 1.0;
        let dx = vec2<f32>(uniforms.main_texel_size_x * step_size, 0.0);

        var acc = textureSample(main_tex, tex_sampler, in.uv).rgb * W0;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dx      ).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dx      ).rgb) * W1;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dx * 2.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dx * 2.0).rgb) * W2;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dx * 3.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dx * 3.0).rgb) * W3;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dx * 4.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dx * 4.0).rgb) * W4;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dx * 5.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dx * 5.0).rgb) * W5;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dx * 6.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dx * 6.0).rgb) * W6;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dx * 7.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dx * 7.0).rgb) * W7;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dx * 8.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dx * 8.0).rgb) * W8;

        return vec4<f32>(acc, 1.0);

    } else if uniforms.mode == 2u {
        // Pass 2: Vertical Gaussian blur (17-tap separable)
        let step_size = uniforms.spread * 5.0 + 1.0;
        let dy = vec2<f32>(0.0, uniforms.main_texel_size_y * step_size);

        var acc = textureSample(main_tex, tex_sampler, in.uv).rgb * W0;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dy      ).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dy      ).rgb) * W1;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dy * 2.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dy * 2.0).rgb) * W2;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dy * 3.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dy * 3.0).rgb) * W3;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dy * 4.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dy * 4.0).rgb) * W4;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dy * 5.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dy * 5.0).rgb) * W5;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dy * 6.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dy * 6.0).rgb) * W6;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dy * 7.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dy * 7.0).rgb) * W7;
        acc += (textureSample(main_tex, tex_sampler, in.uv + dy * 8.0).rgb
              + textureSample(main_tex, tex_sampler, in.uv - dy * 8.0).rgb) * W8;

        return vec4<f32>(acc, 1.0);

    } else {
        // Pass 3: Composite — source + halo × amount
        let src = textureSample(main_tex, tex_sampler, in.uv);
        let halo = textureSample(halo_tex, tex_sampler, in.uv).rgb;
        let result = src.rgb + halo * uniforms.amount;
        return vec4<f32>(result, src.a);
    }
}
