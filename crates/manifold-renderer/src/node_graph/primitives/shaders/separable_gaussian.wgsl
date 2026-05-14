// node.gaussian_blur — single-axis Gaussian blur with one of
// three precomputed kernels (9-tap σ≈2, 17-tap σ≈4, 25-tap σ≈6).
//
// Kernel weights are bit-identical to `fx_depth_of_field_compute.wgsl`
// (and the 17-tap matches `fx_halation_compute.wgsl`'s K17 constants),
// so a Halation H+V pair built from this primitive parity-checks
// against the legacy combined-mode shader once threshold/tint are
// hoisted into their own primitives.
//
// Step controls the per-tap UV stride in pixels (multiplied by
// texel_size internally). Legacy callers pass `spread * 5.0 + 1.0` for
// halation, `coc * 6.0 + 1.0` for DoF (variable-width DoF needs a
// separate primitive — this one expects a uniform step).
//
// Bindings:
//   @binding(0) uniforms (kernel + axis + step + texel_size + 12-byte pad)
//   @binding(1) source_tex
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    kernel_size: u32,  // 0 = 9-tap, 1 = 17-tap, 2 = 25-tap
    axis: u32,         // 0 = horizontal, 1 = vertical
    step: f32,
    texel_x: f32,
    texel_y: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

// 9-tap (σ ≈ 2.0)
const K9_0: f32 = 0.16501;
const K9_1: f32 = 0.15019;
const K9_2: f32 = 0.11325;
const K9_3: f32 = 0.07076;
const K9_4: f32 = 0.03664;

// 17-tap (σ ≈ 4.0) — identical to halation's K17.
const K17_0: f32 = 0.10315;
const K17_1: f32 = 0.09998;
const K17_2: f32 = 0.09103;
const K17_3: f32 = 0.07786;
const K17_4: f32 = 0.06257;
const K17_5: f32 = 0.04723;
const K17_6: f32 = 0.03350;
const K17_7: f32 = 0.02232;
const K17_8: f32 = 0.01396;

// 25-tap (σ ≈ 6.0)
const K25_0:  f32 = 0.07087;
const K25_1:  f32 = 0.06947;
const K25_2:  f32 = 0.06540;
const K25_3:  f32 = 0.05917;
const K25_4:  f32 = 0.05148;
const K25_5:  f32 = 0.04307;
const K25_6:  f32 = 0.03465;
const K25_7:  f32 = 0.02680;
const K25_8:  f32 = 0.01995;
const K25_9:  f32 = 0.01428;
const K25_10: f32 = 0.00983;
const K25_11: f32 = 0.00651;
const K25_12: f32 = 0.00415;

fn sample(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
}

fn blur_9(uv: vec2<f32>, d: vec2<f32>) -> vec4<f32> {
    var acc = sample(uv) * K9_0;
    acc += (sample(uv + d      ) + sample(uv - d      )) * K9_1;
    acc += (sample(uv + d * 2.0) + sample(uv - d * 2.0)) * K9_2;
    acc += (sample(uv + d * 3.0) + sample(uv - d * 3.0)) * K9_3;
    acc += (sample(uv + d * 4.0) + sample(uv - d * 4.0)) * K9_4;
    return acc;
}

fn blur_17(uv: vec2<f32>, d: vec2<f32>) -> vec4<f32> {
    var acc = sample(uv) * K17_0;
    acc += (sample(uv + d      ) + sample(uv - d      )) * K17_1;
    acc += (sample(uv + d * 2.0) + sample(uv - d * 2.0)) * K17_2;
    acc += (sample(uv + d * 3.0) + sample(uv - d * 3.0)) * K17_3;
    acc += (sample(uv + d * 4.0) + sample(uv - d * 4.0)) * K17_4;
    acc += (sample(uv + d * 5.0) + sample(uv - d * 5.0)) * K17_5;
    acc += (sample(uv + d * 6.0) + sample(uv - d * 6.0)) * K17_6;
    acc += (sample(uv + d * 7.0) + sample(uv - d * 7.0)) * K17_7;
    acc += (sample(uv + d * 8.0) + sample(uv - d * 8.0)) * K17_8;
    return acc;
}

fn blur_25(uv: vec2<f32>, d: vec2<f32>) -> vec4<f32> {
    var acc = sample(uv) * K25_0;
    acc += (sample(uv + d       ) + sample(uv - d       )) * K25_1;
    acc += (sample(uv + d *  2.0) + sample(uv - d *  2.0)) * K25_2;
    acc += (sample(uv + d *  3.0) + sample(uv - d *  3.0)) * K25_3;
    acc += (sample(uv + d *  4.0) + sample(uv - d *  4.0)) * K25_4;
    acc += (sample(uv + d *  5.0) + sample(uv - d *  5.0)) * K25_5;
    acc += (sample(uv + d *  6.0) + sample(uv - d *  6.0)) * K25_6;
    acc += (sample(uv + d *  7.0) + sample(uv - d *  7.0)) * K25_7;
    acc += (sample(uv + d *  8.0) + sample(uv - d *  8.0)) * K25_8;
    acc += (sample(uv + d *  9.0) + sample(uv - d *  9.0)) * K25_9;
    acc += (sample(uv + d * 10.0) + sample(uv - d * 10.0)) * K25_10;
    acc += (sample(uv + d * 11.0) + sample(uv - d * 11.0)) * K25_11;
    acc += (sample(uv + d * 12.0) + sample(uv - d * 12.0)) * K25_12;
    return acc;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var d: vec2<f32>;
    if uniforms.axis == 0u {
        d = vec2<f32>(uniforms.texel_x * uniforms.step, 0.0);
    } else {
        d = vec2<f32>(0.0, uniforms.texel_y * uniforms.step);
    }

    var result: vec4<f32>;
    if uniforms.kernel_size == 0u {
        result = blur_9(uv, d);
    } else if uniforms.kernel_size == 1u {
        result = blur_17(uv, d);
    } else {
        result = blur_25(uv, d);
    }

    textureStore(output_tex, vec2<i32>(id.xy), result);
}
