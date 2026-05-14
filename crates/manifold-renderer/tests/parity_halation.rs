//! Pixel-exact parity test for `primitive.halation` vs the legacy
//! `HalationFX` effect. §6.3 commit 3; fused composite (Pass 0
//! applies threshold + tint per Gaussian tap — splitting would
//! quantize through fp16 and break parity).
//!
//! Sweeps amount, threshold, spread, hue (HSV → RGB on the host),
//! and saturation across each fixture. The hue parameter is decoded
//! through legacy's bit-for-bit HSV→RGB; if the implementation diverges
//! the test fails on the colored fixtures first.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::Halation;
use parity::{Fixture, ParityHarness, assert_bytewise_equal, default_ctx, make_default_effect};

#[derive(Debug, Clone, Copy)]
struct Setup {
    label: &'static str,
    amount: f32,
    threshold: f32,
    spread: f32,
    hue: f32,
    saturation: f32,
}

const SETUPS: &[Setup] = &[
    Setup {
        label: "identity",
        amount: 0.0,
        threshold: 0.5,
        spread: 0.5,
        hue: 20.0,
        saturation: 0.6,
    },
    Setup {
        label: "default",
        amount: 0.5,
        threshold: 0.5,
        spread: 0.5,
        hue: 20.0,
        saturation: 0.6,
    },
    Setup {
        label: "low_threshold",
        amount: 1.0,
        threshold: 0.1,
        spread: 0.5,
        hue: 20.0,
        saturation: 0.6,
    },
    Setup {
        label: "high_threshold",
        amount: 1.0,
        threshold: 0.9,
        spread: 0.5,
        hue: 20.0,
        saturation: 0.6,
    },
    Setup {
        label: "wide_spread",
        amount: 1.0,
        threshold: 0.5,
        spread: 1.0,
        hue: 20.0,
        saturation: 0.6,
    },
    Setup {
        label: "blue_tint",
        amount: 1.0,
        threshold: 0.5,
        spread: 0.5,
        hue: 240.0,
        saturation: 0.8,
    },
    Setup {
        label: "desaturated",
        amount: 1.0,
        threshold: 0.5,
        spread: 0.5,
        hue: 20.0,
        saturation: 0.0,
    },
    Setup {
        label: "full_amount",
        amount: 1.0,
        threshold: 0.3,
        spread: 0.7,
        hue: 120.0,
        saturation: 1.0,
    },
];

#[test]
fn halation_is_pixel_exact_across_fixtures_and_setups() {
    let mut h = ParityHarness::new();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for s in SETUPS {
            let mut fx = make_default_effect(EffectTypeId::HALATION);
            fx.param_values[0].value = s.amount;
            fx.param_values[1].value = s.threshold;
            fx.param_values[2].value = s.spread;
            fx.param_values[3].value = s.hue;
            fx.param_values[4].value = s.saturation;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed =
                h.run_primitive_graph(Box::new(Halation::new()), &input, &ctx, |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(s.amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "threshold", ParamValue::Float(s.threshold))
                        .unwrap();
                    graph
                        .set_param(prim_id, "spread", ParamValue::Float(s.spread))
                        .unwrap();
                    graph
                        .set_param(prim_id, "hue", ParamValue::Float(s.hue))
                        .unwrap();
                    graph
                        .set_param(prim_id, "saturation", ParamValue::Float(s.saturation))
                        .unwrap();
                });

            assert_bytewise_equal(
                &format!("halation/{:?}/setup={}", fixture, s.label),
                &legacy,
                &decomposed,
            );
        }
    }
}
