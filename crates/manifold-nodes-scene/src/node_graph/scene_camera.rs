//! Camera effect construction shared by native scene preparation and import.
//! Existing custom chains are preserved. New plumbing is inserted before any
//! authored post effects, with both effects off so preparation preserves the look.

use std::collections::HashSet;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode};
use manifold_core::NodeId;
use super::cinematic_tail::build_cinematic_tail;
use super::scene_graph::{float, plain_node, wire};
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::SceneNodeRef;

mod repair;
pub use repair::restore_camera_effects;

const RENDER: &str = "node.render_scene";
const LENS: &str = "node.camera_lens";
const TAIL: &[&str] = &["node.coc_from_depth", "node.bokeh_gather", "node.motion_blur"];

/// Find effects belonging to this render/camera pair through actual wires,
/// including group boundaries. Unrelated or ambiguous effects never become
/// controls for the selected camera.
pub fn camera_effect_controls(index: &FlatSceneIndex, scene: &SceneNodeRef) -> Vec<NodeId> {
    let Ok(Some(camera)) = index.input(scene, "camera") else { return Vec::new(); };
    let Ok(render) = index.node(scene) else { return Vec::new(); };
    let input = |id: u32, port: &str| {
        let mut sources = index.flat.wires.iter().filter(|edge| edge.to_node == id && edge.to_port == port);
        let first = sources.next()?;
        if sources.next().is_some() { return None; }
        Some((first.from_node, first.from_port.as_str()))
    };
    let camera_source = Some((camera.from_node, camera.from_port.as_str()));
    let mut live = HashSet::new();
    let mut pending: Vec<_> = index.flat.nodes.iter()
        .filter(|node| node.type_id == "system.final_output").map(|node| node.id).collect();
    while let Some(id) = pending.pop() {
        if live.insert(id) {
            pending.extend(index.flat.wires.iter().filter(|edge| edge.to_node == id).map(|edge| edge.from_node));
        }
    }
    let mut controls = Vec::new();
    for ty in ["node.bokeh_gather", "node.motion_blur"] {
        let mut matching = index.flat.nodes.iter().filter(|node| {
            if node.type_id != ty || !live.contains(&node.id) { return false; }
            if ty == "node.motion_blur" {
                return input(node.id, "camera") == camera_source
                    && input(node.id, "velocity") == Some((render.id, "velocity"))
                    && input(node.id, "in")
                        .is_some_and(|(from, port)| color_reaches_render(index, from, port, render.id));
            }
            if !input(node.id, "in")
                .is_some_and(|(from, port)| color_reaches_render(index, from, port, render.id))
            {
                return false;
            }
            let Some((mut id, mut port)) = input(node.id, "width") else { return false; };
            // Older scenes may retain a dilation node between CoC and bokeh.
            for _ in 0..8 {
                let Some(source) = index.flat.nodes.iter().find(|source| source.id == id) else { return false; };
                if port != "out" { return false; }
                if source.type_id == "node.coc_from_depth" {
                    return input(id, "camera") == camera_source
                        && input(id, "depth") == Some((render.id, "depth"));
                }
                if source.type_id != "node.coc_dilate" { return false; }
                let Some(upstream) = input(id, "in") else { return false; };
                (id, port) = upstream;
            }
            false
        });
        if let Some(node) = matching.next() && matching.next().is_none() {
            controls.push(node.node_id.clone());
        }
    }
    controls
}

/// Follow the colour-producing side of a known post chain back to the scene.
/// A random texture feeding an effect must not make that effect a camera
/// control, while the authored AO/post groups remain valid colour lineage.
fn color_reaches_render(index: &FlatSceneIndex, from: u32, port: &str, render_id: u32) -> bool {
    fn visit(index: &FlatSceneIndex, id: u32, port: &str, render_id: u32, seen: &mut HashSet<u32>) -> bool {
        if id == render_id && port == "color" {
            return true;
        }
        if !seen.insert(id) {
            return false;
        }
        if port != "out" {
            return false;
        }
        index
            .flat
            .wires
            .iter()
            .filter(|wire| wire.to_node == id && matches!(wire.to_port.as_str(),
                "in" | "color" | "a" | "b" | "top" | "bottom" | "source"))
            .any(|wire| visit(index, wire.from_node, wire.from_port.as_str(), render_id, seen))
    }
    visit(index, from, port, render_id, &mut HashSet::new())
}

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
    let scenes: Vec<_> = def.nodes.iter().filter(|node| node.type_id == RENDER).collect();
    let [scene] = scenes.as_slice() else {
        return Err("Camera setup needs one scene at the top level; existing scene wiring was preserved.".into());
    };
    let render_id = scene.id;
    let camera_wires: Vec<_> = def.wires.iter()
        .filter(|wire| wire.to_node == render_id && wire.to_port == "camera").collect();
    let [camera_wire] = camera_wires.as_slice() else {
        return Err("Choose one camera for this scene before setting up camera effects.".into());
    };
    let camera = def.nodes.iter().find(|node| node.id == camera_wire.from_node)
        .ok_or("The scene's camera connection has no source.")?;
    if camera_wire.from_port != "out" || !matches!(camera.type_id.as_str(),
        "node.orbit_camera" | "node.free_camera" | "node.look_at_camera" | "node.loop_camera" | LENS)
    {
        return Err("Camera effects need a recognised camera output; the custom camera was preserved.".into());
    }
    if !def.wires.iter().any(|wire| wire.from_node == render_id && wire.from_port == "color") {
        return Err("Connect the scene's colour output before setting up camera effects.".into());
    }
    let camera_id = camera.id;
    let focus = camera.params.get("distance").and_then(|value| match value {
        manifold_core::effect_graph_def::SerializedParamValue::Float { value } => Some(*value),
        _ => None,
    }).unwrap_or(10.0);
    let has_lens = camera.type_id == LENS;
    let mut next = all.iter().map(|node| node.id).max().unwrap_or(0)
        .checked_add(1).ok_or("The scene has exhausted its node IDs.")?;
    if next > u32::MAX - 8 { return Err("The scene has exhausted its node IDs.".into()); }
    let mut used: HashSet<String> = all.iter().map(|node| node.node_id.to_string()).collect();
    let mut fresh_id = || { let id = next; next += 1; id };
    let mut additions = Vec::new();
    let lens_id = if has_lens { camera_id } else {
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
    for node in &mut additions { assign_unique_ids(node, &mut used); }

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
        wire(lens_id, "out", motion, "camera"),
    ]);
    def.nodes.extend(additions);
    Ok(true)
}

fn collect_nodes<'a>(nodes: &'a [EffectGraphNode], result: &mut Vec<&'a EffectGraphNode>) {
    for node in nodes {
        result.push(node);
        if let Some(group) = &node.group { collect_nodes(&group.nodes, result); }
    }
}

fn assign_unique_ids(node: &mut EffectGraphNode, used: &mut HashSet<String>) {
    let base = format!("scene_camera_{}_{}", node.id, node.node_id);
    let mut name = base.clone();
    let mut suffix = 1;
    while !used.insert(name.clone()) { name = format!("{base}_{suffix}"); suffix += 1; }
    node.node_id = NodeId::new(name);
    if let Some(group) = &mut node.group {
        for child in &mut group.nodes { assign_unique_ids(child, used); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::scene_graph::bool_val;

    pub(super) fn bare_scene() -> EffectGraphDef {
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
        ]})).unwrap()
    }

    #[test]
    fn native_camera_effects_preserve_look_post_order_and_round_trip() {
        let mut graph = bare_scene();
        assert!(prepare_camera_effects(&mut graph).unwrap());
        let mut nodes = Vec::new(); collect_nodes(&graph.nodes, &mut nodes);
        for ty in ["node.bokeh_gather", "node.motion_blur"] {
            let node = nodes.iter().find(|node| node.type_id == ty).unwrap();
            assert_eq!(node.params.get("enabled"), Some(&bool_val(false)));
        }
        let lens = nodes.iter().find(|node| node.type_id == LENS).unwrap();
        assert_eq!(lens.params.get("focus_distance"), Some(&float(7.0)));
        let motion = nodes.iter().find(|node| node.type_id == "node.motion_blur").unwrap();
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
        graph.nodes.push(plain_node(8, "custom_blur", "node.motion_blur", "Custom"));
        let original = graph.clone();
        assert!(!prepare_camera_effects(&mut graph).unwrap());
        assert_eq!(graph, original);
    }

    #[test]
    fn camera_controls_follow_wires_across_groups_and_ignore_unrelated_effects() {
        let mut graph = bare_scene();
        prepare_camera_effects(&mut graph).unwrap();
        graph.nodes.insert(0, plain_node(90, "unrelated_blur", "node.motion_blur", "Unrelated"));
        graph.nodes.insert(0, plain_node(91, "unrelated_dof", "node.bokeh_gather", "Unrelated"));
        let scene = SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") };
        let index = FlatSceneIndex::build(&graph).unwrap();
        let controls = camera_effect_controls(&index, &scene);
        assert_eq!(controls.len(), 2);
        assert!(controls.iter().all(|id| id.as_str().starts_with("scene_camera_")));
        let motion = graph.nodes.iter().find(|node| node.type_id == "node.motion_blur" && node.id != 90).unwrap().id;
        // A blur using the raw camera instead of the scene's lens is not a
        // working control for this camera, even if its colour output is live.
        graph.wires.iter_mut().find(|edge| edge.to_node == motion && edge.to_port == "camera").unwrap().from_node = 1;
        let index = FlatSceneIndex::build(&graph).unwrap();
        assert_eq!(camera_effect_controls(&index, &scene), controls[..1]);
    }

    #[test]
    fn effect_with_scene_side_inputs_but_unrelated_colour_is_not_a_camera_control() {
        let mut graph = bare_scene();
        prepare_camera_effects(&mut graph).unwrap();
        graph.nodes.push(plain_node(90, "unrelated_image", "node.constant_color", "Image"));
        let motion = graph.nodes.iter().find(|node| node.type_id == "node.motion_blur").unwrap().id;
        let input = graph.wires.iter_mut().find(|edge| edge.to_node == motion && edge.to_port == "in").unwrap();
        input.from_node = 90;
        input.from_port = "out".into();
        let index = FlatSceneIndex::build(&graph).unwrap();
        let scene = SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") };
        assert!(camera_effect_controls(&index, &scene).is_empty());
    }
}
