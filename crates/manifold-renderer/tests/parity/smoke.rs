//! Inventory smoke test — every registered effect + generator must
//! instantiate and run one frame without panicking or producing NaN /
//! Inf pixels.
//!
//! The parity tests cover effects that have been decomposed into
//! primitives (Phase-4a migration). Effects added *before* a parity
//! test exists for them, and every generator, get their only
//! end-to-end check from this file. Catches the class of bug where a
//! newly-added effect's `apply()` panics on default params, or a
//! shader produces `0.0 / 0.0` at a corner of the input space.
//!
//! Lives in the parity binary because it reuses the same
//! `harness::shared()` device + readback machinery — adding a third
//! integration-test binary just for this would re-pay the ~5s
//! `GpuDevice::new()` cost we just eliminated.
//!
//! If this test ever flakes:
//!
//! 1. A registered effect's `apply()` panicked → check the effect's
//!    parameter validation against the registry defaults
//!    (`align_to_definition` should land it in a valid state).
//! 2. NaN / Inf detected → a shader divides-by-zero or computes
//!    `log(0)` / `1.0 / 0.0` for default-parameter inputs. The error
//!    message names the effect / generator so you can grep its WGSL.

use half::f16;
use manifold_core::effects::EffectInstance;
use manifold_renderer::effect::EffectContext;
use manifold_renderer::effects::registration::EffectFactory;
use manifold_renderer::generator_context::{GeneratorContext, MAX_GEN_PARAMS};
use manifold_renderer::generators::registration::GeneratorFactory;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;

use crate::harness::{self, Fixture, default_ctx};

/// Every registered `EffectFactory` instantiates, runs `apply()` once
/// against a gradient fixture, and produces a finite Rgba16Float
/// output. Any new effect added via `inventory::submit!` lands here
/// automatically — no per-effect test scaffolding required.
#[test]
fn every_registered_effect_runs_without_panicking_or_nans() {
    let h = harness::shared();
    let input = Fixture::Gradient.build(h);
    let ctx = default_ctx(h.width, h.height);

    let mut count = 0_usize;
    for factory in inventory::iter::<EffectFactory> {
        let id = factory.id.clone();
        let mut effect = (factory.create)(&h.device);

        let mut fx = EffectInstance::new(id.clone());
        fx.align_to_definition();
        fx.enabled = true;

        let target = h.make_target(&format!("smoke-effect-{}", id.as_str()));
        let mut enc = h.device.create_encoder(&format!("smoke-{}-enc", id.as_str()));
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            effect.apply(&mut gpu, &input, &target.texture, &fx, &ctx);
        }
        enc.commit_and_wait_completed();

        let bytes = h.readback(&target.texture);
        assert_finite_rgba16f(&format!("effect/{}", id.as_str()), &bytes);
        count += 1;
    }
    assert!(count > 0, "expected inventory::iter to yield ≥1 EffectFactory");
    eprintln!("smoke-tested {count} effects");
}

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

// EffectContext import is required by the call site even though we
// build it via `default_ctx`.
#[allow(dead_code)]
fn _ensure_ctx_import(_: &EffectContext) {}
