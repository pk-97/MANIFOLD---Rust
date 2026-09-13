//! Additive native controls for the Vortex Fragments modifier.

use crate::NodeId;
use crate::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, ParamSpecDef, SerializedParamValue,
};
use crate::effects::ParamConvert;

pub const CONTROL_PREFIX: &str = "math_view_";
pub const CONTROLS: &[(&str, &str, f32, f32, f32)] = &[
    ("mode", "Mode", 0.0, 0.0, 2.0),
    ("grid", "Grid", 1.0, 0.0, 1.0),
    ("fragments", "Fragments", 1.0, 0.0, 1.0),
    ("ghosts", "Ghosts", 1.0, 0.0, 1.0),
    ("vectors", "Vectors", 1.0, 0.0, 1.0),
    ("trails", "Motion trails", 1.0, 0.0, 1.0),
    ("density", "Density", 3.0, 2.0, 8.0),
    ("line_width", "Line Width", 1.0, 0.5, 4.0),
    ("geometry_hue", "Geometry Hue", 0.52, 0.0, 1.0),
    ("path_hue", "Path Hue", 0.13, 0.0, 1.0),
    ("scope", "Scope", 1.0, 0.0, 1.0),
];
fn local_id(suffix: &str) -> String {
    format!("{CONTROL_PREFIX}{suffix}")
}
fn control_node_id(suffix: &str) -> NodeId {
    NodeId::new(format!("__math_view_{suffix}"))
}

fn visit(nodes: &[EffectGraphNode], f: &mut impl FnMut(&EffectGraphNode)) {
    for node in nodes {
        f(node);
        if let Some(group) = &node.group {
            visit(&group.nodes, f);
        }
    }
}

fn eligible(graph: &EffectGraphDef) -> bool {
    let Some(metadata) = &graph.preset_metadata else {
        return false;
    };
    if metadata.id.as_str() != "VortexFragments" {
        return false;
    }
    metadata.bindings.iter().any(|binding| {
        let BindingTarget::Node { node_id, param } = &binding.target else {
            return false;
        };
        if binding.id != "orbit" || param != "orbit" {
            return false;
        }
        let mut found = false;
        visit(&graph.nodes, &mut |node| {
            found |= &node.node_id == node_id && node.type_id == "node.transform_mesh_patches";
        });
        found
    })
}

// Shared controls must be root nodes; nested reserved IDs cannot satisfy
// the compiler's shared route.
fn control_present(graph: &EffectGraphDef, suffix: &str) -> bool {
    let Some(metadata) = &graph.preset_metadata else {
        return false;
    };
    let id = local_id(suffix);
    let nid = control_node_id(suffix);
    let mut matches = 0;
    visit(&graph.nodes, &mut |node| {
        matches += usize::from(node.node_id == nid)
    });
    metadata.params.iter().filter(|param| param.id == id).count() == 1
        && metadata.bindings.iter().filter(|binding| binding.id == id).count() == 1
        && metadata.bindings.iter().any(|binding| binding.id == id
            && binding.convert == ParamConvert::Float
            && matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &nid && param == "value"))
        && matches == 1
        && graph.nodes.iter().any(|node| node.node_id == nid && node.type_id == "node.value"
            && matches!(node.params.get("value"), Some(SerializedParamValue::Float { .. })))
}

pub fn has_math_view_controls(graph: &EffectGraphDef) -> bool {
    eligible(graph)
        && CONTROLS
            .iter()
            .all(|(suffix, ..)| control_present(graph, suffix))
}

/// Add controls without changing authored parameters. Conflicts and capacity
/// errors leave the graph untouched; repeated enrichment is a no-op.
pub fn enrich_math_view_controls(graph: &mut EffectGraphDef) -> Result<bool, String> {
    if !eligible(graph) {
        return Ok(false);
    }
    let metadata = graph.preset_metadata.as_ref().expect("eligible metadata");
    let mut missing = Vec::new();
    for control in CONTROLS {
        let suffix = control.0;
        if control_present(graph, suffix) {
            continue;
        }
        let id = local_id(suffix);
        let nid = control_node_id(suffix);
        let mut reserved = false;
        visit(&graph.nodes, &mut |node| reserved |= node.node_id == nid);
        if reserved
            || metadata.params.iter().any(|param| param.id == id)
            || metadata.bindings.iter().any(|binding| binding.id == id)
        {
            return Err(format!("reserved Math View id conflicts: {id}"));
        }
        missing.push(control);
    }
    if missing.is_empty() {
        return Ok(false);
    }
    let mut next = graph.nodes.iter().map(|node| node.id).max().unwrap_or(0);
    next.checked_add(missing.len() as u32)
        .ok_or("Math View node id space exhausted")?;
    let metadata = graph.preset_metadata.as_mut().expect("eligible metadata");
    for (suffix, label, default, min, max) in missing {
        next += 1; // Capacity checked before mutation.
        let id = local_id(suffix);
        metadata.params.push(ParamSpecDef {
            id: id.clone(),
            name: (*label).into(),
            min: *min,
            max: *max,
            default_value: *default,
            whole_numbers: matches!(*suffix, "mode" | "scope" | "density"),
            is_toggle: matches!(
                *suffix,
                "grid" | "fragments" | "ghosts" | "vectors" | "trails"
            ),
            value_labels: match *suffix {
                "mode" => vec!["Scene".into(), "Math".into(), "Overlay".into()],
                "scope" => vec!["This modifier".into(), "Within chain".into()],
                _ => vec![],
            },
            section: Some("Math View".into()),
            card_visible: true,
            ..Default::default()
        });
        metadata.bindings.push(BindingDef {
            id: id.clone(),
            label: (*label).into(),
            default_value: *default,
            target: BindingTarget::Node {
                node_id: control_node_id(suffix),
                param: "value".into(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        });
        graph.nodes.push(EffectGraphNode {
            id: next,
            node_id: control_node_id(suffix),
            type_id: "node.value".into(),
            handle: Some(id),
            params: [(
                "value".into(),
                SerializedParamValue::Float { value: *default },
            )]
            .into(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        });
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata":{"id":"VortexFragments","displayName":"Vortex","category":"Geometry","oscPrefix":"vortex","params":[],"bindings":[{"id":"orbit","label":"Orbit","defaultValue":1,"target":{"kind":"node","nodeId":"patch","param":"orbit"}}]},
            "nodes":[{"id":1,"nodeId":"stage","typeId":"group","group":{"interface":{"inputs":[],"outputs":[]},"nodes":[{"id":2,"nodeId":"patch","typeId":"node.transform_mesh_patches","params":{"orbit":{"type":"Float","value":1}}}],"wires":[]}}],"wires":[]
        })).unwrap()
    }
    #[test]
    fn nested_math_view_enrichment_round_trips_and_is_idempotent() {
        let mut graph = fixture();
        assert!(enrich_math_view_controls(&mut graph).unwrap());
        assert!(has_math_view_controls(&graph));
        let once = graph.clone();
        assert!(!enrich_math_view_controls(&mut graph).unwrap());
        assert_eq!(graph, once);
        let decoded: EffectGraphDef =
            serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
        assert_eq!(decoded, graph);
        assert!(
            graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .params
                .iter()
                .all(|p| p.section.as_deref() == Some("Math View"))
        );
    }
    #[test]
    fn math_view_enrichment_rejects_reserved_ids_and_overflow_atomically() {
        for wrong_type in [true, false] {
            let mut graph = fixture();
            if wrong_type {
                graph.nodes[0].node_id = control_node_id("mode");
            } else {
                graph.nodes[0].id = u32::MAX;
            }
            let before = graph.clone();
            assert!(enrich_math_view_controls(&mut graph).is_err());
            assert_eq!(graph, before);
        }
    }
}
