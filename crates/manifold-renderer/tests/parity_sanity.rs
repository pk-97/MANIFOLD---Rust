//! Parity harness self-test: running the same legacy effect twice on
//! the same input must produce byte-identical output. This proves the
//! framework is deterministic before any decomposed primitive's
//! parity test relies on it.
//!
//! When this test fails, the cause is one of:
//!
//! 1. Non-determinism in the legacy `EffectChain::apply_chain` path
//!    (uncleared state, race in `clear_all_state`, time-dependent
//!    constant we didn't pin).
//! 2. Non-determinism in `MetalBackend` / `Executor` scratch reuse.
//! 3. Readback racing GPU completion (`commit_and_wait_completed`
//!    not actually waiting).
//!
//! All three are framework bugs to fix before parity-comparing any
//! decomposed primitive — otherwise the parity tests would flap.

mod parity;

use manifold_core::EffectTypeId;
use parity::{
    assert_bytewise_equal, default_ctx, make_default_effect, Fixture, ParityHarness,
};

/// Runs `InvertColors` twice on `Fixture::Gradient` and asserts the
/// two readbacks are identical. InvertColors is the simplest stateless
/// effect (one compute pass, no time dependence, no per-owner state),
/// so a failure here points at the harness, not the effect.
#[test]
fn legacy_invert_is_deterministic_on_gradient() {
    let mut h = ParityHarness::new();
    let input = Fixture::Gradient.build(&h);
    let fx = make_default_effect(EffectTypeId::INVERT_COLORS);
    let ctx = default_ctx(h.width, h.height);

    let first = h.run_legacy(&fx, &input, &ctx);
    let second = h.run_legacy(&fx, &input, &ctx);

    assert_bytewise_equal("invert/gradient run1 vs run2", &first, &second);
}

/// Same determinism check across every canonical fixture. Each
/// fixture exercises a different region of the input space; if one
/// fixture flakes while others don't, the failure surface is narrowed
/// to that input shape.
#[test]
fn legacy_invert_is_deterministic_across_fixtures() {
    let mut h = ParityHarness::new();
    let fx = make_default_effect(EffectTypeId::INVERT_COLORS);
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);
        let first = h.run_legacy(&fx, &input, &ctx);
        let second = h.run_legacy(&fx, &input, &ctx);
        assert_bytewise_equal(
            &format!("invert/{:?} run1 vs run2", fixture),
            &first,
            &second,
        );
    }
}

/// Independent harness instances must produce identical bytes — proves
/// no shared mutable state leaks across `ParityHarness::new()` calls.
/// Without this, parity sweeps that build one harness per effect could
/// silently drift.
#[test]
fn legacy_invert_is_deterministic_across_harness_instances() {
    let fx = make_default_effect(EffectTypeId::INVERT_COLORS);

    let bytes_a = {
        let mut h = ParityHarness::new();
        let input = Fixture::Gradient.build(&h);
        let ctx = default_ctx(h.width, h.height);
        h.run_legacy(&fx, &input, &ctx)
    };
    let bytes_b = {
        let mut h = ParityHarness::new();
        let input = Fixture::Gradient.build(&h);
        let ctx = default_ctx(h.width, h.height);
        h.run_legacy(&fx, &input, &ctx)
    };

    assert_bytewise_equal(
        "invert/gradient harness1 vs harness2",
        &bytes_a,
        &bytes_b,
    );
}
