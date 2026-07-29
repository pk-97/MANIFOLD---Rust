//! BUG-dx6w/BUG-axe9 value-level proof for the RT-R2 specular-history
//! variance clip
//! (`manifold_gpu::raytrace::MetalShadowRayTracer::debug_clamp_refl_history`,
//! which dispatches the EXACT SAME `clamp_refl_history` MSL helper
//! `accumulate_irradiance` calls internally against a caller-supplied 3x3
//! `hi_refl` neighborhood and a history value — no accumulation pass, no
//! ray tracing/RNG involved).
//!
//! `clamp_refl_history` (BUG-axe9) maps every neighborhood sample AND the
//! history through Reinhard-by-luma `t(c) = c / (1 + luma(c))` BEFORE
//! computing `mean`/`sigma`, clamps the MAPPED history to `[mean -
//! gamma*sigma, mean + gamma*sigma]` in mapped space, then inverts with
//! `c = t / (1 - luma(t))` (`luma` coefficients `(0.2126, 0.7152, 0.0722)`
//! mirror the MSL `luma()` helper in
//! `crates/manifold-gpu/src/metal/raytrace.rs`). `gamma` is the MSL
//! constant `RT_REFL_CLAMP_GAMMA` (1.0 as of this test — if that constant
//! is retuned, the expected values below must be recomputed with the same
//! formula and the new gamma).
//!
//! Three cases, CPU-computed expectations in f32 (same width as the GPU
//! path, same `mean-of-squares minus mean-squared` variance formula — see
//! `MAPPED_ROUNDTRIP_TOLERANCE` below for why that formula's known
//! catastrophic-cancellation behavior matters here):
//! - Noisy neighborhood (checkerboard 0.2/0.8): the MAPPED history lands
//!   inside the MAPPED mean±sigma box → round-trips through map/clamp/unmap
//!   unchanged (mapping is invertible, so inside-box in mapped space is a
//!   no-op).
//! - Flat neighborhood (all 0.5, sigma exactly 0 in LINEAR space but not
//!   quite 0 in MAPPED space — 0.5/1.5 isn't binary-exact, so `m2-m1*m1`
//!   leaves a small residual): history 5.0 still collapses to ~0.5
//!   (BUG-axe9 doesn't meaningfully change this — a near-degenerate box
//!   behaves almost the same in either space).
//! - HDR streak case (BUG-axe9's reason to exist): eight texels at 0.0 plus
//!   one at 100.0, history 20.0. In LINEAR space that one hot texel
//!   inflates sigma so much the box swallows the stale history unchanged
//!   (linear_hi computed below, way above 20.0) — the residual streak Peter
//!   flagged. In MAPPED space the box is tight, so the clamp actually
//!   engages and knocks the output down toward black.

use manifold_gpu::raytrace::MetalShadowRayTracer;

use crate::harness;

const TOLERANCE: f32 = 1e-4;
/// Wider tolerance for cases where the clamp box's own width is near-zero
/// in mapped space (flat neighborhood: true sigma is 0; HDR: sigma is
/// small relative to the mapped values' scale). `m2 - m1*m1` (both MSL and
/// this file's CPU mirror) is catastrophic cancellation once the mapped
/// samples aren't exactly binary-representable (e.g. `0.5/1.5`):
/// measured GPU sigma for a perfectly flat 0.5 neighborhood came back
/// ~1.2e-4 instead of exactly 0, not 0 — same nested-`+=` accumulation
/// order in the MSL as everywhere else, just no longer exactly cancelling
/// once the values are irrational-in-binary. The near-singular unmap
/// denominator (`1 - luma(t)`) then amplifies that into a ~2.7e-4 output
/// error. This is intrinsic float32 behavior, not a defect — plain linear-
/// space clamping never hit it because 0.5 and its square are exact.
const MAPPED_ROUNDTRIP_TOLERANCE: f32 = 1e-3;
/// Mirrors the MSL `RT_REFL_CLAMP_GAMMA` constant
/// (`crates/manifold-gpu/src/metal/raytrace.rs`) — retuning that constant
/// requires recomputing the expected values below with this same gamma.
const GAMMA: f32 = 1.0;

fn assert_close(got: [f32; 3], expected: [f32; 3], tolerance: f32, label: &str) {
    for i in 0..3 {
        assert!(
            (got[i] - expected[i]).abs() < tolerance,
            "{label}: component {i} — got {got:?}, expected {expected:?} (tolerance {tolerance})"
        );
    }
}

/// Mean and population-stddev of a set of scalar samples, computed the same
/// way the MSL helper does: mean-of-squares minus mean-squared.
fn mean_and_sigma(samples: &[f32]) -> (f32, f32) {
    let n = samples.len() as f32;
    let m1 = samples.iter().sum::<f32>() / n;
    let m2 = samples.iter().map(|v| v * v).sum::<f32>() / n;
    let sigma = (m2 - m1 * m1).max(0.0).sqrt();
    (m1, sigma)
}

/// Mirrors the MSL `luma()` helper (`crates/manifold-gpu/src/metal/raytrace.rs`).
fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Mirrors `clamp_refl_history`'s forward map `t(c) = c / (1 + luma(c))`.
fn tonemap(c: [f32; 3]) -> [f32; 3] {
    let l = luma(c);
    c.map(|x| x / (1.0 + l))
}

/// Mirrors `clamp_refl_history`'s inverse map `c = t / (1 - luma(t))`,
/// guarded the same way (`min(luma(t), 0.999)`).
fn untonemap(t: [f32; 3]) -> [f32; 3] {
    let l = luma(t).min(0.999);
    let denom = 1.0 - l;
    t.map(|x| x / denom)
}

#[test]
fn clamp_refl_history_noisy_neighborhood_keeps_history_inside_box() {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);

    // Checkerboard 0.2/0.8, same value on every channel, alpha unused.
    let samples = [0.2f32, 0.8, 0.2, 0.8, 0.2, 0.8, 0.2, 0.8, 0.2];
    let neighborhood: [[f32; 4]; 9] = std::array::from_fn(|i| [samples[i], samples[i], samples[i], 1.0]);

    // BUG-axe9: box is built in MAPPED space now — map the 9 samples first
    // (each channel equal, so scalar mean/sigma applies to every channel).
    let mapped_samples: Vec<f32> = samples.iter().map(|&s| tonemap([s, s, s])[0]).collect();
    let (mapped_mean, mapped_sigma) = mean_and_sigma(&mapped_samples);
    let mapped_history_scalar = mapped_mean + 0.5 * mapped_sigma;
    assert!(
        mapped_history_scalar < mapped_mean + GAMMA * mapped_sigma
            && mapped_history_scalar > mapped_mean - GAMMA * mapped_sigma,
        "test fixture bug: mapped history must land inside the mapped box"
    );
    // Unmap to get the LINEAR history value the kernel is called with —
    // round-tripping it back through map/clamp(no-op)/unmap must reproduce
    // it exactly (up to float error).
    let history_scalar = untonemap([mapped_history_scalar; 3])[0];
    let history = [history_scalar; 3];

    let got = tracer.debug_clamp_refl_history(device, &neighborhood, history);
    assert_close(got, history, TOLERANCE, "noisy neighborhood, history inside mapped box");
}

#[test]
fn clamp_refl_history_flat_neighborhood_collapses_to_mean() {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);

    let neighborhood: [[f32; 4]; 9] = std::array::from_fn(|_| [0.5, 0.5, 0.5, 1.0]);

    // BUG-axe9: a degenerate (zero-linear-variance) box behaves almost
    // identically in either space. Mapped sigma is NOT bit-exact zero
    // (`0.5/1.5` isn't binary-exact, so the mean-of-squares variance
    // formula's usual catastrophic cancellation leaves a small residual —
    // see `MAPPED_ROUNDTRIP_TOLERANCE`), but the clamp still collapses to
    // very nearly the mapped mean, which unmaps back to very nearly the
    // same linear mean (0.5).
    let mapped = tonemap([0.5, 0.5, 0.5]);
    let expected = untonemap(mapped);

    let history = [5.0f32; 3];
    let got = tracer.debug_clamp_refl_history(device, &neighborhood, history);
    assert_close(got, expected, MAPPED_ROUNDTRIP_TOLERANCE, "flat neighborhood collapses to mean");
}

/// BUG-axe9: the streak scenario — one hot HDR texel next to black
/// inflates LINEAR sigma enough that stale bright history survives the
/// clamp unchanged. Mapped-space clamping fixes this; this test pins the
/// fixed (mapped) behavior AND proves the linear-space clamp would have
/// failed, so the fix's reason to exist doesn't silently rot.
#[test]
fn clamp_refl_history_hdr_neighborhood_engages_in_mapped_space() {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);

    // 8 texels at 0.0, 1 at 100.0 (alpha 1.0, unused by the kernel).
    let samples = [0.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 100.0];
    let neighborhood: [[f32; 4]; 9] = std::array::from_fn(|i| [samples[i], samples[i], samples[i], 1.0]);
    let history = [20.0f32; 3];

    // Sanity: LINEAR-space clamp would have passed history through with
    // massive headroom — this is BUG-axe9's failure mode, pinned so the
    // fix's reason to exist is checked, not just its result.
    let (linear_mean, linear_sigma) = mean_and_sigma(&samples);
    let linear_hi = linear_mean + GAMMA * linear_sigma;
    assert!(
        linear_hi > 5.0,
        "test fixture bug: linear-space hi={linear_hi} should be far above the \
         history value under test, to prove linear clamping wouldn't engage"
    );

    // MAPPED-space expectation: map all 9 samples, compute mean/sigma
    // there, clamp the mapped history, unmap.
    let mapped_samples: Vec<f32> = samples.iter().map(|&s| tonemap([s, s, s])[0]).collect();
    let (mapped_mean, mapped_sigma) = mean_and_sigma(&mapped_samples);
    let mapped_lo = mapped_mean - GAMMA * mapped_sigma;
    let mapped_hi = mapped_mean + GAMMA * mapped_sigma;
    let mapped_history = tonemap(history)[0];
    let clamped_mapped = mapped_history.clamp(mapped_lo, mapped_hi);
    let expected_scalar = untonemap([clamped_mapped; 3])[0];
    let expected = [expected_scalar; 3];

    assert!(
        expected_scalar < 1.0,
        "test fixture bug: expected mapped-space-clamped output={expected_scalar} should be \
         well below the linear clamp's headroom, to prove the mapped clamp actually engaged"
    );

    let got = tracer.debug_clamp_refl_history(device, &neighborhood, history);
    eprintln!(
        "[BUG-axe9] HDR case — linear_hi={linear_hi:.6} (would pass 20.0 through unchanged), \
         mapped clamp expected={expected_scalar:.6}, got={:.6}",
        got[0]
    );
    assert_close(got, expected, MAPPED_ROUNDTRIP_TOLERANCE, "HDR neighborhood, mapped-space clamp engages");
}
