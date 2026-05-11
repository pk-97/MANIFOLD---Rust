//! Pixel-exact parity test for `primitive.depth_of_field` vs the
//! legacy `DepthOfFieldFX` effect, restricted to **geometric** focus
//! modes (tilt-shift, radial). §6.4 commit 1.
//!
//! Depth mode (focus_mode = 2) is intentionally NOT parity-tested
//! here: it spawns a background MiDaS worker with multi-frame
//! inference latency. The first frame at depth mode has `has_depth =
//! false` so the CoC pass uses the source as a depth proxy — that
//! behaviour matches legacy bit-for-bit, but a single-frame parity
//! test would say very little. End-to-end depth-mode validation
//! belongs in an integration-test setup that runs the harness over
//! several frames, which is out of scope for §6.4.
//!
//! Per-setup `owner_key` is unique because the legacy effect keys
//! per-owner blur-buffer state in its registry. Same lesson as
//! Watercolor (§6.3 commit 4) — without unique owner_keys, the legacy
//! reuses stale state across iterations while the primitive starts
//! fresh, and parity drifts on iterations after the first.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::primitives::DepthOfField;
use manifold_renderer::node_graph::ParamValue;
use parity::{
    assert_bytewise_equal, default_ctx, make_default_effect, Fixture, ParityHarness,
};

#[derive(Debug, Clone, Copy)]
struct Setup {
    label: &'static str,
    amount: f32,
    mode: u32, // 0 = tilt-shift, 1 = radial (no depth)
    focus: f32,
    focus_x: f32,
    width: f32,
    blur: f32,
    angle: f32,
    quality: u32,
}

const SETUPS: &[Setup] = &[
    Setup { label: "identity",          amount: 0.0, mode: 0, focus: 0.5, focus_x: 0.5, width: 0.15, blur: 0.5, angle: 0.0,  quality: 1 },
    Setup { label: "tilt_default",      amount: 1.0, mode: 0, focus: 0.5, focus_x: 0.5, width: 0.15, blur: 0.5, angle: 0.0,  quality: 1 },
    Setup { label: "tilt_rotated",      amount: 1.0, mode: 0, focus: 0.5, focus_x: 0.5, width: 0.15, blur: 0.5, angle: 45.0, quality: 1 },
    Setup { label: "tilt_wide",         amount: 1.0, mode: 0, focus: 0.3, focus_x: 0.5, width: 0.40, blur: 0.8, angle: 0.0,  quality: 2 },
    Setup { label: "tilt_low_quality",  amount: 1.0, mode: 0, focus: 0.5, focus_x: 0.5, width: 0.15, blur: 0.5, angle: 0.0,  quality: 0 },
    Setup { label: "radial_default",    amount: 1.0, mode: 1, focus: 0.5, focus_x: 0.5, width: 0.15, blur: 0.5, angle: 0.0,  quality: 1 },
    Setup { label: "radial_offset",     amount: 1.0, mode: 1, focus: 0.7, focus_x: 0.3, width: 0.20, blur: 0.6, angle: 0.0,  quality: 1 },
    Setup { label: "radial_max_blur",   amount: 1.0, mode: 1, focus: 0.5, focus_x: 0.5, width: 0.10, blur: 1.0, angle: 0.0,  quality: 2 },
];

#[test]
fn depth_of_field_geometric_is_pixel_exact_across_fixtures_and_setups() {
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

            let mut fx = make_default_effect(EffectTypeId::DEPTH_OF_FIELD);
            fx.param_values[0].value = s.amount;
            fx.param_values[1].value = s.mode as f32;
            fx.param_values[2].value = s.focus;
            fx.param_values[3].value = s.focus_x;
            fx.param_values[4].value = s.width;
            fx.param_values[5].value = s.blur;
            fx.param_values[6].value = s.angle;
            fx.param_values[7].value = s.quality as f32;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed = h.run_primitive_graph(
                Box::new(DepthOfField::new()),
                &input,
                &ctx,
                |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(s.amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "mode", ParamValue::Enum(s.mode))
                        .unwrap();
                    graph
                        .set_param(prim_id, "focus", ParamValue::Float(s.focus))
                        .unwrap();
                    graph
                        .set_param(prim_id, "focus_x", ParamValue::Float(s.focus_x))
                        .unwrap();
                    graph
                        .set_param(prim_id, "width", ParamValue::Float(s.width))
                        .unwrap();
                    graph
                        .set_param(prim_id, "blur", ParamValue::Float(s.blur))
                        .unwrap();
                    graph
                        .set_param(prim_id, "angle", ParamValue::Float(s.angle))
                        .unwrap();
                    graph
                        .set_param(prim_id, "quality", ParamValue::Enum(s.quality))
                        .unwrap();
                },
            );

            assert_bytewise_equal(
                &format!("depth_of_field/{:?}/setup={}", fixture, s.label),
                &legacy,
                &decomposed,
            );
        }
    }
}
