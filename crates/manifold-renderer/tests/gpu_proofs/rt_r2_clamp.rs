//! BUG-dx6w value-level proof for the RT-R2 specular-history variance clip
//! (`manifold_gpu::raytrace::MetalShadowRayTracer::debug_clamp_refl_history`,
//! which dispatches the EXACT SAME `clamp_refl_history` MSL helper
//! `accumulate_irradiance` calls internally against a caller-supplied 3x3
//! `hi_refl` neighborhood and a history value — no accumulation pass, no
//! ray tracing/RNG involved).
//!
//! `clamp_refl_history` clamps `hist` to `[mean - gamma*sigma, mean +
//! gamma*sigma]` per channel, where `mean`/`sigma` are the mean and
//! standard deviation of the CURRENT frame's 3x3 neighborhood and `gamma`
//! is the MSL constant `RT_REFL_CLAMP_GAMMA` (1.0 as of this test — if that
//! constant is retuned, the expected values below must be recomputed with
//! the same formula and the new gamma).
//!
//! Two cases, CPU-computed expectations in f32:
//! - Noisy neighborhood (checkerboard 0.2/0.8): history lands inside the
//!   mean±sigma box → returned unchanged.
//! - Flat neighborhood (all 0.5, sigma=0): history collapses to the mean →
//!   returned exactly 0.5 regardless of the input history value.

use manifold_gpu::raytrace::MetalShadowRayTracer;

use crate::harness;

const TOLERANCE: f32 = 1e-4;
/// Mirrors the MSL `RT_REFL_CLAMP_GAMMA` constant
/// (`crates/manifold-gpu/src/metal/raytrace.rs`) — retuning that constant
/// requires recomputing the expected values below with this same gamma.
const GAMMA: f32 = 1.0;

fn assert_close(got: [f32; 3], expected: [f32; 3], label: &str) {
    for i in 0..3 {
        assert!(
            (got[i] - expected[i]).abs() < TOLERANCE,
            "{label}: component {i} — got {got:?}, expected {expected:?} (tolerance {TOLERANCE})"
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

#[test]
fn clamp_refl_history_noisy_neighborhood_keeps_history_inside_box() {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);

    // Checkerboard 0.2/0.8, same value on every channel, alpha unused.
    let samples = [0.2f32, 0.8, 0.2, 0.8, 0.2, 0.8, 0.2, 0.8, 0.2];
    let neighborhood: [[f32; 4]; 9] = std::array::from_fn(|i| [samples[i], samples[i], samples[i], 1.0]);

    let (mean, sigma) = mean_and_sigma(&samples);
    let history_scalar = mean + 0.5 * sigma;
    assert!(
        history_scalar < mean + GAMMA * sigma && history_scalar > mean - GAMMA * sigma,
        "test fixture bug: history must land inside the box"
    );
    let history = [history_scalar; 3];

    let got = tracer.debug_clamp_refl_history(device, &neighborhood, history);
    assert_close(got, history, "noisy neighborhood, history inside box");
}

#[test]
fn clamp_refl_history_flat_neighborhood_collapses_to_mean() {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);

    let samples = [0.5f32; 9];
    let neighborhood: [[f32; 4]; 9] = std::array::from_fn(|_| [0.5, 0.5, 0.5, 1.0]);

    let (mean, sigma) = mean_and_sigma(&samples);
    assert!(sigma < 1e-6, "flat neighborhood must have zero variance");

    let history = [5.0f32; 3];
    let got = tracer.debug_clamp_refl_history(device, &neighborhood, history);
    assert_close(got, [mean; 3], "flat neighborhood collapses to mean");
}
