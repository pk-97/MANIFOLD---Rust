//! Compose authored modifier stages into ordinary graph nodes and wires.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::scene_modifier_preset::{
    SceneContextValue, SceneEndpoint, SceneModifierInstanceDef, SceneNodeRef, SceneStageScope,
    SceneStageSource, is_force_recipe, validate_scene_modifier_schema,
};
use sha2::{Digest, Sha256};

use crate::ports::PortType;
use crate::persistence::{EffectGraphDefExt, PrimitiveRegistry};

use manifold_core::scene_index::FlatSceneIndex;

use {super::SceneModifierExpandError, super::bindings, super::frames, super::namespace, crate::load::expand::routes, super::routes::PreparedSceneModifierGraph};

manifold_core::testkit_visible! {
#[doc(hidden)]
pub(crate) type PortAddress = (u32, String);
}
manifold_core::testkit_visible! {
#[doc(hidden)]
pub(crate) type EndpointKey = (SceneNodeRef, String);
}
type CloneKey = (u32, Option<SceneNodeRef>);
type LeafMap = BTreeMap<String, Vec<NodeId>>;
manifold_core::testkit_visible! {
/// Stable identity of a node Math View generates for one sampled object.
pub(crate) fn math_resource_node_id(modifier: &NodeId, target: &SceneNodeRef, role: &str) -> NodeId {
    let mut parts = vec!["math_view", modifier.as_str(), role];
    parts.extend(target.scope.iter().map(NodeId::as_str));
    parts.push(target.node.as_str());
    namespace::namespace_node_id(&parts)
}
}
pub(crate) mod shatter;



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
        SceneEndpoint::RenderMode => "render_mode",
        SceneEndpoint::Transform => "transform",
        SceneEndpoint::Instances => "instances",
        SceneEndpoint::Vertices => "vertices",
        SceneEndpoint::Acceleration => "acceleration",
    }
}

fn endpoint_scope(endpoint: SceneEndpoint) -> SceneStageScope {
    match endpoint {
        SceneEndpoint::Camera | SceneEndpoint::Atmosphere | SceneEndpoint::RenderMode => {
            SceneStageScope::Scene
        }
        _ => SceneStageScope::EachObject,
    }
}

/// Recheck prepared scene targets after initial manifest values are applied.
/// Serialized defaults alone cannot decide whether the live owner is valid.
pub fn validate_modifier_runtime(
    owner: &EffectGraphDef,
    graph: &crate::graph::Graph,
) -> Result<(), SceneModifierExpandError> {
    for instance in &owner.scene_modifiers {
        let writes_vertices = instance
            .graph
            .preset_metadata
            .as_ref()
            .and_then(|metadata| metadata.scene_modifier.as_ref())
            .is_some_and(|recipe| {
                recipe.stages.iter().any(|stage| {
                    stage
                        .outputs
                        .iter()
                        .any(|output| output.endpoint == SceneEndpoint::Vertices)
                })
            });
        if !writes_vertices {
            continue;
        }
        graph
            .instance_by_node_id(&instance.scene.node)
            .and_then(|id| graph.get_node(id))
            .ok_or_else(|| invalid(instance.id.to_string(), "prepared scene target is absent"))?;
    }
    Ok(())
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

/// Expand scene modifiers, then flatten groups: the one way to get a flat view of
/// any preset that may carry modifiers. Plain `flatten_groups` refuses those.
pub fn flatten_expanded(
    owner: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<EffectGraphDef, String> {
    let expanded = expand_scene_modifiers(owner, registry).map_err(|e| format!("{e:?}"))?;
    manifold_core::flatten::flatten_groups(&expanded).map_err(|e| format!("{e:?}"))
}

/// Produce a derived graph. All saved definitions and target frames remain
/// untouched; a second preparation of the derived graph is an exact no-op.
pub fn expand_scene_modifiers(
    owner: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<EffectGraphDef, SceneModifierExpandError> {
    prepare_scene_modifiers(owner, registry).map(|prepared| prepared.def)
}

/// Prepare the derived graph and explicit editor routes together. Routes are
/// retained by the runtime across value edits and never serialized.
pub fn prepare_scene_modifiers(
    owner: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<PreparedSceneModifierGraph, SceneModifierExpandError> {
    prepare_scene_modifiers_impl(owner, registry)
}

fn prepare_scene_modifiers_impl(
    owner: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<PreparedSceneModifierGraph, SceneModifierExpandError> {
    // Capacity is split (BUG-ty86): stage-carrying modifiers expand per-stage
    // vertex buffers and keep the historic 16 limit; stage-less Math View
    // instances are bounded by their own surveyed budget (see
    // `scene_modifier_math_view::MAX_MATH_VIEW_MODIFIERS`), so a full legacy
    // migration of 16 carriers (16 stripped carriers + 16 views) prepares.
    let stage_carrying = manifold_core::scene_modifier_preset::force_slots_used(&owner.scene_modifiers);
    if stage_carrying > manifold_core::scene_modifier_preset::MAX_STAGE_CARRYING_MODIFIERS {
        return Err(SceneModifierExpandError::CapacityExceeded {
            path: "sceneModifiers".into(),
            detail: "an owner supports at most 16 stage-carrying modifiers".into(),
        });
    }
    let view_count = owner.scene_modifiers.len() - stage_carrying;
    if view_count > manifold_core::scene_modifier_math_view::MAX_MATH_VIEW_MODIFIERS {
        return Err(SceneModifierExpandError::CapacityExceeded {
            path: "sceneModifiers".into(),
            detail: format!(
                "an owner supports at most {} Math View modifiers",
                manifold_core::scene_modifier_math_view::MAX_MATH_VIEW_MODIFIERS
            ),
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
        if !super::fragment_cuts::contains_fragments(owner) {
            return Ok(PreparedSceneModifierGraph {
                def: owner.clone(), routes: Vec::new(), event_routes: Vec::new(),
                impulse_routes: Vec::new(), binding_sources: Vec::new(),
            });
        }
        let mut def = manifold_core::flatten::flatten_groups(owner).map_err(|error| invalid(
            "fragmentCuts", format!("legacy graph flattening failed: {error}"),
        ))?;
        let binding_count = def.preset_metadata.as_ref().map_or(0, |metadata| metadata.bindings.len());
        let mut binding_sources = vec![None; binding_count];
        super::fragment_cuts::apply(&mut def, &mut binding_sources)?;
        return Ok(PreparedSceneModifierGraph {
            def,
            routes: Vec::new(),
            event_routes: Vec::new(),
            impulse_routes: Vec::new(),
            binding_sources,
        });
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
        registry,
        current: BTreeMap::new(),
        reference: BTreeMap::new(),
        written: BTreeSet::new(),
        camera_anchors: BTreeMap::new(),
        contexts: BTreeMap::new(),
        event_routes: Vec::new(),
        math_captures: Vec::new(),
    };
    let mut leaf_maps = BTreeMap::new();
    let mut target_maps = BTreeMap::new();
    for instance in &owner.scene_modifiers {
        if instance
            .graph
            .preset_metadata
            .as_ref()
            .and_then(|metadata| metadata.scene_modifier.as_ref())
            .is_none()
        {
            return Err(invalid(instance.id.to_string(), "instance has no recipe"));
        }
        // No per-scene singleton uniqueness check here: that rule governs NEW
        // authoring (picker projection, action builders), while legacy load
        // migration legitimately produces several Math View instances per
        // scene — one per authored legacy carrier.
        frames::validate_saved_frames(owner, &index, instance)?;
        let targets = if is_force_recipe(&instance.graph) {
            let selected = super::acceleration::selected(
                &index, &instance.targets, &instance.scene, registry,
            )?;
            let mut seen = BTreeSet::new();
            let mut deduplicated = Vec::new();
            for target in selected {
                if let Some(key) =
                    super::acceleration::recipient_key(&index, &target, registry)?
                    && seen.insert(key)
                {
                    deduplicated.push(target);
                }
            }
            deduplicated
        } else {
            let has_acceleration = instance
                .graph
                .preset_metadata
                .as_ref()
                .and_then(|metadata| metadata.scene_modifier.as_ref())
                .is_some_and(|recipe| {
                    recipe.stages.iter().any(|stage| {
                        stage.outputs.iter().any(|output| {
                            output.endpoint == SceneEndpoint::Acceleration
                        })
                    })
                });
            if has_acceleration {
                return Err(SceneModifierExpandError::UnsupportedEndpoint {
                    path: instance.id.to_string(),
                    detail: "acceleration stages cannot be mixed with other endpoint outputs"
                        .into(),
                });
            }
            frames::selected_objects(&index, instance)?
        };
        // A view reads the real chain state at its own position, before
        // anything later in the chain can reach it.
        if manifold_core::scene_modifier_math_view::is_math_view_recipe(&instance.graph) {
            let captures = builder.capture_math_view(instance, &targets);
            builder.math_captures.push((instance.id.clone(), captures));
        }
        let leaves = builder.append_instance(owner, instance, &targets)?;
        leaf_maps.insert(instance.id.to_string(), leaves);
        target_maps.insert(instance.id.to_string(), targets);
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
            .retain(|wire| !(wire.to_node == target && wire.to_port.as_str() == key.1.as_str()));
        builder.derived.wires.push(EffectGraphWire {
            from_node: *producer,
            from_port: port.clone(),
            to_node: target,
            to_port: key.1.clone(),
        });
    }
    builder.finish_math_views(owner, &leaf_maps);
    builder.derived.name = owner.name.clone();
    builder.derived.description = owner.description.clone();
    let (metadata, mut binding_sources) =
        bindings::expand_bindings_with_sources(owner, &leaf_maps)?;
    builder.derived.preset_metadata = metadata;
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
    let is_host = |id: &NodeId| host_handles.contains_key(id.as_str());
    let mut prepared = bindings::seed_local_defaults(&flat, &is_host)?;
    let routes = routes::build_routes(owner, &prepared, &leaf_maps, &target_maps)?;
    super::fragment_cuts::apply(&mut prepared, &mut binding_sources)?;
    shatter::prepare(owner, &mut prepared, &index, &routes, &mut binding_sources, registry)?;
    if prepared.nodes.len() > 65_536 || prepared.wires.len() > 262_144 {
        return Err(SceneModifierExpandError::CapacityExceeded {
            path: "expandedGraph".into(),
            detail: "expanded graph including Math View exceeds 65536 nodes or 262144 wires".into(),
        });
    }
    let mut graph = prepared
        .clone()
        .into_graph(registry, &crate::scene::mesh_change::PreparedMeshRules::default())
        .map_err(|error| invalid("expandedGraph", error.to_string()))?;
    let impulse_routes = super::impulses::prepare(owner, &routes, &mut graph)?;
    validate_binding_leaves(&prepared, &graph)?;
    crate::validation::validate(&graph)
        .map_err(|error| invalid("expandedGraph", error.to_string()))?;
    Ok(PreparedSceneModifierGraph {
        def: prepared,
        routes,
        event_routes: builder.event_routes,
        impulse_routes,
        binding_sources,
    })
}

fn validate_binding_leaves(
    def: &EffectGraphDef,
    graph: &crate::graph::Graph,
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
        if string != (definition.ty == crate::parameters::ParamType::String) {
            return Err(SceneModifierExpandError::InvalidBinding {
                path: id.clone(),
                detail: "numeric/string binding kind does not match its primitive parameter".into(),
            });
        }
    }
    Ok(())
}

manifold_core::testkit_visible! {
#[doc(hidden)]
pub(crate) struct Builder<'a> {
    derived: EffectGraphDef,
    index: &'a FlatSceneIndex,
    registry: &'a PrimitiveRegistry,
    next_id: u32,
    current: BTreeMap<EndpointKey, Option<PortAddress>>,
    reference: BTreeMap<EndpointKey, Option<PortAddress>>,
    written: BTreeSet<EndpointKey>,
    camera_anchors: BTreeMap<SceneNodeRef, EndpointKey>,
    contexts: BTreeMap<String, PortAddress>,
    event_routes: Vec<super::SceneModifierEventRoute>,
    /// Per Math View in chain order: the objects it samples and the real
    /// chain state captured at its position.
    math_captures: Vec<(NodeId, Vec<(SceneNodeRef, MathViewCapture)>)>,
}
}

#[derive(Debug, Clone)]
struct MathViewCapture {
    reference: PortAddress,
    current: PortAddress,
    /// Chain producer of `SceneEndpoint::Instances` at the view's position:
    /// preceding copy/echo modifiers (SpatialEchoes, WavesEchoes, SceneLoop)
    /// or a host-wired instances port. `None` when nothing upstream produces
    /// copies — the diagram then behaves exactly as a vertices-only capture.
    instances: Option<PortAddress>,
    radius: f64,
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
        if recipe
            .stages
            .iter()
            .flat_map(|stage| &stage.inputs)
            .any(|input| {
                matches!(
                    input.source,
                    SceneStageSource::Context {
                        value: SceneContextValue::TriggerCount | SceneContextValue::TriggerBaseline
                    }
                )
            })
        {
            node_count = node_count.saturating_add(2);
        }
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
    #[cfg(any(test, feature = "testkit"))]
    pub fn current_for_test(&self, key: &EndpointKey) -> &Option<PortAddress> { &self.current[key] }

    #[cfg(any(test, feature = "testkit"))]
    pub fn reference_for_test(&self, key: &EndpointKey) -> &Option<PortAddress> { &self.reference[key] }

    #[cfg(any(test, feature = "testkit"))]
    pub fn set_current_for_test(&mut self, key: EndpointKey, value: Option<PortAddress>) {
        self.current.insert(key, value);
    }

    #[cfg(any(test, feature = "testkit"))]
    #[doc(hidden)]
    pub fn for_test<'a>(
        graph: &EffectGraphDef,
        index: &'a FlatSceneIndex,
        registry: &'a PrimitiveRegistry,
    ) -> Builder<'a> {
        Builder {
            derived: graph.clone(),
            index,
            registry,
            next_id: 100,
            current: BTreeMap::new(),
            reference: BTreeMap::new(),
            written: BTreeSet::new(),
            camera_anchors: BTreeMap::new(),
            contexts: BTreeMap::new(),
            event_routes: Vec::new(),
            math_captures: Vec::new(),
        }
    }

    /// The real chain state a Math View reads, per selected object: the full
    /// mesh after every preceding modifier (`current`), the object's own mesh
    /// wire at chain start (`reference`) and any upstream copies. An object
    /// that cannot resolve is skipped; Math View never fails the layer.
    fn capture_math_view(
        &mut self,
        instance: &SceneModifierInstanceDef,
        targets: &[SceneNodeRef],
    ) -> Vec<(SceneNodeRef, MathViewCapture)> {
        let mut captures = Vec::new();
        for target in targets {
            let key = match self.attachment_key(instance, Some(target), SceneEndpoint::Vertices) {
                Ok(key) => key,
                Err(error) => {
                    log::warn!("Math View {}: skipping {target:?}: {error}", instance.id);
                    continue;
                }
            };
            let current = self.current.get(&key).and_then(Clone::clone);
            let reference = self.reference.get(&key).and_then(Clone::clone);
            let (Some(current), Some(reference)) = (current, reference) else {
                log::warn!("Math View {}: skipping {target:?}: no mesh wire", instance.id);
                continue;
            };
            let Some(frame) = instance.mesh_frames.iter().find(|frame| frame.target == *target)
            else {
                log::warn!("Math View {}: skipping {target:?}: no saved mesh frame", instance.id);
                continue;
            };
            let instances = self
                .attachment_key(instance, Some(target), SceneEndpoint::Instances)
                .ok()
                .and_then(|key| self.current.get(&key).and_then(Clone::clone));
            captures.push((
                target.clone(),
                MathViewCapture { reference, current, instances, radius: frame.scene_radius },
            ));
        }
        captures
    }

    /// Build every view's diagrams in the parent graph and chain the views'
    /// mode switches in front of `system.final_output`. A view that cannot
    /// resolve is skipped with a warning and the image passes through it.
    fn finish_math_views(&mut self, owner: &EffectGraphDef, leaf_maps: &BTreeMap<String, LeafMap>) {
        let views = std::mem::take(&mut self.math_captures);
        if views.is_empty() {
            return;
        }
        let Some(final_id) = self
            .derived
            .nodes
            .iter()
            .find(|node| node.type_id == "system.final_output")
            .map(|node| node.id)
        else {
            log::warn!("Math View skipped: the graph has no system.final_output");
            return;
        };
        let Some(scene_image) = self
            .derived
            .wires
            .iter()
            .find(|wire| wire.to_node == final_id && wire.to_port == "in")
            .map(|wire| (wire.from_node, wire.from_port.clone()))
        else {
            log::warn!("Math View skipped: system.final_output has no input image");
            return;
        };
        let mut image = scene_image.clone();
        for (view_id, objects) in views {
            let Some(modifier) = owner.scene_modifiers.iter().find(|m| m.id == view_id) else {
                continue;
            };
            match self.finish_math_view(modifier, objects, leaf_maps, &image) {
                Ok(Some(out)) => image = out,
                Ok(None) => {}
                Err(error) => log::warn!("Math View {view_id} skipped: {error}"),
            }
        }
        if image != scene_image {
            self.derived
                .wires
                .retain(|wire| !(wire.to_node == final_id && wire.to_port == "in"));
            self.push_wire(image, final_id, "in");
        }
    }

    fn push_wire(&mut self, from: PortAddress, to_node: u32, to_port: &str) {
        self.derived.wires.push(EffectGraphWire {
            from_node: from.0,
            from_port: from.1,
            to_node,
            to_port: to_port.into(),
        });
    }

    /// One view: per object, sample the real current and reference meshes,
    /// draw the diagram, stack the diagrams with `node.over`, then select
    /// Scene / Math / Overlay with `node.switch_texture` (selector =
    /// mode × enabled). Everything this needs is resolved before any node is
    /// added, so a skipped view leaves no dangling nodes.
    fn finish_math_view(
        &mut self,
        modifier: &SceneModifierInstanceDef,
        objects: Vec<(SceneNodeRef, MathViewCapture)>,
        leaf_maps: &BTreeMap<String, LeafMap>,
        input: &PortAddress,
    ) -> Result<Option<PortAddress>, SceneModifierExpandError> {
        if objects.is_empty() {
            log::warn!("Math View {} skipped: no sampled objects resolved", modifier.id);
            return Ok(None);
        }
        let controls = self.math_control_sources(modifier, leaf_maps)?;
        let enabled = self.math_enabled_source(modifier, leaf_maps)?;
        let control = |name: &str| {
            controls
                .get(name)
                .cloned()
                .ok_or_else(|| invalid(format!("mathView.{name}"), "control is unavailable"))
        };
        let density = control("density")?;
        let mode = control("mode")?;
        let scene_id = *self
            .index
            .by_ref
            .get(&modifier.scene)
            .ok_or_else(|| invalid("mathView.scene", "scene target missing"))?;
        // Endpoint writes are applied: follow the scene's final camera wire.
        let camera = self
            .derived
            .wires
            .iter()
            .find(|wire| wire.to_node == scene_id && wire.to_port == "camera")
            .map(|wire| (wire.from_node, wire.from_port.clone()))
            .ok_or_else(|| invalid("mathView.camera", "scene has no resolved Camera input"))?;
        let mut resolved = Vec::with_capacity(objects.len());
        for (target, capture) in objects {
            // The object's final transform after the whole chain.
            let transform = self
                .attachment_key(modifier, Some(&target), SceneEndpoint::Transform)
                .ok()
                .and_then(|key| self.current.get(&key).and_then(Clone::clone));
            resolved.push((target, capture, transform));
        }

        let view = modifier.id.as_str();
        let mut diagrams = Vec::with_capacity(resolved.len());
        for (index, (target, capture, transform)) in resolved.into_iter().enumerate() {
            let parts = |role: &'static str| {
                let mut parts = vec!["math_view", view, role];
                parts.extend(target.scope.iter().map(NodeId::as_str));
                parts.push(target.node.as_str());
                parts
            };
            let current_samples = self.add_math_node(
                &parts("current_samples"),
                "node.sample_mesh_triangles",
                "Math View Current Samples",
                BTreeMap::new(),
            )?;
            let reference_samples = self.add_math_node(
                &parts("reference_samples"),
                "node.sample_mesh_triangles",
                "Math View Reference Samples",
                BTreeMap::new(),
            )?;
            for (sampler, mesh) in [
                (current_samples, &capture.current),
                (reference_samples, &capture.reference),
            ] {
                self.push_wire(mesh.clone(), sampler, "in");
                self.push_wire(density.clone(), sampler, "density");
            }
            let diagram = self.add_math_node(
                &parts("diagram"),
                "node.render_mesh_diagram",
                "Math View Diagram",
                BTreeMap::from([
                    ("grid".into(), SerializedParamValue::Bool { value: false }),
                    (
                        "radius".into(),
                        SerializedParamValue::Float { value: capture.radius as f32 },
                    ),
                ]),
            )?;
            // Ghosts and arrow tails read the reference samples, so arrows
            // show the whole preceding chain's displacement. Upstream copies
            // repeat the marks at every copy transform.
            let current = (current_samples, "vertices".to_string());
            let reference = (reference_samples, "vertices".to_string());
            for (from, port) in [
                (Some(current), "current"),
                (Some(reference.clone()), "reference"),
                (Some(reference), "incoming"),
                (Some(capture.current), "current_mesh"),
                (Some(capture.reference), "reference_mesh"),
                (Some(camera.clone()), "camera"),
                (transform, "transform"),
                (capture.instances, "instances"),
                (Some((scene_id, "depth".to_string())), "scene_depth"),
            ] {
                if let Some(from) = from {
                    self.push_wire(from, diagram, port);
                }
            }
            self.wire_math_controls(diagram, &controls, index == 0)?;
            diagrams.push((diagram, "color".to_string()));
        }

        // Later diagrams draw on top of earlier ones.
        let mut composed = diagrams[0].clone();
        for (index, diagram) in diagrams.iter().enumerate().skip(1) {
            let index = index.to_string();
            let over = self.add_math_node(
                &["math_view", view, "compose", &index],
                "node.over",
                "Math View Compose",
                BTreeMap::new(),
            )?;
            self.push_wire(diagram.clone(), over, "top");
            self.push_wire(composed, over, "bottom");
            composed = (over, "out".into());
        }
        let overlay = self.add_math_node(
            &["math_view", view, "overlay"],
            "node.over",
            "Math View Overlay",
            BTreeMap::new(),
        )?;
        self.push_wire(composed.clone(), overlay, "top");
        self.push_wire(input.clone(), overlay, "bottom");
        let selector = self.add_math_node(
            &["math_view", view, "selector"],
            "node.math",
            "Math View Mode Selector",
            BTreeMap::from([("op".into(), SerializedParamValue::Enum { value: 2 })]),
        )?;
        self.push_wire(mode, selector, "a");
        self.push_wire(enabled, selector, "b");
        let switch = self.add_math_node(
            &["math_view", view, "mode"],
            "node.switch_texture",
            "Math View Mode",
            BTreeMap::from([("num_inputs".into(), SerializedParamValue::Int { value: 3 })]),
        )?;
        self.push_wire((selector, "out".into()), switch, "selector");
        self.push_wire(input.clone(), switch, "in_0");
        self.push_wire(composed, switch, "in_1");
        self.push_wire((overlay, "out".into()), switch, "in_2");
        Ok(Some((switch, "out".into())))
    }

    /// The view's enabled toggle, a recipe `node.value` named `mv_enabled`.
    fn math_enabled_source(
        &self,
        modifier: &SceneModifierInstanceDef,
        leaf_maps: &BTreeMap<String, LeafMap>,
    ) -> Result<PortAddress, SceneModifierExpandError> {
        let copies = leaf_maps
            .get(modifier.id.as_str())
            .and_then(|map| map.get("mv_enabled"))
            .ok_or_else(|| invalid("mathView.enabled", "enabled control is absent"))?;
        let [node_id] = copies.as_slice() else {
            return Err(invalid("mathView.enabled", "expected one shared enabled control"));
        };
        self.derived
            .nodes
            .iter()
            .find(|node| node.node_id == *node_id && node.type_id == "node.value")
            .map(|node| (node.id, "out".to_string()))
            .ok_or_else(|| invalid("mathView.enabled", "enabled control must be a node.value"))
    }

    fn math_control_sources(
        &self,
        modifier: &SceneModifierInstanceDef,
        leaf_maps: &BTreeMap<String, LeafMap>,
    ) -> Result<BTreeMap<String, PortAddress>, SceneModifierExpandError> {
        let local_map = leaf_maps.get(modifier.id.as_str()).ok_or_else(|| {
            invalid(
                "mathView.controls",
                "Math View modifier routes are unavailable",
            )
        })?;
        let mut controls = BTreeMap::new();
        for (suffix, _, _, _, _) in manifold_core::scene_modifier_math_view::CONTROLS {
            let local_id = format!("__math_view_{suffix}");
            let copies = local_map.get(&local_id).ok_or_else(|| {
                SceneModifierExpandError::MissingInput {
                    path: format!("mathView.{suffix}"),
                    detail: "Math View control node is absent; enrich the modifier before preparing its view".into(),
                }
            })?;
            if copies.len() != 1 {
                return Err(SceneModifierExpandError::InvalidRecipe {
                    path: format!("mathView.{suffix}"),
                    detail: format!("expected one shared control node, found {}", copies.len()),
                });
            }
            let node_id = &copies[0];
            let node = self
                .derived
                .nodes
                .iter()
                .find(|node| node.node_id == *node_id)
                .ok_or_else(|| {
                    invalid(format!("mathView.{suffix}"), "control node copy is absent")
                })?;
            if node.type_id != "node.value" {
                return Err(SceneModifierExpandError::InvalidRecipe {
                    path: format!("mathView.{suffix}"),
                    detail: "control must be a node.value producer".into(),
                });
            }
            controls.insert(suffix.to_string(), (node.id, "out".into()));
        }
        Ok(controls)
    }

    fn wire_math_controls(
        &mut self,
        diagram_id: u32,
        controls: &BTreeMap<String, PortAddress>,
        include_grid: bool,
    ) -> Result<(), SceneModifierExpandError> {
        for (suffix, _, _, _, _) in manifold_core::scene_modifier_math_view::CONTROLS {
            if !matches!(
                *suffix,
                "mode"
                    | "occlusion"
                    | "grid"
                    | "fragments"
                    | "ghosts"
                    | "vectors"
                    | "axes"
                    | "trails"
                    | "density"
                    | "line_width"
                    | "geometry_hue"
                    | "path_hue"
                    | "grid_brightness"
                    | "fragments_brightness"
                    | "ghosts_brightness"
                    | "vectors_brightness"
                    | "trails_brightness"
            ) || (*suffix == "grid" && !include_grid)
            {
                continue;
            }
            let source = controls.get(*suffix).ok_or_else(|| {
                invalid(
                    format!("mathView.{suffix}"),
                    "presentation control source is unavailable",
                )
            })?;
            self.derived.wires.push(EffectGraphWire {
                from_node: source.0,
                from_port: source.1.clone(),
                to_node: diagram_id,
                to_port: (*suffix).into(),
            });
        }
        Ok(())
    }

    fn add_math_node(
        &mut self,
        parts: &[&str],
        type_id: &str,
        title: &str,
        params: BTreeMap<String, SerializedParamValue>,
    ) -> Result<u32, SceneModifierExpandError> {
        let node_id = namespace::namespace_node_id(parts);
        if self
            .derived
            .nodes
            .iter()
            .any(|node| node.node_id == node_id)
        {
            return Err(SceneModifierExpandError::DuplicateIdentity {
                path: node_id.to_string(),
                detail: "generated Math View node collides with an existing node".into(),
            });
        }
        let id = self.next_id;
        self.next_id = id
            .checked_add(1)
            .ok_or_else(|| invalid("mathView", "numeric node IDs exhausted"))?;
        self.derived.nodes.push(EffectGraphNode {
            id,
            handle: Some(node_id.to_string()),
            node_id,
            type_id: type_id.into(),
            params,
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: Some(title.into()),
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        });
        Ok(id)
    }

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
        let identity_key = serde_json::to_string(&("identity", &key.0, &key.1))
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
            SceneEndpoint::Acceleration => {
                for component in ["x", "y", "z"] {
                    params.insert(component.into(), SerializedParamValue::Float { value: 0.0 });
                }
                self.constant_node(identity_key, "node.uniform_vector_field", params, "out")?
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
            SceneContextValue::TriggerCount | SceneContextValue::TriggerBaseline
        ) {
            if !self
                .event_routes
                .iter()
                .any(|route| route.modifier_id == instance.id)
            {
                let mut nodes = Vec::with_capacity(2);
                for kind in ["count", "baseline"] {
                    let key = serde_json::to_string(&(instance.id.as_str(), "events", kind))
                        .map_err(|error| invalid(instance.id.to_string(), error.to_string()))?;
                    let params = BTreeMap::from([(
                        "value".into(),
                        SerializedParamValue::Float { value: 0.0 },
                    )]);
                    let (id, _) = self.constant_node(key, "node.value", params, "out")?;
                    nodes.push(
                        self.derived
                            .nodes
                            .iter()
                            .find(|node| node.id == id)
                            .unwrap()
                            .node_id
                            .clone(),
                    );
                }
                self.event_routes.push(super::SceneModifierEventRoute {
                    modifier_id: instance.id.clone(),
                    count_node: nodes.remove(0),
                    baseline_node: nodes.remove(0),
                });
            }
            let route = self
                .event_routes
                .iter()
                .find(|route| route.modifier_id == instance.id)
                .unwrap();
            let node_id = if value == SceneContextValue::TriggerCount {
                &route.count_node
            } else {
                &route.baseline_node
            };
            let id = self
                .derived
                .nodes
                .iter()
                .find(|node| &node.node_id == node_id)
                .unwrap()
                .id;
            return Ok((id, "out".into()));
        }
        if matches!(value, SceneContextValue::Time | SceneContextValue::Beat) {
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
                    _ => unreachable!("only time and beat use the host boundary"),
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

manifold_core::testkit_visible! {
    #[doc(hidden)]
    pub(crate) fn attachment_key(
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
        let key = if endpoint == SceneEndpoint::Camera {
            if let Some(key) = self.camera_anchors.get(reference) {
                key.clone()
            } else {
                let key = self.resolve_camera_anchor(instance)?;
                self.camera_anchors.insert(reference.clone(), key.clone());
                key
            }
        } else if endpoint == SceneEndpoint::Acceleration {
            super::acceleration::recipient_key(self.index, reference, self.registry)?
                .ok_or_else(|| SceneModifierExpandError::UnsupportedEndpoint {
                    path: format!("{reference:?}"),
                    detail: "selected object has no physical acceleration recipient".into(),
                })?
        } else {
            (reference.clone(), endpoint_port(endpoint).into())
        };
        if !self.current.contains_key(&key) {
            let initial = self
                .index
                .input(&key.0, key.1.as_str())?
                .map(|wire| (wire.from_node, wire.from_port.clone()));
            self.current.insert(key.clone(), initial.clone());
            self.reference.insert(key.clone(), initial);
        }
        Ok(key)
    }
}

    /// Find the stable insertion point for a scene-wide camera source stage.
    ///
    /// A pass-through camera processor is safe to cross only when its actual
    /// registry shape has one Camera input and the output feeding the current
    /// consumer is Camera. This deliberately leaves muxes and other
    /// ambiguous nodes at the current consumer port instead of selecting a
    /// branch by convention.
    fn resolve_camera_anchor(
        &self,
        instance: &SceneModifierInstanceDef,
    ) -> Result<EndpointKey, SceneModifierExpandError> {
        let mut target = instance.scene.clone();
        let mut port = endpoint_port(SceneEndpoint::Camera).to_string();
        let mut visited = BTreeSet::new();
        loop {
            let key = (target.clone(), port.clone());
            if !visited.insert(key.clone()) {
                return Err(invalid(
                    instance.id.to_string(),
                    "camera processing chain contains a cycle",
                ));
            }
            let Some(wire) = self.index.input(&target, &port)? else {
                return Ok(key);
            };
            let producer_ref = self
                .index
                .by_id
                .get(&wire.from_node)
                .cloned()
                .ok_or_else(|| {
                    invalid(
                        instance.id.to_string(),
                        format!(
                            "camera producer {} has no stable scene reference",
                            wire.from_node
                        ),
                    )
                })?;
            let producer_node = self.index.node(&producer_ref)?;
            let producer = self
                .registry
                .construct(&producer_node.type_id)
                .ok_or_else(|| {
                    invalid(
                        producer_node.type_id.clone(),
                        "camera chain producer is not registered",
                    )
                })?;
            let camera_inputs: Vec<_> = producer
                .inputs()
                .iter()
                .filter(|input| input.ty == PortType::Camera)
                .collect();
            if camera_inputs.len() != 1 {
                return Ok(key);
            }
            let Some(output) = producer
                .outputs()
                .iter()
                .find(|output| output.name.as_ref() == wire.from_port.as_str())
            else {
                return Err(invalid(
                    producer_node.type_id.clone(),
                    format!("camera wire names missing output port '{}'", wire.from_port),
                ));
            };
            if output.ty != PortType::Camera {
                return Ok(key);
            }
            target = producer_ref;
            port = camera_inputs[0].name.to_string();
        }
    }

    fn append_instance(
        &mut self,
        owner: &EffectGraphDef,
        instance: &SceneModifierInstanceDef,
        targets: &[SceneNodeRef],
    ) -> Result<LeafMap, SceneModifierExpandError> {
        let local = bindings::seed_local_defaults(&instance.graph, &|_| false)?;
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
            if stage.scope == SceneStageScope::EachObject
                && targets.is_empty()
                && !is_force_recipe(&instance.graph)
            {
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
            if each && targets.is_empty() {
                // A force with no recipients retains authored controls but has
                // no per-object runtime leaves. Record that explicitly so
                // routing can distinguish inactivity from a missing clone.
                let mut pending = vec![node];
                while let Some(local) = pending.pop() {
                    if !local.node_id.is_empty() {
                        leaves.entry(local.node_id.to_string()).or_default();
                    }
                    if let Some(group) = local.group.as_ref() {
                        pending.extend(&group.nodes);
                    }
                }
            }
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
                            SceneEndpoint::Instances
                                | SceneEndpoint::Acceleration
                                | SceneEndpoint::Atmosphere
                                | SceneEndpoint::RenderMode
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
                    self.current
                        .insert(key.clone(), Some((copy, output.port.clone())));
                    self.written.insert(key);
                }
            }
        }
        Ok(leaves)
    }
}
