//! Pixel-exact parity test for `primitive.highlight_boost` vs the
//! legacy `HdrBoostFX` effect. Ninth §6.1 migration.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::primitives::HighlightBoost;
use manifold_renderer::node_graph::ParamValue;
use parity::{
    assert_bytewise_equal, default_ctx, make_default_effect, Fixture, ParityHarness,
};

#[derive(Debug, Clone, Copy)]
struct Setup {
    label: &'static str,
    amount: f32,
    gain: f32,
    threshold: f32,
    knee: f32,
}

const SETUPS: &[Setup] = &[
    // Identity (amount=0).
    Setup { label: "identity", amount: 0.0, gain: 1.5, threshold: 0.15, knee: 0.3 },
    // Default boost.
    Setup { label: "default", amount: 1.0, gain: 1.5, threshold: 0.15, knee: 0.3 },
    // Gain sweep.
    Setup { label: "gain_zero", amount: 1.0, gain: 0.0, threshold: 0.15, knee: 0.3 },
    Setup { label: "gain_max", amount: 1.0, gain: 5.0, threshold: 0.15, knee: 0.3 },
    // Threshold sweep.
    Setup { label: "thresh_low", amount: 1.0, gain: 2.0, threshold: 0.0, knee: 0.3 },
    Setup { label: "thresh_high", amount: 1.0, gain: 2.0, threshold: 0.9, knee: 0.3 },
    // Knee sweep — controls smoothstep transition width.
    Setup { label: "knee_zero", amount: 1.0, gain: 2.0, threshold: 0.5, knee: 0.0 },
    Setup { label: "knee_max", amount: 1.0, gain: 2.0, threshold: 0.5, knee: 1.0 },
];

#[test]
fn highlight_boost_is_pixel_exact_across_fixtures_and_setups() {
    let mut h = ParityHarness::new();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for s in SETUPS {
            let mut fx = make_default_effect(EffectTypeId::HDR_BOOST);
            fx.param_values[0].value = s.amount;
            fx.param_values[1].value = s.gain;
            fx.param_values[2].value = s.threshold;
            fx.param_values[3].value = s.knee;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed = h.run_primitive_graph(
                Box::new(HighlightBoost::new()),
                &input,
                &ctx,
                |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(s.amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "gain", ParamValue::Float(s.gain))
                        .unwrap();
                    graph
                        .set_param(prim_id, "threshold", ParamValue::Float(s.threshold))
                        .unwrap();
                    graph
                        .set_param(prim_id, "knee", ParamValue::Float(s.knee))
                        .unwrap();
                },
            );

            assert_bytewise_equal(
                &format!(
                    "highlight_boost/{:?}/setup={}: legacy vs primitive.highlight_boost",
                    fixture, s.label
                ),
                &legacy,
                &decomposed,
            );
        }
    }
}
