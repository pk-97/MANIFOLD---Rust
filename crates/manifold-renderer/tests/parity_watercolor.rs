//! Pixel-exact parity test for `primitive.watercolor` vs the legacy
//! `WatercolorFX` effect. §6.3 commit 4; fused composite (7-pass
//! feedback pipeline, non-separable diffusion blur, dual-input
//! slope pass — all reasons decomposition would break parity).
//!
//! Watercolor carries persistent `feedback` state across frames.
//! The parity harness builds a fresh instance per setup, so both
//! paths start with feedback cleared to black and produce identical
//! first-frame output. Time is pinned to `ctx.time` (harness default
//! 1.234), so the procedural fBM flow map is deterministic across
//! both paths.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::Watercolor;
use parity::{Fixture, ParityHarness, assert_bytewise_equal, default_ctx, make_default_effect};

#[derive(Debug, Clone, Copy)]
struct Setup {
    label: &'static str,
    amount: f32,
    displace: f32,
    blur: f32,
    decay: f32,
}

const SETUPS: &[Setup] = &[
    Setup {
        label: "identity",
        amount: 0.0,
        displace: 0.001,
        blur: 2.0,
        decay: 0.99,
    },
    Setup {
        label: "default",
        amount: 0.5,
        displace: 0.001,
        blur: 2.0,
        decay: 0.99,
    },
    Setup {
        label: "full_wet",
        amount: 1.0,
        displace: 0.001,
        blur: 2.0,
        decay: 0.99,
    },
    Setup {
        label: "high_displace",
        amount: 1.0,
        displace: 0.01,
        blur: 2.0,
        decay: 0.99,
    },
    Setup {
        label: "low_displace",
        amount: 1.0,
        displace: 0.0001,
        blur: 2.0,
        decay: 0.99,
    },
    Setup {
        label: "wide_blur",
        amount: 1.0,
        displace: 0.001,
        blur: 8.0,
        decay: 0.99,
    },
    Setup {
        label: "tight_blur",
        amount: 1.0,
        displace: 0.001,
        blur: 0.5,
        decay: 0.99,
    },
    Setup {
        label: "fast_decay",
        amount: 1.0,
        displace: 0.001,
        blur: 2.0,
        decay: 0.90,
    },
];

#[test]
fn watercolor_is_pixel_exact_across_fixtures_and_setups() {
    let mut h = ParityHarness::new();
    // Watercolor owns persistent feedback state in its legacy
    // implementation, keyed by `EffectContext::owner_key`. The parity
    // harness shares one `EffectRegistry` across the whole loop, so
    // every setup must use a unique owner_key — otherwise the legacy
    // sees stale feedback from the previous setup's pass 6, while
    // the primitive (a fresh `Watercolor::new()` per setup) starts
    // with cleared feedback, and parity drifts. Bloom and Halation
    // overwrite their entire state every frame so they don't care;
    // Watercolor's persistent feedback makes it the canary.
    let mut owner_key: i64 = 0;

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for s in SETUPS {
            owner_key += 1;
            let ctx = manifold_renderer::effect::EffectContext {
                owner_key,
                ..default_ctx(h.width, h.height)
            };
            let mut fx = make_default_effect(EffectTypeId::WATERCOLOR);
            fx.param_values[0].value = s.amount;
            fx.param_values[1].value = s.displace;
            fx.param_values[2].value = s.blur;
            fx.param_values[3].value = s.decay;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed = h.run_primitive_graph(
                Box::new(Watercolor::new()),
                &input,
                &ctx,
                |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(s.amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "displace", ParamValue::Float(s.displace))
                        .unwrap();
                    graph
                        .set_param(prim_id, "blur", ParamValue::Float(s.blur))
                        .unwrap();
                    graph
                        .set_param(prim_id, "decay", ParamValue::Float(s.decay))
                        .unwrap();
                    graph
                        .set_param(prim_id, "time", ParamValue::Float(ctx.time))
                        .unwrap();
                },
            );

            assert_bytewise_equal(
                &format!("watercolor/{:?}/setup={}", fixture, s.label),
                &legacy,
                &decomposed,
            );
        }
    }
}
