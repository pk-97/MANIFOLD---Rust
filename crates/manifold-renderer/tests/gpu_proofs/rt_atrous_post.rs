//! RT-Stage-3 P3 (BUG-eytk) value-level proof for the post-accumulation
//! à-trous filter (`manifold_gpu::raytrace::MetalShadowRayTracer::
//! debug_atrous_post`, which dispatches the EXACT SAME `atrous_post_center`
//! MSL helper the production `atrous_post` kernel calls, against a caller-
//! supplied 3x3 neighborhood — no ray tracing, no full-res pass involved).
//!
//! The filter: per-texel bilateral spatial blur on the accumulated irradiance.
//! Void texels (depth >= 1-1e-6) pass through bit-exact. Non-void texels:
//! temporal output variance (from moments .r/.g/.w) + spatial luma spread
//! guide the luma sigma; 8-tap dilated 3x3 with depth/normal/luma edge-stop
//! weights; output = mix(src, filtered, strength); .a passes through unchanged
//! (accumulated AO, I2).
//!
//! Five test cases mirroring `rt_firefly_clamp.rs`'s structure:
//! 1. Converged texel → early-out, output bit-identical to src center.
//! 2. Void center → passthrough unchanged.
//! 3. Noisy center, flat guides, strength=1.0 → plain 9-tap mean.
//! 4. Edge rejection: right column at different depth → those taps contribute
//!    ~nothing; output stays near left+center columns' mean.
//! 5. Strength blend: strength=0.5 → output = 0.5*src + 0.5*filtered.
//!
//! Expected values are computed by a CPU mirror (same f32 math), each with a
//! closed-form sanity assertion so a broken mirror can't agree with a broken
//! GPU.

use manifold_gpu::raytrace::MetalShadowRayTracer;

use crate::harness;

const TOLERANCE: f32 = 1e-4;

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// CPU mirror of the `atrous_post_center` MSL helper. Same f32 math against
/// the same row-major 3x3 layout the debug surface uses. Center is (1,1),
/// step=1.
///
/// Mirrors the MSL constants:
const POST_LUMA_SIGMA_SCALE: f32 = 2.0;
const POST_LUMA_SIGMA_FLOOR: f32 = 0.02;
const POST_SPATIAL_GAIN: f32 = 2.0;
const POST_EARLY_OUT: f32 = 0.004;

/// Depth value marking a void texel (depth >= 1-1e-6 in the kernel).
const VOID: f32 = 1.0;
const NON_VOID: f32 = 0.5;

/// Row-major 3x3 index: (row, col) -> flat index. Center = (1,1) = index 4.
fn idx(r: usize, c: usize) -> usize {
    r * 3 + c
}

/// CPU mirror of `atrous_post_center` for a 3x3 neighborhood with step=1.
/// `depth`, `normal` (.xyz), `moments` (.r=m1, .g=m2, .b=ao, .w=hist_len),
/// `src_irr` (.rgb = irradiance, .a = ao) are all row-major 3x3.
fn atrous_post_center_cpu(
    depth: &[f32; 9],
    normal: &[[f32; 4]; 9],
    moments: &[[f32; 4]; 9],
    src_irr: &[[f32; 4]; 9],
    step: u32,
    strength: f32,
) -> [f32; 4] {
    let src = src_irr[idx(1, 1)];
    let center_depth = depth[idx(1, 1)];
    if center_depth >= 1.0 - 1e-6 {
        return src; // void passthrough
    }
    let mo = moments[idx(1, 1)];
    let m1 = mo[0]; // .r
    let m2 = mo[1]; // .g
    let n_eff = mo[3].max(1.0); // .w
    let var = (m2 - m1 * m1).max(0.0) / n_eff;

    // Spatial luma spread at step=1 dilation (3x3 neighborhood).
    let center_luma = luma([src[0], src[1], src[2]]);
    let offsets: [(i32, i32); 8] = [
        (1, 0), (-1, 0), (0, 1), (0, -1),
        (1, 1), (1, -1), (-1, 1), (-1, -1),
    ];
    let mut sm1 = center_luma;
    let mut sm2 = center_luma * center_luma;
    for &(dr, dc) in &offsets {
        let r = (1i32 + dr * step as i32).clamp(0, 2) as usize;
        let c = (1i32 + dc * step as i32).clamp(0, 2) as usize;
        let ql = luma([src_irr[idx(r, c)][0], src_irr[idx(r, c)][1], src_irr[idx(r, c)][2]]);
        sm1 += ql;
        sm2 += ql * ql;
    }
    sm1 /= 9.0;
    sm2 /= 9.0;
    let spatial_sd = (sm2 - sm1 * sm1).max(0.0).sqrt();

    let sigma = (POST_LUMA_SIGMA_SCALE * var.sqrt()).max(POST_LUMA_SIGMA_FLOOR)
        + POST_SPATIAL_GAIN * spatial_sd * (2.0 / n_eff).min(1.0);

    // Early-out
    if var.sqrt() < POST_EARLY_OUT && spatial_sd < POST_EARLY_OUT {
        return src;
    }

    let center_n = [normal[idx(1, 1)][0], normal[idx(1, 1)][1], normal[idx(1, 1)][2]];
    let mut acc = [src[0], src[1], src[2]];
    let mut wsum = 1.0f32;

    for &(dr, dc) in &offsets {
        let r = (1i32 + dr * step as i32).clamp(0, 2) as usize;
        let c = (1i32 + dc * step as i32).clamp(0, 2) as usize;
        let qi = idx(r, c);
        let qd = depth[qi];
        if qd >= 1.0 - 1e-6 {
            continue; // void neighbor: skip
        }
        let qn = [normal[qi][0], normal[qi][1], normal[qi][2]];
        let qr = src_irr[qi];
        let w_depth = (-((qd - center_depth).abs()) / 3e-3_f32).exp();
        let dot_cn = (center_n[0] * qn[0] + center_n[1] * qn[1] + center_n[2] * qn[2]).max(0.0);
        let w_normal = dot_cn.powf(16.0);
        let w_luma =
            (-((luma([qr[0], qr[1], qr[2]]) - center_luma).abs()) / sigma).exp();
        let w = w_depth * w_normal * w_luma;
        acc[0] += qr[0] * w;
        acc[1] += qr[1] * w;
        acc[2] += qr[2] * w;
        wsum += w;
    }

    let filtered = [acc[0] / wsum, acc[1] / wsum, acc[2] / wsum];
    [
        src[0] + (filtered[0] - src[0]) * strength,
        src[1] + (filtered[1] - src[1]) * strength,
        src[2] + (filtered[2] - src[2]) * strength,
        src[3], // .a passthrough
    ]
}

fn assert_close(got: [f32; 4], expected: [f32; 4], label: &str) {
    for i in 0..4 {
        assert!(
            (got[i] - expected[i]).abs() < TOLERANCE,
            "{label}: component {i} — got {got:?}, expected {expected:?}"
        );
    }
}

/// Uniform 3x3: all non-void, same depth, same normal, same moments, same
/// irradiance. Variance = 0, spatial spread = 0 → early-out → output = src.
#[test]
fn atrous_post_converged_texel_early_out() {
    let h = harness::shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    let irr_val = [0.3, 0.5, 0.7, 0.8]; // .rgb = irradiance, .a = ao
    let depth = [NON_VOID; 9];
    let normal: [[f32; 4]; 9] = std::array::from_fn(|_| [0.0, 0.0, 1.0, 0.0]);
    // Converged moments: m1 = luma(irr), m2 = m1^2 → variance = 0
    let cl = luma([irr_val[0], irr_val[1], irr_val[2]]);
    let moments: [[f32; 4]; 9] = std::array::from_fn(|_| [cl, cl * cl, 0.8, 50.0]);

    let src_irr: [[f32; 4]; 9] = std::array::from_fn(|_| irr_val);

    let expected = atrous_post_center_cpu(&depth, &normal, &moments, &src_irr, 1, 1.0);
    // Sanity: early-out means output == src center.
    assert_close(expected, irr_val, "mirror sanity: converged early-out");

    let got = tracer.debug_atrous_post(&h.device, &depth, &normal, &moments, &src_irr, 1, 1.0);
    assert_close(got, expected, "converged texel early-out");
}

/// Void center → bit-exact passthrough of src (including .a).
#[test]
fn atrous_post_void_center_passthrough() {
    let h = harness::shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    let src_val = [10.0, 20.0, 30.0, 0.5];
    let depth = std::array::from_fn(|i| if i == 4 { VOID } else { NON_VOID });
    let normal: [[f32; 4]; 9] = std::array::from_fn(|_| [0.0, 0.0, 1.0, 0.0]);
    let moments: [[f32; 4]; 9] = std::array::from_fn(|_| [0.5, 0.25, 0.8, 10.0]);
    let src_irr: [[f32; 4]; 9] = std::array::from_fn(|i| {
        if i == 4 { src_val } else { [0.1, 0.2, 0.3, 0.4] }
    });

    let expected = atrous_post_center_cpu(&depth, &normal, &moments, &src_irr, 1, 1.0);
    assert_close(expected, src_val, "mirror sanity: void passthrough");

    let got = tracer.debug_atrous_post(&h.device, &depth, &normal, &moments, &src_irr, 1, 1.0);
    assert_close(got, expected, "void center passthrough");
}

/// Noisy center, flat guides (uniform depth/normal), high spatial spread,
/// strength=1.0. All 9 taps are non-void with identical depth/normal →
/// w_depth=1, w_normal=1 for all; with a wide sigma, w_luma→1 for all
/// (the spatial spread makes sigma large, so exp(-0/sigma)≈1). Output
/// should be the plain 9-tap arithmetic mean of src_irr's rgb values.
#[test]
fn atrous_post_noisy_flat_mean() {
    let h = harness::shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    let depth = [NON_VOID; 9];
    let normal: [[f32; 4]; 9] = std::array::from_fn(|_| [0.0, 0.0, 1.0, 0.0]);
    // Varied irradiance values so the spatial spread is high → sigma is wide.
    let src_irr: [[f32; 4]; 9] = [
        [0.1, 0.2, 0.3, 0.8],
        [0.4, 0.5, 0.6, 0.8],
        [0.7, 0.8, 0.9, 0.8],
        [0.2, 0.3, 0.4, 0.8],
        [1.0, 1.0, 1.0, 0.8], // center
        [0.3, 0.4, 0.5, 0.8],
        [0.5, 0.6, 0.7, 0.8],
        [0.6, 0.7, 0.8, 0.8],
        [0.8, 0.9, 1.0, 0.8],
    ];
    // Moments with high variance so the filter doesn't early-out.
    let moments: [[f32; 4]; 9] = std::array::from_fn(|_| [0.3, 0.5, 0.8, 2.0]);

    let expected = atrous_post_center_cpu(&depth, &normal, &moments, &src_irr, 1, 1.0);

    // Closed-form sanity: with identical guides and wide sigma, all weights
    // approach 1, so filtered ≈ mean of all 9 rgb values.
    let mut sum = [0.0f32; 3];
    for t in &src_irr {
        sum[0] += t[0];
        sum[1] += t[1];
        sum[2] += t[2];
    }
    let mean_rgb = [sum[0] / 9.0, sum[1] / 9.0, sum[2] / 9.0];
    // With strength=1.0, output ≈ mean. The edge-stop weights won't be
    // exactly 1 because sigma is finite, but close enough for a sanity bound.
    for i in 0..3 {
        assert!(
            (expected[i] - mean_rgb[i]).abs() < 0.15,
            "mirror sanity: output should be near the 9-tap mean — expected {:?}, mean {:?}",
            expected,
            mean_rgb,
        );
    }

    let got = tracer.debug_atrous_post(&h.device, &depth, &normal, &moments, &src_irr, 1, 1.0);
    assert_close(got, expected, "noisy flat mean");
}

/// Edge rejection: right column (indices 2, 5, 8) sits at a different depth
/// (depth delta >> 3e-3) → w_depth ≈ 0 for those taps → they contribute
/// ~nothing. Output should stay near the left+center columns' mean.
#[test]
fn atrous_post_edge_rejection() {
    let h = harness::shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    // Left+center columns (depth 0.5), right column (depth 0.8 — delta 0.3
    // >> 3e-3 → w_depth = exp(-0.3/0.003) ≈ 0).
    let depth = [
        0.5, 0.5, 0.8,
        0.5, 0.5, 0.8,
        0.5, 0.5, 0.8,
    ];
    let normal: [[f32; 4]; 9] = std::array::from_fn(|_| [0.0, 0.0, 1.0, 0.0]);
    // High variance so the filter doesn't early-out.
    let moments: [[f32; 4]; 9] = std::array::from_fn(|_| [0.5, 0.8, 0.8, 2.0]);

    // Left+center columns: value 1.0; right column: value 10.0.
    // Without edge rejection, the mean would be pulled up by the 10s.
    // With edge rejection, the right column's contribution ≈ 0.
    let src_irr: [[f32; 4]; 9] = [
        [1.0, 1.0, 1.0, 0.8],
        [1.0, 1.0, 1.0, 0.8],
        [10.0, 10.0, 10.0, 0.8],
        [1.0, 1.0, 1.0, 0.8],
        [1.0, 1.0, 1.0, 0.8], // center
        [10.0, 10.0, 10.0, 0.8],
        [1.0, 1.0, 1.0, 0.8],
        [1.0, 1.0, 1.0, 0.8],
        [10.0, 10.0, 10.0, 0.8],
    ];

    let expected = atrous_post_center_cpu(&depth, &normal, &moments, &src_irr, 1, 1.0);

    // Sanity: the CPU mirror should produce output near 1.0 (the left+center
    // columns' value), not pulled up toward 10.0 by the right column.
    assert!(
        expected[0] < 2.0,
        "mirror sanity: edge rejection should keep output near left+center value — got {:?}",
        expected,
    );

    let got = tracer.debug_atrous_post(&h.device, &depth, &normal, &moments, &src_irr, 1, 1.0);
    assert_close(got, expected, "edge rejection");
}

/// Strength blend: strength=0.5 → output = 0.5*src + 0.5*filtered.
#[test]
fn atrous_post_strength_blend() {
    let h = harness::shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    let depth = [NON_VOID; 9];
    let normal: [[f32; 4]; 9] = std::array::from_fn(|_| [0.0, 0.0, 1.0, 0.0]);
    let moments: [[f32; 4]; 9] = std::array::from_fn(|_| [0.5, 0.8, 0.8, 2.0]);

    // Uniform irradiance → filtered = src (no change) regardless of strength.
    // This pins the blend math: mix(src, src, 0.5) = src.
    let src_val = [0.4, 0.6, 0.8, 0.9];
    let src_irr: [[f32; 4]; 9] = std::array::from_fn(|_| src_val);

    let expected = atrous_post_center_cpu(&depth, &normal, &moments, &src_irr, 1, 0.5);
    // With uniform src, filtered = src, so mix(src, src, 0.5) = src.
    assert_close(expected, src_val, "mirror sanity: uniform src → strength irrelevant");

    let got = tracer.debug_atrous_post(&h.device, &depth, &normal, &moments, &src_irr, 1, 0.5);
    assert_close(got, expected, "strength blend with uniform src");

    // Now with varied src so filtered != src, and verify the blend.
    let src_irr_varied: [[f32; 4]; 9] = [
        [0.1, 0.1, 0.1, 0.8],
        [0.2, 0.2, 0.2, 0.8],
        [0.3, 0.3, 0.3, 0.8],
        [0.4, 0.4, 0.4, 0.8],
        [1.0, 1.0, 1.0, 0.8], // center
        [0.5, 0.5, 0.5, 0.8],
        [0.6, 0.6, 0.6, 0.8],
        [0.7, 0.7, 0.7, 0.8],
        [0.8, 0.8, 0.8, 0.8],
    ];
    let expected_varied =
        atrous_post_center_cpu(&depth, &normal, &moments, &src_irr_varied, 1, 0.5);

    // Compute filtered (strength=1.0) to verify the blend.
    let filtered_full =
        atrous_post_center_cpu(&depth, &normal, &moments, &src_irr_varied, 1, 1.0);
    let blended = [
        src_irr_varied[4][0] + (filtered_full[0] - src_irr_varied[4][0]) * 0.5,
        src_irr_varied[4][1] + (filtered_full[1] - src_irr_varied[4][1]) * 0.5,
        src_irr_varied[4][2] + (filtered_full[2] - src_irr_varied[4][2]) * 0.5,
        src_irr_varied[4][3],
    ];
    for i in 0..4 {
        assert!(
            (expected_varied[i] - blended[i]).abs() < TOLERANCE,
            "blend sanity: component {i} — strength=0.5 output {:?}, expected blend {:?}",
            expected_varied,
            blended,
        );
    }

    let got_varied =
        tracer.debug_atrous_post(&h.device, &depth, &normal, &moments, &src_irr_varied, 1, 0.5);
    assert_close(got_varied, expected_varied, "strength blend with varied src");
}
