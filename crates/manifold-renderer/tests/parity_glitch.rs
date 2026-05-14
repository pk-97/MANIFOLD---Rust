//! Pixel-exact parity test for `primitive.glitch` vs the legacy
//! `GlitchFX` effect. Thirteenth and final §6.1 migration; fused
//! composite.
//!
//! Glitch's output is highly time-sensitive — `floor(time * speed *
//! 12)` advances the hash. The parity test runs at the harness's
//! default `ctx.time = 1.234`, which falls in a regime where the
//! displacement / scanline / RGB-split paths all fire.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::Glitch;
use parity::{Fixture, ParityHarness, assert_bytewise_equal, default_ctx, make_default_effect};

#[derive(Debug, Clone, Copy)]
struct Setup {
    label: &'static str,
    amount: f32,
    block_size: f32,
    rgb_shift: f32,
    scanline: f32,
    speed: f32,
}

const SETUPS: &[Setup] = &[
    Setup {
        label: "identity",
        amount: 0.0,
        block_size: 16.0,
        rgb_shift: 0.01,
        scanline: 0.3,
        speed: 2.0,
    },
    Setup {
        label: "default",
        amount: 1.0,
        block_size: 16.0,
        rgb_shift: 0.01,
        scanline: 0.3,
        speed: 2.0,
    },
    Setup {
        label: "small_blocks",
        amount: 1.0,
        block_size: 4.0,
        rgb_shift: 0.01,
        scanline: 0.3,
        speed: 2.0,
    },
    Setup {
        label: "large_blocks",
        amount: 1.0,
        block_size: 64.0,
        rgb_shift: 0.01,
        scanline: 0.3,
        speed: 2.0,
    },
    Setup {
        label: "rgb_max",
        amount: 1.0,
        block_size: 16.0,
        rgb_shift: 0.05,
        scanline: 0.3,
        speed: 2.0,
    },
    Setup {
        label: "scanline_max",
        amount: 1.0,
        block_size: 16.0,
        rgb_shift: 0.01,
        scanline: 1.0,
        speed: 2.0,
    },
    Setup {
        label: "fast_time",
        amount: 1.0,
        block_size: 16.0,
        rgb_shift: 0.01,
        scanline: 0.3,
        speed: 10.0,
    },
    Setup {
        label: "half_amount",
        amount: 0.5,
        block_size: 16.0,
        rgb_shift: 0.025,
        scanline: 0.5,
        speed: 2.0,
    },
];

#[test]
fn glitch_is_pixel_exact_across_fixtures_and_setups() {
    let mut h = ParityHarness::new();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for s in SETUPS {
            let mut fx = make_default_effect(EffectTypeId::GLITCH);
            fx.param_values[0].value = s.amount;
            fx.param_values[1].value = s.block_size;
            fx.param_values[2].value = s.rgb_shift;
            fx.param_values[3].value = s.scanline;
            fx.param_values[4].value = s.speed;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed =
                h.run_primitive_graph(Box::new(Glitch::new()), &input, &ctx, |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(s.amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "block_size", ParamValue::Float(s.block_size))
                        .unwrap();
                    graph
                        .set_param(prim_id, "rgb_shift", ParamValue::Float(s.rgb_shift))
                        .unwrap();
                    graph
                        .set_param(prim_id, "scanline", ParamValue::Float(s.scanline))
                        .unwrap();
                    graph
                        .set_param(prim_id, "speed", ParamValue::Float(s.speed))
                        .unwrap();
                    graph
                        .set_param(prim_id, "time", ParamValue::Float(ctx.time))
                        .unwrap();
                });

            assert_bytewise_equal(
                &format!("glitch/{:?}/setup={}", fixture, s.label),
                &legacy,
                &decomposed,
            );
        }
    }
}
