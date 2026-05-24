// node.gaussian_blur — single-axis Gaussian blur, two algorithms
// behind one primitive:
//
//   Fixed   (radius_mode = 0): one of three precomputed kernels —
//           9-tap σ≈2, 17-tap σ≈4, 25-tap σ≈6 — selected by
//           `kernel_size`. Step controls per-tap UV stride in pixels.
//           This is the cheap, deterministic path used by Halation /
//           DoF / Bloom and OilyFluid's quarter-res velocity blur.
//
//   Dynamic (radius_mode = 1): bit-exact port of the legacy fluid sim
//           `gaussian_blur_compute.wgsl` — sigma = max(radius/3, 1),
//           bilinear tap-pair loop, `radius` is in pixels. The per-
//           clip-trigger feel of FluidSim2D's density / vector-field
//           blur comes from this curve specifically; the Fixed kernels
//           cover ~half the perceived width at the same slider value.
//           `kernel_size` and `step` are ignored in this mode (radius
//           drives coverage). Radius=0 collapses to a single-tap
//           sample — the legacy "downsample via radius=0" trick.
//
// Bindings:
//   @binding(0) uniforms (kernel + axis + step + texel_size + radius)
//   @binding(1) source_tex
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    kernel_size: u32,   // Fixed mode: 0 = 9-tap, 1 = 17-tap, 2 = 25-tap
    axis: u32,          // 0 = horizontal, 1 = vertical
    step: f32,          // Fixed mode only — per-tap UV stride (pixels)
    texel_x: f32,
    texel_y: f32,
    radius_mode: u32,   // 0 = Fixed (precomputed kernel), 1 = Dynamic (radius-in-pixels)
    radius: f32,        // Dynamic mode only — pixel radius; sigma = max(radius/3, 1)
    _pad: f32,
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

// Dynamic-radius separable Gaussian — bit-exact port of legacy
// `gaussian_blur_compute.wgsl`. Bilinear tap-pair optimisation:
// adjacent samples (j, j+1) collapse to one bilinear fetch at their
// weighted midpoint — halves the sample count.
fn blur_dynamic(uv: vec2<f32>, axis_dir: vec2<f32>, radius: f32) -> vec4<f32> {
    let sigma = max(radius / 3.0, 1.0);
    let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);

    var result = sample(uv);
    var total_weight = 1.0;

    let radius_int = i32(radius);
    var j: i32 = 1;
    loop {
        if j > radius_int { break; }

        let fj = f32(j);
        let w_a = exp(-(fj * fj) * inv_two_sigma_sq);

        if j + 1 <= radius_int {
            let fj1 = f32(j + 1);
            let w_b = exp(-(fj1 * fj1) * inv_two_sigma_sq);
            let w_ab = w_a + w_b;
            let offset = fj + w_b / w_ab;

            result += sample(uv + axis_dir * offset) * w_ab;
            result += sample(uv - axis_dir * offset) * w_ab;
            total_weight += w_ab * 2.0;
        } else {
            // Unpaired last tap (odd radius)
            result += sample(uv + axis_dir * fj) * w_a;
            result += sample(uv - axis_dir * fj) * w_a;
            total_weight += w_a * 2.0;
        }

        j += 2;
    }

    return result / total_weight;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var result: vec4<f32>;
    if uniforms.radius_mode == 1u {
        // Dynamic mode — radius drives sigma, step ignored. axis picks
        // the 1-D direction in texel units (matches legacy fluid blur).
        var axis_dir: vec2<f32>;
        if uniforms.axis == 0u {
            axis_dir = vec2<f32>(uniforms.texel_x, 0.0);
        } else {
            axis_dir = vec2<f32>(0.0, uniforms.texel_y);
        }
        result = blur_dynamic(uv, axis_dir, uniforms.radius);
    } else {
        // Fixed mode — pick precomputed kernel; step scales the
        // per-tap UV stride in pixels.
        var d: vec2<f32>;
        if uniforms.axis == 0u {
            d = vec2<f32>(uniforms.texel_x * uniforms.step, 0.0);
        } else {
            d = vec2<f32>(0.0, uniforms.texel_y * uniforms.step);
        }
        if uniforms.kernel_size == 0u {
            result = blur_9(uv, d);
        } else if uniforms.kernel_size == 1u {
            result = blur_17(uv, d);
        } else {
            result = blur_25(uv, d);
        }
    }

    textureStore(output_tex, vec2<i32>(id.xy), result);
}
