//! Pixel-exact parity test for `primitive.strobe` vs the legacy
//! `StrobeFX` effect. Eleventh §6.1 migration; fused composite.
//!
//! The legacy effect maps a rate-slider index through NOTE_RATES to
//! get strobes-per-beat. The primitive accepts the resolved rate
//! directly so the parity test indexes the same table inline (it
//! also lives on the primitive as `STROBE_NOTE_RATES` for the
//! Strobe preset graph that will replace StrobeFX).

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::{STROBE_NOTE_RATES, Strobe};
use parity::{Fixture, ParityHarness, assert_bytewise_equal, default_ctx, make_default_effect};

/// (rate_idx, mode, amount, label).
const SETUPS: &[(usize, u32, f32, &str)] = &[
    (6, 0, 0.0, "identity"),
    (0, 0, 1.0, "slowest_opacity"),
    (6, 0, 1.0, "default_opacity"),
    (9, 0, 1.0, "fastest_opacity"),
    (6, 1, 1.0, "white"),
    (6, 2, 1.0, "gain"),
    (3, 1, 0.5, "half_amount_white"),
];

#[test]
fn strobe_is_pixel_exact_across_fixtures_and_setups() {
    let mut h = ParityHarness::new();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for &(rate_idx, mode, amount, label) in SETUPS {
            let rate = STROBE_NOTE_RATES[rate_idx];

            let mut fx = make_default_effect(EffectTypeId::STROBE);
            fx.param_values[0].value = amount;
            fx.param_values[1].value = rate_idx as f32;
            fx.param_values[2].value = mode as f32;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed =
                h.run_primitive_graph(Box::new(Strobe::new()), &input, &ctx, |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "rate", ParamValue::Float(rate))
                        .unwrap();
                    graph
                        .set_param(prim_id, "mode", ParamValue::Enum(mode))
                        .unwrap();
                    // Match the legacy's `ctx.beat` (default_ctx
                    // pins it to 2.5).
                    graph
                        .set_param(prim_id, "beat", ParamValue::Float(ctx.beat))
                        .unwrap();
                });

            assert_bytewise_equal(
                &format!("strobe/{:?}/setup={label}", fixture),
                &legacy,
                &decomposed,
            );
        }
    }
}
