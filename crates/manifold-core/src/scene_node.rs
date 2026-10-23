//! The sealed Scene node (SCENE_DOCUMENT_DESIGN.md ruling 18 (One writer per
//! region)). A scene layer's graph is the user's: the compiled scene sits in
//! it as one group node of type [`SCENE_NODE_TYPE_ID`], and everything wired
//! around that node is post-processing the graph editor owns. Blender's
//! compositor Render Layers node is the model.
//!
//! The node is an ordinary group underneath (`EffectGraphNode::group`), so the
//! flattener, the scene index and the loader see the scene exactly as before.
//! Only the scene compiler fills its body ([`seal`]); the graph editor shows it
//! as one node it can move and wire from, never open, edit, delete or group.
//! A save keeps the node with an empty body and no compiled metadata
//! ([`saved`]); a load compiles the document back into it.

use crate::NodeId;
use crate::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GroupDef,
    GroupInterface, InterfacePortDef, PresetMetadata,
};

/// Type id of the sealed Scene node. Its body is the compiled scene.
pub const SCENE_NODE_TYPE_ID: &str = "system.scene";

/// The Scene node's stable id and handle. The compiler names nothing else
/// this word: its own nodes are `<owner>:<role>`.
pub const SCENE_NODE_ID: &str = "scene";

/// The render passes the Scene node outputs: the finished picture, then the
/// raw passes the renderer draws when something is wired to them (clip-space
/// depth and screen-space motion). Normals render only inside the ray-traced
/// denoise feed and the item-id buffer does not exist yet, so neither is a
/// port.
pub const SCENE_OUTPUTS: [&str; 3] = ["color", "depth", "velocity"];

/// The layer input ports the Scene node takes, when the scene reads them: the
/// frame size alone (a grid cast from the camera, like the Ocean's, matches
/// the frame's aspect). Never the layer's picture.
pub const SCENE_INPUTS: [&str; 2] = ["output_width", "output_height"];

const GENERATOR_INPUT_TYPE_ID: &str = "system.generator_input";
const FINAL_OUTPUT_TYPE_ID: &str = "system.final_output";
const RENDER_SCENE_TYPE_ID: &str = crate::effect_graph_def::RENDER_SCENE_TYPE_ID;

/// User-visible reasons for refused edits on a scene layer's graph.
pub const INSIDE_SCENE_NODE: &str =
    "The Scene node is built from the Scene panel; edit the scene there.";
pub const SCENE_NODE_STAYS: &str =
    "The Scene node is the scene itself: it can be moved and wired from, not deleted, grouped or renamed.";
pub const SCENE_NODE_NOT_COPIED: &str =
    "The Scene node belongs to its scene layer and can't be copied into a graph.";

/// Whether `node` is the sealed Scene node.
pub fn is_scene_node(node: &EffectGraphNode) -> bool {
    node.type_id == SCENE_NODE_TYPE_ID
}

/// Whether any node in `nodes`, or in a group body below them, is a Scene node.
pub fn contains_scene_node(nodes: &[EffectGraphNode]) -> bool {
    nodes.iter().any(|node| {
        is_scene_node(node) || node.group.as_deref().is_some_and(|body| contains_scene_node(&body.nodes))
    })
}

/// Whether the group path `scope` (group node ids from the root) enters a
/// Scene node anywhere along the way.
pub fn scope_enters_scene_node(nodes: &[EffectGraphNode], scope: &[u32]) -> bool {
    let mut level = nodes;
    for hop in scope {
        let Some(node) = level.iter().find(|node| node.id == *hop) else {
            return false;
        };
        if is_scene_node(node) {
            return true;
        }
        let Some(body) = node.group.as_deref() else {
            return false;
        };
        level = &body.nodes;
    }
    false
}

/// The layer graph's Scene node.
pub fn scene_node(graph: &EffectGraphDef) -> Option<&EffectGraphNode> {
    graph.nodes.iter().find(|node| is_scene_node(node))
}

/// Whether the Scene node holds a compiled scene (a loaded file holds an
/// empty one until its document compiles).
pub fn is_filled(graph: &EffectGraphDef) -> bool {
    scene_node(graph).and_then(|node| node.group.as_deref()).is_some_and(|body| !body.nodes.is_empty())
}

/// Put a freshly compiled scene into the layer graph `layer`, leaving every
/// other node and wire as it is; with no layer graph yet, build the default
/// one: the layer input, the Scene node, and its colour into the layer output.
/// The layer graph takes the compiled manifest and scene modifiers, which the
/// compiler owns. The card controls the user exposed from the nodes around
/// the Scene node stay, so their maps and automation keep working. A scene
/// that reads the frame size ([`SCENE_INPUTS`]) takes it through Scene node
/// inputs wired from the layer input; any other read of the layer input is
/// refused.
pub fn seal(compiled: EffectGraphDef, layer: Option<&EffectGraphDef>) -> Result<EffectGraphDef, String> {
    let EffectGraphDef { version, name, description, preset_metadata, scene_modifiers, nodes, wires } = compiled;
    let find = |type_id: &str| -> Result<u32, String> {
        let mut found = nodes.iter().filter(|node| node.type_id == type_id).map(|node| node.id);
        let id = found.next().ok_or_else(|| format!("the compiled scene has no {type_id}"))?;
        match found.next() {
            Some(_) => Err(format!("the compiled scene has two {type_id} nodes")),
            None => Ok(id),
        }
    };
    let (input, output, render) =
        (find(GENERATOR_INPUT_TYPE_ID)?, find(FINAL_OUTPUT_TYPE_ID)?, find(RENDER_SCENE_TYPE_ID)?);
    // Only the frame size crosses in: a scene that sees the layer's pixels
    // would stop being the sealed render the Scene panel describes.
    if wires.iter().any(|wire| wire.from_node == input && !SCENE_INPUTS.contains(&wire.from_port.as_str())) {
        return Err("the compiled scene reads the layer input, which the Scene node does not take".into());
    }
    let inputs: Vec<&str> = SCENE_INPUTS
        .into_iter()
        .filter(|port| wires.iter().any(|wire| wire.from_node == input && wire.from_port == *port))
        .collect();
    let picture = wires
        .iter()
        .find(|wire| wire.to_node == output)
        .map(|wire| (wire.from_node, wire.from_port.clone()))
        .ok_or("the compiled scene sends nothing to its output")?;

    let boundary_id = nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1;
    let mut boundary: Vec<EffectGraphNode> = Vec::with_capacity(2);
    let mut body_nodes = Vec::with_capacity(nodes.len());
    for node in nodes {
        if node.id == input || node.id == output {
            boundary.push(node);
        } else {
            body_nodes.push(node);
        }
    }
    body_nodes.push(bare(boundary_id, NodeId::default(), GROUP_OUTPUT_TYPE_ID, None));
    let size_in = boundary_id + 1;
    if !inputs.is_empty() {
        body_nodes.push(bare(size_in, NodeId::default(), GROUP_INPUT_TYPE_ID, None));
    }
    let mut body_wires: Vec<EffectGraphWire> = wires
        .into_iter()
        .filter(|wire| wire.to_node != output)
        .map(|wire| if wire.from_node == input { EffectGraphWire { from_node: size_in, ..wire } } else { wire })
        .collect();
    let sources = [picture, (render, "depth".to_owned()), (render, "velocity".to_owned())];
    for (port, (from_node, from_port)) in SCENE_OUTPUTS.iter().zip(sources) {
        body_wires.push(EffectGraphWire { from_node, from_port, to_node: boundary_id, to_port: (*port).to_owned() });
    }
    let body = GroupDef {
        interface: GroupInterface {
            inputs: inputs
                .iter()
                .map(|port| InterfacePortDef { name: (*port).to_owned(), port_type: "Scalar(F32)".to_owned() })
                .collect(),
            outputs: SCENE_OUTPUTS
                .iter()
                .map(|port| InterfacePortDef { name: (*port).to_owned(), port_type: "Texture2D".to_owned() })
                .collect(),
            params: Vec::new(),
        },
        nodes: body_nodes,
        wires: body_wires,
        tint: None,
    };

    let mut graph = match layer.filter(|graph| scene_node(graph).is_some()) {
        Some(graph) => graph.clone(),
        None => default_layer_graph(boundary)?,
    };
    graph.version = version;
    graph.name = name;
    graph.description = description;
    let user = graph.preset_metadata.as_ref().and_then(user_controls);
    graph.preset_metadata = match (preset_metadata, user) {
        (Some(mut compiled), Some(user)) => {
            compiled.params.extend(user.params);
            compiled.bindings.extend(user.bindings);
            Some(compiled)
        }
        (compiled, user) => compiled.or(user),
    };
    graph.scene_modifiers = scene_modifiers;
    let node = graph.nodes.iter_mut().find(|node| is_scene_node(node)).expect("the layer graph has its Scene node");
    *node = EffectGraphNode {
        group: Some(Box::new(body)),
        title: Some("Scene".to_owned()),
        ..bare(node.id, NodeId::new(SCENE_NODE_ID), SCENE_NODE_TYPE_ID, node.editor_pos)
    };
    node.handle = Some(SCENE_NODE_ID.to_owned());
    let scene = node.id;
    wire_frame_size(&mut graph, scene, &inputs)?;
    Ok(graph)
}

/// Wire the layer input's frame size into the Scene node's size `inputs`, and
/// drop size wires the scene no longer reads.
fn wire_frame_size(graph: &mut EffectGraphDef, scene: u32, inputs: &[&str]) -> Result<(), String> {
    graph.wires.retain(|wire| !(wire.to_node == scene && SCENE_INPUTS.contains(&wire.to_port.as_str())));
    if inputs.is_empty() {
        return Ok(());
    }
    let layer_input = graph
        .nodes
        .iter()
        .find(|node| node.type_id == GENERATOR_INPUT_TYPE_ID)
        .map(|node| node.id)
        .ok_or("the scene needs the frame size, and the layer graph has no layer input")?;
    for port in inputs {
        graph.wires.push(EffectGraphWire {
            from_node: layer_input,
            from_port: (*port).to_owned(),
            to_node: scene,
            to_port: (*port).to_owned(),
        });
    }
    Ok(())
}

/// The layer graph a new scene layer starts with: its input, the Scene node,
/// and the Scene's colour into its output, laid out left to right.
fn default_layer_graph(boundary: Vec<EffectGraphNode>) -> Result<EffectGraphDef, String> {
    const SCENE: u32 = 2;
    let mut nodes = Vec::with_capacity(3);
    let mut output = None;
    for mut node in boundary {
        if node.type_id == GENERATOR_INPUT_TYPE_ID {
            node.id = 1;
            node.editor_pos = Some((-320.0, 0.0));
        } else {
            node.id = 3;
            node.editor_pos = Some((320.0, 0.0));
            output = Some(node.id);
        }
        nodes.push(node);
    }
    nodes.push(bare(SCENE, NodeId::new(SCENE_NODE_ID), SCENE_NODE_TYPE_ID, Some((0.0, 0.0))));
    let output = output.ok_or("the compiled scene has no output")?;
    Ok(EffectGraphDef {
        version: 0,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes,
        wires: vec![EffectGraphWire {
            from_node: SCENE,
            from_port: SCENE_OUTPUTS[0].to_owned(),
            to_node: output,
            to_port: "in".to_owned(),
        }],
    })
}

fn bare(id: u32, node_id: NodeId, type_id: &str, editor_pos: Option<(f32, f32)>) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id,
        type_id: type_id.to_owned(),
        handle: None,
        params: Default::default(),
        exposed_params: Default::default(),
        editor_pos,
        wgsl_source: None,
        title: None,
        output_formats: Default::default(),
        output_canvas_scales: Default::default(),
        group: None,
    }
}

/// What a save writes for a scene layer's graph: every node and wire the user
/// placed, and the Scene node with its body emptied. Of the metadata only the
/// card controls the user exposed are saved. The compiled scene, its
/// manifest and its scene modifiers are never saved; a load compiles them
/// from the document.
pub fn saved(graph: &EffectGraphDef) -> EffectGraphDef {
    let mut saved = EffectGraphDef {
        version: graph.version,
        name: None,
        description: None,
        preset_metadata: graph.preset_metadata.as_ref().and_then(user_controls),
        scene_modifiers: Vec::new(),
        nodes: Vec::with_capacity(graph.nodes.len()),
        wires: graph.wires.clone(),
    };
    for node in &graph.nodes {
        if is_scene_node(node) {
            let mut sealed = bare(node.id, node.node_id.clone(), SCENE_NODE_TYPE_ID, node.editor_pos);
            sealed.handle.clone_from(&node.handle);
            sealed.group = node.group.as_deref().map(|body| {
                Box::new(GroupDef { interface: body.interface.clone(), nodes: Vec::new(), wires: Vec::new(), tint: None })
            });
            saved.nodes.push(sealed);
        } else {
            saved.nodes.push(node.clone());
        }
    }
    saved
}

/// A scene layer's saved graph is closed: one Scene node at the top, empty,
/// and none of what the compiler writes.
pub fn check_saved(graph: &EffectGraphDef) -> Result<(), String> {
    let mut scenes = graph.nodes.iter().filter(|node| is_scene_node(node));
    let Some(scene) = scenes.next() else {
        return Err("a scene layer's graph has no Scene node".into());
    };
    if scenes.next().is_some() {
        return Err("a scene layer's graph has two Scene nodes".into());
    }
    if scene.group.as_deref().is_none_or(|body| !body.nodes.is_empty() || !body.wires.is_empty()) {
        return Err("a scene layer saves its document; its compiled Scene node is never saved".into());
    }
    let nested = graph.nodes.iter().filter_map(|node| node.group.as_deref()).any(|body| contains_scene_node(&body.nodes));
    let compiled_meta = graph.preset_metadata.as_ref().is_some_and(|meta| user_controls(meta).as_ref() != Some(meta));
    if nested || compiled_meta || !graph.scene_modifiers.is_empty() {
        return Err("a scene layer's saved graph carries what only the scene compiler writes".into());
    }
    Ok(())
}

/// The card controls the user exposed from nodes around the Scene node: the
/// user-added bindings and their params, and nothing the compiler writes.
/// `None` when there are none.
pub fn user_controls(meta: &PresetMetadata) -> Option<PresetMetadata> {
    let bindings: Vec<_> = meta.bindings.iter().filter(|binding| binding.user_added).cloned().collect();
    if bindings.is_empty() {
        return None;
    }
    let params = meta.params.iter().filter(|spec| bindings.iter().any(|binding| binding.id == spec.id)).cloned().collect();
    Some(PresetMetadata {
        params,
        bindings,
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
        scene_bounds: None,
        scene_modifier: None,
        ..meta.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire { from_node, from_port: from_port.to_owned(), to_node, to_port: to_port.to_owned() }
    }

    /// A compiled scene: layer input, a grid reading `read` from it, the
    /// render and the output.
    fn compiled(read: &str) -> EffectGraphDef {
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare(1, NodeId::new("input"), GENERATOR_INPUT_TYPE_ID, None),
                bare(2, NodeId::new("grid"), "node.projected_grid", None),
                bare(3, NodeId::new("render"), RENDER_SCENE_TYPE_ID, None),
                bare(4, NodeId::new("output"), FINAL_OUTPUT_TYPE_ID, None),
            ],
            wires: vec![wire(1, read, 2, "width"), wire(3, "color", 4, "in")],
        }
    }

    #[test]
    fn the_frame_size_crosses_into_the_scene_node_and_the_picture_does_not() {
        let graph = seal(compiled("output_width"), None).expect("the frame size seals");
        let scene = scene_node(&graph).unwrap();
        let body = scene.group.as_deref().unwrap();
        assert_eq!(body.interface.inputs.iter().map(|port| port.name.as_str()).collect::<Vec<_>>(), ["output_width"]);
        let size_in = body.nodes.iter().find(|node| node.type_id == GROUP_INPUT_TYPE_ID).expect("a size input inside").id;
        assert!(body.wires.contains(&wire(size_in, "output_width", 2, "width")));
        let layer_input = graph.nodes.iter().find(|node| node.type_id == GENERATOR_INPUT_TYPE_ID).unwrap().id;
        assert!(graph.wires.contains(&wire(layer_input, "output_width", scene.id, "output_width")));

        // A reseal wires the size once; a recompile that no longer reads it
        // drops the wire.
        let plain = seal(compiled("output_width"), Some(&graph)).unwrap();
        assert_eq!(plain.wires.iter().filter(|w| w.to_node == scene.id).count(), 1);
        let mut none = compiled("output_width");
        none.wires.retain(|w| w.from_node != 1);
        let unsized_graph = seal(none, Some(&plain)).unwrap();
        assert!(unsized_graph.wires.iter().all(|w| w.to_node != scene.id));

        for picture in ["out", "time"] {
            let refused = seal(compiled(picture), None).expect_err("only the frame size crosses in");
            assert!(refused.contains("reads the layer input"), "{picture}: {refused}");
        }
    }
}
