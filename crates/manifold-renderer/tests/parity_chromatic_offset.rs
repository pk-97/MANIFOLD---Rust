//! Pixel-exact parity test for `primitive.chromatic_offset` vs the
//! legacy `ChromaticAberrationFX` effect. Sixth §6.1 migration.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::primitives::ChromaticOffset;
use manifold_renderer::node_graph::ParamValue;
use parity::{
    assert_bytewise_equal, default_ctx, make_default_effect, Fixture, ParityHarness,
};

#[derive(Debug, Clone, Copy)]
struct Setup {
    label: &'static str,
    amount: f32,
    offset: f32,
    mode: u32,
    angle: f32,
    falloff: f32,
}

const SETUPS: &[Setup] = &[
    // Identity (amount=0). Both paths preserve source bytes exactly.
    Setup { label: "identity", amount: 0.0, offset: 0.01, mode: 0, angle: 0.0, falloff: 0.5 },
    // Radial sweep — exercises smoothstep + normalize + falloff math.
    Setup { label: "radial_mid", amount: 0.6, offset: 0.02, mode: 0, angle: 0.0, falloff: 0.5 },
    Setup { label: "radial_max", amount: 1.0, offset: 0.05, mode: 0, angle: 0.0, falloff: 1.0 },
    Setup { label: "radial_no_falloff", amount: 1.0, offset: 0.03, mode: 0, angle: 0.0, falloff: 0.0 },
    // Linear sweep — angle controls a fixed direction. 0° = +X, 90° = +Y, 45° = diagonal.
    Setup { label: "linear_x", amount: 1.0, offset: 0.025, mode: 1, angle: 0.0, falloff: 0.5 },
    Setup { label: "linear_y", amount: 1.0, offset: 0.025, mode: 1, angle: 90.0, falloff: 0.5 },
    Setup { label: "linear_diag", amount: 1.0, offset: 0.025, mode: 1, angle: 45.0, falloff: 0.5 },
    Setup { label: "linear_wrap", amount: 1.0, offset: 0.025, mode: 1, angle: 359.0, falloff: 0.5 },
];

#[test]
fn chromatic_offset_is_pixel_exact_across_fixtures_and_setups() {
    let mut h = ParityHarness::new();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for s in SETUPS {
            let mut fx = make_default_effect(EffectTypeId::CHROMATIC_ABERRATION);
            // Legacy `ChromaticAberrationFX::apply` param order:
            //   [0]=amount [1]=offset [2]=mode [3]=angle [4]=falloff.
            fx.param_values[0].value = s.amount;
            fx.param_values[1].value = s.offset;
            fx.param_values[2].value = s.mode as f32;
            fx.param_values[3].value = s.angle;
            fx.param_values[4].value = s.falloff;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed = h.run_primitive_graph(
                Box::new(ChromaticOffset::new()),
                &input,
                &ctx,
                |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(s.amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "offset", ParamValue::Float(s.offset))
                        .unwrap();
                    graph
                        .set_param(prim_id, "mode", ParamValue::Enum(s.mode))
                        .unwrap();
                    graph
                        .set_param(prim_id, "angle", ParamValue::Float(s.angle))
                        .unwrap();
                    graph
                        .set_param(prim_id, "falloff", ParamValue::Float(s.falloff))
                        .unwrap();
                },
            );

            assert_bytewise_equal(
                &format!(
                    "chromatic_offset/{:?}/setup={}: legacy vs primitive.chromatic_offset",
                    fixture, s.label
                ),
                &legacy,
                &decomposed,
            );
        }
    }
}
