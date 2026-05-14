//! Pixel-exact parity test for `primitive.clamp_stretch` vs the
//! legacy `EdgeStretchFX` effect. Third §6.1 migration.
//!
//! Three params (amount, source_width, mode) × three modes
//! (Horiz/Vert/Both) × three source widths × four fixtures = a
//! tractable sweep that exercises every axis-clamp branch and
//! verifies the CPU-side source_width clamp ([0.1, 0.9]) matches
//! between paths.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::ClampStretch;
use parity::{Fixture, ParityHarness, assert_bytewise_equal, default_ctx, make_default_effect};

const MODES: &[(u32, &str)] = &[(0, "horiz"), (1, "vert"), (2, "both")];
const WIDTHS: &[f32] = &[0.2, 0.433, 0.8];

#[test]
fn clamp_stretch_is_pixel_exact_across_fixtures_modes_widths() {
    let mut h = ParityHarness::new();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for &(mode_u, mode_label) in MODES {
            for &width in WIDTHS {
                let mut fx = make_default_effect(EffectTypeId::EDGE_STRETCH);
                fx.param_values[0].value = 1.0;
                fx.param_values[1].value = width;
                fx.param_values[2].value = mode_u as f32;

                let legacy = h.run_legacy(&fx, &input, &ctx);
                let decomposed = h.run_primitive_graph(
                    Box::new(ClampStretch::new()),
                    &input,
                    &ctx,
                    |graph, prim_id| {
                        graph
                            .set_param(prim_id, "amount", ParamValue::Float(1.0))
                            .unwrap();
                        graph
                            .set_param(prim_id, "source_width", ParamValue::Float(width))
                            .unwrap();
                        graph
                            .set_param(prim_id, "mode", ParamValue::Enum(mode_u))
                            .unwrap();
                    },
                );

                assert_bytewise_equal(
                    &format!(
                        "clamp_stretch/{:?}/mode={mode_label}/width={width}",
                        fixture
                    ),
                    &legacy,
                    &decomposed,
                );
            }
        }
    }
}

/// Source-width values outside the declared [0.1, 0.9] range must be
/// clamped identically on both paths. Legacy clamps in the CPU before
/// uniform pack; the primitive must do the same.
#[test]
fn clamp_stretch_clamps_out_of_range_source_width() {
    let mut h = ParityHarness::new();
    let input = Fixture::Gradient.build(&h);
    let ctx = default_ctx(h.width, h.height);

    for &width in &[-0.5f32, 0.0, 0.05, 0.95, 1.5] {
        let mut fx = make_default_effect(EffectTypeId::EDGE_STRETCH);
        fx.param_values[0].value = 1.0;
        fx.param_values[1].value = width;
        fx.param_values[2].value = 2.0; // Both

        let legacy = h.run_legacy(&fx, &input, &ctx);
        let decomposed = h.run_primitive_graph(
            Box::new(ClampStretch::new()),
            &input,
            &ctx,
            |graph, prim_id| {
                graph
                    .set_param(prim_id, "amount", ParamValue::Float(1.0))
                    .unwrap();
                graph
                    .set_param(prim_id, "source_width", ParamValue::Float(width))
                    .unwrap();
                graph
                    .set_param(prim_id, "mode", ParamValue::Enum(2))
                    .unwrap();
            },
        );

        assert_bytewise_equal(
            &format!("clamp_stretch/oob-width={width}"),
            &legacy,
            &decomposed,
        );
    }
}
