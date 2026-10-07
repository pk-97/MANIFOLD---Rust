use crate::preset_runtime::*;
use crate::node_graph::*;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
#[test]
fn physics_carry_matches_owners_across_actual_fused_topology() {
    let mut def: EffectGraphDef = serde_json::from_str(include_str!(
        "../../../assets/generator-presets/WaterBasin.json"
    ))
    .unwrap();
    // A fusible image segment after the scene changes the execution plan,
    // while the native simulation and its CPU ancestry stay authored nodes.
    for (id, name) in [(400, "gain_a"), (401, "gain_b")] {
        def.nodes.push(
            serde_json::from_value(serde_json::json!({
                "id": id, "nodeId": name, "typeId": "node.exposure"
            }))
            .unwrap(),
        );
    }
    def.wires.retain(|wire| wire.to_node != 31);
    for (from_node, from_port, to_node, to_port) in [
        (30, "color", 400, "in"),
        (400, "out", 401, "in"),
        (401, "out", 31, "in"),
    ] {
        def.wires
            .push(manifold_core::effect_graph_def::EffectGraphWire {
                from_node,
                from_port: from_port.into(),
                to_node,
                to_port: to_port.into(),
            });
    }
    let registry = PrimitiveRegistry::with_builtin();
    let mut prior =
        PresetRuntime::from_def_for_render(def.clone(), &registry, None, false).unwrap();
    let mut fused = PresetRuntime::from_def_for_render(def, &registry, None, true).unwrap();
    assert!(
        fused.graph.nodes().count() < prior.graph.nodes().count(),
        "fixture must really fuse"
    );
    let owner = |runtime: &PresetRuntime| {
        let id = runtime
            .graph
            .instance_by_node_id(&NodeId::new("fluid_surface"))
            .unwrap();
        &*runtime.graph.get_node(id).unwrap().node as *const dyn EffectNode as *const ()
    };
    let native_owner = owner(&prior);
    assert_ne!(owner(&fused), native_owner);
    crate::preset_runtime::testkit::set_last_physics_frame_time(&mut prior, Some(FrameTime {
        seconds: Seconds(0.5),
        beats: Beats(1.0),
        delta: Seconds(1.0 / 30.0),
        frame_count: 15,
    }));
    fused.carry_generator_state_from(&mut prior);
    assert_eq!(owner(&fused), native_owner);
    assert_eq!(crate::preset_runtime::testkit::last_physics_frame_time(&fused).unwrap().seconds, Seconds(0.5));
}

use manifold_core::NodeId;
