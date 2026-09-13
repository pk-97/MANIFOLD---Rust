//! Immutable production Apply outputs exercise lossless source adoption.
use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_renderer::node_graph::{
    PrimitiveRegistry, scene_modifier_legacy_migration::migrate_legacy_scene_modifiers,
};

const LOOP: &str = include_str!("fixtures/scene-modifiers/scene_loop_applied_v2.json");
const FOG: &str = include_str!("fixtures/scene-modifiers/scene_fog_applied_v2.json");
const BOTH: &str = include_str!("fixtures/scene-modifiers/scene_loop_scene_fog_applied_v2.json");

fn find<'a>(nodes: &'a [EffectGraphNode], id: &str) -> Option<&'a EffectGraphNode> {
    for n in nodes {
        if n.node_id.as_str() == id {
            return Some(n);
        }
        if let Some(group) = &n.group && let Some(found) = find(&group.nodes, id) {
            return Some(found);
        }
    }
    None
}

#[test]
fn complete_sources_adopt_once_and_preserve_original_parameter_addresses() {
    let registry = PrimitiveRegistry::with_builtin();
    for (text, count) in [(LOOP, 1), (FOG, 1), (BOTH, 2)] {
        let mut graph: EffectGraphDef = serde_json::from_str(text).unwrap();
        let before = graph.clone();
        let report = migrate_legacy_scene_modifiers(&mut graph, &registry);
        assert!(report.changed, "{:?}", report.diagnostics);
        assert!(report.diagnostics.is_empty());
        assert_eq!(graph.scene_modifiers.len(), count);
        let old = before.preset_metadata.as_ref().unwrap();
        let new = graph.preset_metadata.as_ref().unwrap();
        for spec in &old.params {
            assert!(new.params.contains(spec), "lost manifest {}", spec.id);
        }
        for binding in &old.bindings {
            let BindingTarget::Node { node_id, param } = &binding.target else {
                panic!("fixture binding");
            };
            if let Some(local) = graph
                .scene_modifiers
                .iter()
                .find(|i| find(&i.graph.nodes, node_id.as_str()).is_some())
            {
                assert!(
                    local
                        .graph
                        .preset_metadata
                        .as_ref()
                        .unwrap()
                        .bindings
                        .contains(binding),
                    "lost binding {node_id}.{param}"
                );
                assert!(new.bindings.iter().any(|b| b.id==binding.id&&matches!(&b.target,BindingTarget::SceneModifier {modifier_id,param_id} if *modifier_id==local.id&&*param_id==binding.id)));
            } else {
                assert!(new.bindings.contains(binding));
            }
        }
        for instance in &graph.scene_modifiers {
            for original in &before.nodes {
                if let Some(local) = find(&instance.graph.nodes, original.node_id.as_str()) {
                    assert_eq!(local, original, "migration must extract actual nodes");
                }
            }
        }
        let saved = serde_json::to_string(&graph).unwrap();
        let reloaded: EffectGraphDef = serde_json::from_str(&saved).unwrap();
        assert_eq!(graph, reloaded);
        assert!(!migrate_legacy_scene_modifiers(&mut graph, &registry).changed);
        assert_eq!(graph, reloaded);
    }
}

#[test]
fn loop_preserves_camera_off_and_nondefault_corridor_values() {
    let mut graph: EffectGraphDef = serde_json::from_str(LOOP).unwrap();
    for n in &mut graph.nodes {
        match n.node_id.as_str() {
            "loop_cam_switch" => {
                n.params
                    .insert("select".into(), SerializedParamValue::Enum { value: 0 });
            }
            "loop_camera" => {
                n.params
                    .insert("home".into(), SerializedParamValue::Float { value: -9.25 });
                n.handle = Some("My travel lens".into());
            }
            "loop_phase" => {
                n.params
                    .insert("bars".into(), SerializedParamValue::Float { value: 3.5 });
            }
            _ => {}
        }
    }
    let report = migrate_legacy_scene_modifiers(&mut graph, &PrimitiveRegistry::with_builtin());
    assert!(report.changed, "{:?}", report.diagnostics);
    let local = &graph.scene_modifiers[0].graph;
    assert_eq!(
        find(&local.nodes, "loop_cam_switch").unwrap().params["select"],
        SerializedParamValue::Enum { value: 0 }
    );
    assert_eq!(
        find(&local.nodes, "loop_camera").unwrap().params["home"],
        SerializedParamValue::Float { value: -9.25 }
    );
    assert_eq!(
        find(&local.nodes, "loop_camera").unwrap().handle.as_deref(),
        Some("My travel lens")
    );
    let meta = local.preset_metadata.as_ref().unwrap();
    let enabled = &meta.scene_modifier.as_ref().unwrap().enabled_param;
    assert_eq!(
        meta.params
            .iter()
            .find(|p| p.id == *enabled)
            .unwrap()
            .default_value,
        0.0
    );
    assert!(
        find(&local.nodes, "scene_array").is_some(),
        "camera off retains corridor instances"
    );
}

#[test]
fn incomplete_or_custom_source_ownership_preserves_entire_graph() {
    let registry = PrimitiveRegistry::with_builtin();
    for kind in 0..3 {
        let mut graph: EffectGraphDef = serde_json::from_str(BOTH).unwrap();
        match kind {
            0 => graph
                .nodes
                .retain(|n| n.node_id.as_str() != "loop_cam_switch"),
            1 => {
                let fog = graph
                    .nodes
                    .iter()
                    .find(|n| n.node_id.as_str() == "fog_atm")
                    .unwrap()
                    .id;
                graph.wires.push(EffectGraphWire {
                    from_node: fog,
                    from_port: "out".into(),
                    to_node: 31,
                    to_port: "custom".into(),
                });
            }
            _ => {
                let camera = graph
                    .nodes
                    .iter()
                    .find(|n| n.node_id.as_str() == "loop_camera")
                    .unwrap()
                    .id;
                graph.wires.push(EffectGraphWire {
                    from_node: 31,
                    from_port: "out".into(),
                    to_node: camera,
                    to_port: "custom".into(),
                });
            }
        }
        let before = graph.clone();
        let report = migrate_legacy_scene_modifiers(&mut graph, &registry);
        assert!(!report.changed);
        assert!(!report.diagnostics.is_empty());
        assert_eq!(
            graph, before,
            "must not adopt Fog separately from an ambiguous legacy graph"
        );
    }
}
