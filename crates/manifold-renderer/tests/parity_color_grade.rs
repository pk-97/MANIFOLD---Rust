//! Pixel-exact parity test for `primitive.color_grade` vs the legacy
//! `ColorGradeFX` effect. Second §6.1 migration.
//!
//! ColorGrade has 9 parameters — a full Cartesian sweep would be
//! thousands of comparisons. Instead this test uses 6 representative
//! grade presets that each exercise a different region of the math:
//!
//! - **identity**: amount=0; should equal the input through both
//!   paths (legacy short-circuits via `should_skip`).
//! - **gain**: amount=1, gain=1.5; pure exposure adjustment.
//! - **desaturate**: amount=1, saturation=0; luma collapse.
//! - **hue_rotate**: amount=1, hue=90; HSV path exercised.
//! - **contrast_push**: amount=1, contrast=1.5; pivot math + the
//!   negative-clamp safeguard.
//! - **colorize_warm**: amount=1, colorize=0.7, tint=red; colorize
//!   pipeline with highlight/neutral masking.
//!
//! All 6 × 4 fixtures = 24 bytewise comparisons per run. If any
//! grade fails, the failure message localizes which grade and which
//! fixture diverged.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::primitives::ColorGrade;
use manifold_renderer::node_graph::ParamValue;
use parity::{
    assert_bytewise_equal, default_ctx, make_default_effect, Fixture, ParityHarness,
};

/// One grade preset. Field order matches the legacy
/// `ColorGradeFX::apply` mapping (`param_values[0..9]`).
#[derive(Debug, Clone, Copy)]
struct Grade {
    label: &'static str,
    amount: f32,
    gain: f32,
    saturation: f32,
    hue: f32,
    contrast: f32,
    colorize: f32,
    colorize_hue: f32,
    colorize_saturation: f32,
    colorize_focus: f32,
}

const GRADES: &[Grade] = &[
    Grade {
        label: "identity",
        amount: 0.0,
        gain: 1.0,
        saturation: 1.0,
        hue: 0.0,
        contrast: 1.0,
        colorize: 0.0,
        colorize_hue: 0.0,
        colorize_saturation: 1.0,
        colorize_focus: 0.75,
    },
    Grade {
        label: "gain",
        amount: 1.0,
        gain: 1.5,
        saturation: 1.0,
        hue: 0.0,
        contrast: 1.0,
        colorize: 0.0,
        colorize_hue: 0.0,
        colorize_saturation: 1.0,
        colorize_focus: 0.75,
    },
    Grade {
        label: "desaturate",
        amount: 1.0,
        gain: 1.0,
        saturation: 0.0,
        hue: 0.0,
        contrast: 1.0,
        colorize: 0.0,
        colorize_hue: 0.0,
        colorize_saturation: 1.0,
        colorize_focus: 0.75,
    },
    Grade {
        label: "hue_rotate",
        amount: 1.0,
        gain: 1.0,
        saturation: 1.0,
        hue: 90.0,
        contrast: 1.0,
        colorize: 0.0,
        colorize_hue: 0.0,
        colorize_saturation: 1.0,
        colorize_focus: 0.75,
    },
    Grade {
        label: "contrast_push",
        amount: 1.0,
        gain: 1.0,
        saturation: 1.0,
        hue: 0.0,
        contrast: 1.5,
        colorize: 0.0,
        colorize_hue: 0.0,
        colorize_saturation: 1.0,
        colorize_focus: 0.75,
    },
    Grade {
        label: "colorize_warm",
        amount: 1.0,
        gain: 1.0,
        saturation: 1.0,
        hue: 0.0,
        contrast: 1.0,
        colorize: 0.7,
        colorize_hue: 20.0,
        colorize_saturation: 0.8,
        colorize_focus: 0.6,
    },
];

#[test]
fn color_grade_is_pixel_exact_across_fixtures_and_grades() {
    let mut h = ParityHarness::new();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for grade in GRADES {
            let mut fx = make_default_effect(EffectTypeId::COLOR_GRADE);
            // Patch all 9 slots in declaration order. The legacy
            // ColorGradeFX::apply reads them positionally; the
            // primitive looks them up by name. Both must produce
            // bit-identical output.
            fx.param_values[0].value = grade.amount;
            fx.param_values[1].value = grade.gain;
            fx.param_values[2].value = grade.saturation;
            fx.param_values[3].value = grade.hue;
            fx.param_values[4].value = grade.contrast;
            fx.param_values[5].value = grade.colorize;
            fx.param_values[6].value = grade.colorize_hue;
            fx.param_values[7].value = grade.colorize_saturation;
            fx.param_values[8].value = grade.colorize_focus;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed = h.run_primitive_graph(
                Box::new(ColorGrade::new()),
                &input,
                &ctx,
                |graph, prim_id| {
                    let set = |g: &mut manifold_renderer::node_graph::Graph,
                               name: &'static str,
                               v: f32| {
                        g.set_param(prim_id, name, ParamValue::Float(v))
                            .expect("node.color_grade must accept param");
                    };
                    set(graph, "amount", grade.amount);
                    set(graph, "gain", grade.gain);
                    set(graph, "saturation", grade.saturation);
                    set(graph, "hue", grade.hue);
                    set(graph, "contrast", grade.contrast);
                    set(graph, "colorize", grade.colorize);
                    set(graph, "colorize_hue", grade.colorize_hue);
                    set(graph, "colorize_saturation", grade.colorize_saturation);
                    set(graph, "colorize_focus", grade.colorize_focus);
                },
            );

            assert_bytewise_equal(
                &format!(
                    "color_grade/{:?}/grade={}: legacy vs primitive.color_grade",
                    fixture, grade.label
                ),
                &legacy,
                &decomposed,
            );
        }
    }
}
