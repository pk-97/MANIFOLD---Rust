//! Pixel-exact parity test for `primitive.edge_detect` vs the
//! legacy `EdgeDetectFX` effect. Tenth §6.1 migration; first fused
//! composite primitive.


use manifold_core::PresetTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::EdgeDetect;
use crate::harness::{self, Fixture, assert_bytewise_equal, default_ctx, make_default_effect};

const SETUPS: &[(f32, f32, &str)] = &[
    (0.0, 0.1, "identity"),
    (0.5, 0.1, "half_amount"),
    (1.0, 0.0, "low_threshold"),
    (1.0, 0.1, "default_threshold"),
    (1.0, 0.5, "mid_threshold"),
    (1.0, 1.0, "max_threshold"),
];

#[test]
fn edge_detect_is_pixel_exact_across_fixtures_and_setups() {
    let h = harness::shared();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(h);

        for &(amount, threshold, label) in SETUPS {
            let mut fx = make_default_effect(PresetTypeId::EDGE_DETECT);
            fx.param_values[0].value = amount;
            fx.param_values[1].value = threshold;
            // param_values[2] (mode) is declared but unused by the
            // legacy shader; leave at default.

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed = h.run_primitive_graph(
                Box::new(EdgeDetect::new()),
                &input,
                &ctx,
                |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "threshold", ParamValue::Float(threshold))
                        .unwrap();
                },
            );

            assert_bytewise_equal(
                &format!("edge_detect/{:?}/setup={label}", fixture),
                &legacy,
                &decomposed,
            );
        }
    }
}
