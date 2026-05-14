// node.halation — pixel-exact replacement for legacy
// `effects/shaders/fx_halation_compute.wgsl`. Mechanically copied
// verbatim. Splitting pass 0 (threshold + tint + H Gaussian, fused
// per-tap) into atomic primitives would store an fp16 intermediate
// texture and lose bit-exact parity, so the legacy shader ships
// fused (same pattern as Bloom, Glitch, EdgeDetect).
//
// Pass 0 (mode 0): Threshold + Tint + Horizontal Gaussian blur (combined)
// Pass 1 (mode 1): Vertical Gaussian blur
// Pass 2 (mode 2): Composite — source + blurred halo × amount

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
@group(0) @binding(1) var source_tex_a: texture_2d<f32>;
@group(0) @binding(2) var source_tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

const W0: f32 = 0.10315;
const W1: f32 = 0.09998;
const W2: f32 = 0.09103;
const W3: f32 = 0.07786;
const W4: f32 = 0.06257;
const W5: f32 = 0.04723;
const W6: f32 = 0.03350;
const W7: f32 = 0.02232;
const W8: f32 = 0.01396;

fn threshold_tint(col: vec3<f32>) -> vec3<f32> {
    let lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
    let mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
    let tint = vec3<f32>(uniforms.tint_r, uniforms.tint_g, uniforms.tint_b);
    return col * mk * tint;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    var color: vec4<f32>;

    if uniforms.mode == 0u {
        let step_size = uniforms.spread * 5.0 + 1.0;
        let dx = vec2<f32>(uniforms.main_texel_size_x * step_size, 0.0);

        var acc = threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0).rgb) * W0;
        acc += (threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv + dx      , 0.0).rgb)
              + threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv - dx      , 0.0).rgb)) * W1;
        acc += (threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv + dx * 2.0, 0.0).rgb)
              + threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv - dx * 2.0, 0.0).rgb)) * W2;
        acc += (threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv + dx * 3.0, 0.0).rgb)
              + threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv - dx * 3.0, 0.0).rgb)) * W3;
        acc += (threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv + dx * 4.0, 0.0).rgb)
              + threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv - dx * 4.0, 0.0).rgb)) * W4;
        acc += (threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv + dx * 5.0, 0.0).rgb)
              + threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv - dx * 5.0, 0.0).rgb)) * W5;
        acc += (threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv + dx * 6.0, 0.0).rgb)
              + threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv - dx * 6.0, 0.0).rgb)) * W6;
        acc += (threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv + dx * 7.0, 0.0).rgb)
              + threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv - dx * 7.0, 0.0).rgb)) * W7;
        acc += (threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv + dx * 8.0, 0.0).rgb)
              + threshold_tint(textureSampleLevel(source_tex_a, tex_sampler, uv - dx * 8.0, 0.0).rgb)) * W8;

        color = vec4<f32>(acc, 1.0);

    } else if uniforms.mode == 1u {
        let step_size = uniforms.spread * 5.0 + 1.0;
        let dy = vec2<f32>(0.0, uniforms.main_texel_size_y * step_size);

        var acc = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0).rgb * W0;
        acc += (textureSampleLevel(source_tex_a, tex_sampler, uv + dy      , 0.0).rgb
              + textureSampleLevel(source_tex_a, tex_sampler, uv - dy      , 0.0).rgb) * W1;
        acc += (textureSampleLevel(source_tex_a, tex_sampler, uv + dy * 2.0, 0.0).rgb
              + textureSampleLevel(source_tex_a, tex_sampler, uv - dy * 2.0, 0.0).rgb) * W2;
        acc += (textureSampleLevel(source_tex_a, tex_sampler, uv + dy * 3.0, 0.0).rgb
              + textureSampleLevel(source_tex_a, tex_sampler, uv - dy * 3.0, 0.0).rgb) * W3;
        acc += (textureSampleLevel(source_tex_a, tex_sampler, uv + dy * 4.0, 0.0).rgb
              + textureSampleLevel(source_tex_a, tex_sampler, uv - dy * 4.0, 0.0).rgb) * W4;
        acc += (textureSampleLevel(source_tex_a, tex_sampler, uv + dy * 5.0, 0.0).rgb
              + textureSampleLevel(source_tex_a, tex_sampler, uv - dy * 5.0, 0.0).rgb) * W5;
        acc += (textureSampleLevel(source_tex_a, tex_sampler, uv + dy * 6.0, 0.0).rgb
              + textureSampleLevel(source_tex_a, tex_sampler, uv - dy * 6.0, 0.0).rgb) * W6;
        acc += (textureSampleLevel(source_tex_a, tex_sampler, uv + dy * 7.0, 0.0).rgb
              + textureSampleLevel(source_tex_a, tex_sampler, uv - dy * 7.0, 0.0).rgb) * W7;
        acc += (textureSampleLevel(source_tex_a, tex_sampler, uv + dy * 8.0, 0.0).rgb
              + textureSampleLevel(source_tex_a, tex_sampler, uv - dy * 8.0, 0.0).rgb) * W8;

        color = vec4<f32>(acc, 1.0);

    } else {
        let src = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
        let halo = textureSampleLevel(source_tex_b, tex_sampler, uv, 0.0).rgb;
        let result = src.rgb + halo * uniforms.amount;
        color = vec4<f32>(result, src.a);
    }

    textureStore(output_tex, vec2<i32>(gid.xy), color);
}
