//! Pixel-exact parity test for `primitive.wireframe_depth` vs the
//! legacy `WireframeDepthFX` effect. §6.5 commit 3; monolithic
//! wrapper.
//!
//! WireframeDepth runs a 15-pass pipeline driven by a MiDaS depth
//! DNN worker plus an optional native optical-flow worker. On the
//! first frame neither worker has produced a result yet, so the
//! depth-texture and flow-texture fallbacks are exercised. That's
//! the single-frame parity slice; multi-frame DNN convergence is
//! identical legacy code in both paths.
//!
//! Unique `owner_key` per setup, same lesson as Watercolor / DoF /
//! AutoGain / BlobTracking.


use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::WireframeDepth;
use crate::harness::{self, Fixture, assert_bytewise_equal, default_ctx, make_default_effect};

#[derive(Debug, Clone, Copy)]
struct Setup {
    label: &'static str,
    amount: f32,
    density: f32,
    width: f32,
    z_scale: f32,
    smooth: f32,
    subject: f32,
    blend: u32,
    wire_res: f32,
    mesh_rate: u32,
    flow: u32,
    lock: u32,
    edge_follow: f32,
}

const DEFAULT: Setup = Setup {
    label: "default",
    amount: 1.0,
    density: 260.0,
    width: 1.335,
    z_scale: 1.35,
    smooth: 0.90,
    subject: 0.52,
    blend: 6,
    wire_res: 1.0,
    mesh_rate: 1,
    flow: 1,
    lock: 1,
    edge_follow: 0.5,
};

const SETUPS: &[Setup] = &[
    Setup {
        label: "identity",
        amount: 0.0,
        ..DEFAULT
    },
    DEFAULT,
    Setup {
        label: "low_density",
        density: 16.0,
        ..DEFAULT
    },
    Setup {
        label: "high_density",
        density: 280.0,
        ..DEFAULT
    },
    Setup {
        label: "wide_lines",
        width: 3.0,
        ..DEFAULT
    },
    Setup {
        label: "blend_add",
        blend: 1,
        ..DEFAULT
    },
    Setup {
        label: "blend_screen",
        blend: 3,
        ..DEFAULT
    },
    Setup {
        label: "flow_off",
        flow: 0,
        lock: 0,
        ..DEFAULT
    },
];

#[test]
fn wireframe_depth_is_pixel_exact_across_fixtures_and_setups() {
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

            let mut fx = make_default_effect(EffectTypeId::WIREFRAME_DEPTH);
            fx.param_values[0].value = s.amount;
            fx.param_values[1].value = s.density;
            fx.param_values[2].value = s.width;
            fx.param_values[3].value = s.z_scale;
            fx.param_values[4].value = s.smooth;
            fx.param_values[5].value = s.subject;
            fx.param_values[6].value = s.blend as f32;
            fx.param_values[7].value = s.wire_res;
            fx.param_values[8].value = s.mesh_rate as f32;
            fx.param_values[9].value = s.flow as f32;
            fx.param_values[10].value = s.lock as f32;
            fx.param_values[11].value = s.edge_follow;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let s_copy = *s;
            let decomposed = h.run_primitive_graph(
                Box::new(WireframeDepth::new()),
                &input,
                &ctx,
                |graph, prim_id| {
                    graph
                        .set_param(prim_id, "amount", ParamValue::Float(s_copy.amount))
                        .unwrap();
                    graph
                        .set_param(prim_id, "density", ParamValue::Float(s_copy.density))
                        .unwrap();
                    graph
                        .set_param(prim_id, "width", ParamValue::Float(s_copy.width))
                        .unwrap();
                    graph
                        .set_param(prim_id, "z_scale", ParamValue::Float(s_copy.z_scale))
                        .unwrap();
                    graph
                        .set_param(prim_id, "smooth", ParamValue::Float(s_copy.smooth))
                        .unwrap();
                    graph
                        .set_param(prim_id, "subject", ParamValue::Float(s_copy.subject))
                        .unwrap();
                    graph
                        .set_param(prim_id, "blend", ParamValue::Enum(s_copy.blend))
                        .unwrap();
                    graph
                        .set_param(prim_id, "wire_res", ParamValue::Float(s_copy.wire_res))
                        .unwrap();
                    graph
                        .set_param(prim_id, "mesh_rate", ParamValue::Enum(s_copy.mesh_rate))
                        .unwrap();
                    graph
                        .set_param(prim_id, "flow", ParamValue::Enum(s_copy.flow))
                        .unwrap();
                    graph
                        .set_param(prim_id, "lock", ParamValue::Enum(s_copy.lock))
                        .unwrap();
                    graph
                        .set_param(
                            prim_id,
                            "edge_follow",
                            ParamValue::Float(s_copy.edge_follow),
                        )
                        .unwrap();
                },
            );

            assert_bytewise_equal(
                &format!("wireframe_depth/{:?}/setup={}", fixture, s.label),
                &legacy,
                &decomposed,
            );
        }
    }
}
