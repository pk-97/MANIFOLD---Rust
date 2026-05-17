//! Pixel-exact parity test for `primitive.voronoi_prism` vs the
//! legacy `VoronoiPrismFX` effect. Twelfth §6.1 migration; fused
//! composite.
//!
//! `source_width` is now a real slider on the effect's third param
//! slot; the parity test pins it explicitly to 0.5625 on both paths so
//! the legacy chain dispatcher and the standalone primitive graph see
//! the same value (the audit moved the metadata default to 0.5; this
//! test deliberately exercises a different value).


use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::VoronoiPrism;
use crate::harness::{self, Fixture, assert_bytewise_equal, default_ctx, make_default_effect};

const SETUPS: &[(f32, f32, &str)] = &[
    (0.0, 16.0, "identity"),
    (0.5, 8.0, "half_amount_8cells"),
    (1.0, 4.0, "min_cells"),
    (1.0, 16.0, "default_cells"),
    (1.0, 32.0, "many_cells"),
    (1.0, 64.0, "max_cells"),
];

#[test]
fn voronoi_prism_is_pixel_exact_across_fixtures_and_setups() {
    let h = harness::shared();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(h);

        for &(amount, cells, label) in SETUPS {
            let mut fx = make_default_effect(EffectTypeId::VORONOI_PRISM);
            fx.param_values[0].value = amount;
            fx.param_values[1].value = cells;
            fx.param_values[2].value = 0.5625;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed = h.run_primitive_graph(
                Box::new(VoronoiPrism::new()),
                &input,
                &ctx,
                |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "cell_count", ParamValue::Float(cells))
                        .unwrap();
                    graph
                        .set_param(prim_id, "beat", ParamValue::Float(ctx.beat))
                        .unwrap();
                    graph
                        .set_param(prim_id, "source_width", ParamValue::Float(0.5625))
                        .unwrap();
                },
            );

            assert_bytewise_equal(
                &format!("voronoi_prism/{:?}/setup={label}", fixture),
                &legacy,
                &decomposed,
            );
        }
    }
}
