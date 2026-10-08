//! Internal fragment graphs. The saved scene keeps one logical object.
use super::super::{SceneModifierNodeRoute, bindings::SceneModifierBindingSource};
use super::*;
use manifold_core::effect_graph_def::BindingTarget;

pub(crate) fn copy_id(modifier: &NodeId, source: &NodeId, role: &str, index: usize) -> NodeId {
    namespace::namespace_node_id(&[
        "shatter",
        modifier.as_str(),
        source.as_str(),
        role,
        &index.to_string(),
    ])
}
fn float(value: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value }
}
fn number(value: Option<&SerializedParamValue>) -> Option<f32> {
    match value {
        Some(SerializedParamValue::Float { value }) => Some(*value),
        Some(SerializedParamValue::Int { value }) => Some(*value as f32),
        Some(SerializedParamValue::Enum { value }) => Some(*value as f32),
        Some(SerializedParamValue::Bool { value }) => Some(u8::from(*value) as f32),
        _ => None,
    }
}
fn input(def: &EffectGraphDef, target: u32, port: &str) -> Option<PortAddress> {
    def.wires
        .iter()
        .find(|w| w.to_node == target && w.to_port == port)
        .map(|w| (w.from_node, w.from_port.clone()))
}
fn node(def: &EffectGraphDef, id: u32) -> Result<&EffectGraphNode, SceneModifierExpandError> {
    def.nodes
        .iter()
        .find(|n| n.id == id)
        .ok_or_else(|| invalid("shatter", "missing source node"))
}
fn wire(def: &mut EffectGraphDef, from: PortAddress, target: u32, port: &str) {
    def.wires.push(EffectGraphWire {
        from_node: from.0,
        from_port: from.1,
        to_node: target,
        to_port: port.into(),
    });
}
fn clone_node(
    def: &mut EffectGraphDef,
    source: &EffectGraphNode,
    id: NodeId,
) -> Result<u32, SceneModifierExpandError> {
    if def.nodes.iter().any(|n| n.node_id == id) {
        return Err(invalid("shatter", "duplicate generated identity"));
    }
    let numeric = def
        .nodes
        .iter()
        .map(|n| n.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| invalid("shatter", "node IDs exhausted"))?;
    let mut copy = source.clone();
    copy.id = numeric;
    copy.node_id = id;
    copy.handle = None;
    copy.exposed_params.clear();
    def.nodes.push(copy);
    Ok(numeric)
}
fn generated<'a>(
    def: &'a EffectGraphDef,
    routes: &[SceneModifierNodeRoute],
    modifier: &NodeId,
    local: &NodeId,
) -> Result<&'a EffectGraphNode, SceneModifierExpandError> {
    let route = routes
        .iter()
        .find(|r| r.modifier_id == *modifier && r.local.node == *local)
        .ok_or_else(|| invalid("shatter", "control has no prepared route"))?;
    if route.copies.len() != 1 {
        return Err(invalid("shatter", "controls must have scene scope"));
    }
    def.nodes
        .iter()
        .find(|n| n.node_id == route.copies[0].node_id)
        .ok_or_else(|| invalid("shatter", "prepared control is absent"))
}
fn control(
    def: &EffectGraphDef,
    routes: &[SceneModifierNodeRoute],
    instance: &SceneModifierInstanceDef,
    param: &str,
) -> Result<f32, SceneModifierExpandError> {
    let binding = instance
        .graph
        .preset_metadata
        .as_ref()
        .and_then(|m| m.bindings.iter().find(|b| b.id == param))
        .ok_or_else(|| invalid("shatter", format!("control '{param}' has no binding")))?;
    let BindingTarget::Node { node_id, param } = &binding.target else {
        return Err(invalid("shatter", "control must bind a node"));
    };
    number(
        generated(def, routes, &instance.id, node_id)?
            .params
            .get(param),
    )
    .ok_or_else(|| invalid("shatter", "control must be numeric"))
}

pub(super) fn prepare(
    owner: &EffectGraphDef,
    def: &mut EffectGraphDef,
    index: &FlatSceneIndex,
    routes: &[SceneModifierNodeRoute],
    binding_sources: &mut Vec<Option<SceneModifierBindingSource>>,
) -> Result<(), SceneModifierExpandError> {
    let mut used_parents = BTreeSet::new();
    for instance in &owner.scene_modifiers {
        let recipe = instance
            .graph
            .preset_metadata
            .as_ref()
            .and_then(|m| m.scene_modifier.as_ref())
            .expect("validated recipe");
        let Some(shatter) = &recipe.shatter else {
            continue;
        };
        if control(def, routes, instance, &recipe.enabled_param)? < 0.5 {
            continue;
        }
        let count = control(def, routes, instance, &shatter.fragments_param)?;
        if !count.is_finite() || !(2.0..=32.0).contains(&count) {
            return Err(invalid("shatter", "Pieces must be between 2 and 32"));
        }
        let count = count.round() as usize;
        let release = (
            generated(def, routes, &instance.id, &shatter.trigger_node)?.id,
            shatter.trigger_port.clone(),
        );
        let scene = def.nodes.iter().find(|n| n.node_id == instance.scene.node)
            .ok_or_else(|| invalid("shatter", "prepared scene is missing"))?.id;
        let targets = frames::selected_objects(index, instance)?;
        if targets.is_empty() {
            return Err(invalid(
                "shatter",
                "select an imported object with Physics enabled",
            ));
        }
        let mut assemblies: BTreeMap<(u32, usize), Vec<u32>> = BTreeMap::new();
        for target in targets {
            let object = def.nodes.iter().find(|n| n.node_id == target.node)
                .ok_or_else(|| invalid("shatter", "prepared object is missing"))?.id;
            let pose = input(def, object, "parent_transform").or_else(|| input(def, object, "transform"))
                .ok_or_else(|| invalid("shatter", "object needs Physics before Shatter"))?;
            if node(def, pose.0)?.type_id != "node.physics_world" {
                return Err(invalid(
                    "shatter",
                    "enable Physics on the object before adding Shatter",
                ));
            }
            let parent = pose
                .1
                .strip_prefix("pose_")
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or_else(|| invalid("shatter", "object has no rigid body pose"))?;
            assemblies.entry((pose.0, parent)).or_default().push(object);
        }
        for ((world, parent_slot), parts) in assemblies {
            if !used_parents.insert((world, parent_slot)) {
                return Err(invalid(
                    "shatter",
                    "an object can have only one active Shatter modifier",
                ));
            }
            if count < parts.len() {
                return Err(invalid(
                    "shatter",
                    format!(
                        "this scan needs at least {} Pieces to preserve its material parts",
                        parts.len()
                    ),
                ));
            }
            let parent_input = input(def, world, &format!("body_{parent_slot}"))
                .ok_or_else(|| invalid("shatter", "parent body is missing"))?;
            let parent_acceleration =
                input(def, world, &format!("body_acceleration_{parent_slot}"));
            let parent = node(def, parent_input.0)?.clone();
            wire(def, release.clone(), parent.id, "release_count");
            let authored_pose = input(def, parent.id, "transform")
                .ok_or_else(|| invalid("shatter", "parent authored transform is missing"))?;
            let used: BTreeSet<usize> = def
                .wires
                .iter()
                .filter_map(|w| {
                    if w.to_node == world {
                        w.to_port
                            .strip_prefix("body_acceleration_")
                            .or_else(|| w.to_port.strip_prefix("body_"))
                            .and_then(|slot| slot.parse().ok())
                    } else if w.from_node == world {
                        w.from_port
                            .strip_prefix("pose_")
                            .and_then(|slot| slot.parse().ok())
                    } else {
                        None
                    }
                })
                .collect();
            let slots: Vec<_> = (0..crate::water::physics::MAX_BODIES)
                .filter(|slot| !used.contains(slot))
                .take(count)
                .collect();
            if slots.len() != count {
                return Err(invalid(
                    "shatter",
                    "not enough physics body slots for these Pieces",
                ));
            }
            // Distribute the total across materials; every original triangle
            // and its attributes belong to exactly one resulting fragment.
            let mut allocation = vec![1usize; parts.len()];
            let mut sources = Vec::with_capacity(parts.len());
            for &part in &parts {
                let source = input(def, part, "vertices")
                    .ok_or_else(|| invalid("shatter", "mesh source is missing"))?;
                let source = node(def, source.0)?.clone();
                if source.type_id != "node.gltf_mesh_source" {
                    return Err(invalid(
                        "shatter",
                        "Shatter currently needs an undeformed static imported mesh",
                    ));
                }
                sources.push(source);
            }
            for _ in parts.len()..count {
                let biggest = (0..parts.len())
                    .max_by(|&a, &b| {
                        let weight = |i: usize| {
                            number(sources[i].params.get("source_vertex_count")).unwrap_or(1.0)
                                / allocation[i] as f32
                        };
                        weight(a).total_cmp(&weight(b)).then_with(|| b.cmp(&a))
                    })
                    .expect("nonempty parts");
                allocation[biggest] += 1;
            }
            let mut slot = slots.into_iter();
            for ((part, source), pieces) in parts.into_iter().zip(sources).zip(allocation) {
                let original = node(def, part)?.clone();
                let nested = input(def, part, "parent_transform").is_some();
                let local = nested.then(|| input(def, part, "transform")).flatten();
                let incoming: Vec<_> = def
                    .wires
                    .iter()
                    .filter(|w| {
                        w.to_node == part && w.to_port != "vertices"
                            && w.to_port != "parent_transform"
                            && (nested || w.to_port != "transform")
                    })
                    .cloned()
                    .collect();
                let first_port = def
                    .wires
                    .iter()
                    .find(|w| {
                        w.from_node == part
                            && w.to_node == scene
                            && w.to_port.starts_with("object_")
                    })
                    .map(|w| w.to_port.clone())
                    .ok_or_else(|| {
                        invalid("shatter", "selected object is not connected to the scene")
                    })?;
                def.wires.retain(|w| {
                    !(w.from_node == part && w.to_node == scene && w.to_port == first_port)
                });
                for piece in 0..pieces {
                    let body_slot = slot.next().expect("checked slot count");
                    let source_copy = clone_node(
                        def,
                        &source,
                        copy_id(&instance.id, &original.node_id, "mesh", piece),
                    )?;
                    let mesh = def.nodes.last_mut().expect("cloned mesh");
                    if let Some(path) = parent.params.get("path") {
                        mesh.params.insert("path".into(), path.clone());
                    }
                    mesh.params
                        .insert("fragment_count".into(), float(pieces as f32));
                    mesh.params
                        .insert("fragment_index".into(), float(piece as f32));
                    if let Some(vertices) = number(source.params.get("max_capacity")) {
                        // The existing partitioner repeatedly halves the largest
                        // triangle set. Bound every piece without allocating a
                        // full scan-sized GPU buffer for each of them.
                        let divisor = 1usize << pieces.ilog2();
                        let triangles = (vertices as usize).div_ceil(3);
                        mesh.params.insert(
                            "max_capacity".into(),
                            SerializedParamValue::Int {
                                value: (triangles.div_ceil(divisor) * 3).max(3) as i32,
                            },
                        );
                    }
                    let body_copy = clone_node(
                        def,
                        &parent,
                        copy_id(&instance.id, &original.node_id, "body", piece),
                    )?;
                    let body = def.nodes.last_mut().expect("cloned body");
                    body.params.remove("compound_materials");
                    for key in [
                        "path",
                        "mesh_index",
                        "primitive_index",
                        "material_index",
                        "fit",
                        "recenter",
                        "translate_x",
                        "translate_y",
                        "translate_z",
                    ] {
                        if let Some(value) = source.params.get(key) {
                            body.params.insert(key.into(), value.clone());
                        }
                    }
                    body.params
                        .insert("enabled".into(), SerializedParamValue::Bool { value: true });
                    body.params.insert("motion".into(), SerializedParamValue::Enum { value: 1 });
                    body.params
                        .insert("fragment_count".into(), float(pieces as f32));
                    body.params
                        .insert("fragment_index".into(), float(piece as f32));
                    body.params
                        .insert("fragment_parent".into(), float(parent_slot as f32));
                    body.params.insert("collider_parts".into(), float(1.0));
                    // A piece is the parent's material; its mass follows
                    // its own hull.
                    body.params.insert(
                        "density".into(),
                        float(
                            number(parent.params.get("density"))
                                .unwrap_or(crate::water::physics::DEFAULT_DENSITY),
                        ),
                    );
                    wire(def, authored_pose.clone(), body_copy, "transform");
                    if let Some(local) = &local {
                        wire(def, local.clone(), body_copy, "source_transform");
                    }
                    wire(
                        def,
                        (body_copy, "body".into()),
                        world,
                        &format!("body_{body_slot}"),
                    );
                    if let Some(parent_acceleration) = &parent_acceleration {
                        wire(
                            def,
                            parent_acceleration.clone(),
                            world,
                            &format!("body_acceleration_{body_slot}"),
                        );
                    }
                    let object_copy = clone_node(
                        def,
                        &original,
                        copy_id(&instance.id, &original.node_id, "object", piece),
                    )?;
                    let object_id = def.nodes.last().expect("cloned object").node_id.clone();
                    wire(
                        def,
                        (source_copy, "vertices".into()),
                        object_copy,
                        "vertices",
                    );
                    wire(
                        def,
                        (world, format!("pose_{body_slot}")),
                        object_copy,
                        if nested { "parent_transform" } else { "transform" },
                    );
                    for old in &incoming {
                        wire(
                            def,
                            (old.from_node, old.from_port.clone()),
                            object_copy,
                            &old.to_port,
                        );
                    }
                    let port = if piece == 0 {
                        first_port.clone()
                    } else {
                        let next = def
                            .wires
                            .iter()
                            .filter(|w| w.to_node == scene)
                            .filter_map(|w| {
                                w.to_port
                                    .strip_prefix("object_")
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .max()
                            .map_or(0, |n| n + 1);
                        format!("object_{next}")
                    };
                    wire(def, (object_copy, "object".into()), scene, &port);
                    if let Some(metadata) = &mut def.preset_metadata {
                        let old_len = metadata.bindings.len();
                        for i in 0..old_len {
                            if let BindingTarget::Node { node_id, .. } =
                                &metadata.bindings[i].target
                                && *node_id == original.node_id
                            {
                                let mut binding = metadata.bindings[i].clone();
                                if let BindingTarget::Node { node_id, .. } = &mut binding.target {
                                    *node_id = object_id.clone();
                                }
                                metadata.bindings.push(binding);
                                binding_sources.push(binding_sources[i].clone());
                            }
                        }
                    }
                }
            }
            let object_count = def
                .wires
                .iter()
                .filter(|w| w.to_node == scene)
                .filter_map(|w| {
                    w.to_port
                        .strip_prefix("object_")
                        .and_then(|s| s.parse::<usize>().ok())
                })
                .max()
                .map_or(0, |n| n + 1);
            def.nodes
                .iter_mut()
                .find(|n| n.id == scene)
                .expect("scene exists")
                .params
                .insert("objects".into(), float(object_count as f32));
        }
    }
    Ok(())
}
