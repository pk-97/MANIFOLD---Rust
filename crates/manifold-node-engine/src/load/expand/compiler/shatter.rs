//! Internal fragment graphs. The saved scene keeps one logical object.
use super::super::{SceneModifierNodeRoute, bindings::SceneModifierBindingSource};
use super::*;
use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::physics_members::{ACCELERATION_ALIAS, BODY_TYPE_ID, Family, world_member};
use manifold_foundation::mesh_partition::MAX_PIECES;

/// An imported mesh's source: its pieces are cut by its own fragment params.
const GLTF_SOURCE: &str = "node.gltf_mesh_source";

/// The shape `body` collides `part` with: its single `source`, or the
/// `mesh_k` whose `part_k` places the part (the transform the part draws by).
fn body_shape(def: &EffectGraphDef, body: u32, part: u32) -> Option<PortAddress> {
    if let Some(source) = input(def, body, "source") {
        return Some(source);
    }
    let placed = input(def, part, "transform")?;
    (0..)
        .map_while(|k| input(def, body, &format!("part_{k}")).map(|at| (k, at)))
        .find(|(_, at)| *at == placed)
        .and_then(|(k, _)| input(def, body, &format!("mesh_{k}")))
}

/// The rigid body that poses `object`: wired straight to its pose, or
/// through the transforms it is placed in (a body's pose frame, the groups
/// inside a group body).
fn posing_body(def: &EffectGraphDef, object: u32) -> Option<u32> {
    let mut at = input(def, object, "parent_transform").or_else(|| input(def, object, "transform"))?;
    for _ in 0..def.nodes.len() {
        let producer = def.nodes.iter().find(|n| n.id == at.0)?;
        if producer.type_id == BODY_TYPE_ID {
            return (at.1 == "pose").then_some(at.0);
        }
        if producer.type_id != TRANSFORM_TYPE_ID {
            return None;
        }
        at = input(def, at.0, "parent")?;
    }
    None
}

const TRANSFORM_TYPE_ID: &str = "node.transform_3d";

/// A `node.mesh_fragment` cutting piece `index` of `count`.
fn fragment_node(def: &mut EffectGraphDef, id: NodeId, count: usize, index: usize) -> Result<u32, SceneModifierExpandError> {
    let template = EffectGraphNode {
        id: 0,
        node_id: NodeId::default(),
        type_id: "node.mesh_fragment".into(),
        params: [("fragment_count".into(), float(count as f32)), ("fragment_index".into(), float(index as f32))].into_iter().collect(),
        ..serde_json::from_value(serde_json::json!({"id": 0, "typeId": "node.mesh_fragment"}))
            .map_err(|error| invalid("shatter", error.to_string()))?
    };
    clone_node(def, &template, id)
}

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
        .find(|r| r.modifier_id == *modifier && r.local == *local)
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
    registry: &PrimitiveRegistry,
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
        if !count.is_finite() || !(2.0..=MAX_PIECES as f32).contains(&count) {
            return Err(invalid("shatter", format!("Pieces must be between 2 and {MAX_PIECES}")));
        }
        let count = count.round() as usize;
        let release = (
            generated(def, routes, &instance.id, &shatter.trigger_node)?.id,
            shatter.trigger_port.clone(),
        );
        let scene = def.nodes.iter().find(|n| n.node_id == instance.scene)
            .ok_or_else(|| invalid("shatter", "prepared scene is missing"))?.id;
        let targets = frames::selected_objects(index, instance)?;
        if targets.is_empty() {
            return Err(invalid(
                "shatter",
                "select an object whose Physics is Body",
            ));
        }
        let mut assemblies: BTreeMap<(u32, usize), (u32, Vec<u32>)> = BTreeMap::new();
        for target in targets {
            let object = def.nodes.iter().find(|n| n.node_id == target)
                .ok_or_else(|| invalid("shatter", "prepared object is missing"))?.id;
            let body = posing_body(def, object).ok_or_else(|| {
                invalid("shatter", "enable Physics on the object before adding Shatter")
            })?;
            let member = world_member(&def.nodes, &def.wires, body)
                .filter(|member| member.family == Family::Body)
                .ok_or_else(|| invalid("shatter", "object has no rigid body pose"))?;
            assemblies
                .entry((member.world, member.k as usize))
                .or_insert_with(|| (body, Vec::new()))
                .1
                .push(object);
        }
        for ((world, parent_slot), (parent_body, parts)) in assemblies {
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
            let parent = node(def, parent_body)?.clone();
            let parent_acceleration = input(def, parent.id, ACCELERATION_ALIAS);
            let mass = number(parent.params.get("mass"))
                .or_else(|| {
                    registry
                        .construct(&parent.type_id)?
                        .parameters()
                        .iter()
                        .find(|param| param.name == "mass")?
                        .default
                        .as_scalar()
                })
                .ok_or_else(|| invalid("shatter", "parent body has no numeric mass default"))?;
            wire(def, release.clone(), parent.id, "release_count");
            let authored_pose = input(def, parent.id, "transform")
                .ok_or_else(|| invalid("shatter", "parent authored transform is missing"))?;
            let slots: Vec<u32> = manifold_core::physics_body_slots::BodySlots::of(&def.wires, world)
                .free_slots()
                .take(count)
                .collect();
            // Distribute the total across materials; every original triangle
            // and its attributes belong to exactly one resulting fragment.
            let mut allocation = vec![1usize; parts.len()];
            let mut sources = Vec::with_capacity(parts.len());
            // An imported part breaks in its file source; any other part
            // breaks from the shape its body collides with.
            let mut shapes = Vec::with_capacity(parts.len());
            for &part in &parts {
                let drawn = input(def, part, "vertices")
                    .ok_or_else(|| invalid("shatter", "mesh source is missing"))?;
                let source = node(def, drawn.0)?.clone();
                let shape = if source.type_id == GLTF_SOURCE {
                    None
                } else {
                    Some(body_shape(def, parent_body, part).ok_or_else(|| {
                        invalid("shatter", "the object's body has no shape to break")
                    })?)
                };
                shapes.push((drawn, shape));
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
            for (((part, source), pieces), (drawn, shape)) in parts.into_iter().zip(sources).zip(allocation).zip(shapes) {
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
                    let mesh_id = copy_id(&instance.id, &original.node_id, "mesh", piece);
                    let body_copy = clone_node(
                        def,
                        &parent,
                        copy_id(&instance.id, &original.node_id, "body", piece),
                    )?;
                    let body = def.nodes.last_mut().expect("cloned body");
                    body.params.remove("compound_materials");
                    // A piece is one shape, not the parent's parts.
                    body.params.remove("parts");
                    body.params
                        .insert("enabled".into(), SerializedParamValue::Bool { value: true });
                    body.params.insert("motion".into(), SerializedParamValue::Enum { value: 1 });
                    body.params
                        .insert("fragment_count".into(), float(pieces as f32));
                    body.params
                        .insert("fragment_index".into(), float(piece as f32));
                    body.params
                        .insert("fragment_parent".into(), float(parent_slot as f32));
                    // A piece is the parent's material: on release the world gives
                    // it the parent's density over its own hull; this is the placeholder.
                    body.params.insert("mass".into(), float(mass));
                    let source_copy = match &shape {
                        // The piece is cut from the shape on the CPU; its body
                        // collides with the same piece.
                        Some(shape) => {
                            let fragment = fragment_node(def, mesh_id, pieces, piece)?;
                            wire(def, shape.clone(), fragment, "shape");
                            wire(def, drawn.clone(), fragment, "in");
                            wire(def, (fragment, "source".into()), body_copy, "source");
                            fragment
                        }
                        None => {
                            let source_copy = clone_node(def, &source, mesh_id)?;
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
                            let body = def.nodes.iter_mut().find(|n| n.id == body_copy).expect("cloned body");
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
                            source_copy
                        }
                    };
                    wire(def, authored_pose.clone(), body_copy, "transform");
                    if let Some(local) = &local {
                        wire(def, local.clone(), body_copy, "source_transform");
                    }
                    wire(
                        def,
                        (body_copy, "body".into()),
                        world,
                        &Family::Body.member_port(body_slot),
                    );
                    if let Some(parent_acceleration) = &parent_acceleration {
                        wire(
                            def,
                            parent_acceleration.clone(),
                            body_copy,
                            ACCELERATION_ALIAS,
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
                        (body_copy, "pose".into()),
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
            manifold_core::physics_body_slots::fit_world_bodies(&mut def.nodes, &def.wires, world);
        }
    }
    Ok(())
}
