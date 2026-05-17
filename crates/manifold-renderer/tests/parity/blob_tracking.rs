//! Pixel-exact parity test for `primitive.blob_tracking` vs the
//! legacy `BlobTrackingFX` effect. §6.5 commit 2; monolithic wrapper.
//!
//! BlobTracking spawns a background native plugin (via FFI) on
//! construction. On the first frame the detector has no results yet,
//! so the apply pass renders with an empty blob set — that's the
//! single-frame parity slice we exercise here. Multi-frame blob
//! tracking + One-Euro smoother behaviour is the same code in both
//! paths since the primitive delegates.
//!
//! If the native plugin isn't available in the test environment,
//! both paths still match because the construction failure mode is
//! identical (legacy's `BlobTrackingFX::new` returns a `worker: None`
//! and my primitive wraps the same `BlobTrackingFX::new`).
//!
//! Unique `owner_key` per setup, same lesson as Watercolor / DoF /
//! AutoGain.


use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::BlobTracking;
use crate::harness::{self, Fixture, assert_bytewise_equal, default_ctx, make_default_effect};

#[derive(Debug, Clone, Copy)]
struct Setup {
    label: &'static str,
    amount: f32,
    thresh: f32,
    sens: f32,
    smooth: f32,
    connect: f32,
}

const SETUPS: &[Setup] = &[
    Setup {
        label: "identity",
        amount: 0.0,
        thresh: 0.65,
        sens: 0.85,
        smooth: 0.7,
        connect: 0.35,
    },
    Setup {
        label: "default",
        amount: 1.0,
        thresh: 0.65,
        sens: 0.85,
        smooth: 0.7,
        connect: 0.35,
    },
    Setup {
        label: "low_thresh",
        amount: 1.0,
        thresh: 0.05,
        sens: 0.85,
        smooth: 0.7,
        connect: 0.35,
    },
    Setup {
        label: "high_thresh",
        amount: 1.0,
        thresh: 0.9,
        sens: 0.85,
        smooth: 0.7,
        connect: 0.35,
    },
    Setup {
        label: "max_connect",
        amount: 1.0,
        thresh: 0.65,
        sens: 1.0,
        smooth: 0.0,
        connect: 1.0,
    },
];

#[test]
fn blob_tracking_is_pixel_exact_across_fixtures_and_setups() {
    let h = harness::shared();
    let mut owner_key: i64 = 0;

    for &fixture in Fixture::all() {
        let input = fixture.build(h);

        for s in SETUPS {
            owner_key += 1;
            let ctx = manifold_renderer::effect::EffectContext {
                owner_key,
                ..default_ctx(h.width, h.height)
            };

            let mut fx = make_default_effect(EffectTypeId::BLOB_TRACKING);
            fx.param_values[0].value = s.amount;
            fx.param_values[1].value = s.thresh;
            fx.param_values[2].value = s.sens;
            fx.param_values[3].value = s.smooth;
            fx.param_values[4].value = s.connect;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed = h.run_primitive_graph(
                Box::new(BlobTracking::new()),
                &input,
                &ctx,
                |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(s.amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "threshold", ParamValue::Float(s.thresh))
                        .unwrap();
                    graph
                        .set_param(prim_id, "sensitivity", ParamValue::Float(s.sens))
                        .unwrap();
                    graph
                        .set_param(prim_id, "smoothing", ParamValue::Float(s.smooth))
                        .unwrap();
                    graph
                        .set_param(prim_id, "connect", ParamValue::Float(s.connect))
                        .unwrap();
                },
            );

            assert_bytewise_equal(
                &format!("blob_tracking/{:?}/setup={}", fixture, s.label),
                &legacy,
                &decomposed,
            );
        }
    }
}
