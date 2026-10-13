//! Math View face samplers and export boundaries; authored deformation stays intact.
use super::super::SceneModifierNodeRoute;
use super::*;

manifold_core::testkit_visible! {
pub(crate) fn resource_node_id(modifier: &NodeId, target: &SceneNodeRef, role: &str) -> NodeId {
    let mut parts = vec!["math_view", modifier.as_str(), role];
    parts.extend(target.scope.iter().map(NodeId::as_str));
    parts.push(target.node.as_str());
    namespace::namespace_node_id(&parts)
}
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

/// Add small real-face samplers in the scene and borrowed inputs in each view.
/// The scene graph exports each sampled object's faces and the scene depth;
/// the derived view turns that export into a `system.mesh_input` boundary.
pub(super) fn prepare(
    owner: &EffectGraphDef,
    def: &mut EffectGraphDef,
    index: &FlatSceneIndex,
    routes: &[SceneModifierNodeRoute],
    request: Option<MathViewRequest<'_>>,
) -> Result<(), SceneModifierExpandError> {
    for modifier in &owner.scene_modifiers {
        if !manifold_core::scene_modifier_math_view::is_math_view_recipe(&modifier.graph) {
            continue;
        }
        if request.is_some_and(|r| *r.modifier_id != modifier.id) {
            continue;
        }
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
        for frame in &modifier.mesh_frames {
            if request.is_some() {
                // Keep the old sampler's stable identity and routes, changing
                // only its boundary role. Parent resources are prebound later.
                let sample_id = sample_node_id(&modifier.id, &frame.target);
                let sample = def
                    .nodes
                    .iter_mut()
                    .find(|n| n.node_id == sample_id)
                    .ok_or_else(|| invalid("mathView", "sample boundary missing"))?;
                sample.type_id = "system.mesh_input".into();
                sample.params.clear();
                let sample = sample.id;
                def.wires.retain(|w| w.to_node != sample);
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
                let source_port = index
                    .input(&frame.target, "vertices")?
                    .filter(|wire| wire.from_node == source)
                    .map(|wire| wire.from_port.clone())
                    .ok_or_else(|| {
                        invalid("mathView", "saved source no longer feeds the object")
                    })?;
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
            }
        }
    }
    Ok(())
}
