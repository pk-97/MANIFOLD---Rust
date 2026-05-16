//! Pixel-exact parity test for `primitive.bloom` vs the legacy
//! `BloomFX` effect. §6.3 commit 2; fused composite (Unity-style
//! Blur9 tent + Blur13 filmic kernels with a ping-ponging dual mip
//! chain — see `primitives/bloom.rs` for why decomposition was
//! rejected).
//!
//! Bloom is stateful (owns a mip pyramid). The harness builds a
//! fresh `Bloom::new()` per setup, so pyramid allocation is exercised
//! every iteration. At 128×128 the pyramid is 128 → 64 → 32 → 16
//! (MIN_SIZE=16), `BLOOM_LEVELS = 3` so the downsample / upsample
//! loops run twice each.


use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::Bloom;
use crate::harness::{self, Fixture, assert_bytewise_equal, default_ctx, make_default_effect};

const SETUPS: &[(f32, &str)] = &[
    (0.0, "identity"),
    (0.187, "default"),
    (0.5, "mid"),
    (1.0, "max_smoothstep"),
    (2.5, "wide_radius"),
    (5.0, "max_amount"),
];

#[test]
fn bloom_is_pixel_exact_across_fixtures_and_setups() {
    let h = harness::shared();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(h);

        for &(amount, label) in SETUPS {
            let mut fx = make_default_effect(EffectTypeId::BLOOM);
            fx.param_values[0].value = amount;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed =
                h.run_primitive_graph(Box::new(Bloom::new()), &input, &ctx, |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(amount))
                        .unwrap();
                });

            assert_bytewise_equal(
                &format!("bloom/{:?}/setup={label}", fixture),
                &legacy,
                &decomposed,
            );
        }
    }
}
