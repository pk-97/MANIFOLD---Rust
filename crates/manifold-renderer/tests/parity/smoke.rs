//! Inventory smoke test — every registered generator must instantiate
//! and run one frame without panicking or producing NaN / Inf pixels.
//!
//! The legacy per-effect smoke test that iterated `EffectFactory` is
//! gone with §11 block 8 — effects no longer run through singletons.
//! Per-effect runtime correctness is covered by the parity tests
//! (against fixtures) and `every_bundled_preset_loads_validates_and_compiles`
//! (chain-buildable). Generators are still inventory-based and remain
//! covered here until the equivalent generator JSON migration lands.
//!
//! Lives in the parity binary because it reuses the same
//! `harness::shared()` device + readback machinery — adding a third
//! integration-test binary just for this would re-pay the ~5s
//! `GpuDevice::new()` cost we just eliminated.
//!
//! If this test ever flakes:
//!
//! - A generator's `render()` panicked → check parameter validation
//!   against the registry defaults.
//! - NaN / Inf detected → a shader divides-by-zero or computes
//!   `log(0)` / `1.0 / 0.0` for default-parameter inputs. The error
//!   message names the generator so you can grep its WGSL.

use half::f16;
use manifold_renderer::generator_context::{GeneratorContext, MAX_GEN_PARAMS};
use manifold_renderer::generators::registration::GeneratorFactory;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;

use crate::harness;

/// Every registered `GeneratorFactory` instantiates and renders one
/// frame into a fresh target with default parameters; output must be
/// finite. Generators have no parity-test layer, so this is their
/// primary integration check.
#[test]
fn every_registered_generator_runs_without_panicking_or_nans() {
    let h = harness::shared();
    let ctx = default_generator_ctx(h.width, h.height);

    let mut count = 0_usize;
    for factory in inventory::iter::<GeneratorFactory> {
        let id = factory.id.clone();
        let mut generator = (factory.create)(&h.device);

        let target = h.make_target(&format!("smoke-gen-{}", id.as_str()));
        let mut enc = h.device.create_encoder(&format!("smoke-gen-{}-enc", id.as_str()));
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            generator.render(&mut gpu, &target.texture, &ctx);
        }
        enc.commit_and_wait_completed();

        let bytes = h.readback(&target.texture);
        assert_finite_rgba16f(&format!("generator/{}", id.as_str()), &bytes);
        count += 1;
    }
    assert!(count > 0, "expected inventory::iter to yield ≥1 GeneratorFactory");
    eprintln!("smoke-tested {count} generators");
}

/// Deterministic `GeneratorContext` paralleling `default_ctx` for
/// effects. Time / beat fixed so any time-dependent generator
/// (FluidSimulation, Plasma) produces reproducible output across runs.
fn default_generator_ctx(width: u32, height: u32) -> GeneratorContext {
    GeneratorContext {
        time: 1.234,
        beat: 2.5,
        dt: 1.0 / 60.0,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: width as f32 / height as f32,
        anim_progress: 0.0,
        trigger_count: 0,
        // Default-zero params: matches what align_to_definition lands
        // for unspecified slots. If a generator panics here, its
        // metadata defaults aren't being read — which is the bug the
        // test exists to catch.
        params: [0.0; MAX_GEN_PARAMS],
        param_count: 0,
    }
}

/// Reinterpret an Rgba16Float byte stream as f32 pixels and assert
/// every channel is finite. Naming the failing effect / generator at
/// the call site makes the test's failure log a one-line bug report.
fn assert_finite_rgba16f(label: &str, bytes: &[u8]) {
    assert_eq!(bytes.len() % 2, 0, "{label}: odd byte length, expected RGBA16Float");
    let mut nans = 0_usize;
    let mut infs = 0_usize;
    let mut first_bad: Option<(usize, f32)> = None;
    for (i, chunk) in bytes.chunks_exact(2).enumerate() {
        let bits = u16::from_le_bytes([chunk[0], chunk[1]]);
        let v = f16::from_bits(bits).to_f32();
        if v.is_nan() {
            nans += 1;
            first_bad.get_or_insert((i, v));
        } else if v.is_infinite() {
            infs += 1;
            first_bad.get_or_insert((i, v));
        }
    }
    if nans > 0 || infs > 0 {
        let (idx, val) = first_bad.unwrap();
        panic!(
            "{label}: non-finite output — {nans} NaN(s) + {infs} Inf(s) of {} channels. First at channel {idx} = {val}.",
            bytes.len() / 2,
        );
    }
}

