//! Explicit restoration preserves authored colour processing and parameters.
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID};
use super::{LENS, RENDER, TAIL, collect_nodes, assign_unique_ids, prepare_camera_effects, camera_effect_controls};
use super::super::scene_graph::{plain_node, float, wire};

/// Restore a bare scene or the side inputs of a complete standard tail.
/// Ambiguous/custom or incomplete colour chains stay untouched.
pub fn restore_camera_effects(def: &mut EffectGraphDef) -> Result<bool, String> {
    let mut staged = def.clone();
    restore(&mut staged)?;
    let changed = staged != *def;
    if changed { *def = staged; }
    Ok(changed)
}

fn restore(def: &mut EffectGraphDef) -> Result<(), String> {
    let scenes: Vec<_> = def.nodes.iter().filter(|node| node.type_id == RENDER).collect();
    let [scene] = scenes.as_slice() else {
        return Err("Camera setup needs one scene at the top level.".into());
    };
    let scene_id = scene.id;
    let scene_ref = manifold_core::SceneNodeRef { scope: Vec::new(), node: scene.node_id.clone() };
    let mut edges = def.wires.iter().filter(|edge| edge.to_node == scene_id && edge.to_port == "camera");
    let camera_edge = edges.next().cloned();
    if edges.next().is_some() { return Err("The scene has more than one camera connection.".into()); }
    let camera_id = match camera_edge {
        Some(edge) if def.nodes.iter().any(|node| node.id == edge.from_node) => {
            if edge.from_port != "out" { return Err("The custom camera output was preserved.".into()); }
            edge.from_node
        }
        _ => {
            let camera = fresh_node(def, "node.orbit_camera", "Camera")?;
            let id = camera.id;
            def.nodes.push(camera);
            set_input(&mut def.wires, id, "out", scene_id, "camera")?;
            id
        }
    };
    let camera = def.nodes.iter().find(|node| node.id == camera_id).unwrap();
    if !matches!(camera.type_id.as_str(), LENS | "node.orbit_camera" | "node.free_camera" | "node.look_at_camera" | "node.loop_camera") {
        return Err("The custom camera was preserved; automatic setup cannot determine its lens.".into());
    }
    let has_lens = camera.type_id == LENS;
    if has_lens {
        let sources: Vec<_> = def.wires.iter().filter(|edge| edge.to_node == camera_id && edge.to_port == "camera").collect();
        if sources.len() > 1 { return Err("The lens has conflicting camera connections.".into()); }
        if !sources.first().is_some_and(|edge| def.nodes.iter().any(|node| node.id == edge.from_node)) {
            let camera = fresh_node(def, "node.orbit_camera", "Camera")?;
            let id = camera.id;
            def.nodes.push(camera);
            set_input(&mut def.wires, id, "out", camera_id, "camera")?;
        }
    }
    if prepare_camera_effects(def)? { return Ok(()); }

    let mut all = Vec::new();
    collect_nodes(&def.nodes, &mut all);
    if TAIL.iter().any(|ty| all.iter().filter(|node| node.type_id == *ty).count() != 1) {
        return Err("Camera effects form a partial or ambiguous chain. No changes were made.".into());
    }
    let coc = all.iter().find(|node| node.type_id == TAIL[0]).unwrap().node_id.clone();
    let bokeh = all.iter().find(|node| node.type_id == TAIL[1]).unwrap().node_id.clone();
    let motion = def.nodes.iter().find(|node| node.type_id == TAIL[2])
        .ok_or("Motion blur is inside a custom group; its wiring was preserved.")?.id;
    drop(all);
    let lens = if has_lens { camera_id } else {
        let mut lens = fresh_node(def, LENS, "Lens")?;
        // Preserve the neutral raw-camera lens, including zero shutter blur.
        lens.params.insert("f_stop".into(), float(32.0));
        lens.params.insert("shutter_angle".into(), float(0.0));
        let id = lens.id;
        def.nodes.push(lens);
        set_input(&mut def.wires, camera_id, "out", id, "camera")?;
        set_input(&mut def.wires, id, "out", scene_id, "camera")?;
        id
    };
    if let Some(node) = def.nodes.iter().find(|node| node.node_id == coc) {
        let id = node.id;
        if !def.nodes.iter().any(|node| node.node_id == bokeh) {
            return Err("Depth of field crosses a custom group; its wiring was preserved.".into());
        }
        set_input(&mut def.wires, lens, "out", id, "camera")?;
        set_input(&mut def.wires, scene_id, "depth", id, "depth")?;
    } else {
        let group_node = def.nodes.iter_mut().find(|node| node.group.as_ref().is_some_and(|group|
            group.nodes.iter().any(|node| node.node_id == coc)))
            .ok_or("Depth of field is nested in a custom group; its wiring was preserved.")?;
        let group = group_node.group.as_mut().unwrap();
        if !group.nodes.iter().any(|node| node.node_id == bokeh)
            || group.nodes.iter().any(|node| !matches!(node.type_id.as_str(),
                "system.group_input" | "system.group_output" | "node.coc_from_depth" | "node.coc_dilate" | "node.bokeh_gather"))
            || !["camera", "depth"].iter().all(|port| group.interface.inputs.iter().any(|input| input.name == *port))
        { return Err("The custom depth-of-field group was preserved.".into()); }
        let inputs: Vec<_> = group.nodes.iter().filter(|node| node.type_id == GROUP_INPUT_TYPE_ID).collect();
        let [input] = inputs.as_slice() else { return Err("Depth-of-field group input is ambiguous.".into()); };
        let input = input.id;
        let coc_id = group.nodes.iter().find(|node| node.node_id == coc).unwrap().id;
        set_input(&mut group.wires, input, "camera", coc_id, "camera")?;
        set_input(&mut group.wires, input, "depth", coc_id, "depth")?;
        let id = group_node.id;
        set_input(&mut def.wires, lens, "out", id, "camera")?;
        set_input(&mut def.wires, scene_id, "depth", id, "depth")?;
    }
    set_input(&mut def.wires, lens, "out", motion, "camera")?;
    set_input(&mut def.wires, scene_id, "velocity", motion, "velocity")?;
    let index = manifold_core::scene_index::FlatSceneIndex::build(def).map_err(|error| error.to_string())?;
    if camera_effect_controls(&index, &scene_ref).len() != 2 {
        return Err("Camera effects have incomplete or custom colour wiring. No changes were made.".into());
    }
    Ok(())
}

fn fresh_node(def: &EffectGraphDef, ty: &str, title: &str) -> Result<EffectGraphNode, String> {
    let mut all = Vec::new();
    collect_nodes(&def.nodes, &mut all);
    let id = all.iter().map(|node| node.id).max().unwrap_or(0)
        .checked_add(1).ok_or("The scene has exhausted its node IDs.")?;
    let mut used = all.iter().map(|node| node.node_id.to_string()).collect();
    let mut node = plain_node(id, title, ty, title);
    assign_unique_ids(&mut node, &mut used);
    Ok(node)
}

fn set_input(wires: &mut Vec<EffectGraphWire>, from: u32, from_port: &str, to: u32, port: &str) -> Result<(), String> {
    let mut inputs = wires.iter_mut().filter(|edge| edge.to_node == to && edge.to_port == port);
    let current = inputs.next();
    if inputs.next().is_some() { return Err(format!("The '{port}' input has conflicting connections.")); }
    let replacement = wire(from, from_port, to, port);
    if let Some(current) = current { *current = replacement; }
    else { wires.push(replacement); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoration_preserves_colour_order_nodes_params_and_bindings() {
        let mut graph = super::super::tests::bare_scene();
        prepare_camera_effects(&mut graph).unwrap();
        let dof = graph.nodes.iter().find(|node| node.group.is_some()).unwrap().id;
        graph.nodes.push(plain_node(90, "grade", "node.gain", "Grade"));
        let edge = graph.wires.iter_mut().find(|edge| edge.to_node == dof && edge.to_port == "color").unwrap();
        edge.from_node = 90;
        edge.from_port = "out".into();
        graph.wires.push(wire(2, "color", 90, "in"));
        let expected = graph.clone();
        let motion = graph.nodes.iter().find(|node| node.type_id == TAIL[2]).unwrap().id;
        graph.wires.iter_mut().find(|edge| edge.to_node == motion && edge.to_port == "camera").unwrap().from_node = 1;
        graph.wires.retain(|edge| !(edge.to_node == dof && edge.to_port == "depth"));
        assert!(restore_camera_effects(&mut graph).unwrap());
        assert_eq!(graph.nodes, expected.nodes);
        assert_eq!(graph.preset_metadata, expected.preset_metadata);
        assert_eq!(graph.wires.len(), expected.wires.len());
        assert!(expected.wires.iter().all(|wire| graph.wires.contains(wire)));
        assert!(!restore_camera_effects(&mut graph).unwrap());
    }

    #[test]
    fn restoration_rejects_partial_or_conflicting_chains_atomically() {
        let mut graph = super::super::tests::bare_scene();
        prepare_camera_effects(&mut graph).unwrap();
        let motion = graph.nodes.iter().find(|node| node.type_id == TAIL[2]).unwrap().id;
        graph.wires.push(wire(1, "out", motion, "camera"));
        let before = graph.clone();
        assert!(restore_camera_effects(&mut graph).is_err());
        assert_eq!(graph, before);
        graph.nodes.retain(|node| node.id != motion);
        let before = graph.clone();
        assert!(restore_camera_effects(&mut graph).is_err());
        assert_eq!(graph, before);
    }

    #[test]
    fn missing_camera_and_bare_tail_restore_together() {
        let mut graph = super::super::tests::bare_scene();
        graph.nodes.retain(|node| node.id != 1);
        assert!(restore_camera_effects(&mut graph).unwrap());
        let saved = graph.clone();
        assert!(!restore_camera_effects(&mut graph).unwrap());
        assert_eq!(graph, saved);
        let camera_id = graph.nodes.iter().find(|node| node.type_id == "node.orbit_camera").unwrap().id;
        graph.nodes.retain(|node| node.id != camera_id);
        assert!(restore_camera_effects(&mut graph).unwrap());
        assert!(!restore_camera_effects(&mut graph).unwrap());
    }
}
