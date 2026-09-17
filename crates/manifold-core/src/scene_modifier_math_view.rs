//! Additive native controls for the Vortex Fragments modifier.

use crate::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, ParamSpecDef, SerializedParamValue,
};
use crate::effects::ParamConvert;
use crate::NodeId;

pub const CONTROL_PREFIX: &str = "math_view_";
pub const CONTROLS: &[(&str, &str, f32, f32, f32)] = &[
    ("mode", "Mode", 0.0, 0.0, 2.0),
    ("occlusion", "Occlusion", 0.0, 0.0, 1.0),
    ("grid", "Grid", 1.0, 0.0, 1.0),
    ("fragments", "Fragments", 1.0, 0.0, 1.0),
    ("ghosts", "Ghosts", 1.0, 0.0, 1.0),
    ("vectors", "Vectors", 1.0, 0.0, 1.0),
    ("trails", "Motion trails", 1.0, 0.0, 1.0),
    ("grid_brightness", "Grid Brightness", 1.0, 0.0, 2.0),
    (
        "fragments_brightness",
        "Fragments Brightness",
        1.0,
        0.0,
        2.0,
    ),
    ("ghosts_brightness", "Ghosts Brightness", 1.0, 0.0, 2.0),
    ("vectors_brightness", "Vectors Brightness", 1.0, 0.0, 2.0),
    ("trails_brightness", "Trails Brightness", 1.0, 0.0, 2.0),
    ("pulse", "Pulse", 0.0, 0.0, 1.0),
    ("pulse_strength", "Pulse Strength", 1.0, -1.0, 3.0),
    ("pulse_target", "Pulse Target", 0.0, 0.0, 5.0),
    ("pulse_beats", "Pulse Beats", 1.0, 0.0625, 32.0),
    ("pulse_trigger", "Pulse Trigger", 0.0, 0.0, 1.0),
    ("scan_amount", "Scan Amount", 0.0, 0.0, 1.0),
    ("scan_progress", "Scan Progress", 0.0, 0.0, 1.0),
    ("scan_width", "Scan Width", 0.2, 0.01, 2.0),
    ("scan_direction", "Scan Direction", 2.0, 0.0, 5.0),
    ("scan_mode", "Scan Mode", 0.0, 0.0, 1.0),
    ("scan_target", "Scan Target", 0.0, 0.0, 5.0),
    ("scan_beats", "Scan Beats", 4.0, 0.0625, 32.0),
    ("scan_trigger", "Scan Trigger", 0.0, 0.0, 1.0),
    ("connect_mesh", "Connect to Mesh", 0.0, 0.0, 1.0),
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

fn whole_numbers(suffix: &str) -> bool {
    matches!(
        suffix,
        "mode"
            | "occlusion"
            | "scope"
            | "density"
            | "pulse_target"
            | "scan_direction"
            | "scan_mode"
            | "scan_target"
    )
}

fn is_toggle(suffix: &str) -> bool {
    matches!(
        suffix,
        "grid"
            | "fragments"
            | "ghosts"
            | "vectors"
            | "trails"
            | "pulse_trigger"
            | "scan_trigger"
            | "connect_mesh"
    )
}

fn is_trigger_gate(suffix: &str) -> bool {
    matches!(suffix, "pulse_trigger" | "scan_trigger")
}

fn value_labels(suffix: &str) -> Vec<String> {
    match suffix {
        "mode" => vec!["Scene".into(), "Math".into(), "Overlay".into()],
        "occlusion" => vec!["X-ray".into(), "Depth".into()],
        "scope" => vec!["This modifier".into(), "Within chain".into()],
        "pulse_target" | "scan_target" => {
            ["All", "Grid", "Fragments", "Ghosts", "Vectors", "Trails"]
                .into_iter()
                .map(Into::into)
                .collect()
        }
        "scan_direction" => ["+X", "-X", "+Y", "-Y", "+Z", "-Z"]
            .into_iter()
            .map(Into::into)
            .collect(),
        "scan_mode" => vec!["Highlight".into(), "Reveal".into()],
        _ => vec![],
    }
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
            whole_numbers: whole_numbers(suffix),
            is_toggle: is_toggle(suffix),
            is_trigger_gate: is_trigger_gate(suffix),
            value_labels: value_labels(suffix),
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
        assert!(graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .all(|p| p.section.as_deref() == Some("Math View")));
    }

    #[test]
    fn new_math_view_controls_have_neutral_defaults_and_metadata() {
        let mut graph = fixture();
        enrich_math_view_controls(&mut graph).unwrap();
        let metadata = graph.preset_metadata.as_ref().unwrap();
        let expected = [
            ("occlusion", 0.0, 0.0, 1.0),
            ("grid_brightness", 1.0, 0.0, 2.0),
            ("fragments_brightness", 1.0, 0.0, 2.0),
            ("ghosts_brightness", 1.0, 0.0, 2.0),
            ("vectors_brightness", 1.0, 0.0, 2.0),
            ("trails_brightness", 1.0, 0.0, 2.0),
            ("pulse", 0.0, 0.0, 1.0),
            ("pulse_strength", 1.0, -1.0, 3.0),
            ("pulse_target", 0.0, 0.0, 5.0),
            ("pulse_beats", 1.0, 0.0625, 32.0),
            ("pulse_trigger", 0.0, 0.0, 1.0),
            ("scan_amount", 0.0, 0.0, 1.0),
            ("scan_progress", 0.0, 0.0, 1.0),
            ("scan_width", 0.2, 0.01, 2.0),
            ("scan_direction", 2.0, 0.0, 5.0),
            ("scan_mode", 0.0, 0.0, 1.0),
            ("scan_target", 0.0, 0.0, 5.0),
            ("scan_beats", 4.0, 0.0625, 32.0),
            ("scan_trigger", 0.0, 0.0, 1.0),
            ("connect_mesh", 0.0, 0.0, 1.0),
        ];
        for (suffix, default, min, max) in expected {
            let id = format!("{CONTROL_PREFIX}{suffix}");
            let spec = metadata.params.iter().find(|param| param.id == id).unwrap();
            assert_eq!(
                (spec.default_value, spec.min, spec.max),
                (default, min, max)
            );
            assert_eq!(spec.section.as_deref(), Some("Math View"));
            assert_eq!(
                spec.is_toggle,
                matches!(suffix, "pulse_trigger" | "scan_trigger" | "connect_mesh")
            );
            assert_eq!(
                spec.is_trigger_gate,
                matches!(suffix, "pulse_trigger" | "scan_trigger")
            );
        }
        let pulse_target = metadata
            .params
            .iter()
            .find(|param| param.id == "math_view_pulse_target")
            .unwrap();
        assert_eq!(
            pulse_target.value_labels,
            ["All", "Grid", "Fragments", "Ghosts", "Vectors", "Trails"]
        );
        let direction = metadata
            .params
            .iter()
            .find(|param| param.id == "math_view_scan_direction")
            .unwrap();
        assert_eq!(direction.value_labels, ["+X", "-X", "+Y", "-Y", "+Z", "-Z"]);
        let mode = metadata
            .params
            .iter()
            .find(|param| param.id == "math_view_scan_mode")
            .unwrap();
        assert_eq!(mode.value_labels, ["Highlight", "Reveal"]);
    }

    #[test]
    fn occlusion_depth_override_survives_serialization() {
        let mut graph = fixture();
        enrich_math_view_controls(&mut graph).unwrap();
        let id = format!("{CONTROL_PREFIX}occlusion");
        let spec = graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .find(|param| param.id == id)
            .unwrap();
        assert!(spec.whole_numbers);
        assert_eq!(spec.value_labels, ["X-ray", "Depth"]);
        graph
            .preset_metadata
            .as_mut()
            .unwrap()
            .params
            .iter_mut()
            .find(|param| param.id == id)
            .unwrap()
            .default_value = 1.0;
        graph
            .preset_metadata
            .as_mut()
            .unwrap()
            .bindings
            .iter_mut()
            .find(|binding| binding.id == id)
            .unwrap()
            .default_value = 1.0;
        graph
            .nodes
            .iter_mut()
            .find(|node| node.node_id == control_node_id("occlusion"))
            .unwrap()
            .params
            .insert("value".into(), SerializedParamValue::Float { value: 1.0 });

        let decoded: EffectGraphDef =
            serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
        let metadata = decoded.preset_metadata.as_ref().unwrap();
        assert_eq!(
            metadata
                .params
                .iter()
                .find(|param| param.id == id)
                .unwrap()
                .default_value,
            1.0
        );
        assert_eq!(
            metadata
                .bindings
                .iter()
                .find(|binding| binding.id == id)
                .unwrap()
                .default_value,
            1.0
        );
        assert_eq!(
            decoded
                .nodes
                .iter()
                .find(|node| node.node_id == control_node_id("occlusion"))
                .unwrap()
                .params
                .get("value"),
            Some(&SerializedParamValue::Float { value: 1.0 })
        );
    }

    #[test]
    fn old_enriched_graph_keeps_overrides_when_new_controls_are_added() {
        let mut graph = fixture();
        enrich_math_view_controls(&mut graph).unwrap();
        let old_suffixes = [
            "mode",
            "grid",
            "fragments",
            "ghosts",
            "vectors",
            "trails",
            "density",
            "line_width",
            "geometry_hue",
            "path_hue",
            "scope",
        ];
        let is_old = |id: &str| {
            old_suffixes
                .iter()
                .any(|suffix| id == format!("{CONTROL_PREFIX}{suffix}"))
        };
        let metadata = graph.preset_metadata.as_mut().unwrap();
        metadata.params.retain(|param| is_old(&param.id));
        metadata
            .bindings
            .retain(|binding| is_old(&binding.id) || binding.id == "orbit");
        graph.nodes.retain(|node| {
            node.node_id == "stage"
                || old_suffixes
                    .iter()
                    .any(|suffix| node.node_id == control_node_id(suffix))
        });

        let grid_id = format!("{CONTROL_PREFIX}grid");
        graph
            .preset_metadata
            .as_mut()
            .unwrap()
            .params
            .iter_mut()
            .find(|param| param.id == grid_id)
            .unwrap()
            .default_value = 0.25;
        graph
            .preset_metadata
            .as_mut()
            .unwrap()
            .bindings
            .iter_mut()
            .find(|binding| binding.id == grid_id)
            .unwrap()
            .default_value = 0.25;
        graph
            .nodes
            .iter_mut()
            .find(|node| node.node_id == control_node_id("grid"))
            .unwrap()
            .params
            .insert("value".into(), SerializedParamValue::Float { value: 0.25 });

        assert!(enrich_math_view_controls(&mut graph).unwrap());
        let metadata = graph.preset_metadata.as_ref().unwrap();
        assert_eq!(
            metadata
                .params
                .iter()
                .find(|param| param.id == grid_id)
                .unwrap()
                .default_value,
            0.25
        );
        assert_eq!(
            metadata
                .bindings
                .iter()
                .find(|binding| binding.id == grid_id)
                .unwrap()
                .default_value,
            0.25
        );
        assert_eq!(
            graph
                .nodes
                .iter()
                .find(|node| node.node_id == control_node_id("grid"))
                .unwrap()
                .params
                .get("value"),
            Some(&SerializedParamValue::Float { value: 0.25 })
        );
        assert_eq!(
            metadata
                .params
                .iter()
                .find(|param| param.id == "math_view_scan_width")
                .unwrap()
                .default_value,
            0.2
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
