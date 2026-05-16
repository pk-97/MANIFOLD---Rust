//! Pixel-exact parity test for `primitive.dither_pattern` vs the
//! legacy `DitherFX` effect. Seventh §6.1 migration.


use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::DitherPattern;
use crate::harness::{self, Fixture, assert_bytewise_equal, default_ctx, make_default_effect};

const ALGOS: &[(u32, &str)] = &[
    (0, "bayer"),
    (1, "halftone"),
    (2, "lines"),
    (3, "xhatch"),
    (4, "noise"),
    (5, "diamond"),
];

const AMOUNTS: &[f32] = &[0.0, 0.5, 1.0];

#[test]
fn dither_pattern_is_pixel_exact_across_fixtures_algos_amounts() {
    let h = harness::shared();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(h);

        for &(algo_u, algo_label) in ALGOS {
            for &amount in AMOUNTS {
                let mut fx = make_default_effect(EffectTypeId::DITHER);
                fx.param_values[0].value = amount;
                fx.param_values[1].value = algo_u as f32;

                let legacy = h.run_legacy(&fx, &input, &ctx);
                let decomposed = h.run_primitive_graph(
                    Box::new(DitherPattern::new()),
                    &input,
                    &ctx,
                    |graph, prim_id| {
                        graph
                            .set_param(prim_id, "amount", ParamValue::Float(amount))
                            .unwrap();
                        graph
                            .set_param(prim_id, "algorithm", ParamValue::Enum(algo_u))
                            .unwrap();
                    },
                );

                assert_bytewise_equal(
                    &format!(
                        "dither_pattern/{:?}/algo={algo_label}/amount={amount}",
                        fixture
                    ),
                    &legacy,
                    &decomposed,
                );
            }
        }
    }
}
