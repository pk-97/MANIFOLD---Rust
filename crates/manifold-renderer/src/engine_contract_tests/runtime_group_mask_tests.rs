use manifold_core::PresetTypeId;
use manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id;
#[test]
fn blob_v2_invalid_inverted_mask_is_zero() {
    let view = loaded_preset_view_by_id(&PresetTypeId::new("MaskBlob"))
        .expect("MaskBlob preset is registered");
    let def = &view.canonical_def;
    let node = |name: &str| {
        def.nodes
            .iter()
            .find(|node| node.node_id.as_str() == name)
            .unwrap_or_else(|| panic!("MaskBlob missing {name}"))
    };
    let invert = node("invert");
    let valid_gate = node("valid_gate");
    let final_output = node("final_output");
    let reachable = |start: u32, target: u32| {
        let mut pending = vec![start];
        let mut visited = std::collections::BTreeSet::new();
        while let Some(current) = pending.pop() {
            if !visited.insert(current) {
                continue;
            }
            if current == target {
                return true;
            }
            pending.extend(
                def.wires
                    .iter()
                    .filter(|wire| wire.from_node == current)
                    .map(|wire| wire.to_node),
            );
        }
        false
    };
    assert!(
        reachable(invert.id, valid_gate.id),
        "validity must be applied after inversion"
    );
    assert!(
        reachable(valid_gate.id, final_output.id),
        "validity gate must feed the final mask output"
    );
    assert!(matches!(
        valid_gate.params.get("scale"),
        Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value })
            if value.abs() < f32::EPSILON
    ));
}
