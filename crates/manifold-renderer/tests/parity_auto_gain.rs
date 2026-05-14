//! Pixel-exact parity test for `primitive.auto_gain` vs the legacy
//! `AutoGainFX` effect. §6.5 commit 1; monolithic wrapper.
//!
//! AutoGain initialises its envelope from the first frame's measured
//! luminance and returns gain = 1.0 (no correction) on that first
//! frame — so single-frame parity exercises the apply pass with
//! gain=1.0, the character-mode shader specialization, and the
//! `color_push` / `hdr_ret` / `amount` (wet/dry) parameter wiring.
//! Multi-frame envelope behaviour is out of scope for unit parity;
//! it's the same code in both paths since the primitive delegates.
//!
//! Unique `owner_key` per setup — AutoGain keys its per-owner
//! envelope + measurement-buffer state in its registry, same caveat
//! as Watercolor (§6.3 c4) and DoF (§6.4).

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::AutoGain;
use parity::{Fixture, ParityHarness, assert_bytewise_equal, default_ctx, make_default_effect};

#[derive(Debug, Clone, Copy)]
struct Setup {
    label: &'static str,
    amount: f32,
    ratio: f32,
    punch: f32,
    target: f32,
    hdr_ret: f32,
    color: f32,
    character: u32,
}

const SETUPS: &[Setup] = &[
    Setup {
        label: "identity",
        amount: 0.0,
        ratio: 0.5,
        punch: 0.5,
        target: 0.5,
        hdr_ret: 0.5,
        color: 0.0,
        character: 0,
    },
    Setup {
        label: "clean_default",
        amount: 1.0,
        ratio: 0.5,
        punch: 0.5,
        target: 0.5,
        hdr_ret: 0.5,
        color: 0.0,
        character: 0,
    },
    Setup {
        label: "warm_default",
        amount: 1.0,
        ratio: 0.5,
        punch: 0.5,
        target: 0.5,
        hdr_ret: 0.5,
        color: 0.0,
        character: 1,
    },
    Setup {
        label: "film_default",
        amount: 1.0,
        ratio: 0.5,
        punch: 0.5,
        target: 0.5,
        hdr_ret: 0.5,
        color: 0.0,
        character: 2,
    },
    Setup {
        label: "vivid_default",
        amount: 1.0,
        ratio: 0.5,
        punch: 0.5,
        target: 0.5,
        hdr_ret: 0.5,
        color: 0.0,
        character: 3,
    },
    Setup {
        label: "grit_default",
        amount: 1.0,
        ratio: 0.5,
        punch: 0.5,
        target: 0.5,
        hdr_ret: 0.5,
        color: 0.0,
        character: 4,
    },
    Setup {
        label: "color_pos",
        amount: 1.0,
        ratio: 0.5,
        punch: 0.5,
        target: 0.5,
        hdr_ret: 0.5,
        color: 1.0,
        character: 0,
    },
    Setup {
        label: "color_neg",
        amount: 1.0,
        ratio: 0.5,
        punch: 0.5,
        target: 0.5,
        hdr_ret: 0.5,
        color: -1.0,
        character: 0,
    },
    Setup {
        label: "high_hdr_ret",
        amount: 1.0,
        ratio: 0.5,
        punch: 0.5,
        target: 0.5,
        hdr_ret: 1.0,
        color: 0.0,
        character: 0,
    },
];

#[test]
fn auto_gain_is_pixel_exact_across_fixtures_and_setups() {
    let mut h = ParityHarness::new();
    let mut owner_key: i64 = 0;

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for s in SETUPS {
            owner_key += 1;
            let ctx = manifold_renderer::effect::EffectContext {
                owner_key,
                ..default_ctx(h.width, h.height)
            };

            let mut fx = make_default_effect(EffectTypeId::AUTO_GAIN);
            fx.param_values[0].value = s.amount;
            fx.param_values[1].value = s.ratio;
            fx.param_values[2].value = s.punch;
            fx.param_values[3].value = s.target;
            fx.param_values[4].value = s.hdr_ret;
            fx.param_values[5].value = s.color;
            fx.param_values[6].value = s.character as f32;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed =
                h.run_primitive_graph(Box::new(AutoGain::new()), &input, &ctx, |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(s.amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "ratio", ParamValue::Float(s.ratio))
                        .unwrap();
                    graph
                        .set_param(prim_id, "punch", ParamValue::Float(s.punch))
                        .unwrap();
                    graph
                        .set_param(prim_id, "target", ParamValue::Float(s.target))
                        .unwrap();
                    graph
                        .set_param(prim_id, "hdr_ret", ParamValue::Float(s.hdr_ret))
                        .unwrap();
                    graph
                        .set_param(prim_id, "color", ParamValue::Float(s.color))
                        .unwrap();
                    graph
                        .set_param(prim_id, "char", ParamValue::Enum(s.character))
                        .unwrap();
                });

            assert_bytewise_equal(
                &format!("auto_gain/{:?}/setup={}", fixture, s.label),
                &legacy,
                &decomposed,
            );
        }
    }
}
