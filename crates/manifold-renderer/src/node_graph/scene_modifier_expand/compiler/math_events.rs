//! Prepared resource sharing and appearance masks; authored deformation stays intact.
use super::super::SceneModifierNodeRoute;
use super::*;

pub(crate) fn resource_node_id(modifier: &NodeId, target: &SceneNodeRef, role: &str) -> NodeId {
    let mut parts = vec!["math_view", modifier.as_str(), role];
    parts.extend(target.scope.iter().map(NodeId::as_str));
    parts.push(target.node.as_str());
    namespace::namespace_node_id(&parts)
}
pub(crate) fn sample_node_id(modifier: &NodeId, target: &SceneNodeRef) -> NodeId {
    let mut parts = vec!["math_view", modifier.as_str()];
    parts.extend(target.scope.iter().map(NodeId::as_str));
    parts.push(target.node.as_str());
    namespace::namespace_node_id(&parts)
}

fn add(
    def: &mut EffectGraphDef,
    node_id: NodeId,
    ty: &str,
    params: BTreeMap<String, SerializedParamValue>,
) -> Result<u32, SceneModifierExpandError> {
    if def.nodes.iter().any(|node| node.node_id == node_id) {
        return Err(invalid(
            node_id.to_string(),
            "generated Math View resource identity collides",
        ));
    }
    let id = def
        .nodes
        .iter()
        .map(|n| n.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| invalid("mathView", "node IDs exhausted"))?;
    def.nodes.push(EffectGraphNode {
        id,
        node_id,
        type_id: ty.into(),
        params,
        handle: None,
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: Default::default(),
        output_canvas_scales: Default::default(),
        group: None,
    });
    Ok(id)
}
fn wire(def: &mut EffectGraphDef, from: PortAddress, to: u32, port: &str) {
    def.wires.push(EffectGraphWire {
        from_node: from.0,
        from_port: from.1,
        to_node: to,
        to_port: port.into(),
    });
}
fn float(value: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value }
}

/// Add small real-face sources in the scene and borrowed inputs in each view.
/// A scene mask is shared with every connected view. Off-mode masks remain
/// presentation-only and are evaluated over the bounded reference samples.
///
/// Connect to Mesh is supported only when exactly one preceding modifier in
/// the view's scene chain carries a reference patch transform for every
/// selected object; anything else leaves the mask presentation-only so the
/// scene is never partially affected.
pub(super) fn prepare(
    owner: &EffectGraphDef,
    def: &mut EffectGraphDef,
    index: &FlatSceneIndex,
    routes: &[SceneModifierNodeRoute],
    request: Option<MathViewRequest<'_>>,
) -> Result<(), SceneModifierExpandError> {
    let mut exports = Vec::new();
    for modifier in &owner.scene_modifiers {
        if !manifold_core::scene_modifier_math_view::is_math_view_recipe(&modifier.graph) {
            continue;
        }
        if request.is_some_and(|r| *r.modifier_id != modifier.id) {
            continue;
        }
        // Preceding modifiers of the same scene, in chain order.
        let preceding: Vec<NodeId> = owner
            .scene_modifiers
            .iter()
            .take_while(|item| item.id != modifier.id)
            .filter(|item| item.scene == modifier.scene)
            .map(|item| item.id.clone())
            .collect();
        let control = |name: &str| -> Result<u32, SceneModifierExpandError> {
            let local = NodeId::new(format!("__math_view_{name}"));
            let copy = routes
                .iter()
                .find(|r| r.modifier_id == modifier.id && r.local.node == local)
                .and_then(|r| r.copies.first())
                .ok_or_else(|| invalid("mathView", format!("missing {name} route")))?;
            def.nodes
                .iter()
                .find(|n| n.node_id == copy.node_id)
                .map(|n| n.id)
                .ok_or_else(|| invalid("mathView", "missing control node"))
        };
        let density = control("density")?;
        // The event count/baseline producers are installed by the normal
        // compiler context seam, including while all diagrams are inactive.
        // Connect support is all-or-nothing across the view's objects.
        let patches: Vec<Option<EffectGraphNode>> = modifier
            .mesh_frames
            .iter()
            .map(|frame| {
                let found: Vec<_> = routes
                    .iter()
                    .filter(|r| preceding.contains(&r.modifier_id))
                    .flat_map(|r| &r.copies)
                    .filter(|c| c.object.as_ref() == Some(&frame.target))
                    .filter_map(|c| {
                        def.nodes.iter().find(|n| {
                            n.node_id == c.node_id && n.type_id == "node.transform_mesh_patches"
                        })
                    })
                    .collect();
                (found.len() == 1).then(|| (*found[0]).clone())
            })
            .collect();
        let connect_supported = !patches.is_empty() && patches.iter().all(Option::is_some);
        for (frame, patch) in modifier.mesh_frames.iter().zip(patches) {
            let mask = add(
                def,
                resource_node_id(&modifier.id, &frame.target, "weights"),
                "node.mesh_spatial_mask",
                BTreeMap::from([
                    (
                        "sample_mode".into(),
                        SerializedParamValue::Enum { value: 2 },
                    ),
                    ("shape".into(), SerializedParamValue::Enum { value: 0 }),
                    ("amount".into(), float(0.0)),
                    ("low".into(), float(1.0)),
                    ("high".into(), float(1.0)),
                    ("feather".into(), float(0.03)),
                ]),
            )?;
            for (port, default) in [
                ("cell_size", 0.15),
                ("scale", 1.0),
                ("source_offset_x", 0.0),
                ("source_offset_y", 0.0),
                ("source_offset_z", 0.0),
            ] {
                let borrowed = patch.as_ref().and_then(|patch| {
                    def.wires
                        .iter()
                        .find(|w| w.to_node == patch.id && w.to_port == port)
                        .cloned()
                });
                if let Some(w) = borrowed {
                    wire(def, (w.from_node, w.from_port), mask, port);
                } else {
                    let value = patch
                        .as_ref()
                        .and_then(|patch| patch.params.get(port).cloned())
                        .unwrap_or_else(|| float(default));
                    def.nodes
                        .iter_mut()
                        .find(|n| n.id == mask)
                        .unwrap()
                        .params
                        .insert(port.into(), value);
                }
            }
            if request.is_some() {
                // Keep the old sampler's stable identity and routes, changing
                // only its boundary role. Parent resources are prebound later.
                let mut parts = vec!["math_view", modifier.id.as_str()];
                parts.extend(frame.target.scope.iter().map(NodeId::as_str));
                parts.push(frame.target.node.as_str());
                let sample_id = namespace::namespace_node_id(&parts);
                let sample = def
                    .nodes
                    .iter_mut()
                    .find(|n| n.node_id == sample_id)
                    .ok_or_else(|| invalid("mathView", "sample boundary missing"))?;
                sample.type_id = "system.mesh_input".into();
                sample.params.clear();
                let sample = sample.id;
                def.wires.retain(|w| w.to_node != sample);
                wire(def, (sample, "vertices".into()), mask, "in");
                let diagram_id = resource_node_id(&modifier.id, &frame.target, "diagram");
                let diagram = def
                    .nodes
                    .iter()
                    .find(|n| n.node_id == diagram_id)
                    .ok_or_else(|| invalid("mathView", "diagram missing"))?
                    .id;
                let surface_id = resource_node_id(&modifier.id, &frame.target, "surface");
                let surface = def
                    .nodes
                    .iter()
                    .find(|n| n.node_id == surface_id)
                    .ok_or_else(|| invalid("mathView", "surface missing"))?
                    .id;
                wire(def, (sample, "weights".into()), diagram, "mesh_weights");
                wire(def, (mask, "weights".into()), diagram, "scan_weights");
                wire(def, (sample, "weights".into()), surface, "mesh_weights");
                wire(def, (mask, "weights".into()), surface, "scan_weights");
                // The view's boundary input borrows the parent render_scene
                // depth at runtime. Every presentation node consumes that
                // same borrowed texture, so the derived plan never revives
                // the original scene producer.
                wire(def, (sample, "depth".into()), diagram, "scene_depth");
                wire(def, (sample, "depth".into()), surface, "scene_depth");
            } else {
                let source = *index
                    .by_ref
                    .get(&frame.source)
                    .ok_or_else(|| invalid("mathView", "saved source missing"))?;
                let object = *index
                    .by_ref
                    .get(&frame.target)
                    .ok_or_else(|| invalid("mathView", "saved object missing"))?;
                let source_port = index
                    .input(&frame.target, "vertices")?
                    .filter(|wire| wire.from_node == source)
                    .map(|wire| wire.from_port.clone())
                    .ok_or_else(|| {
                        invalid("mathView", "saved source no longer feeds the object")
                    })?;
                wire(def, (source, source_port.clone()), mask, "in");
                if connect_supported {
                    // connect_supported implies one patch per frame.
                    let patch = patch.expect("supported connect has a patch");
                    if !def.wires.iter().any(|wire| {
                        wire.to_node == patch.id
                            && wire.to_port == "reference"
                            && wire.from_node == source
                            && wire.from_port == source_port
                    }) {
                        return Err(invalid(
                            "mathView",
                            "patch reference must use the saved original mesh for connected face correspondence",
                        ));
                    }
                    if let Some(prior) = def
                        .wires
                        .iter()
                        .find(|w| w.to_node == object && w.to_port == "weights")
                        .cloned()
                    {
                        wire(def, (prior.from_node, prior.from_port), mask, "weights");
                        def.wires
                            .retain(|w| !(w.to_node == object && w.to_port == "weights"));
                    }
                    wire(def, (mask, "weights".into()), object, "weights");
                }
                let sample = add(
                    def,
                    resource_node_id(&modifier.id, &frame.target, "samples"),
                    "node.sample_mesh_triangles",
                    BTreeMap::new(),
                )?;
                wire(def, (source, source_port), sample, "in");
                wire(def, (density, "out".into()), sample, "density");
                let output = add(
                    def,
                    resource_node_id(&modifier.id, &frame.target, "export"),
                    "system.mesh_output",
                    BTreeMap::new(),
                )?;
                wire(def, (sample, "vertices".into()), output, "vertices");
                // Preserve the parent scene's single resolved depth surface
                // on the export boundary. Math View variants borrow this
                // resource through their `system.mesh_input` depth output.
                let scene = *index
                    .by_ref
                    .get(&modifier.scene)
                    .ok_or_else(|| invalid("mathView", "scene target missing"))?;
                wire(def, (scene, "depth".into()), output, "depth");
                for (kind, port) in [("count", "trigger_count"), ("baseline", "trigger_baseline")] {
                    let key = serde_json::to_string(&(modifier.id.as_str(), "events", kind))
                        .map_err(|e| invalid("mathView", e.to_string()))?;
                    let nid = namespace::namespace_node_id(&["context", &key]);
                    let node = def
                        .nodes
                        .iter()
                        .find(|n| n.node_id == nid)
                        .ok_or_else(|| invalid("mathView", "event context node missing"))?
                        .id;
                    wire(def, (node, "out".into()), output, port);
                }
                if connect_supported {
                    exports.push((object, output));
                } else {
                    // Without a qualified patch carrier the mask never reaches
                    // the scene object; the view still reads its own event mask.
                    wire(def, (mask, "weights".into()), output, "weights");
                }
            }
        }
    }
    // Export the composed final visibility for this object, so all connected
    // cards agree with the actual scene even when several masks overlap.
    for (object, output) in exports {
        let weights = def
            .wires
            .iter()
            .find(|w| w.to_node == object && w.to_port == "weights")
            .cloned()
            .ok_or_else(|| invalid("mathView", "scene mask missing"))?;
        wire(
            def,
            (weights.from_node, weights.from_port),
            output,
            "weights",
        );
    }
    Ok(())
}
