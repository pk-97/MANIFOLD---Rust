//! Stable authored identity for the inputs of native physics graphs.
//!
//! This module deliberately describes authored sources only.  Runtime node
//! identities, fusion metadata, live parameter values, and cache selection are
//! outside this digest.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, ParamSpecDef,
    StringBindingDef,
};
use manifold_core::flatten::flatten_groups;
use manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID;
use sha2::{Digest, Sha256};

use crate::persistence::PrimitiveRegistry;
use crate::water::physics::RigidImpulseTargets;
use crate::water::physics_events::ImpulseTarget;
use crate::load::expand::{SceneModifierImpulseRoute, impulse_recipients, prepare_coupled_scenes};

/// Authored identity for one fluid source graph.
pub(super) struct PhysicsSourceGraph {
    pub(super) fluid: NodeId,
    pub(super) digest: [u8; 32],
    pub(super) control_ids: Vec<String>,
    /// Resolved destinations of relevant host string bindings. Values are
    /// observed after the existing binding policy has applied overrides.
    pub(super) string_targets: Vec<(NodeId, String)>,
    /// Relevant file loaders from the existing central asset inventory.
    pub(super) asset_nodes: Vec<NodeId>,
}

/// Build the stable source identity for every authored fluid domain.
///
/// `expanded` is the unfused, prepared graph. `canonical` owns the authored
/// scene-modifier stack and its target declarations.  The latter is consulted
/// through the same resolver used to prepare runtime coupling and impulses.
pub(super) fn prepare(
    expanded: &EffectGraphDef,
    canonical: &EffectGraphDef,
    impulse_routes: &[SceneModifierImpulseRoute],
    registry: &PrimitiveRegistry,
) -> Result<Vec<PhysicsSourceGraph>, String> {
    let mut flat =
        flatten_groups(expanded).map_err(|error| format!("physics source flatten: {error}"))?;
    for node in &mut flat.nodes {
        if node.node_id.is_empty()
            && let Some(handle) = node.handle.as_deref()
        {
            node.node_id = NodeId::new(handle);
        }
        if node.type_id == FLIP_DOMAIN_TYPE_ID && node.node_id.is_empty() {
            return Err(
                "fluid source has no stable node identity or legacy handle; recording provenance is unsupported"
                    .into(),
            );
        }
    }
    let has_fluid = flat
        .nodes
        .iter()
        .any(|node| node.type_id == FLIP_DOMAIN_TYPE_ID);

    // A normal generator must retain all of its existing load behaviour.  In
    // particular, do not validate unrelated malformed wires or invoke the
    // scene resolvers unless this graph actually contains a fluid domain.
    if !has_fluid {
        return Ok(Vec::new());
    }

    let graph = SourceGraph::new(flat, registry)?;
    let fluids: Vec<NodeId> = graph
        .nodes
        .values()
        .filter(|node| node.type_id == FLIP_DOMAIN_TYPE_ID)
        .map(|node| node.node_id.clone())
        .collect();
    let coupled = prepare_coupled_scenes(canonical, registry)
        .map_err(|error| format!("physics coupling: {error:?}"))?;
    let mut pairs = BTreeMap::new();
    for binding in coupled {
        if !graph.has_node(&binding.fluid) {
            return Err(format!(
                "physics coupling references fluid node '{}' absent from expanded graph",
                binding.fluid
            ));
        }
        if !graph.has_node(&binding.rigid) {
            return Err(format!(
                "physics coupling references rigid node '{}' absent from expanded graph",
                binding.rigid
            ));
        }
        if pairs
            .insert(
                binding.fluid.as_str().to_owned(),
                (binding.rigid, binding.colliders),
            )
            .is_some()
        {
            return Err(format!(
                "physics coupling resolves fluid node '{}' more than once",
                binding.fluid
            ));
        }
    }

    let mut sources = Vec::with_capacity(fluids.len());
    for fluid in fluids {
        let mut selected = BTreeSet::new();
        graph.collect_ancestry(&fluid, &mut selected)?;

        let pair = pairs.get(fluid.as_str());
        if let Some((rigid, _)) = pair {
            graph.collect_ancestry(rigid, &mut selected)?;
        }

        let events = collect_event_sources(
            &graph,
            canonical,
            impulse_routes,
            &fluid,
            pair.map(|(rigid, _)| rigid),
            registry,
            &mut selected,
        )?;
        let control_ids = relevant_control_ids(&graph, canonical, &selected, &events);
        sources.push(PhysicsSourceGraph {
            fluid: fluid.clone(),
            digest: digest_source(&graph, &selected, pair, &events, canonical)?,
            control_ids,
            string_targets: relevant_string_targets(&graph, &selected),
            asset_nodes: selected
                .iter()
                .filter_map(|id| {
                    let node = graph.nodes.get(id)?;
                    is_source_asset(&node.type_id).then(|| node.node_id.clone())
                })
                .collect(),
        });
    }
    sources.sort_by(|left, right| left.fluid.as_str().cmp(right.fluid.as_str()));
    Ok(sources)
}

struct SourceGraph {
    nodes: BTreeMap<String, EffectGraphNode>,
    numbers: BTreeMap<u32, NodeId>,
    incoming: BTreeMap<u32, Vec<EffectGraphWire>>,
    metadata: Option<manifold_core::effect_graph_def::PresetMetadata>,
    /// Loader-declared paths backed by the separate loaded-content identity.
    asset_paths: BTreeMap<String, &'static [&'static str]>,
}

impl SourceGraph {
    fn new(def: EffectGraphDef, registry: &PrimitiveRegistry) -> Result<Self, String> {
        let metadata = def.preset_metadata.clone();
        let mut nodes = BTreeMap::new();
        let mut numbers = BTreeMap::new();
        for node in def.nodes {
            if !node.node_id.is_empty()
                && nodes
                    .insert(node.node_id.as_str().to_owned(), node.clone())
                    .is_some()
            {
                return Err(format!(
                    "duplicate physics source node identity '{}'",
                    node.node_id
                ));
            }
            if numbers.insert(node.id, node.node_id.clone()).is_some() {
                return Err(format!("duplicate physics source node number {}", node.id));
            }
        }
        let mut incoming: BTreeMap<u32, Vec<EffectGraphWire>> = BTreeMap::new();
        for wire in def.wires {
            if !numbers.contains_key(&wire.from_node) || !numbers.contains_key(&wire.to_node) {
                return Err(format!(
                    "physics source wire references missing node ({} -> {})",
                    wire.from_node, wire.to_node
                ));
            }
            incoming.entry(wire.to_node).or_default().push(wire);
        }
        let asset_paths = nodes
            .iter()
            .filter(|(_, node)| is_source_asset(&node.type_id))
            .filter_map(|(id, node)| {
                let paths = registry.construct(&node.type_id)?.source_asset_paths();
                (!paths.is_empty()).then(|| (id.clone(), paths))
            })
            .collect();
        Ok(Self {
            nodes,
            numbers,
            incoming,
            metadata,
            asset_paths,
        })
    }

    fn is_asset_path(&self, node: &NodeId, param: &str) -> bool {
        self.asset_paths
            .get(node.as_str())
            .is_some_and(|paths| paths.contains(&param))
    }

    fn has_node(&self, id: &NodeId) -> bool {
        self.nodes.contains_key(id.as_str())
    }

    fn collect_ancestry(
        &self,
        root: &NodeId,
        selected: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        let root_number = self
            .nodes
            .get(root.as_str())
            .map(|node| node.id)
            .ok_or_else(|| format!("physics source references unknown node '{}'", root))?;
        let mut pending = vec![root_number];
        while let Some(number) = pending.pop() {
            let id = self
                .numbers
                .get(&number)
                .ok_or_else(|| format!("physics source references unknown node number {number}"))?;
            if id.is_empty() {
                return Err(format!(
                    "physics source ancestry reaches node number {number} without stable identity"
                ));
            }
            if !selected.insert(id.as_str().to_owned()) {
                continue;
            }
            if let Some(wires) = self.incoming.get(&number) {
                for wire in wires {
                    pending.push(wire.from_node);
                }
            }
        }
        Ok(())
    }

    fn number(&self, id: &str) -> Option<u32> {
        self.nodes.get(id).map(|node| node.id)
    }
}

#[derive(Clone)]
struct EventSource {
    modifier_id: NodeId,
    param_id: String,
    field_node: NodeId,
    field_port: String,
    recipients: Vec<(NodeId, ImpulseTarget)>,
}

fn collect_event_sources(
    graph: &SourceGraph,
    canonical: &EffectGraphDef,
    routes: &[SceneModifierImpulseRoute],
    fluid: &NodeId,
    rigid: Option<&NodeId>,
    registry: &PrimitiveRegistry,
    selected: &mut BTreeSet<String>,
) -> Result<Vec<EventSource>, String> {
    let mut events = Vec::new();
    for route in routes {
        if !graph.has_node(&route.field_node) {
            return Err(format!(
                "impulse route '{}' references field node '{}' absent from expanded graph",
                route.modifier_id, route.field_node
            ));
        }
        let modifier = canonical
            .scene_modifiers
            .iter()
            .find(|modifier| modifier.id == route.modifier_id)
            .ok_or_else(|| {
                format!(
                    "impulse route references unknown modifier '{}'",
                    route.modifier_id
                )
            })?;
        let recipients =
            impulse_recipients(canonical, &modifier.scene, &modifier.targets, registry)
                .map_err(|error| format!("physics impulse recipients: {error:?}"))?;
        let relevant: Vec<_> = recipients
            .iter()
            .filter(|(node, target)| {
                (node == fluid && target.affects_fluid())
                    || rigid.is_some_and(|rigid| node == rigid && target.rigid_targets().is_some())
            })
            .cloned()
            .collect();
        if relevant.is_empty() {
            continue;
        }
        graph.collect_ancestry(&route.field_node, selected)?;
        events.push(EventSource {
            modifier_id: route.modifier_id.clone(),
            param_id: route.param_id.clone(),
            field_node: route.field_node.clone(),
            field_port: route.field_port.clone(),
            recipients: relevant,
        });
    }
    events.sort_by(|left, right| {
        left.modifier_id
            .as_str()
            .cmp(right.modifier_id.as_str())
            .then(left.param_id.cmp(&right.param_id))
            .then(left.field_node.as_str().cmp(right.field_node.as_str()))
            .then(left.field_port.cmp(&right.field_port))
    });
    Ok(events)
}

fn relevant_control_ids(
    graph: &SourceGraph,
    canonical: &EffectGraphDef,
    selected: &BTreeSet<String>,
    events: &[EventSource],
) -> Vec<String> {
    let modifier_ids: BTreeSet<_> = events
        .iter()
        .map(|event| event.modifier_id.as_str())
        .collect();
    let mut ids = BTreeSet::new();
    if let Some(metadata) = graph.metadata.as_ref() {
        for binding in &metadata.bindings {
            if let BindingTarget::Node { node_id, param } = &binding.target
                && selected.contains(node_id.as_str())
                && !is_fluid_cache_param(graph, node_id, param)
            {
                ids.insert(binding.id.clone());
            }
        }
    }
    if let Some(metadata) = canonical.preset_metadata.as_ref() {
        for binding in &metadata.bindings {
            if let BindingTarget::SceneModifier { modifier_id, .. } = &binding.target
                && modifier_ids.contains(modifier_id.as_str())
            {
                ids.insert(binding.id.clone());
            }
        }
    }
    ids.into_iter().collect()
}

fn relevant_string_targets(
    graph: &SourceGraph,
    selected: &BTreeSet<String>,
) -> Vec<(NodeId, String)> {
    let targets: BTreeSet<_> = graph
        .metadata
        .as_ref()
        .into_iter()
        .flat_map(|metadata| &metadata.string_bindings)
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, param }
                if selected.contains(node_id.as_str())
                    && !is_fluid_cache_param(graph, node_id, param)
                    && !graph.is_asset_path(node_id, param) =>
            {
                Some((node_id.as_str().to_owned(), param.clone()))
            }
            _ => None,
        })
        .collect();
    targets
        .into_iter()
        .map(|(id, param)| (NodeId::new(id), param))
        .collect()
}

fn digest_source(
    graph: &SourceGraph,
    selected: &BTreeSet<String>,
    pair: Option<&(NodeId, RigidImpulseTargets)>,
    events: &[EventSource],
    canonical: &EffectGraphDef,
) -> Result<[u8; 32], String> {
    let mut writer = DigestWriter::default();
    writer.str("manifold.physics.source.v2");
    writer.u32(selected.len() as u32);
    for id in selected {
        let node = graph
            .nodes
            .get(id)
            .ok_or_else(|| format!("selected physics node '{}' is missing", id))?;
        writer.str(id.as_str());
        writer.str(&node.type_id);
        let params: Vec<_> = node
            .params
            .iter()
            .filter(|(param, _)| {
                !(is_fluid_cache_param(graph, &node.node_id, param)
                    || graph.is_asset_path(&node.node_id, param))
            })
            .collect();
        writer.u32(params.len() as u32);
        for (param, value) in params {
            writer.str(param);
            writer.json(value)?;
        }
        writer.option_str(node.wgsl_source.as_deref());
        writer.map_str(&node.output_formats);
        writer.map_scale(&node.output_canvas_scales);
    }

    let selected_numbers: BTreeSet<u32> =
        selected.iter().filter_map(|id| graph.number(id)).collect();
    let mut wires = Vec::new();
    for (to_number, incoming) in &graph.incoming {
        for wire in incoming {
            if !selected_numbers.contains(&wire.from_node) || !selected_numbers.contains(to_number)
            {
                continue;
            }
            let from = graph
                .numbers
                .get(&wire.from_node)
                .expect("validated wire source");
            let to = graph.numbers.get(to_number).expect("validated wire target");
            wires.push((
                from.clone(),
                wire.from_port.clone(),
                to.clone(),
                wire.to_port.clone(),
            ));
        }
    }
    wires.sort_by(|left, right| {
        left.0
            .as_str()
            .cmp(right.0.as_str())
            .then(left.1.cmp(&right.1))
            .then(left.2.as_str().cmp(right.2.as_str()))
            .then(left.3.cmp(&right.3))
    });
    writer.u32(wires.len() as u32);
    for (from, from_port, to, to_port) in wires {
        writer.str(from.as_str());
        writer.str(&from_port);
        writer.str(to.as_str());
        writer.str(&to_port);
    }

    match pair {
        Some((rigid, targets)) => {
            writer.bool(true);
            writer.str(rigid.as_str());
            writer.u64(targets.bodies);
            writer.bool(targets.copies);
        }
        None => writer.bool(false),
    }
    writer.u32(events.len() as u32);
    for event in events {
        writer.str(event.modifier_id.as_str());
        writer.str(&event.param_id);
        writer.str(event.field_node.as_str());
        writer.str(&event.field_port);
        let mut recipients = event.recipients.clone();
        recipients.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        writer.u32(recipients.len() as u32);
        for (node, target) in recipients {
            writer.str(node.as_str());
            write_impulse_target(&mut writer, target);
        }
    }

    write_relevant_bindings(&mut writer, graph, canonical, selected, events)?;
    Ok(writer.finish())
}

fn write_relevant_bindings(
    writer: &mut DigestWriter,
    graph: &SourceGraph,
    canonical: &EffectGraphDef,
    selected: &BTreeSet<String>,
    events: &[EventSource],
) -> Result<(), String> {
    let modifier_ids: BTreeSet<_> = events
        .iter()
        .map(|event| event.modifier_id.as_str())
        .collect();
    let expanded_metadata = graph.metadata.as_ref();
    let mut bindings: Vec<(&BindingDef, Vec<u8>)> = Vec::new();
    if let Some(metadata) = expanded_metadata {
        for binding in &metadata.bindings {
            let BindingTarget::Node { node_id, param } = &binding.target else {
                continue;
            };
            if selected.contains(node_id.as_str()) && !is_fluid_cache_param(graph, node_id, param) {
                let key = serde_json::to_vec(&binding.target)
                    .map_err(|error| format!("physics source binding encoding: {error}"))?;
                bindings.push((binding, key));
            }
        }
    }
    bindings.sort_by(|left, right| left.0.id.cmp(&right.0.id).then(left.1.cmp(&right.1)));
    writer.u32(bindings.len() as u32);
    for (binding, _) in bindings {
        writer.str(&binding.id);
        writer.f32(binding.default_value);
        writer.json(&binding.target)?;
        writer.json(&binding.convert)?;
        writer.f32(binding.scale);
        writer.f32(binding.offset);
        writer.bool(binding.user_added);
        writer.bool(binding.default_mirrors_node_param);
    }

    let mut aliases: Vec<(&BindingDef, Vec<u8>)> = Vec::new();
    if let Some(metadata) = &canonical.preset_metadata {
        for binding in &metadata.bindings {
            let BindingTarget::SceneModifier { modifier_id, .. } = &binding.target else {
                continue;
            };
            if modifier_ids.contains(modifier_id.as_str()) {
                let key = serde_json::to_vec(&binding.target)
                    .map_err(|error| format!("physics source alias encoding: {error}"))?;
                aliases.push((binding, key));
            }
        }
    }
    aliases.sort_by(|left, right| left.0.id.cmp(&right.0.id).then(left.1.cmp(&right.1)));
    writer.u32(aliases.len() as u32);
    for (binding, _) in aliases {
        writer.str(&binding.id);
        writer.f32(binding.default_value);
        writer.json(&binding.target)?;
        writer.json(&binding.convert)?;
        writer.f32(binding.scale);
        writer.f32(binding.offset);
        writer.bool(binding.user_added);
        writer.bool(binding.default_mirrors_node_param);
    }

    let mut strings: Vec<(&StringBindingDef, Vec<u8>)> = Vec::new();
    if let Some(metadata) = expanded_metadata {
        for binding in &metadata.string_bindings {
            let BindingTarget::Node { node_id, param } = &binding.target else {
                continue;
            };
            if selected.contains(node_id.as_str()) && !is_fluid_cache_param(graph, node_id, param) {
                let key = serde_json::to_vec(&binding.target)
                    .map_err(|error| format!("physics source string binding encoding: {error}"))?;
                strings.push((binding, key));
            }
        }
    }
    strings.sort_by(|left, right| left.0.id.cmp(&right.0.id).then(left.1.cmp(&right.1)));
    writer.u32(strings.len() as u32);
    for (binding, _) in strings {
        writer.str(&binding.id);
        let asset_path = matches!(&binding.target, BindingTarget::Node { node_id, param }
            if graph.is_asset_path(node_id, param));
        writer.bool(asset_path);
        if !asset_path {
            writer.str(effective_string_default(expanded_metadata, binding));
        }
        writer.json(&binding.target)?;
    }

    write_relevant_specs(
        writer,
        expanded_metadata,
        graph,
        selected,
        canonical,
        &modifier_ids,
    )?;
    Ok(())
}

fn effective_string_default<'a>(
    metadata: Option<&'a manifold_core::effect_graph_def::PresetMetadata>,
    binding: &'a StringBindingDef,
) -> &'a str {
    metadata
        .and_then(|metadata| {
            metadata
                .string_params
                .iter()
                .find(|param| param.id == binding.id)
        })
        .map_or(binding.default_value.as_str(), |param| {
            param.default_value.as_str()
        })
}

fn write_relevant_specs(
    writer: &mut DigestWriter,
    metadata: Option<&manifold_core::effect_graph_def::PresetMetadata>,
    graph: &SourceGraph,
    selected: &BTreeSet<String>,
    canonical: &EffectGraphDef,
    modifier_ids: &BTreeSet<&str>,
) -> Result<(), String> {
    if let Some(metadata) = metadata {
        let scalar_ids: BTreeSet<_> = metadata
            .bindings
            .iter()
            .filter_map(|binding| match &binding.target {
                BindingTarget::Node { node_id, param }
                    if selected.contains(node_id.as_str())
                        && !is_fluid_cache_param(graph, node_id, param) =>
                {
                    Some(binding.id.as_str())
                }
                _ => None,
            })
            .collect();
        let string_ids: BTreeSet<_> = metadata
            .string_bindings
            .iter()
            .filter_map(|binding| match &binding.target {
                BindingTarget::Node { node_id, param }
                    if selected.contains(node_id.as_str())
                        && !is_fluid_cache_param(graph, node_id, param) =>
                {
                    Some(binding.id.as_str())
                }
                _ => None,
            })
            .collect();

        let mut specs: Vec<&ParamSpecDef> = metadata
            .params
            .iter()
            .filter(|param| scalar_ids.contains(param.id.as_str()))
            .collect();
        specs.sort_by(|left, right| left.id.cmp(&right.id));
        writer.u32(specs.len() as u32);
        for spec in specs {
            write_param_spec(writer, spec)?;
        }

        let mut string_specs: Vec<_> = metadata
            .string_params
            .iter()
            .filter(|param| string_ids.contains(param.id.as_str()))
            .collect();
        string_specs.sort_by(|left, right| left.id.cmp(&right.id));
        writer.u32(string_specs.len() as u32);
        for spec in string_specs {
            writer.str(&spec.id);
            // One string may feed both an authenticated file path and an
            // ordinary text input. That text still participates in physics.
            let has_text_target = metadata.string_bindings.iter().any(|binding| {
                binding.id == spec.id
                    && matches!(&binding.target, BindingTarget::Node { node_id, param }
                        if selected.contains(node_id.as_str())
                            && !is_fluid_cache_param(graph, node_id, param)
                            && !graph.is_asset_path(node_id, param))
            });
            writer.bool(has_text_target);
            if has_text_target {
                writer.str(&spec.default_value);
            }
            writer.bool(spec.is_file_picker);
            writer.bool(spec.use_dropdown);
            writer.bool(spec.is_file_path);
        }
    } else {
        writer.u32(0);
        writer.u32(0);
    }

    let alias_ids: BTreeSet<_> = canonical
        .preset_metadata
        .as_ref()
        .into_iter()
        .flat_map(|metadata| metadata.bindings.iter())
        .filter_map(|binding| match &binding.target {
            BindingTarget::SceneModifier { modifier_id, .. }
                if modifier_ids.contains(modifier_id.as_str()) =>
            {
                Some(binding.id.as_str())
            }
            _ => None,
        })
        .collect();
    let mut alias_specs: Vec<&ParamSpecDef> = canonical
        .preset_metadata
        .as_ref()
        .into_iter()
        .flat_map(|metadata| metadata.params.iter())
        .filter(|param| alias_ids.contains(param.id.as_str()))
        .collect();
    alias_specs.sort_by(|left, right| left.id.cmp(&right.id));
    writer.u32(alias_specs.len() as u32);
    for spec in alias_specs {
        write_param_spec(writer, spec)?;
    }
    Ok(())
}

fn write_param_spec(writer: &mut DigestWriter, spec: &ParamSpecDef) -> Result<(), String> {
    writer.str(&spec.id);
    writer.f32(spec.min);
    writer.f32(spec.max);
    writer.f32(spec.default_value);
    writer.json(&spec.curve)?;
    writer.bool(spec.invert);
    writer.bool(spec.whole_numbers);
    writer.bool(spec.is_toggle);
    writer.bool(spec.is_trigger);
    writer.bool(spec.wraps);
    writer.bool(spec.is_trigger_gate);
    writer.bool(!spec.value_labels.is_empty());
    Ok(())
}

fn is_fluid_cache_param(graph: &SourceGraph, node_id: &NodeId, param: &str) -> bool {
    matches!(param, "cache_mode" | "cache_path" | "reset")
        && graph
            .nodes
            .get(node_id.as_str())
            .is_some_and(|node| node.type_id == FLIP_DOMAIN_TYPE_ID)
}

fn is_source_asset(type_id: &str) -> bool {
    use manifold_core::file_loader::{AssetFamily, NodeFileLoad, file_loader_kind};
    !matches!(
        file_loader_kind(type_id),
        None | Some(NodeFileLoad::Folder(AssetFamily::Physics))
    )
}

fn write_impulse_target(writer: &mut DigestWriter, target: ImpulseTarget) {
    match target {
        ImpulseTarget::Fluid => writer.u8(0),
        ImpulseTarget::Rigid(targets) => {
            writer.u8(1);
            writer.u64(targets.bodies);
            writer.bool(targets.copies);
        }
        ImpulseTarget::FluidAndRigid(targets) => {
            writer.u8(2);
            writer.u64(targets.bodies);
            writer.bool(targets.copies);
        }
    }
}

#[derive(Default)]
struct DigestWriter {
    bytes: Vec<u8>,
}

impl DigestWriter {
    fn finish(self) -> [u8; 32] {
        Sha256::digest(self.bytes).into()
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.u64(bytes.len() as u64);
        self.bytes.extend_from_slice(bytes);
    }

    fn str(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn json<T: serde::Serialize>(&mut self, value: &T) -> Result<(), String> {
        let bytes = serde_json::to_vec(value)
            .map_err(|error| format!("physics source encoding: {error}"))?;
        self.bytes(&bytes);
        Ok(())
    }

    fn option_str(&mut self, value: Option<&str>) {
        match value {
            Some(value) => {
                self.bool(true);
                self.str(value);
            }
            None => self.bool(false),
        }
    }

    fn map_str(&mut self, map: &BTreeMap<String, String>) {
        self.u32(map.len() as u32);
        for (key, value) in map {
            self.str(key);
            self.str(value);
        }
    }

    fn map_scale(&mut self, map: &BTreeMap<String, [u32; 2]>) {
        self.u32(map.len() as u32);
        for (key, value) in map {
            self.str(key);
            self.u32(value[0]);
            self.u32(value[1]);
        }
    }

    fn bool(&mut self, value: bool) {
        self.u8(value as u8);
    }

    fn f32(&mut self, value: f32) {
        self.bytes.extend_from_slice(&value.to_bits().to_be_bytes());
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }
}
