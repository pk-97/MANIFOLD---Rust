//! Pixel-exact parity test for `primitive.kaleido_fold` vs legacy
//! `KaleidoscopeFX`. Fourth §6.1 migration.
//!
//! 5 segment counts × 3 amounts × 4 fixtures = 60 bytewise
//! comparisons. The segment sweep includes the minimum (2, the
//! degenerate fold) and the maximum declared (16); the amount sweep
//! includes the passthrough boundary (0), midpoint, and full (1).

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::KaleidoFold;
use parity::{Fixture, ParityHarness, assert_bytewise_equal, default_ctx, make_default_effect};

const SEGMENTS: &[f32] = &[2.0, 4.0, 6.0, 10.0, 16.0];
const AMOUNTS: &[f32] = &[0.0, 0.5, 1.0];

#[test]
fn kaleido_fold_is_pixel_exact_across_fixtures_segments_amounts() {
    let mut h = ParityHarness::new();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for &segments in SEGMENTS {
            for &amount in AMOUNTS {
                let mut fx = make_default_effect(EffectTypeId::KALEIDOSCOPE);
                fx.param_values[0].value = amount;
                fx.param_values[1].value = segments;

                let legacy = h.run_legacy(&fx, &input, &ctx);
                let decomposed = h.run_primitive_graph(
                    Box::new(KaleidoFold::new()),
                    &input,
                    &ctx,
                    |graph, prim_id| {
                        graph
                            .set_param(prim_id, "amount", ParamValue::Float(amount))
                            .unwrap();
                        graph
                            .set_param(prim_id, "segments", ParamValue::Float(segments))
                            .unwrap();
                    },
                );

                assert_bytewise_equal(
                    &format!("kaleido_fold/{:?}/seg={segments}/amount={amount}", fixture),
                    &legacy,
                    &decomposed,
                );
            }
        }
    }
}

/// Below-min segments (< 2) must be clamped identically on both
/// paths. Legacy CPU-clamps via `.max(2.0)`; primitive must too.
#[test]
fn kaleido_fold_clamps_below_min_segments() {
    let mut h = ParityHarness::new();
    let input = Fixture::Gradient.build(&h);
    let ctx = default_ctx(h.width, h.height);

    for &segments in &[-1.0f32, 0.0, 1.0, 1.5] {
        let mut fx = make_default_effect(EffectTypeId::KALEIDOSCOPE);
        fx.param_values[0].value = 1.0;
        fx.param_values[1].value = segments;

        let legacy = h.run_legacy(&fx, &input, &ctx);
        let decomposed = h.run_primitive_graph(
            Box::new(KaleidoFold::new()),
            &input,
            &ctx,
            |graph, prim_id| {
                graph
                    .set_param(prim_id, "amount", ParamValue::Float(1.0))
                    .unwrap();
                graph
                    .set_param(prim_id, "segments", ParamValue::Float(segments))
                    .unwrap();
            },
        );

        assert_bytewise_equal(
            &format!("kaleido_fold/below-min-segments={segments}"),
            &legacy,
            &decomposed,
        );
    }
}
