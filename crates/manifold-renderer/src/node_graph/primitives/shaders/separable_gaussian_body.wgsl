// node.gaussian_blur — fusable body (freeze §12), GATHER via the STENCIL-FETCH
// ABI. Single-axis Gaussian blur, two algorithms behind one primitive. `in` is
// gathered along one axis: the body reads it through `fetch_in(uv)` — defined
// by the codegen as the real textureSampleLevel over the bound texture
// (standalone / fused real external), or as a recomputed upstream chain (fused
// virtual source). The texel step is recovered from the ambient `dims`. The
// sampler's address mode (Clamp/Repeat/Mirror) is chosen host-side in run(),
// so the body ignores the address_mode param. Matches separable_gaussian.wgsl.
// PARAMS: [kernel_size (Enum->u32), axis (Enum->u32), step, radius_mode
// (Enum->u32), radius, address_mode (Enum->u32, host-side sampler)].

// 9-tap (sigma ~= 2.0)
const SG_K9_0: f32 = 0.16501;
const SG_K9_1: f32 = 0.15019;
const SG_K9_2: f32 = 0.11325;
const SG_K9_3: f32 = 0.07076;
const SG_K9_4: f32 = 0.03664;

// 17-tap (sigma ~= 4.0)
const SG_K17_0: f32 = 0.10315;
const SG_K17_1: f32 = 0.09998;
const SG_K17_2: f32 = 0.09103;
const SG_K17_3: f32 = 0.07786;
const SG_K17_4: f32 = 0.06257;
const SG_K17_5: f32 = 0.04723;
const SG_K17_6: f32 = 0.03350;
const SG_K17_7: f32 = 0.02232;
const SG_K17_8: f32 = 0.01396;

// 25-tap (sigma ~= 6.0)
const SG_K25_0:  f32 = 0.07087;
const SG_K25_1:  f32 = 0.06947;
const SG_K25_2:  f32 = 0.06540;
const SG_K25_3:  f32 = 0.05917;
const SG_K25_4:  f32 = 0.05148;
const SG_K25_5:  f32 = 0.04307;
const SG_K25_6:  f32 = 0.03465;
const SG_K25_7:  f32 = 0.02680;
const SG_K25_8:  f32 = 0.01995;
const SG_K25_9:  f32 = 0.01428;
const SG_K25_10: f32 = 0.00983;
const SG_K25_11: f32 = 0.00651;
const SG_K25_12: f32 = 0.00415;

fn sg_blur_9(uv: vec2<f32>, d: vec2<f32>) -> vec4<f32> {
    var acc = fetch_in(uv) * SG_K9_0;
    acc += (fetch_in(uv + d      ) + fetch_in(uv - d      )) * SG_K9_1;
    acc += (fetch_in(uv + d * 2.0) + fetch_in(uv - d * 2.0)) * SG_K9_2;
    acc += (fetch_in(uv + d * 3.0) + fetch_in(uv - d * 3.0)) * SG_K9_3;
    acc += (fetch_in(uv + d * 4.0) + fetch_in(uv - d * 4.0)) * SG_K9_4;
    return acc;
}

fn sg_blur_17(uv: vec2<f32>, d: vec2<f32>) -> vec4<f32> {
    var acc = fetch_in(uv) * SG_K17_0;
    acc += (fetch_in(uv + d      ) + fetch_in(uv - d      )) * SG_K17_1;
    acc += (fetch_in(uv + d * 2.0) + fetch_in(uv - d * 2.0)) * SG_K17_2;
    acc += (fetch_in(uv + d * 3.0) + fetch_in(uv - d * 3.0)) * SG_K17_3;
    acc += (fetch_in(uv + d * 4.0) + fetch_in(uv - d * 4.0)) * SG_K17_4;
    acc += (fetch_in(uv + d * 5.0) + fetch_in(uv - d * 5.0)) * SG_K17_5;
    acc += (fetch_in(uv + d * 6.0) + fetch_in(uv - d * 6.0)) * SG_K17_6;
    acc += (fetch_in(uv + d * 7.0) + fetch_in(uv - d * 7.0)) * SG_K17_7;
    acc += (fetch_in(uv + d * 8.0) + fetch_in(uv - d * 8.0)) * SG_K17_8;
    return acc;
}

fn sg_blur_25(uv: vec2<f32>, d: vec2<f32>) -> vec4<f32> {
    var acc = fetch_in(uv) * SG_K25_0;
    acc += (fetch_in(uv + d       ) + fetch_in(uv - d       )) * SG_K25_1;
    acc += (fetch_in(uv + d *  2.0) + fetch_in(uv - d *  2.0)) * SG_K25_2;
    acc += (fetch_in(uv + d *  3.0) + fetch_in(uv - d *  3.0)) * SG_K25_3;
    acc += (fetch_in(uv + d *  4.0) + fetch_in(uv - d *  4.0)) * SG_K25_4;
    acc += (fetch_in(uv + d *  5.0) + fetch_in(uv - d *  5.0)) * SG_K25_5;
    acc += (fetch_in(uv + d *  6.0) + fetch_in(uv - d *  6.0)) * SG_K25_6;
    acc += (fetch_in(uv + d *  7.0) + fetch_in(uv - d *  7.0)) * SG_K25_7;
    acc += (fetch_in(uv + d *  8.0) + fetch_in(uv - d *  8.0)) * SG_K25_8;
    acc += (fetch_in(uv + d *  9.0) + fetch_in(uv - d *  9.0)) * SG_K25_9;
    acc += (fetch_in(uv + d * 10.0) + fetch_in(uv - d * 10.0)) * SG_K25_10;
    acc += (fetch_in(uv + d * 11.0) + fetch_in(uv - d * 11.0)) * SG_K25_11;
    acc += (fetch_in(uv + d * 12.0) + fetch_in(uv - d * 12.0)) * SG_K25_12;
    return acc;
}

// Dynamic-radius separable Gaussian — bit-exact port of the legacy fluid-sim
// blur. Bilinear tap-pair optimisation: adjacent samples (j, j+1) collapse to one
// bilinear fetch at their weighted midpoint.
fn sg_blur_dynamic(uv: vec2<f32>, axis_dir: vec2<f32>, radius: f32) -> vec4<f32> {
    let sigma = max(radius / 3.0, 1.0);
    let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);

    var result = fetch_in(uv);
    var total_weight = 1.0;

    // BUG-290: cap the tap loop. `radius` is port-shadowed, so a wired
    // control can deliver values far past the 0-256 param range (inf casts
    // to i32::MAX = a firmware-watchdog-length loop). 2048 admits every
    // canvas-scaled radius (256 * bw/640 = 1536 at 4K) and bounds garbage.
    let radius_int = min(i32(radius), 2048);
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

            result += fetch_in(uv + axis_dir * offset) * w_ab;
            result += fetch_in(uv - axis_dir * offset) * w_ab;
            total_weight += w_ab * 2.0;
        } else {
            result += fetch_in(uv + axis_dir * fj) * w_a;
            result += fetch_in(uv - axis_dir * fj) * w_a;
            total_weight += w_a * 2.0;
        }

        j += 2;
    }

    return result / total_weight;
}

// Linear-radius separable Gaussian — exact port of node.blur's per-axis pass
// (blur.wgsl mode 0): sigma = max(radius/2, 0.5), one tap per integer offset
// -r..r (no tap-pair trick), normalized by the running weight sum, radius
// clamped to 32. radius <= 0 collapses to a single center sample. Taps land on
// texel centers, so fusing through this mode is filter-exact by construction.
fn sg_blur_linear(uv: vec2<f32>, axis_dir: vec2<f32>, radius: f32) -> vec4<f32> {
    // Single-exit on purpose: an early return here leaves unreachable blocks
    // that abort spirv-opt's merge-return pass, and the whole fused kernel
    // then ships UNOPTIMIZED (no inlining/DCE — measured slow enough to flip
    // the perf gate). Same arithmetic, same order, just one return.
    let r_int = min(i32(radius), 32);
    var result: vec4<f32>;
    if r_int <= 0 {
        result = fetch_in(uv);
    } else {
        let sigma = max(radius * 0.5, 0.5);
        let two_sigma_sq = 2.0 * sigma * sigma;

        var sum = vec4<f32>(0.0);
        var weight_sum = 0.0;
        for (var d: i32 = -r_int; d <= r_int; d = d + 1) {
            let s = fetch_in(uv + axis_dir * f32(d));
            let dist_sq = f32(d * d);
            let w = exp(-dist_sq / two_sigma_sq);
            sum = sum + s * w;
            weight_sum = weight_sum + w;
        }
        result = sum / max(weight_sum, 1e-6);
    }
    return result;
}

fn body(uv: vec2<f32>, dims: vec2<f32>, kernel_size: u32, axis: u32, step: f32, radius_mode: u32, radius: f32, address_mode: u32) -> vec4<f32> {
    let texel = vec2<f32>(1.0) / dims;

    var result: vec4<f32>;
    if radius_mode == 1u || radius_mode == 2u {
        var axis_dir: vec2<f32>;
        if axis == 0u {
            axis_dir = vec2<f32>(texel.x, 0.0);
        } else {
            axis_dir = vec2<f32>(0.0, texel.y);
        }
        if radius_mode == 2u {
            result = sg_blur_linear(uv, axis_dir, radius);
        } else {
            result = sg_blur_dynamic(uv, axis_dir, radius);
        }
    } else {
        var d: vec2<f32>;
        if axis == 0u {
            d = vec2<f32>(texel.x * step, 0.0);
        } else {
            d = vec2<f32>(0.0, texel.y * step);
        }
        if kernel_size == 0u {
            result = sg_blur_9(uv, d);
        } else if kernel_size == 1u {
            result = sg_blur_17(uv, d);
        } else {
            result = sg_blur_25(uv, d);
        }
    }

    return result;
}
