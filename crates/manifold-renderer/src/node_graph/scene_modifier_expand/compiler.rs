//! Compose authored modifier stages into ordinary graph nodes and wires.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::scene_modifier_preset::{
    SceneContextValue, SceneEndpoint, SceneModifierInstanceDef, SceneNodeRef, SceneStageScope,
    SceneStageSource, validate_scene_modifier_schema,
};
use sha2::{Digest, Sha256};

use crate::node_graph::persistence::{EffectGraphDefExt, PrimitiveRegistry};

use super::{SceneModifierExpandError, bindings, frames, index::FlatSceneIndex, namespace};

type PortAddress = (u32, String);
type EndpointKey = (SceneNodeRef, &'static str);
type CloneKey = (u32, Option<SceneNodeRef>);
type LeafMap = BTreeMap<String, Vec<NodeId>>;

#[cfg(test)]
mod conformance;

#[cfg(test)]
mod tests;

fn invalid(path: impl Into<String>, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidRecipe {
        path: path.into(),
        detail: detail.into(),
    }
}

fn endpoint_port(endpoint: SceneEndpoint) -> &'static str {
    match endpoint {
        SceneEndpoint::Camera => "camera",
        SceneEndpoint::Atmosphere => "atmosphere",
        SceneEndpoint::Transform => "transform",
        SceneEndpoint::Instances => "instances",
        SceneEndpoint::Vertices => "vertices",
    }
}

fn endpoint_scope(endpoint: SceneEndpoint) -> SceneStageScope {
    match endpoint {
        SceneEndpoint::Camera | SceneEndpoint::Atmosphere => SceneStageScope::Scene,
        _ => SceneStageScope::EachObject,
    }
}

/// Validate a complete proposed attachment using the same expansion as load.
pub fn validate_modifier_attachment(
    owner: &EffectGraphDef,
    instance: &SceneModifierInstanceDef,
    registry: &PrimitiveRegistry,
) -> Result<(), SceneModifierExpandError> {
    let mut candidate = owner.clone();
    if let Some(existing) = candidate
        .scene_modifiers
        .iter_mut()
        .find(|item| item.id == instance.id)
    {
        *existing = instance.clone();
    } else {
        candidate.scene_modifiers.push(instance.clone());
    }
    candidate.version = candidate.version.max(3);
    expand_scene_modifiers(&candidate, registry).map(|_| ())
}

/// Produce a derived graph. All saved definitions and target frames remain
/// untouched; a second preparation of the derived graph is an exact no-op.
pub fn expand_scene_modifiers(
    owner: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<EffectGraphDef, SceneModifierExpandError> {
    if owner.scene_modifiers.len() > 16 {
        return Err(SceneModifierExpandError::CapacityExceeded {
            path: "sceneModifiers".into(),
            detail: "an owner supports at most 16 modifiers".into(),
        });
    }
    validate_scene_modifier_schema(owner)?;
    if owner
        .preset_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.scene_modifier.is_some())
    {
        return Err(SceneModifierExpandError::MissingScene {
            path: "presetMetadata.sceneModifier".into(),
            detail: "a standalone recipe must be attached to a host scene before preparation"
                .into(),
        });
    }
    if owner.scene_modifiers.is_empty() {
        return Ok(owner.clone());
    }
    let index = FlatSceneIndex::build(owner)?;
    preflight_expansion(owner, &index)?;
    let mut builder = Builder {
        next_id: index
            .flat
            .nodes
            .iter()
            .map(|node| node.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| invalid("graph", "numeric node IDs exhausted"))?,
        derived: index.flat.clone(),
        index: &index,
        current: BTreeMap::new(),
        reference: BTreeMap::new(),
        written: BTreeSet::new(),
        contexts: BTreeMap::new(),
    };
    let mut leaf_maps = BTreeMap::new();
    let mut singletons = BTreeSet::new();
    for instance in &owner.scene_modifiers {
        let recipe = instance
            .graph
            .preset_metadata
            .as_ref()
            .and_then(|metadata| metadata.scene_modifier.as_ref())
            .ok_or_else(|| invalid(instance.id.to_string(), "instance has no recipe"))?;
        if recipe.singleton {
            let id = &instance
                .graph
                .preset_metadata
                .as_ref()
                .expect("recipe metadata exists")
                .id;
            if !singletons.insert((instance.scene.clone(), id.as_str().to_string())) {
                return Err(invalid(
                    instance.id.to_string(),
                    "singleton recipe is already applied to this scene",
                ));
            }
        }
        frames::validate_saved_frames(owner, &index, instance)?;
        let targets = frames::selected_objects(&index, instance)?;
        let leaves = builder.append_instance(owner, instance, &targets)?;
        leaf_maps.insert(instance.id.to_string(), leaves);
    }
    for key in &builder.written {
        let target = *index
            .by_ref
            .get(&key.0)
            .ok_or_else(|| invalid(format!("{:?}", key.0), "attachment target disappeared"))?;
        let (producer, port) = builder
            .current
            .get(key)
            .and_then(|value| value.as_ref())
            .ok_or_else(|| invalid(format!("{:?}", key.0), "stage has no final producer"))?;
        builder
            .derived
            .wires
            .retain(|wire| !(wire.to_node == target && wire.to_port == key.1));
        builder.derived.wires.push(EffectGraphWire {
            from_node: *producer,
            from_port: port.clone(),
            to_node: target,
            to_port: key.1.into(),
        });
    }
    builder.derived.name = owner.name.clone();
    builder.derived.description = owner.description.clone();
    builder.derived.preset_metadata = bindings::expand_bindings(owner, &leaf_maps)?;
    // Host leaves have already crossed their group boundaries. Temporarily
    // remove their flattened display handles so the group flattener does not
    // mistake its own '/' separators for newly authored invalid handles.
    let host_handles: BTreeMap<_, _> = index
        .flat
        .nodes
        .iter()
        .map(|node| (node.node_id.clone(), node.handle.clone()))
        .map(|(id, handle)| (id.to_string(), handle))
        .collect();
    for node in &mut builder.derived.nodes {
        if host_handles.contains_key(node.node_id.as_str()) {
            node.handle = None;
        }
    }
    let mut flat = manifold_core::flatten::flatten_groups(&builder.derived)
        .map_err(|error| invalid("expandedGraph", error.to_string()))?;
    for node in &mut flat.nodes {
        if let Some(handle) = host_handles.get(node.node_id.as_str()) {
            node.handle = handle.clone();
        }
    }
    if flat.nodes.len() > 65_536 || flat.wires.len() > 262_144 {
        return Err(SceneModifierExpandError::CapacityExceeded {
            path: "expandedGraph".into(),
            detail: "expanded graph exceeds 65536 nodes or 262144 wires".into(),
        });
    }
    // This also rejects collisions between generated IDs and authored host IDs.
    FlatSceneIndex::build(&flat)?;
    let prepared = bindings::seed_local_defaults(&flat)?;
    let graph = prepared
        .clone()
        .into_graph(registry)
        .map_err(|error| invalid("expandedGraph", error.to_string()))?;
    validate_binding_leaves(&prepared, &graph)?;
    crate::node_graph::validation::validate(&graph)
        .map_err(|error| invalid("expandedGraph", error.to_string()))?;
    Ok(prepared)
}

fn validate_binding_leaves(
    def: &EffectGraphDef,
    graph: &crate::node_graph::Graph,
) -> Result<(), SceneModifierExpandError> {
    use manifold_core::effect_graph_def::BindingTarget;
    let Some(metadata) = &def.preset_metadata else {
        return Ok(());
    };
    for (id, target, string) in metadata
        .bindings
        .iter()
        .map(|binding| (&binding.id, &binding.target, false))
        .chain(
            metadata
                .string_bindings
                .iter()
                .map(|binding| (&binding.id, &binding.target, true)),
        )
    {
        let BindingTarget::Node { node_id, param } = target else {
            if matches!(target, BindingTarget::SceneModifier { .. }) {
                return Err(invalid(id, "unexpanded modifier binding"));
            }
            continue;
        };
        let node = graph
            .instance_by_node_id(node_id)
            .and_then(|id| graph.get_node(id));
        let definition = node.and_then(|node| {
            node.node
                .parameters()
                .iter()
                .find(|definition| definition.name.as_ref() == param)
        });
        let definition = definition.ok_or_else(|| SceneModifierExpandError::InvalidBinding {
            path: id.clone(),
            detail: format!(
                "binding leaf {node_id}.{param} does not name a real primitive parameter"
            ),
        })?;
        if string != (definition.ty == crate::node_graph::parameters::ParamType::String) {
            return Err(SceneModifierExpandError::InvalidBinding {
                path: id.clone(),
                detail: "numeric/string binding kind does not match its primitive parameter".into(),
            });
        }
    }
    Ok(())
}

struct Builder<'a> {
    derived: EffectGraphDef,
    index: &'a FlatSceneIndex,
    next_id: u32,
    current: BTreeMap<EndpointKey, Option<PortAddress>>,
    reference: BTreeMap<EndpointKey, Option<PortAddress>>,
    written: BTreeSet<EndpointKey>,
    contexts: BTreeMap<String, PortAddress>,
}

fn preflight_expansion(
    owner: &EffectGraphDef,
    index: &FlatSceneIndex,
) -> Result<(), SceneModifierExpandError> {
    fn count(
        node: &EffectGraphNode,
        depth: usize,
    ) -> Result<(usize, usize), SceneModifierExpandError> {
        if depth > 64 {
            return Err(invalid(
                node.node_id.to_string(),
                "modifier group depth exceeds 64",
            ));
        }
        let mut nodes = 1_usize;
        let mut wires = 0_usize;
        if let Some(group) = &node.group {
            wires = group.wires.len();
            for child in &group.nodes {
                let (child_nodes, child_wires) = count(child, depth + 1)?;
                nodes = nodes.saturating_add(child_nodes);
                wires = wires.saturating_add(child_wires);
                if nodes > 65_536 || wires > 262_144 {
                    return Err(capacity(node.node_id.as_str()));
                }
            }
        }
        Ok((nodes, wires))
    }
    fn capacity(path: &str) -> SceneModifierExpandError {
        SceneModifierExpandError::CapacityExceeded { path: path.into(), detail: "modifier preparation exceeds 65536 nodes or 262144 wires, including group boundaries and context inputs".into() }
    }
    let mut node_count = index.flat.nodes.len();
    let mut wire_count = index.flat.wires.len();
    for instance in &owner.scene_modifiers {
        let target_count = frames::selected_objects(index, instance)?.len();
        let recipe = instance
            .graph
            .preset_metadata
            .as_ref()
            .and_then(|metadata| metadata.scene_modifier.as_ref())
            .ok_or_else(|| invalid(instance.id.to_string(), "instance has no recipe"))?;
        for node in &instance.graph.nodes {
            let (nodes, wires) = count(node, 0)?;
            let stage = recipe
                .stages
                .iter()
                .find(|stage| stage.group == node.node_id);
            let multiplier =
                if stage.is_some_and(|stage| stage.scope == SceneStageScope::EachObject) {
                    target_count
                } else {
                    1
                };
            let inputs = stage.map_or(0, |stage| stage.inputs.len());
            node_count =
                node_count.saturating_add(nodes.saturating_add(inputs).saturating_mul(multiplier));
            wire_count =
                wire_count.saturating_add(wires.saturating_add(inputs).saturating_mul(multiplier));
        }
        wire_count = wire_count.saturating_add(
            instance
                .graph
                .wires
                .len()
                .saturating_mul(target_count.max(1)),
        );
        if node_count > 65_536 || wire_count > 262_144 {
            return Err(capacity(instance.id.as_str()));
        }
    }
    Ok(())
}

impl Builder<'_> {
    fn constant_node(
        &mut self,
        key: String,
        type_id: &str,
        params: BTreeMap<String, SerializedParamValue>,
        port: &str,
    ) -> Result<PortAddress, SceneModifierExpandError> {
        if let Some(address) = self.contexts.get(&key) {
            return Ok(address.clone());
        }
        let id = self.next_id;
        self.next_id = id
            .checked_add(1)
            .ok_or_else(|| invalid(&key, "numeric node IDs exhausted"))?;
        let node_id = namespace::namespace_node_id(&["context", &key]);
        self.derived.nodes.push(EffectGraphNode {
            id,
            handle: Some(node_id.to_string()),
            node_id,
            type_id: type_id.into(),
            params,
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        });
        let address = (id, port.to_string());
        self.contexts.insert(key, address.clone());
        Ok(address)
    }

    fn endpoint_input(
        &mut self,
        instance: &SceneModifierInstanceDef,
        key: &EndpointKey,
        endpoint: SceneEndpoint,
        reference: bool,
    ) -> Result<PortAddress, SceneModifierExpandError> {
        let table = if reference {
            &self.reference
        } else {
            &self.current
        };
        if let Some(Some(address)) = table.get(key) {
            return Ok(address.clone());
        }
        let identity_key = serde_json::to_string(&("identity", &key.0, key.1))
            .map_err(|error| invalid(instance.id.to_string(), error.to_string()))?;
        let mut params = BTreeMap::new();
        let address = match endpoint {
            SceneEndpoint::Transform => {
                self.constant_node(identity_key, "node.transform_3d", params, "transform")?
            }
            SceneEndpoint::Instances => {
                for (key, value) in [("max_capacity", 1), ("active_count", 1)] {
                    params.insert(key.into(), SerializedParamValue::Int { value });
                }
                for key in ["extent_x", "extent_y", "extent_z"] {
                    params.insert(key.into(), SerializedParamValue::Float { value: 0.0 });
                }
                params.insert(
                    "base_scale".into(),
                    SerializedParamValue::Float { value: 1.0 },
                );
                self.constant_node(identity_key, "node.arrange_copies", params, "instances")?
            }
            _ => {
                return Err(SceneModifierExpandError::MissingInput {
                    path: format!("{:?}.{}", key.0, key.1),
                    detail: "stage requires an existing endpoint producer".into(),
                });
            }
        };
        if self.reference.get(key).is_some_and(Option::is_none) {
            self.reference.insert(key.clone(), Some(address.clone()));
        }
        if self.current.get(key).is_some_and(Option::is_none) {
            self.current.insert(key.clone(), Some(address.clone()));
        }
        Ok(address)
    }

    fn context(
        &mut self,
        owner: &EffectGraphDef,
        instance: &SceneModifierInstanceDef,
        target: Option<&SceneNodeRef>,
        targets: &[SceneNodeRef],
        value: SceneContextValue,
    ) -> Result<PortAddress, SceneModifierExpandError> {
        if matches!(
            value,
            SceneContextValue::Time | SceneContextValue::Beat | SceneContextValue::TriggerCount
        ) {
            let mut sources = self
                .index
                .flat
                .nodes
                .iter()
                .filter(|node| node.type_id == "system.generator_input");
            let source = sources.next().ok_or_else(|| {
                invalid(
                    instance.id.to_string(),
                    "live context requires a generator input boundary",
                )
            })?;
            if sources.next().is_some() {
                return Err(invalid(
                    instance.id.to_string(),
                    "live context has multiple generator input boundaries",
                ));
            }
            return Ok((
                source.id,
                match value {
                    SceneContextValue::Time => "time",
                    SceneContextValue::Beat => "beat",
                    _ => "trigger_count",
                }
                .into(),
            ));
        }
        let per_object = matches!(
            value,
            SceneContextValue::ObjectOrdinal
                | SceneContextValue::ObjectSeed
                | SceneContextValue::SourceOffsetX
                | SceneContextValue::SourceOffsetY
                | SceneContextValue::SourceOffsetZ
        );
        if per_object && target.is_none() {
            return Err(invalid(
                instance.id.to_string(),
                "per-object context is unavailable in a shared Scene stage",
            ));
        }
        let key = serde_json::to_string(&(
            instance.id.as_str(),
            if per_object { target } else { None },
            value,
        ))
        .map_err(|error| invalid(instance.id.to_string(), error.to_string()))?;
        if let Some(address) = self.contexts.get(&key) {
            return Ok(address.clone());
        }
        let mut params = BTreeMap::new();
        if matches!(
            value,
            SceneContextValue::SceneMin | SceneContextValue::SceneMax
        ) {
            let (min, max) = owner
                .preset_metadata
                .as_ref()
                .and_then(|metadata| metadata.scene_bounds)
                .ok_or_else(|| {
                    invalid(
                        instance.id.to_string(),
                        "scene bounds context requires imported bounds",
                    )
                })?;
            if (0..3).any(|axis| {
                !min[axis].is_finite() || !max[axis].is_finite() || min[axis] > max[axis]
            }) {
                return Err(invalid(
                    instance.id.to_string(),
                    "scene bounds must be finite and ordered",
                ));
            }
            let vector = if value == SceneContextValue::SceneMin {
                min
            } else {
                max
            };
            for (name, value) in ["x", "y", "z"].into_iter().zip(vector) {
                params.insert(name.into(), SerializedParamValue::Float { value });
            }
            return self.constant_node(key, "node.compose_vec3", params, "out");
        }
        let scalar = match value {
            SceneContextValue::ObjectCount => targets.len() as f64,
            SceneContextValue::ObjectOrdinal => targets
                .iter()
                .position(|candidate| Some(candidate) == target)
                .ok_or_else(|| {
                    invalid(instance.id.to_string(), "object ordinal target is missing")
                })? as f64,
            SceneContextValue::ObjectSeed => {
                let bytes = serde_json::to_vec(&target)
                    .map_err(|error| invalid(instance.id.to_string(), error.to_string()))?;
                let digest = Sha256::digest(bytes);
                f64::from(u32::from_be_bytes([0, digest[0], digest[1], digest[2]]))
            }
            SceneContextValue::SceneRadius
            | SceneContextValue::SourceOffsetX
            | SceneContextValue::SourceOffsetY
            | SceneContextValue::SourceOffsetZ => {
                let frame = if per_object {
                    instance
                        .mesh_frames
                        .iter()
                        .find(|frame| Some(&frame.target) == target)
                } else {
                    instance.mesh_frames.first()
                }
                .ok_or_else(|| {
                    invalid(
                        instance.id.to_string(),
                        "coordinate context has no saved frame",
                    )
                })?;
                match value {
                    SceneContextValue::SceneRadius => frame.scene_radius,
                    SceneContextValue::SourceOffsetX => frame.source_offset[0],
                    SceneContextValue::SourceOffsetY => frame.source_offset[1],
                    _ => frame.source_offset[2],
                }
            }
            _ => {
                return Err(invalid(
                    instance.id.to_string(),
                    "unsupported scalar context",
                ));
            }
        };
        if !scalar.is_finite() || !(scalar as f32).is_finite() {
            return Err(invalid(
                instance.id.to_string(),
                "context value is not finite f32",
            ));
        }
        params.insert(
            "value".into(),
            SerializedParamValue::Float {
                value: scalar as f32,
            },
        );
        self.constant_node(key, "node.value", params, "out")
    }

    fn reject_dynamic_rt(
        &self,
        owner: &EffectGraphDef,
        instance: &SceneModifierInstanceDef,
    ) -> Result<(), SceneModifierExpandError> {
        let scene = self.index.node(&instance.scene)?;
        let mut enabled = matches!(
            scene.params.get("rt_enabled"),
            Some(SerializedParamValue::Bool { value: true })
        );
        if let Some(metadata) = &owner.preset_metadata {
            for binding in &metadata.bindings {
                if !binding.default_mirrors_node_param
                    && matches!(&binding.target, manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } if *node_id == scene.node_id && param == "rt_enabled")
                {
                    enabled = matches!(
                        crate::node_graph::param_binding::convert_param_value(
                            binding.convert,
                            binding.default_value * binding.scale + binding.offset
                        ),
                        crate::node_graph::parameters::ParamValue::Bool(true)
                    );
                }
            }
        }
        if enabled {
            return Err(SceneModifierExpandError::UnsupportedRenderMode {
                path: instance.id.to_string(), detail: "vertex modifiers are not qualified for dynamic ray tracing; choose raster rendering or remove the modifier (BUG-e3p6.4)".into(),
            });
        }
        Ok(())
    }

    fn attachment_key(
        &mut self,
        instance: &SceneModifierInstanceDef,
        target: Option<&SceneNodeRef>,
        endpoint: SceneEndpoint,
    ) -> Result<EndpointKey, SceneModifierExpandError> {
        let reference = match endpoint_scope(endpoint) {
            SceneStageScope::Scene => &instance.scene,
            SceneStageScope::EachObject => target.ok_or_else(|| {
                invalid(
                    instance.id.to_string(),
                    "object endpoint used in a Scene stage",
                )
            })?,
        };
        let key = (reference.clone(), endpoint_port(endpoint));
        if !self.current.contains_key(&key) {
            let initial = self
                .index
                .input(reference, key.1)?
                .map(|wire| (wire.from_node, wire.from_port.clone()));
            self.current.insert(key.clone(), initial.clone());
            self.reference.insert(key.clone(), initial);
        }
        Ok(key)
    }

    fn append_instance(
        &mut self,
        owner: &EffectGraphDef,
        instance: &SceneModifierInstanceDef,
        targets: &[SceneNodeRef],
    ) -> Result<LeafMap, SceneModifierExpandError> {
        let local = bindings::seed_local_defaults(&instance.graph)?;
        let recipe = local
            .preset_metadata
            .as_ref()
            .and_then(|metadata| metadata.scene_modifier.as_ref())
            .ok_or_else(|| invalid(instance.id.to_string(), "instance has no recipe"))?;
        let mut stages = BTreeMap::new();
        let mut root_ids = BTreeSet::new();
        for node in &local.nodes {
            if !root_ids.insert(node.id) {
                return Err(invalid(
                    instance.id.to_string(),
                    "duplicate template document ID",
                ));
            }
        }
        for (position, stage) in recipe.stages.iter().enumerate() {
            let node = local
                .nodes
                .iter()
                .find(|node| node.node_id == stage.group)
                .ok_or_else(|| {
                    invalid(stage.group.to_string(), "stage must name a top-level group")
                })?;
            let group = node
                .group
                .as_ref()
                .ok_or_else(|| invalid(stage.group.to_string(), "stage node is not a group"))?;
            if stages.insert(node.id, (position, stage)).is_some() {
                return Err(invalid(
                    stage.group.to_string(),
                    "group appears in multiple stages",
                ));
            }
            if stage.scope == SceneStageScope::EachObject && targets.is_empty() {
                return Err(SceneModifierExpandError::MissingTarget {
                    path: instance.id.to_string(),
                    detail: "EachObject stage needs at least one selected object".into(),
                });
            }
            let mut inputs = BTreeSet::new();
            for input in &stage.inputs {
                if !inputs.insert(&input.port)
                    || !group
                        .interface
                        .inputs
                        .iter()
                        .any(|port| port.name == input.port)
                {
                    return Err(invalid(
                        stage.group.to_string(),
                        format!("unknown or duplicate input '{}'", input.port),
                    ));
                }
            }
            let mut endpoints = BTreeSet::new();
            for output in &stage.outputs {
                if endpoint_scope(output.endpoint) != stage.scope
                    || !endpoints.insert(endpoint_port(output.endpoint))
                    || !group
                        .interface
                        .outputs
                        .iter()
                        .any(|port| port.name == output.port)
                {
                    return Err(SceneModifierExpandError::UnsupportedEndpoint {
                        path: stage.group.to_string(),
                        detail: format!("invalid output endpoint or port '{}'", output.port),
                    });
                }
            }
        }
        let mut copies: BTreeMap<CloneKey, u32> = BTreeMap::new();
        let mut leaves: LeafMap = BTreeMap::new();
        for node in &local.nodes {
            if matches!(
                node.type_id.as_str(),
                "node.render_scene" | "system.final_output" | "system.generator_input"
            ) {
                return Err(invalid(
                    node.node_id.to_string(),
                    "recipe must use typed stages and context instead of host boundaries",
                ));
            }
            let each = stages
                .get(&node.id)
                .is_some_and(|(_, stage)| stage.scope == SceneStageScope::EachObject);
            let selected: Vec<Option<&SceneNodeRef>> = if each {
                targets.iter().map(Some).collect()
            } else {
                vec![None]
            };
            for target in selected {
                let mut parts = vec![
                    "instance",
                    instance.id.as_str(),
                    if each { "object" } else { "shared" },
                ];
                if let Some(target) = target {
                    parts.extend(target.scope.iter().map(NodeId::as_str));
                    parts.push(target.node.as_str());
                }
                let (copy, map) = namespace::clone_template_node(node, &parts, &mut self.next_id)?;
                copies.insert((node.id, target.cloned()), copy.id);
                for (old, new) in map {
                    leaves.entry(old).or_default().push(new);
                }
                self.derived.nodes.push(copy);
            }
        }
        for wire in &local.wires {
            let from_each = stages
                .get(&wire.from_node)
                .is_some_and(|(_, stage)| stage.scope == SceneStageScope::EachObject);
            let to_each = stages
                .get(&wire.to_node)
                .is_some_and(|(_, stage)| stage.scope == SceneStageScope::EachObject);
            if from_each && !to_each {
                return Err(invalid(
                    instance.id.to_string(),
                    "EachObject output cannot feed a shared consumer without an explicit reduction",
                ));
            }
            if let (Some((from, _)), Some((to, _))) =
                (stages.get(&wire.from_node), stages.get(&wire.to_node))
                && from >= to
            {
                return Err(invalid(
                    instance.id.to_string(),
                    "ordinary stage wires must follow recipe order",
                ));
            }
            let selected: Vec<Option<&SceneNodeRef>> = if to_each {
                targets.iter().map(Some).collect()
            } else {
                vec![None]
            };
            for target in selected {
                let from = copies
                    .get(&(
                        wire.from_node,
                        if from_each { target.cloned() } else { None },
                    ))
                    .ok_or_else(|| {
                        invalid(
                            instance.id.to_string(),
                            "ordinary wire has an unknown producer",
                        )
                    })?;
                let to = copies
                    .get(&(wire.to_node, target.cloned()))
                    .ok_or_else(|| {
                        invalid(
                            instance.id.to_string(),
                            "ordinary wire has an unknown consumer",
                        )
                    })?;
                self.derived.wires.push(EffectGraphWire {
                    from_node: *from,
                    from_port: wire.from_port.clone(),
                    to_node: *to,
                    to_port: wire.to_port.clone(),
                });
            }
        }
        for (position, stage) in recipe.stages.iter().enumerate() {
            let root = local
                .nodes
                .iter()
                .find(|node| node.node_id == stage.group)
                .expect("stage resolved above");
            let selected: Vec<Option<&SceneNodeRef>> = if stage.scope == SceneStageScope::EachObject
            {
                targets.iter().map(Some).collect()
            } else {
                vec![None]
            };
            for target in selected {
                let copy = copies[&(root.id, target.cloned())];
                for input in &stage.inputs {
                    let producer = match &input.source {
                        SceneStageSource::Previous { endpoint }
                        | SceneStageSource::Reference { endpoint } => {
                            if endpoint_scope(*endpoint) != stage.scope {
                                return Err(invalid(
                                    stage.group.to_string(),
                                    "endpoint input has the wrong stage scope",
                                ));
                            }
                            let key = self.attachment_key(instance, target, *endpoint)?;
                            let reference =
                                matches!(input.source, SceneStageSource::Reference { .. });
                            self.endpoint_input(instance, &key, *endpoint, reference)?
                        }
                        SceneStageSource::Context { value } => {
                            self.context(owner, instance, target, targets, *value)?
                        }
                        SceneStageSource::StageOutput {
                            stage: source,
                            port,
                        } => {
                            let source_position = recipe
                                .stages
                                .iter()
                                .position(|candidate| candidate.group == *source)
                                .filter(|source| *source < position)
                                .ok_or_else(|| {
                                    invalid(
                                        stage.group.to_string(),
                                        "StageOutput must name an earlier stage",
                                    )
                                })?;
                            let source_stage = &recipe.stages[source_position];
                            let source_root = local
                                .nodes
                                .iter()
                                .find(|node| node.node_id == source_stage.group)
                                .expect("stage resolved above");
                            let source_target = match source_stage.scope {
                                SceneStageScope::Scene => None,
                                SceneStageScope::EachObject => Some(
                                    target
                                        .ok_or_else(|| {
                                            invalid(
                                                stage.group.to_string(),
                                                "EachObject output cannot feed Scene stage",
                                            )
                                        })?
                                        .clone(),
                                ),
                            };
                            if !source_root
                                .group
                                .as_ref()
                                .expect("stage is group")
                                .interface
                                .outputs
                                .iter()
                                .any(|output| output.name == *port)
                            {
                                return Err(invalid(
                                    stage.group.to_string(),
                                    "StageOutput port is not declared",
                                ));
                            }
                            (copies[&(source_root.id, source_target)], port.clone())
                        }
                    };
                    self.derived.wires.push(EffectGraphWire {
                        from_node: producer.0,
                        from_port: producer.1,
                        to_node: copy,
                        to_port: input.port.clone(),
                    });
                }
                for output in &stage.outputs {
                    let key = self.attachment_key(instance, target, output.endpoint)?;
                    let consumes_previous = stage.inputs.iter().any(|input| matches!(input.source, SceneStageSource::Previous { endpoint } if endpoint == output.endpoint));
                    if !consumes_previous
                        && matches!(
                            output.endpoint,
                            SceneEndpoint::Instances | SceneEndpoint::Atmosphere
                        )
                        && self.current.get(&key).is_some_and(Option::is_some)
                    {
                        return Err(SceneModifierExpandError::ConflictingSource {
                            path: stage.group.to_string(),
                            detail: format!(
                                "source stage would replace existing {} producer",
                                key.1
                            ),
                        });
                    }
                    if output.endpoint == SceneEndpoint::Vertices {
                        self.reject_dynamic_rt(owner, instance)?;
                    }
                    self.current
                        .insert(key.clone(), Some((copy, output.port.clone())));
                    self.written.insert(key);
                }
            }
        }
        Ok(leaves)
    }
}
