//! Camera effect construction shared by native scene preparation and import.
//! Existing custom chains are preserved. New plumbing is inserted before any
//! authored post effects, with both effects off so preparation preserves the look.

use super::cinematic_tail::build_cinematic_tail;
use super::scene_graph::{float, plain_node, wire};
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode};
use std::collections::HashSet;

const RENDER: &str = "node.render_scene";
const LENS: &str = "node.camera_lens";
const TAIL: &[&str] = &[
    "node.coc_from_depth",
    "node.bokeh_gather",
    "node.motion_blur",
];

/// Add the standard camera effects to an unambiguous bare scene. This is a
/// cold authoring/load operation, never a render-time repair. The caller owns
/// committing the graph and refreshing its parameter manifest.
///
/// Existing effect chains (including partial/custom ones) are not rewritten.
/// Missing/ambiguous camera or colour wiring is reported without any mutation.
pub fn prepare_camera_effects(def: &mut EffectGraphDef) -> Result<bool, String> {
    let mut all = Vec::new();
    collect_nodes(&def.nodes, &mut all);
    if !all.iter().any(|node| node.type_id == RENDER)
        || all.iter().any(|node| TAIL.contains(&node.type_id.as_str()))
    {
        return Ok(false);
    }
    let scenes: Vec<_> = def
        .nodes
        .iter()
        .filter(|node| node.type_id == RENDER)
        .collect();
    let [scene] = scenes.as_slice() else {
        return Err(
            "Camera setup needs one scene at the top level; existing scene wiring was preserved."
                .into(),
        );
    };
    let render_id = scene.id;
    let camera_wires: Vec<_> = def
        .wires
        .iter()
        .filter(|wire| wire.to_node == render_id && wire.to_port == "camera")
        .collect();
    let [camera_wire] = camera_wires.as_slice() else {
        return Err("Choose one camera for this scene before setting up camera effects.".into());
    };
    let camera = def
        .nodes
        .iter()
        .find(|node| node.id == camera_wire.from_node)
        .ok_or("The scene's camera connection has no source.")?;
    if camera_wire.from_port != "out"
        || !matches!(
            camera.type_id.as_str(),
            "node.orbit_camera"
                | "node.free_camera"
                | "node.look_at_camera"
                | "node.loop_camera"
                | LENS
        )
    {
        return Err(
            "Camera effects need a recognised camera output; the custom camera was preserved."
                .into(),
        );
    }
    if !def
        .wires
        .iter()
        .any(|wire| wire.from_node == render_id && wire.from_port == "color")
    {
        return Err("Connect the scene's colour output before setting up camera effects.".into());
    }
    let camera_id = camera.id;
    let focus = camera
        .params
        .get("distance")
        .and_then(|value| match value {
            manifold_core::effect_graph_def::SerializedParamValue::Float { value } => Some(*value),
            _ => None,
        })
        .unwrap_or(10.0);
    let has_lens = camera.type_id == LENS;
    let mut next = all
        .iter()
        .map(|node| node.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("The scene has exhausted its node IDs.")?;
    if next > u32::MAX - 8 {
        return Err("The scene has exhausted its node IDs.".into());
    }
    let mut used: HashSet<String> = all.iter().map(|node| node.node_id.to_string()).collect();
    let mut fresh_id = || {
        let id = next;
        next += 1;
        id
    };
    let mut additions = Vec::new();
    let lens_id = if has_lens {
        camera_id
    } else {
        let id = fresh_id();
        let mut lens = plain_node(id, "lens", LENS, "Lens");
        lens.params.insert("focus_distance".into(), float(focus));
        lens.params.insert("f_stop".into(), float(32.0));
        lens.params.insert("shutter_angle".into(), float(180.0));
        additions.push(lens);
        id
    };
    // Native graph units retain the primitive's metre-scale calibration.
    // Imported assets pass their measured scene radius to the same builder.
    let tail = build_cinematic_tail(&mut fresh_id, 1.0, false);
    let dof = tail.dof_group_id;
    let motion = tail.motion_blur_id;
    additions.extend(tail.nodes);
    for node in &mut additions {
        assign_unique_ids(node, &mut used);
    }

    // All admission checks precede mutation. Existing parameters and bindings
    // retain their IDs, values, and targets.
    if !has_lens {
        for edge in &mut def.wires {
            if edge.from_node == camera_id && edge.from_port == "out" {
                edge.from_node = lens_id;
            }
        }
        def.wires.push(wire(camera_id, "out", lens_id, "camera"));
    }
    for edge in &mut def.wires {
        if edge.from_node == render_id && edge.from_port == "color" {
            edge.from_node = motion;
            edge.from_port = "out".into();
        }
    }
    def.wires.extend([
        wire(render_id, "color", dof, "color"),
        wire(render_id, "depth", dof, "depth"),
        wire(lens_id, "out", dof, "camera"),
        wire(dof, "out", motion, "in"),
        wire(render_id, "velocity", motion, "velocity"),
        wire(render_id, "depth", motion, "depth"),
        wire(lens_id, "out", motion, "camera"),
    ]);
    def.nodes.extend(additions);
    Ok(true)
}

fn collect_nodes<'a>(nodes: &'a [EffectGraphNode], result: &mut Vec<&'a EffectGraphNode>) {
    for node in nodes {
        result.push(node);
        if let Some(group) = &node.group {
            collect_nodes(&group.nodes, result);
        }
    }
}

fn assign_unique_ids(node: &mut EffectGraphNode, used: &mut HashSet<String>) {
    let base = format!("scene_camera_{}_{}", node.id, node.node_id);
    let mut name = base.clone();
    let mut suffix = 1;
    while !used.insert(name.clone()) {
        name = format!("{base}_{suffix}");
        suffix += 1;
    }
    node.node_id = NodeId::new(name);
    if let Some(group) = &mut node.group {
        for child in &mut group.nodes {
            assign_unique_ids(child, used);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::scene_graph::bool_val;
    use super::*;

    fn bare_scene() -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({"version":2,"nodes":[
            {"id":1,"nodeId":"camera","typeId":"node.orbit_camera",
             "params":{"distance":{"type":"Float","value":7.0}}},
            {"id":2,"nodeId":"scene","typeId":"node.render_scene"},
            {"id":3,"nodeId":"tone","typeId":"node.tone_map"},
            {"id":4,"nodeId":"final","typeId":"system.final_output"}
        ],"wires":[
            {"fromNode":1,"fromPort":"out","toNode":2,"toPort":"camera"},
            {"fromNode":2,"fromPort":"color","toNode":3,"toPort":"in"},
            {"fromNode":3,"fromPort":"out","toNode":4,"toPort":"in"}
        ]}))
        .unwrap()
    }

    #[test]
    fn native_camera_effects_preserve_look_post_order_and_round_trip() {
        let mut graph = bare_scene();
        assert!(prepare_camera_effects(&mut graph).unwrap());
        let mut nodes = Vec::new();
        collect_nodes(&graph.nodes, &mut nodes);
        for ty in ["node.bokeh_gather", "node.motion_blur"] {
            let node = nodes.iter().find(|node| node.type_id == ty).unwrap();
            assert_eq!(node.params.get("enabled"), Some(&bool_val(false)));
        }
        let lens = nodes.iter().find(|node| node.type_id == LENS).unwrap();
        assert_eq!(lens.params.get("focus_distance"), Some(&float(7.0)));
        let motion = nodes
            .iter()
            .find(|node| node.type_id == "node.motion_blur")
            .unwrap();
        assert!(graph.wires.contains(&wire(motion.id, "out", 3, "in")));
        assert!(graph.wires.contains(&wire(3, "out", 4, "in")));
        manifold_core::scene_index::FlatSceneIndex::build(&graph).unwrap();
        let saved = serde_json::to_string(&graph).unwrap();
        let mut reopened = serde_json::from_str(&saved).unwrap();
        assert!(!prepare_camera_effects(&mut reopened).unwrap());
        assert_eq!(saved, serde_json::to_string(&reopened).unwrap());
    }

    #[test]
    fn ambiguous_camera_setup_is_atomic_and_custom_tail_is_preserved() {
        let mut graph = bare_scene();
        graph.wires.push(wire(3, "out", 2, "camera"));
        let original = graph.clone();
        assert!(prepare_camera_effects(&mut graph).is_err());
        assert_eq!(graph, original);
        let mut graph = bare_scene();
        graph
            .nodes
            .push(plain_node(8, "custom_blur", "node.motion_blur", "Custom"));
        let original = graph.clone();
        assert!(!prepare_camera_effects(&mut graph).unwrap());
        assert_eq!(graph, original);
    }
}
