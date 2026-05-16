//! Pixel-exact parity test for `primitive.strobe` vs the legacy
//! `StrobeFX` effect. Eleventh §6.1 migration; fused composite.
//!
//! Both the legacy effect's binding apply path and the primitive
//! itself surface `rate` as a note-rate enum index. The primitive's
//! `run()` looks up `NOTE_RATE_VALUES[index]` internally before the
//! uniform reaches the shader. The parity test passes the same enum
//! index on both sides.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::Strobe;
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
                    // Primitive surfaces `rate` as the note-rate enum
                    // index — same as the binding's EnumRound convert
                    // produces on the legacy chain path. The
                    // index→strobes-per-beat lookup happens inside
                    // the primitive's `run()`.
                    graph
                        .set_param(prim_id, "rate", ParamValue::Enum(rate_idx as u32))
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
