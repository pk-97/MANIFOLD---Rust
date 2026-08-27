//! RT-Stage-3 P1 (BUG-mkgh) value-level proof for the pre-blur firefly clamp
//! (`manifold_gpu::raytrace::MetalShadowRayTracer::debug_firefly_clamp`, which
//! dispatches the EXACT SAME `firefly_clamp_center` MSL helper the production
//! `firefly_clamp` kernel calls, against a caller-supplied 3x3 color + depth
//! neighborhood — no ray tracing, no full-res pass involved).
//!
//! The clamp: median luma over the non-void texels of the 3x3 (center
//! included — the "3..9-element non-void subset"; void = depth >= 1-1e-6);
//! a center void texel or fewer than 3 non-void texels passes through
//! untouched; otherwise `threshold = gain * max(median, floor)` and
//! `rgb *= threshold / luma` when the center's luma exceeds it. `luma`
//! mirrors the MSL `luma()` helper (0.2126/0.7152/0.0722); the median
//! mirrors `firefly_median_luma`'s partial-selection `mid = n/2` convention
//! (full sort then take index `n/2` — identical order statistic).
//!
//! I7's two passthrough cases (sun-disc-bright void texel; isolated 1-px
//! glint with <3 non-void neighbors) are pinned as MUST-NOT-clamp, plus the
//! positive case (hot outlier surrounded by dim non-void neighbors clamps to
//! `gain * max(median, floor)`) and a median-value case (neighbors 1..8,
//! center 100 => median 5, clamped to 8*5 = 40). Expected values are computed
//! by a CPU mirror (same f32 math), each with a closed-form sanity assertion
//! so a broken mirror can't agree with a broken GPU.

use manifold_gpu::raytrace::MetalShadowRayTracer;

use crate::harness;

const TOLERANCE: f32 = 1e-4;
/// Mirrors the MSL `FIREFLY_MEDIAN_GAIN` constant.
const GAIN: f32 = 8.0;
/// Depth value marking a void texel (depth >= 1-1e-6 in the kernel).
const VOID: f32 = 1.0;
const NON_VOID: f32 = 0.5;

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Mirrors the MSL `firefly_median_luma`: partial selection, return the
/// element at sorted index `n/2` (odd n = middle; even n = index n/2).
fn median_luma(samples: &[f32]) -> f32 {
    let mut l = samples.to_vec();
    l.sort_by(|a, b| a.partial_cmp(b).unwrap());
    l[l.len() / 2]
}

/// CPU mirror of `firefly_clamp_center` — the same void/sub-3/median/clamp
/// logic, against the same row-major 3x3 layout the debug surface uses.
fn clamp_center(color: &[[f32; 4]; 9], depth: &[f32; 9], gain: f32, floor: f32) -> [f32; 3] {
    let center = [color[4][0], color[4][1], color[4][2]];
    if depth[4] >= 1.0 - 1e-6 {
        return center; // void center: passthrough
    }
    let mut lumas = vec![luma(center)];
    // 8 neighbors in row-major order (indices 0,1,2,3,5,6,7,8 around center 4).
    for i in [0usize, 1, 2, 3, 5, 6, 7, 8] {
        if depth[i] >= 1.0 - 1e-6 {
            continue;
        }
        lumas.push(luma([color[i][0], color[i][1], color[i][2]]));
    }
    if lumas.len() < 3 {
        return center; // <3 non-void texels: passthrough
    }
    let median = median_luma(&lumas);
    let threshold = gain * median.max(floor);
    let center_luma = luma(center);
    if center_luma > threshold {
        let s = threshold / center_luma;
        return [center[0] * s, center[1] * s, center[2] * s];
    }
    center
}

fn assert_close(got: [f32; 3], expected: [f32; 3], label: &str) {
    for i in 0..3 {
        assert!(
            (got[i] - expected[i]).abs() < TOLERANCE,
            "{label}: component {i} — got {got:?}, expected {expected:?}"
        );
    }
}

#[test]
fn firefly_clamp_void_center_passes_through_unclamped() {
    let h = harness::shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    // I7 case 1: a sun-disc-bright texel in the void background is legit
    // content, never clamped — even at luma 100 with an 8.0 gain.
    let color: [[f32; 4]; 9] = std::array::from_fn(|i| {
        if i == 4 {
            [100.0, 100.0, 100.0, 1.0]
        } else {
            [0.0, 0.0, 0.0, 1.0]
        }
    });
    let depth: [f32; 9] = std::array::from_fn(|i| if i == 4 { VOID } else { NON_VOID });

    let expected = clamp_center(&color, &depth, GAIN, 1.0);
    assert_close(expected, [100.0, 100.0, 100.0], "mirror sanity: void passthrough");

    let got = tracer.debug_firefly_clamp(&h.device, &color, &depth, GAIN, 1.0);
    assert_close(got, expected, "void center passthrough");
}

#[test]
fn firefly_clamp_isolated_glint_passes_through_unclamped() {
    let h = harness::shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    // I7 case 2: an isolated 1-px glint — center non-void, every neighbor
    // void — has 1 non-void texel (<3), so no median can be established.
    let color: [[f32; 4]; 9] = std::array::from_fn(|i| {
        if i == 4 {
            [50.0, 50.0, 50.0, 1.0]
        } else {
            [0.0, 0.0, 0.0, 1.0]
        }
    });
    let depth: [f32; 9] = std::array::from_fn(|i| if i == 4 { NON_VOID } else { VOID });

    let expected = clamp_center(&color, &depth, GAIN, 1.0);
    assert_close(expected, [50.0, 50.0, 50.0], "mirror sanity: isolated glint passthrough");

    let got = tracer.debug_firefly_clamp(&h.device, &color, &depth, GAIN, 1.0);
    assert_close(got, expected, "isolated glint passthrough");
}

#[test]
fn firefly_clamp_hot_outlier_surrounded_by_dim_neighbors_is_clamped() {
    let h = harness::shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    // Positive case: hot center (luma 100) surrounded by 8 dim non-void
    // neighbors (luma 1). Median of the 9-element non-void subset = 1;
    // threshold = 8 * max(1, 1) = 8; center clamps to [8,8,8].
    let color: [[f32; 4]; 9] = std::array::from_fn(|i| {
        if i == 4 {
            [100.0, 100.0, 100.0, 1.0]
        } else {
            [1.0, 1.0, 1.0, 1.0]
        }
    });
    let depth = [NON_VOID; 9];

    let expected = clamp_center(&color, &depth, GAIN, 1.0);
    assert_close(expected, [8.0, 8.0, 8.0], "mirror sanity: clamps to gain * max(median, floor)");

    let got = tracer.debug_firefly_clamp(&h.device, &color, &depth, GAIN, 1.0);
    assert_close(got, expected, "hot outlier clamps to gain * max(median, floor)");
}

#[test]
fn firefly_clamp_median_matches_cpu_expected() {
    let h = harness::shared();
    let tracer = MetalShadowRayTracer::new(&h.device);

    // Median-value case: neighbors with lumas 1..8, center luma 100. The
    // 9-element sorted subset is [1,2,3,4,5,6,7,8,100]; median = index 4 = 5.
    // threshold = 8 * max(5, 1) = 40; center clamps to [40,40,40]. Pins the
    // median selection (the 5th-smallest, not 4 or 6) and the clamp math.
    let color: [[f32; 4]; 9] = [
        [1.0, 1.0, 1.0, 1.0],
        [2.0, 2.0, 2.0, 1.0],
        [3.0, 3.0, 3.0, 1.0],
        [4.0, 4.0, 4.0, 1.0],
        [100.0, 100.0, 100.0, 1.0],
        [5.0, 5.0, 5.0, 1.0],
        [6.0, 6.0, 6.0, 1.0],
        [7.0, 7.0, 7.0, 1.0],
        [8.0, 8.0, 8.0, 1.0],
    ];
    let depth = [NON_VOID; 9];

    // Sanity on the median mirror itself (closed form).
    assert!(
        (median_luma(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 100.0]) - 5.0).abs() < TOLERANCE,
        "mirror sanity: 9-element median should be the 5th-smallest (5.0)"
    );

    let expected = clamp_center(&color, &depth, GAIN, 1.0);
    assert_close(expected, [40.0, 40.0, 40.0], "mirror sanity: median 5 => clamp to 40");

    let got = tracer.debug_firefly_clamp(&h.device, &color, &depth, GAIN, 1.0);
    assert_close(got, expected, "median selection + clamp math");
}
