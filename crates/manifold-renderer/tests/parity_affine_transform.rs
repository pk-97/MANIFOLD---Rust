//! Pixel-exact parity test for `node.affine_transform` vs the
//! legacy `TransformFX` effect. Fifth §6.1 migration.
//!
//! The primitive accepts rotation in radians; the legacy effect
//! accepts it in degrees and applies a Y-down sign-flip in the CPU
//! before packing the uniform. The parity test does the same
//! conversion (`deg * -PI / 180`) at the primitive's parameter
//! boundary, mirroring what a future Transform preset graph will do.
//!
//! `is_clip_level = false` for all parity runs — the legacy
//! TransformFX::apply branches to identity uniforms when clip-level;
//! that's an effect-UX concern, not a primitive concern.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::primitives::AffineTransform;
use manifold_renderer::node_graph::ParamValue;
use parity::{
    assert_bytewise_equal, default_ctx, make_default_effect, Fixture, ParityHarness,
};

const DEG2RAD: f32 = std::f32::consts::PI / 180.0;

/// One transform preset. Field naming matches `TransformFX::apply`.
#[derive(Debug, Clone, Copy)]
struct Xform {
    label: &'static str,
    x: f32,
    y: f32,
    zoom: f32,
    rot_deg: f32,
}

const XFORMS: &[Xform] = &[
    Xform { label: "identity", x: 0.0, y: 0.0, zoom: 1.0, rot_deg: 0.0 },
    Xform { label: "translate", x: 0.2, y: -0.15, zoom: 1.0, rot_deg: 0.0 },
    Xform { label: "zoom_in", x: 0.0, y: 0.0, zoom: 2.0, rot_deg: 0.0 },
    Xform { label: "zoom_out", x: 0.0, y: 0.0, zoom: 0.5, rot_deg: 0.0 },
    Xform { label: "rotate_30", x: 0.0, y: 0.0, zoom: 1.0, rot_deg: 30.0 },
    Xform { label: "rotate_neg_90", x: 0.0, y: 0.0, zoom: 1.0, rot_deg: -90.0 },
    Xform { label: "combo", x: 0.1, y: 0.1, zoom: 1.3, rot_deg: 45.0 },
];

#[test]
fn affine_transform_is_pixel_exact_across_fixtures_and_xforms() {
    let mut h = ParityHarness::new();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for xf in XFORMS {
            let mut fx = make_default_effect(EffectTypeId::TRANSFORM);
            fx.param_values[0].value = xf.x;
            fx.param_values[1].value = xf.y;
            fx.param_values[2].value = xf.zoom;
            fx.param_values[3].value = xf.rot_deg;

            // Mirror the legacy CPU conversion: deg → -rad. The Transform
            // preset graph will do the same at its parameter boundary.
            let rotation_rad = -(xf.rot_deg * DEG2RAD);

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed = h.run_primitive_graph(
                Box::new(AffineTransform::new()),
                &input,
                &ctx,
                |graph, prim_id| {
                    graph
                        .set_param(prim_id, "translate_x", ParamValue::Float(xf.x))
                        .unwrap();
                    graph
                        .set_param(prim_id, "translate_y", ParamValue::Float(xf.y))
                        .unwrap();
                    graph
                        .set_param(prim_id, "scale", ParamValue::Float(xf.zoom))
                        .unwrap();
                    graph
                        .set_param(prim_id, "rotation", ParamValue::Float(rotation_rad))
                        .unwrap();
                },
            );

            assert_bytewise_equal(
                &format!(
                    "affine_transform/{:?}/xform={}: legacy vs node.affine_transform",
                    fixture, xf.label
                ),
                &legacy,
                &decomposed,
            );
        }
    }
}
