mod tests {
use manifold_node_engine::preview_encoding::PreviewEncoding;
use manifold_core::{NodeId, effect_graph_def::{EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire}};
use std::collections::{BTreeMap,BTreeSet};
    #[test]
    fn field_node_descriptor_picks_vector() {
        // gradient_central_diff / rotate_vec2_by_angle are FieldsAndCoordinates.
        assert_eq!(
            PreviewEncoding::derive("node.edge_slope", "out"),
            PreviewEncoding::VectorField
        );
        assert_eq!(
            PreviewEncoding::derive("node.rotate_vector", "out"),
            PreviewEncoding::VectorField
        );
    }

    fn node(id: u32, node_id: &str, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(node_id),
            type_id: type_id.to_string(),
            handle: None,
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire {
            from_node,
            from_port: from_port.to_string(),
            to_node,
            to_port: to_port.to_string(),
        }
    }

    fn def(nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphDef {
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes,
            wires,
        }
    }

    #[test]
    fn blur_inherits_field_kind_through_propagation() {
        // field_gen (vector) -> blur (transparent) -> blur2 (transparent).
        // Selecting either blur should preview as a vector field.
        let d = def(
            vec![
                node(0, "field", "node.edge_slope"),
                node(1, "blur", "node.gaussian_blur"),
                node(2, "blur2", "node.gaussian_blur"),
            ],
            vec![wire(0, "out", 1, "src"), wire(1, "out", 2, "src")],
        );
        let kinds = PreviewEncoding::propagate(&d);
        assert_eq!(kinds[&NodeId::new("field")], PreviewEncoding::VectorField);
        assert_eq!(
            kinds[&NodeId::new("blur")],
            PreviewEncoding::VectorField,
            "blur should inherit the field kind, not assert Color"
        );
        assert_eq!(
            kinds[&NodeId::new("blur2")],
            PreviewEncoding::VectorField,
            "kind propagates through a chain of filters"
        );
    }
}
