//! Lossless extraction of the three pre-v3 photo-scan mesh modifier shapes.
//!
//! This module only recognises the frozen, known shape emitted by the old
//! photo-scan commands.  It deliberately does not call the current factories:
//! the legacy graph is the source of truth for authored parameters, handles,
//! and stage topology.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GroupDef,
    InterfacePortDef, ParamSpecDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::effects::ParamConvert;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::scene_modifier_preset::{
    SceneContextValue, SceneEndpoint, SceneMeshReferenceFrame, SceneModifierInstanceDef,
    SceneModifierRecipe, SceneModifierStageDef, SceneNodeRef, SceneStageInput, SceneStageOutput,
    SceneStageScope, SceneStageSource, SceneTargetSelection,
};
use manifold_core::scene_source_identity::scene_source_definition_hash;
use manifold_core::NodeId;

const PREFIX: &str = "photoscan/";
const KINDS: &[Kind] = &[Kind::Elastic, Kind::Surface, Kind::Vortex];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Elastic,
    Surface,
    Vortex,
}

impl Kind {
    fn id(self) -> &'static str {
        match self {
            Self::Elastic => "elastic_sculpture",
            Self::Surface => "surface_peel",
            Self::Vortex => "vortex_fragments",
        }
    }

    fn atom_type(self) -> &'static str {
        match self {
            Self::Elastic => "node.wave_shear_mesh",
            Self::Surface | Self::Vortex => "node.transform_mesh_patches",
        }
    }

    fn atom_count(self) -> usize {
        match self {
            Self::Elastic => 2,
            Self::Surface | Self::Vortex => 1,
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Elastic => "Elastic Sculpture",
            Self::Surface => "Surface Peel",
            Self::Vortex => "Vortex Fragments",
        }
    }

    fn preset_id(self) -> &'static str {
        match self {
            Self::Elastic => "ElasticSculpture",
            Self::Surface => "SurfacePeel",
            Self::Vortex => "VortexFragments",
        }
    }

    fn from_stage_id(id: &str) -> Option<Self> {
        let rest = id.strip_prefix(PREFIX)?;
        KINDS.iter().copied().find(|kind| {
            let Some(target) = rest
                .strip_prefix(kind.id())
                .and_then(|suffix| suffix.strip_prefix('/'))
                .and_then(|suffix| suffix.strip_suffix("/stage"))
            else {
                return false;
            };
            !target.is_empty()
        })
    }
}

#[derive(Clone)]
struct StageCapture {
    kind: Kind,
    target: SceneNodeRef,
    source: SceneNodeRef,
    source_node: EffectGraphNode,
    stage: EffectGraphNode,
    order_before: BTreeSet<Kind>,
    order_after: BTreeSet<Kind>,
    radius: f64,
    offset: [f64; 3],
}

#[derive(Clone)]
struct Controller {
    kind: Kind,
    param: String,
    host_id: String,
    node: EffectGraphNode,
}

/// Extract a complete, known photo-scan legacy footprint into v3 scene
/// modifier instances. `None` means the owner has no recognised footprint;
/// every malformed or partial footprint is an error so the caller can retain
/// the original snapshot byte-for-byte.
pub(super) fn extract(owner: &EffectGraphDef) -> Result<Option<EffectGraphDef>, String> {
    if !owner.scene_modifiers.is_empty() {
        return Ok(None);
    }
    let Some(metadata) = owner.preset_metadata.as_ref() else {
        if contains_photoscan(&owner.nodes) {
            return Err("photoscan footprint has no host metadata".into());
        }
        return Ok(None);
    };

    let mut candidate = owner.clone();
    let mut stages = Vec::new();
    collect_and_restore(&mut candidate.nodes, &mut candidate.wires, &[], &mut stages)?;
    let controllers = collect_controllers(owner, metadata)?;
    validate_known_nodes(&owner.nodes, &[], false)?;
    validate_host_fanout(metadata)?;
    let recognized = !stages.is_empty() || !controllers.is_empty();
    if !recognized {
        return Ok(None);
    }
    if stages.is_empty() {
        return Err("photoscan controls exist without a stage".into());
    }
    let controller_ids: BTreeSet<u32> = controllers
        .iter()
        .map(|controller| controller.node.id)
        .collect();
    candidate
        .nodes
        .retain(|node| !controller_ids.contains(&node.id));

    let mut by_kind: BTreeMap<Kind, Vec<StageCapture>> = BTreeMap::new();
    for stage in stages {
        by_kind.entry(stage.kind).or_default().push(stage);
    }
    let allowed_binding_paths = allowed_binding_paths(&by_kind, &controllers);
    validate_order(&by_kind)?;
    let scene = find_scene(&candidate.nodes)?;
    let mut instances = Vec::new();
    for kind in ordered_kinds(&by_kind)? {
        let captures = by_kind.get(&kind).expect("ordered kind exists");
        instances.push(build_instance(
            owner,
            &candidate,
            metadata,
            scene.clone(),
            kind,
            captures,
            &controllers,
        )?);
    }

    let modifier_ids: BTreeMap<Kind, NodeId> = by_kind
        .keys()
        .map(|kind| (*kind, NodeId::new(format!("photoscan_{}", kind.id()))))
        .collect();
    retarget_host_bindings(&mut candidate, &modifier_ids, &allowed_binding_paths)?;
    candidate.scene_modifiers = instances;
    candidate.version = 3;
    Ok(Some(candidate))
}

fn contains_photoscan(nodes: &[EffectGraphNode]) -> bool {
    nodes.iter().any(|node| {
        node.node_id.as_str().starts_with(PREFIX)
            || node
                .group
                .as_deref()
                .is_some_and(|group| contains_photoscan(&group.nodes))
    })
}

fn validate_known_nodes(
    nodes: &[EffectGraphNode],
    scope: &[NodeId],
    inside_stage: bool,
) -> Result<(), String> {
    for node in nodes {
        if node.node_id.as_str().starts_with(PREFIX) {
            if inside_stage {
                // Stage children carry the same legacy prefix as their
                // wrapper; validate_stage_body performs the exact shape check.
            } else if stage_kind(node).is_none()
                && (scope.is_empty() && parse_controller(node.node_id.as_str()).is_none())
            {
                return Err(format!("unrecognised photoscan node '{}'", node.node_id));
            } else if !scope.is_empty() && stage_kind(node).is_none() {
                return Err(format!(
                    "nested photoscan control '{}' is unsupported",
                    node.node_id
                ));
            }
        }
        if let Some(group) = node.group.as_deref() {
            let mut child_scope = scope.to_vec();
            child_scope.push(node.node_id.clone());
            validate_known_nodes(
                &group.nodes,
                &child_scope,
                inside_stage || stage_kind(node).is_some(),
            )?;
        }
    }
    Ok(())
}

fn parse_controller(id: &str) -> Option<(Kind, String)> {
    let mut pieces = id.split('/');
    if pieces.next()? != "photoscan" {
        return None;
    }
    let kind_name = pieces.next()?;
    let kind = KINDS
        .iter()
        .copied()
        .find(|kind| kind_name == kind.id())?;
    let param = pieces.next()?.to_string();
    if pieces.next().is_some() || param.is_empty() {
        return None;
    }
    Some((kind, param))
}

fn collect_controllers(
    owner: &EffectGraphDef,
    metadata: &PresetMetadata,
) -> Result<Vec<Controller>, String> {
    let mut result = Vec::new();
    for node in &owner.nodes {
        let Some((kind, param)) = parse_controller(node.node_id.as_str()) else {
            if node.node_id.as_str().starts_with(PREFIX) {
                return Err(format!("unrecognised photoscan node '{}'", node.node_id));
            }
            continue;
        };
        if node.type_id != "node.value" {
            return Err(format!(
                "photoscan controller '{}' is not node.value",
                node.node_id
            ));
        }
        if owner
            .wires
            .iter()
            .any(|wire| wire.from_node == node.id || wire.to_node == node.id)
        {
            return Err(format!("photoscan controller '{}' is wired", node.node_id));
        }
        let host_id = metadata
            .bindings
            .iter()
            .find(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param: target_param } if node_id == &node.node_id && target_param == "value"))
            .map(|binding| binding.id.clone())
            .ok_or_else(|| format!("photoscan controller '{}' has no host binding", node.node_id))?;
        if result
            .iter()
            .any(|controller: &Controller| controller.kind == kind && controller.param == param)
        {
            return Err(format!("duplicate photoscan controller '{}'", node.node_id));
        }
        result.push(Controller {
            kind,
            param,
            host_id,
            node: node.clone(),
        });
    }
    Ok(result)
}

fn stage_kind(node: &EffectGraphNode) -> Option<Kind> {
    (node.type_id == "group")
        .then(|| Kind::from_stage_id(node.node_id.as_str()))
        .flatten()
}

fn stage_target(kind: Kind, id: &str) -> Option<&str> {
    let prefix = format!("photoscan/{}/", kind.id());
    let target = id.strip_prefix(&prefix)?.strip_suffix("/stage")?;
    (!target.is_empty()).then_some(target)
}

fn collect_and_restore(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    scope: &[NodeId],
    captures: &mut Vec<StageCapture>,
) -> Result<(), String> {
    let stage_ids: BTreeSet<u32> = nodes
        .iter()
        .filter_map(|node| stage_kind(node).map(|_| node.id))
        .collect();
    let pending: Vec<_> = nodes
        .iter()
        .filter_map(|node| stage_kind(node).map(|kind| (node.id, kind)))
        .collect();
    let mut restores: BTreeMap<u32, (u32, String)> = BTreeMap::new();
    for (stage_id, kind) in pending {
        let stage = nodes
            .iter()
            .find(|node| node.id == stage_id)
            .cloned()
            .ok_or("stage disappeared")?;
        let declared_target = stage_target(kind, stage.node_id.as_str())
            .ok_or_else(|| format!("{} stage has an invalid identity", stage.node_id))?;
        let group = stage
            .group
            .as_deref()
            .ok_or_else(|| format!("photoscan stage '{}' has no body", stage.node_id))?;
        validate_stage_body(kind, group)?;
        let terminal_id = stage_terminal(stage_id, &stage_ids, nodes, wires)?;
        let target_node = nodes
            .iter()
            .find(|node| node.id == terminal_id)
            .ok_or_else(|| format!("stage '{}' target disappeared", stage.node_id))?;
        if target_node.type_id != "node.scene_object" {
            return Err(format!(
                "stage '{}' does not target a scene object",
                stage.node_id
            ));
        }
        if declared_target != target_node.node_id.as_str() {
            return Err(format!(
                "stage '{}' target does not match terminal '{}'",
                stage.node_id, target_node.node_id
            ));
        }
        if wires
            .iter()
            .filter(|wire| wire.to_node == terminal_id && wire.to_port == "vertices")
            .count()
            != 1
        {
            return Err(format!(
                "stage '{}' target has side consumers",
                stage.node_id
            ));
        }
        if wires
            .iter()
            .filter(|wire| wire.to_node == stage_id)
            .any(|wire| !matches!(wire.to_port.as_str(), "current" | "reference"))
        {
            return Err(format!(
                "stage '{}' has an unknown input port",
                stage.node_id
            ));
        }
        let _output = exactly_one(
            wires
                .iter()
                .filter(|wire| wire.from_node == stage_id && wire.from_port == "vertices"),
            "stage output",
        )?;
        if wires
            .iter()
            .filter(|wire| wire.from_node == stage_id)
            .any(|wire| wire.from_port != "vertices")
        {
            return Err(format!(
                "stage '{}' has an unknown output port",
                stage.node_id
            ));
        }
        let current = exactly_one(
            wires
                .iter()
                .filter(|wire| wire.to_node == stage_id && wire.to_port == "current"),
            "stage current",
        )?;
        let reference = exactly_one(
            wires
                .iter()
                .filter(|wire| wire.to_node == stage_id && wire.to_port == "reference"),
            "stage reference",
        )?;
        let (source_id, source_port) = base_source(
            current.from_node,
            current.from_port.as_str(),
            &stage_ids,
            wires,
        )?;
        let (reference_id, reference_port) = base_source(
            reference.from_node,
            reference.from_port.as_str(),
            &stage_ids,
            wires,
        )?;
        if source_id != reference_id || source_port != reference_port {
            return Err(format!(
                "stage '{}' has different current/reference sources",
                stage.node_id
            ));
        }
        if source_port != "vertices" || reference_port != "vertices" {
            return Err(format!(
                "stage '{}' uses an unknown base source port",
                stage.node_id
            ));
        }
        if reference.from_node != source_id || reference.from_port != source_port {
            return Err(format!(
                "stage '{}' reference is not the exact base source",
                stage.node_id
            ));
        }
        let source_node = nodes
            .iter()
            .find(|node| node.id == source_id)
            .cloned()
            .ok_or("stage source disappeared")?;
        if !matches!(
            source_node.type_id.as_str(),
            "node.gltf_mesh_source" | "node.cube_mesh"
        ) || wires.iter().any(|wire| wire.to_node == source_id)
        {
            return Err(format!(
                "stage '{}' source is not a direct static mesh source",
                stage.node_id
            ));
        }
        let target = SceneNodeRef {
            scope: scope.to_vec(),
            node: target_node.node_id.clone(),
        };
        let source = SceneNodeRef {
            scope: scope.to_vec(),
            node: source_node.node_id.clone(),
        };
        let (radius, offset) = stage_calibration(kind, group)?;
        let before = incoming_stage_kinds(current.from_node, &stage_ids, nodes, wires, kind, true);
        let after = outgoing_stage_kinds(stage_id, &stage_ids, nodes, wires, kind);
        captures.push(StageCapture {
            kind,
            target,
            source,
            source_node,
            stage,
            order_before: before,
            order_after: after,
            radius,
            offset,
        });
        if let Some((previous_source, previous_port)) = restores.get(&terminal_id) {
            if *previous_source != source_id || previous_port != &source_port {
                return Err(format!(
                    "stage target '{}' has incompatible base sources",
                    target_node.node_id
                ));
            }
        } else {
            restores.insert(terminal_id, (source_id, source_port));
        }
    }
    wires.retain(|wire| !stage_ids.contains(&wire.from_node) && !stage_ids.contains(&wire.to_node));
    nodes.retain(|node| !stage_ids.contains(&node.id));
    for (target_id, (source_id, source_port)) in restores {
        if wires.iter().any(|wire| {
            wire.from_node == source_id
                && wire.from_port == source_port
                && wire.to_node == target_id
                && wire.to_port == "vertices"
        }) {
            continue;
        }
        wires.push(EffectGraphWire {
            from_node: source_id,
            from_port: source_port,
            to_node: target_id,
            to_port: "vertices".into(),
        });
    }
    for node in nodes.iter_mut() {
        if let Some(group) = node.group.as_mut() {
            let mut child_scope = scope.to_vec();
            child_scope.push(node.node_id.clone());
            collect_and_restore(&mut group.nodes, &mut group.wires, &child_scope, captures)?;
        }
    }
    Ok(())
}

fn stage_terminal(
    start: u32,
    stage_ids: &BTreeSet<u32>,
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
) -> Result<u32, String> {
    let mut current = start;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return Err("photoscan stage output chain contains a cycle".into());
        }
        let outgoing: Vec<_> = wires
            .iter()
            .filter(|wire| wire.from_node == current)
            .collect();
        if outgoing.iter().any(|wire| wire.from_port != "vertices") {
            return Err("photoscan stage output chain has an unknown output port".into());
        }
        let output = exactly_one(
            outgoing
                .into_iter()
                .filter(|wire| wire.from_port == "vertices"),
            "stage output chain",
        )?;
        if stage_ids.contains(&output.to_node) {
            if output.to_port != "current" {
                return Err("photoscan stage output chain crosses a non-current port".into());
            }
            current = output.to_node;
            continue;
        }
        let terminal = nodes
            .iter()
            .find(|node| node.id == output.to_node)
            .ok_or("photoscan stage output has an unknown terminal")?;
        if terminal.type_id != "node.scene_object" || output.to_port != "vertices" {
            return Err("photoscan stage output has an invalid terminal".into());
        }
        return Ok(terminal.id);
    }
}

fn exactly_one<'a, I>(mut values: I, what: &str) -> Result<&'a EffectGraphWire, String>
where
    I: Iterator<Item = &'a EffectGraphWire>,
{
    let Some(first) = values.next() else {
        return Err(format!("missing {what}"));
    };
    if values.next().is_some() {
        return Err(format!("ambiguous {what}"));
    }
    Ok(first)
}

fn base_source<'a>(
    mut node: u32,
    mut port: &'a str,
    stage_ids: &BTreeSet<u32>,
    wires: &'a [EffectGraphWire],
) -> Result<(u32, String), String> {
    let mut visited = BTreeSet::new();
    while stage_ids.contains(&node) {
        if port != "vertices" {
            return Err("photoscan stage source has an unknown output port".into());
        }
        if !visited.insert((node, port.to_string())) {
            return Err("photoscan stage chain contains a cycle".into());
        }
        let wire = exactly_one(
            wires
                .iter()
                .filter(|wire| wire.to_node == node && wire.to_port == "current"),
            "stage source",
        )?;
        node = wire.from_node;
        port = wire.from_port.as_str();
    }
    Ok((node, port.into()))
}

fn incoming_stage_kinds(
    node: u32,
    stage_ids: &BTreeSet<u32>,
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    kind: Kind,
    _before: bool,
) -> BTreeSet<Kind> {
    let mut result = BTreeSet::new();
    let mut current = node;
    while stage_ids.contains(&current) {
        let Some(stage) = nodes.iter().find(|node| node.id == current) else {
            break;
        };
        if let Some(stage_kind) = stage_kind(stage) && stage_kind != kind {
            result.insert(stage_kind);
        }
        let Some(wire) = wires
            .iter()
            .find(|wire| wire.to_node == current && wire.to_port == "current")
        else {
            break;
        };
        current = wire.from_node;
    }
    result
}

fn outgoing_stage_kinds(
    stage_id: u32,
    stage_ids: &BTreeSet<u32>,
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    kind: Kind,
) -> BTreeSet<Kind> {
    let mut result = BTreeSet::new();
    for wire in wires
        .iter()
        .filter(|wire| wire.from_node == stage_id && wire.from_port == "vertices")
    {
        if !stage_ids.contains(&wire.to_node) {
            continue;
        }
        if let Some(stage) = nodes.iter().find(|node| node.id == wire.to_node)
            && let Some(stage_kind) = stage_kind(stage) && stage_kind != kind {
            result.insert(stage_kind);
        }
    }
    result
}

fn validate_stage_body(kind: Kind, group: &GroupDef) -> Result<(), String> {
    let input_names: BTreeSet<_> = group
        .interface
        .inputs
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    let output_names: BTreeSet<_> = group
        .interface
        .outputs
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    if group.interface.inputs.len() != 2
        || input_names != ["current", "reference"].into_iter().collect()
        || group.interface.outputs.len() != 1
        || output_names != ["vertices"].into_iter().collect()
        || !group.interface.params.is_empty()
    {
        return Err(format!(
            "{} stage interface is not the known v2 shape",
            kind.id()
        ));
    }
    let atoms: Vec<_> = group
        .nodes
        .iter()
        .filter(|node| node.type_id == kind.atom_type())
        .collect();
    if atoms.len() != kind.atom_count()
        || group.nodes.iter().any(|node| {
            !matches!(
                node.type_id.as_str(),
                "system.group_input"
                    | "system.group_output"
                    | "node.wave_shear_mesh"
                    | "node.transform_mesh_patches"
            )
        })
    {
        return Err(format!(
            "{} stage contains an unknown or partial body",
            kind.id()
        ));
    }
    if group
        .nodes
        .iter()
        .filter(|node| node.type_id == "system.group_input")
        .count()
        != 2
        || group
            .nodes
            .iter()
            .filter(|node| node.type_id == "system.group_output")
            .count()
            != 1
    {
        return Err(format!("{} stage has invalid boundary nodes", kind.id()));
    }
    for atom in atoms {
        scalar_param(atom, "scale")?;
        if kind == Kind::Elastic {
            for key in ["origin_x", "origin_y", "origin_z"] {
                scalar_param(atom, key)?;
            }
        } else {
            for key in [
                "source_offset_x",
                "source_offset_y",
                "source_offset_z",
                "cell_size",
            ] {
                scalar_param(atom, key)?;
            }
        }
    }
    Ok(())
}

fn scalar_param(node: &EffectGraphNode, key: &str) -> Result<f64, String> {
    match node.params.get(key) {
        Some(SerializedParamValue::Float { value }) if value.is_finite() => Ok(f64::from(*value)),
        Some(SerializedParamValue::Int { value }) => Ok(f64::from(*value)),
        _ => Err(format!("{} missing finite numeric {}", node.node_id, key)),
    }
}

fn stage_calibration(kind: Kind, group: &GroupDef) -> Result<(f64, [f64; 3]), String> {
    let atoms: Vec<_> = group
        .nodes
        .iter()
        .filter(|node| node.type_id == kind.atom_type())
        .collect();
    let radius = scalar_param(atoms[0], "scale")?;
    let mut offset = [0.0; 3];
    if kind == Kind::Elastic {
        for (index, key) in ["origin_x", "origin_y", "origin_z"].into_iter().enumerate() {
            offset[index] = -scalar_param(atoms[0], key)?;
        }
    } else {
        for (index, key) in ["source_offset_x", "source_offset_y", "source_offset_z"]
            .into_iter()
            .enumerate()
        {
            offset[index] = scalar_param(atoms[0], key)?;
        }
    }
    for atom in atoms.iter().skip(1) {
        if scalar_param(atom, "scale")? != radius {
            return Err(format!("{} stage atoms disagree on radius", kind.id()));
        }
        let other_offset = if kind == Kind::Elastic {
            [
                -scalar_param(atom, "origin_x")?,
                -scalar_param(atom, "origin_y")?,
                -scalar_param(atom, "origin_z")?,
            ]
        } else {
            [
                scalar_param(atom, "source_offset_x")?,
                scalar_param(atom, "source_offset_y")?,
                scalar_param(atom, "source_offset_z")?,
            ]
        };
        if other_offset != offset {
            return Err(format!(
                "{} stage atoms disagree on source offset",
                kind.id()
            ));
        }
    }
    Ok((radius, offset))
}

fn validate_host_fanout(metadata: &PresetMetadata) -> Result<(), String> {
    let mut seen: BTreeMap<&str, (&str, f32, bool, bool)> = BTreeMap::new();
    for binding in &metadata.bindings {
        if !binding_is_photoscan(binding) {
            continue;
        }
        let entry = (
            binding.label.as_str(),
            binding.default_value,
            binding.default_mirrors_node_param,
            binding.user_added,
        );
        if let Some(previous) = seen.insert(binding.id.as_str(), entry) && previous != entry {
            return Err(format!(
                "photoscan binding fanout '{}' disagrees on host metadata",
                binding.id
            ));
        }
    }
    Ok(())
}

fn binding_is_photoscan(binding: &BindingDef) -> bool {
    matches!(&binding.target, BindingTarget::Node { node_id, .. } if node_id.as_str().starts_with(PREFIX))
}

fn allowed_binding_paths(
    by_kind: &BTreeMap<Kind, Vec<StageCapture>>,
    controllers: &[Controller],
) -> BTreeSet<String> {
    let mut allowed: BTreeSet<String> = controllers
        .iter()
        .map(|controller| format!("photoscan/{}/{}", controller.kind.id(), controller.param))
        .collect();
    for (kind, captures) in by_kind {
        for capture in captures {
            if let Some(group) = capture.stage.group.as_deref() {
                for node in &group.nodes {
                    if node.type_id == kind.atom_type() && !node.node_id.is_empty() {
                        allowed.insert(format!(
                            "photoscan/{}/{}/{}",
                            kind.id(),
                            capture.target.node,
                            node.node_id.as_str().rsplit('/').next().unwrap_or_default()
                        ));
                    }
                }
            }
        }
    }
    allowed
}

fn validate_order(by_kind: &BTreeMap<Kind, Vec<StageCapture>>) -> Result<(), String> {
    let kinds: BTreeSet<_> = by_kind.keys().copied().collect();
    let mut edges = BTreeSet::new();
    for captures in by_kind.values() {
        for capture in captures {
            for before in &capture.order_before {
                edges.insert((*before, capture.kind));
            }
            for after in &capture.order_after {
                edges.insert((capture.kind, *after));
            }
        }
    }
    for (a, b) in &edges {
        if edges.contains(&(*b, *a)) {
            return Err(format!(
                "photoscan modifier order conflicts between {} and {}",
                a.id(),
                b.id()
            ));
        }
    }
    if kinds.len() > 1 {
        let mut indegree: BTreeMap<Kind, usize> = kinds.iter().map(|kind| (*kind, 0)).collect();
        for (_, b) in &edges {
            *indegree.get_mut(b).expect("edge kind exists") += 1;
        }
        let roots = indegree.values().filter(|degree| **degree == 0).count();
        if roots != 1 {
            return Err("photoscan modifier stack order is ambiguous".into());
        }
    }
    Ok(())
}

fn ordered_kinds(by_kind: &BTreeMap<Kind, Vec<StageCapture>>) -> Result<Vec<Kind>, String> {
    let mut result = Vec::new();
    let mut remaining: BTreeSet<_> = by_kind.keys().copied().collect();
    while !remaining.is_empty() {
        let mut candidates = Vec::new();
        for kind in &remaining {
            let blocked = by_kind
                .values()
                .flat_map(|captures| captures.iter())
                .any(|capture| {
                    capture.order_after.contains(kind) && remaining.contains(&capture.kind)
                });
            if !blocked {
                candidates.push(*kind);
            }
        }
        if candidates.len() != 1 {
            return Err("photoscan modifier stack order is ambiguous".into());
        }
        remaining.remove(&candidates[0]);
        result.push(candidates[0]);
    }
    Ok(result)
}

fn find_scene(nodes: &[EffectGraphNode]) -> Result<SceneNodeRef, String> {
    let scenes: Vec<_> = nodes
        .iter()
        .filter(|node| node.type_id == "node.render_scene")
        .collect();
    if scenes.len() != 1 {
        return Err("photoscan migration requires exactly one render_scene".into());
    }
    Ok(SceneNodeRef {
        scope: Vec::new(),
        node: scenes[0].node_id.clone(),
    })
}

fn build_instance(
    original: &EffectGraphDef,
    _candidate: &EffectGraphDef,
    host_metadata: &PresetMetadata,
    scene: SceneNodeRef,
    kind: Kind,
    captures: &[StageCapture],
    controllers: &[Controller],
) -> Result<SceneModifierInstanceDef, String> {
    let first = captures.first().ok_or("empty photoscan kind")?;
    let signature = stage_signature(kind, &first.stage, &first.target)?;
    for capture in captures.iter().skip(1) {
        if stage_signature(kind, &capture.stage, &capture.target)? != signature {
            return Err(format!("{} stage bodies diverge across targets", kind.id()));
        }
    }
    let mut stage = first.stage.clone();
    normalize_stage_wrapper_handle(kind, &first.target, &mut stage);
    let group = stage.group.as_mut().ok_or("photoscan stage has no body")?;
    reduce_stage_ids(kind, &first.target, group)?;
    let kind_controllers: Vec<_> = controllers
        .iter()
        .filter(|controller| controller.kind == kind)
        .collect();
    if kind_controllers.is_empty() {
        return Err(format!("{} stage has no controllers", kind.id()));
    }
    add_context_ports(kind, group)?;
    add_controllers_and_wires(group, &kind_controllers, host_metadata, kind)?;
    stage.id = 1;
    stage.node_id = NodeId::new(format!("{}_stage", kind.id()));
    let params = unique_kind_params(host_metadata, kind, &kind_controllers)?;
    let bindings = local_bindings(host_metadata, kind, captures, &kind_controllers)?;
    let enabled_param = kind_controllers
        .iter()
        .find(|controller| controller.param == "enabled")
        .map(|controller| controller.host_id.clone())
        .ok_or_else(|| format!("{} controller set has no enabled binding", kind.id()))?;
    let metadata = PresetMetadata {
        id: PresetTypeId::from_string(kind.preset_id().into()),
        display_name: kind.display_name().into(),
        category: host_metadata.category.clone(),
        osc_prefix: format!("photoscan_{}", kind.id()),
        legacy_discriminant: None,
        available: false,
        is_line_based: false,
        layer_types: host_metadata.layer_types.clone(),
        params,
        bindings,
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
        scene_bounds: None,
        scene_modifier: Some(SceneModifierRecipe {
            schema_version: 1,
            singleton: false,
            enabled_param,
            preparation_params: Vec::new(),
            initializers: Vec::new(),
            calibrations: Vec::new(),
            stages: vec![SceneModifierStageDef {
                group: stage.node_id.clone(),
                scope: SceneStageScope::EachObject,
                inputs: vec![
                    SceneStageInput {
                        port: "current".into(),
                        source: SceneStageSource::Previous {
                            endpoint: SceneEndpoint::Vertices,
                        },
                    },
                    SceneStageInput {
                        port: "reference".into(),
                        source: SceneStageSource::Reference {
                            endpoint: SceneEndpoint::Vertices,
                        },
                    },
                    SceneStageInput {
                        port: "sourceRadius".into(),
                        source: SceneStageSource::Context {
                            value: SceneContextValue::SceneRadius,
                        },
                    },
                    SceneStageInput {
                        port: "sourceOffsetX".into(),
                        source: SceneStageSource::Context {
                            value: SceneContextValue::SourceOffsetX,
                        },
                    },
                    SceneStageInput {
                        port: "sourceOffsetY".into(),
                        source: SceneStageSource::Context {
                            value: SceneContextValue::SourceOffsetY,
                        },
                    },
                    SceneStageInput {
                        port: "sourceOffsetZ".into(),
                        source: SceneStageSource::Context {
                            value: SceneContextValue::SourceOffsetZ,
                        },
                    },
                ],
                outputs: vec![SceneStageOutput {
                    port: "vertices".into(),
                    endpoint: SceneEndpoint::Vertices,
                }],
            }],
        }),
    };
    let source_frames = frames(original, _candidate, scene.clone(), captures)?;
    Ok(SceneModifierInstanceDef {
        id: NodeId::new(format!("photoscan_{}", kind.id())),
        scene,
        targets: SceneTargetSelection::Explicit {
            objects: captures
                .iter()
                .map(|capture| capture.target.clone())
                .collect(),
        },
        mesh_frames: source_frames,
        legacy_math_view_carrier: None,
        graph: Box::new(EffectGraphDef {
            version: 3,
            name: Some(kind.display_name().into()),
            description: None,
            preset_metadata: Some(metadata),
            scene_modifiers: Vec::new(),
            nodes: vec![stage],
            wires: Vec::new(),
        }),
    })
}

fn stage_signature(
    kind: Kind,
    stage: &EffectGraphNode,
    target: &SceneNodeRef,
) -> Result<Vec<u8>, String> {
    let mut copy = stage.clone();
    copy.id = 0;
    copy.node_id = NodeId::new("stage");
    normalize_stage_wrapper_handle(kind, target, &mut copy);
    let group = copy.group.as_mut().ok_or("photoscan stage has no body")?;
    reduce_stage_ids(kind, target, group)?;
    for node in &mut group.nodes {
        if let Some(params) = node.params.get_mut("scale") {
            *params = SerializedParamValue::Float { value: 0.0 };
        }
        for key in [
            "origin_x",
            "origin_y",
            "origin_z",
            "source_offset_x",
            "source_offset_y",
            "source_offset_z",
        ] {
            if let Some(params) = node.params.get_mut(key) {
                *params = SerializedParamValue::Float { value: 0.0 };
            }
        }
    }
    serde_json::to_vec(&copy).map_err(|error| error.to_string())
}

fn normalize_stage_wrapper_handle(kind: Kind, target: &SceneNodeRef, stage: &mut EffectGraphNode) {
    let generated = format!(
        "Photoscan_{}_{}_stage",
        kind.id(),
        target.node.as_str().replace('/', "_")
    );
    if stage.handle.as_deref() == Some(generated.as_str()) {
        stage.handle = Some(format!("{}Stage", kind.display_name().replace(' ', "")));
    }
}

fn reduce_stage_ids(kind: Kind, target: &SceneNodeRef, group: &mut GroupDef) -> Result<(), String> {
    let prefix = format!("photoscan/{}/{}/", kind.id(), target.node);
    let handle_prefix = format!(
        "photoscan_{}_{}_",
        kind.id(),
        target.node.as_str().replace('/', "_")
    );
    for node in &mut group.nodes {
        if let Some(rest) = node.node_id.as_str().strip_prefix(&prefix) {
            node.node_id = NodeId::new(rest);
        } else {
            return Err(format!(
                "{} stage has an unexpected node id '{}'",
                kind.id(),
                node.node_id
            ));
        }
        if let Some(handle) = node.handle.as_mut()
            && let Some(rest) = handle.strip_prefix(&handle_prefix) {
            *handle = rest.into();
        }
    }
    Ok(())
}

fn add_context_ports(kind: Kind, group: &mut GroupDef) -> Result<(), String> {
    for (name, port_type) in [
        ("sourceRadius", "Scalar(F32)"),
        ("sourceOffsetX", "Scalar(F32)"),
        ("sourceOffsetY", "Scalar(F32)"),
        ("sourceOffsetZ", "Scalar(F32)"),
    ] {
        group.interface.inputs.push(InterfacePortDef {
            name: name.into(),
            port_type: port_type.into(),
        });
    }
    let mut next_id = group
        .nodes
        .iter()
        .map(|node| node.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let atom_ids: Vec<u32> = group
        .nodes
        .iter()
        .filter(|node| node.type_id == kind.atom_type())
        .map(|node| node.id)
        .collect();
    for (name, port) in [
        ("sourceRadius", "scale"),
        ("sourceOffsetX", "source_offset_x"),
        ("sourceOffsetY", "source_offset_y"),
        ("sourceOffsetZ", "source_offset_z"),
    ] {
        let input_id = next_id;
        next_id += 1;
        let input_node_id = format!("group_{}", name.to_ascii_lowercase());
        group.nodes.push(EffectGraphNode {
            id: input_id,
            node_id: NodeId::new(&input_node_id),
            type_id: "system.group_input".into(),
            handle: Some(input_node_id.clone()),
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        });
        for atom_id in &atom_ids {
            if kind == Kind::Elastic && port.starts_with("source_offset") {
                continue;
            }
            group.wires.push(EffectGraphWire {
                from_node: input_id,
                from_port: name.into(),
                to_node: *atom_id,
                to_port: port.into(),
            });
        }
    }
    if kind == Kind::Elastic {
        for (axis, offset) in [
            ("x", "sourceOffsetX"),
            ("y", "sourceOffsetY"),
            ("z", "sourceOffsetZ"),
        ] {
            let math_id = next_id;
            next_id += 1;
            let math_node_id = format!("origin_{}_neg", axis);
            let mut params = BTreeMap::new();
            params.insert("a".into(), SerializedParamValue::Float { value: 0.0 });
            params.insert("op".into(), SerializedParamValue::Enum { value: 1 });
            group.nodes.push(EffectGraphNode {
                id: math_id,
                node_id: NodeId::new(&math_node_id),
                type_id: "node.math".into(),
                handle: Some(math_node_id.clone()),
                params,
                exposed_params: BTreeSet::new(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            });
            let input_id = group
                .nodes
                .iter()
                .find(|node| {
                    node.node_id == NodeId::new(format!("group_{}", offset.to_ascii_lowercase()))
                })
                .map(|node| node.id)
                .ok_or("context input disappeared")?;
            group.wires.push(EffectGraphWire {
                from_node: input_id,
                from_port: offset.into(),
                to_node: math_id,
                to_port: "b".into(),
            });
            for atom_id in &atom_ids {
                group.wires.push(EffectGraphWire {
                    from_node: math_id,
                    from_port: "out".into(),
                    to_node: *atom_id,
                    to_port: format!("origin_{}", axis),
                });
            }
        }
    }
    Ok(())
}

fn add_controllers_and_wires(
    group: &mut GroupDef,
    controllers: &[&Controller],
    _metadata: &PresetMetadata,
    kind: Kind,
) -> Result<(), String> {
    let mut next_id = group
        .nodes
        .iter()
        .map(|node| node.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    for controller in controllers {
        let mut node = controller.node.clone();
        node.id = next_id;
        next_id += 1;
        node.node_id = NodeId::new(format!("controller_{}", controller.host_id));
        group.nodes.push(node.clone());
    }
    if !group
        .nodes
        .iter()
        .any(|node| node.type_id == kind.atom_type())
    {
        return Err(format!("{} stage has no transform atom", kind.id()));
    }
    Ok(())
}

fn unique_kind_params(
    metadata: &PresetMetadata,
    kind: Kind,
    controllers: &[&Controller],
) -> Result<Vec<ParamSpecDef>, String> {
    let ids: BTreeSet<_> = controllers
        .iter()
        .map(|controller| controller.host_id.as_str())
        .collect();
    let result: Vec<_> = metadata
        .params
        .iter()
        .filter(|param| ids.contains(param.id.as_str()))
        .cloned()
        .collect();
    if result.len() != ids.len() {
        return Err(format!("{} controller metadata is incomplete", kind.id()));
    }
    Ok(result)
}

fn local_bindings(
    metadata: &PresetMetadata,
    kind: Kind,
    captures: &[StageCapture],
    controllers: &[&Controller],
) -> Result<Vec<BindingDef>, String> {
    let mut result = Vec::new();
    for controller in controllers {
        let Some(binding) = metadata.bindings.iter().find(|binding| {
            matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id.as_str() == format!("photoscan/{}/{}", kind.id(), controller.param) && param == "value")
        }) else {
            return Err(format!(
                "{} controller '{}' has no local binding",
                kind.id(),
                controller.param
            ));
        };
        let mut local = binding.clone();
        local.target = BindingTarget::Node {
            node_id: NodeId::new(format!("controller_{}", controller.host_id)),
            param: "value".into(),
        };
        result.push(local);
    }
    let mut expected_leaf_set = None;
    for capture in captures {
        let prefix = format!("photoscan/{}/{}/", kind.id(), capture.target.node);
        let group = capture
            .stage
            .group
            .as_deref()
            .ok_or("photoscan stage has no body")?;
        let allowed_leaf_ids: BTreeSet<_> = group
            .nodes
            .iter()
            .filter(|node| node.type_id == kind.atom_type())
            .filter_map(|node| node.node_id.as_str().strip_prefix(&prefix))
            .collect();
        let mut leaf_set = Vec::new();
        for binding in &metadata.bindings {
            let BindingTarget::Node { node_id, param } = &binding.target else {
                continue;
            };
            let Some(rest) = node_id.as_str().strip_prefix(&prefix) else {
                continue;
            };
            if rest.is_empty() || rest.ends_with('/') {
                return Err(format!("{} has an invalid leaf binding target", kind.id()));
            }
            if !allowed_leaf_ids.contains(rest) {
                return Err(format!(
                    "{} has an unknown leaf binding target '{}'",
                    kind.id(),
                    node_id
                ));
            }
            let mut local = binding.clone();
            local.target = BindingTarget::Node {
                node_id: NodeId::new(rest),
                param: param.clone(),
            };
            leaf_set.push(local);
        }
        if leaf_set.is_empty() {
            return Err(format!("{} target has no leaf bindings", kind.id()));
        }
        sort_bindings(&mut leaf_set)?;
        leaf_set.dedup();
        if let Some(expected) = &expected_leaf_set {
            if expected != &leaf_set {
                return Err(format!(
                    "{} target leaf binding metadata diverges",
                    kind.id()
                ));
            }
        } else {
            expected_leaf_set = Some(leaf_set);
        }
    }
    if let Some(mut leaf_set) = expected_leaf_set {
        result.append(&mut leaf_set);
    }
    if result.is_empty() {
        return Err(format!("{} has no local bindings", kind.id()));
    }
    Ok(result)
}

fn sort_bindings(bindings: &mut [BindingDef]) -> Result<(), String> {
    let mut keyed = Vec::with_capacity(bindings.len());
    for binding in bindings.iter().cloned() {
        let key = serde_json::to_vec(&binding).map_err(|error| error.to_string())?;
        keyed.push((key, binding));
    }
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    for (slot, (_, binding)) in bindings.iter_mut().zip(keyed) {
        *slot = binding;
    }
    Ok(())
}

fn frames(
    original: &EffectGraphDef,
    _candidate: &EffectGraphDef,
    _scene: SceneNodeRef,
    captures: &[StageCapture],
) -> Result<Vec<SceneMeshReferenceFrame>, String> {
    let first_radius = captures
        .first()
        .map(|capture| capture.radius)
        .ok_or("no captures")?;
    if captures
        .iter()
        .any(|capture| capture.radius != first_radius)
    {
        return Err("photoscan material radii disagree".into());
    }
    captures
        .iter()
        .map(|capture| {
            Ok(SceneMeshReferenceFrame {
                target: capture.target.clone(),
                source: capture.source.clone(),
                source_definition_hash: scene_source_definition_hash(
                    original,
                    &capture.source_node,
                )
                .map_err(|error| error.to_string())?,
                source_offset: capture.offset,
                scene_radius: capture.radius,
            })
        })
        .collect()
}

fn retarget_host_bindings(
    candidate: &mut EffectGraphDef,
    modifier_ids: &BTreeMap<Kind, NodeId>,
    allowed_paths: &BTreeSet<String>,
) -> Result<(), String> {
    let Some(metadata) = candidate.preset_metadata.as_mut() else {
        return Err("photoscan host metadata disappeared".into());
    };
    let bindings = std::mem::take(&mut metadata.bindings);
    let mut retained = Vec::with_capacity(bindings.len());
    let mut seen = BTreeSet::new();
    for mut binding in bindings {
        let BindingTarget::Node { node_id, .. } = &binding.target else {
            retained.push(binding);
            continue;
        };
        let Some((kind, _)) = parse_controller(node_id.as_str()).or_else(|| {
            KINDS.iter().copied().find_map(|kind| {
                node_id
                    .as_str()
                    .strip_prefix(&format!("photoscan/{}/", kind.id()))
                    .map(|_| (kind, String::new()))
            })
        }) else {
            if node_id.as_str().starts_with(PREFIX) {
                return Err(format!(
                    "unrecognised photoscan binding target '{}'",
                    node_id
                ));
            }
            retained.push(binding);
            continue;
        };
        if !allowed_paths.contains(node_id.as_str()) {
            return Err(format!(
                "unrecognised photoscan binding target '{}'",
                node_id
            ));
        }
        let modifier_id = modifier_ids
            .get(&kind)
            .ok_or_else(|| format!("binding '{}' has no migrated modifier", binding.id))?
            .clone();
        if !seen.insert((kind, binding.id.clone())) {
            continue;
        }
        let original_id = binding.id.clone();
        binding.target = BindingTarget::SceneModifier {
            modifier_id,
            param_id: original_id,
        };
        binding.convert = ParamConvert::Float;
        binding.scale = 1.0;
        binding.offset = 0.0;
        retained.push(binding);
    }
    metadata.bindings = retained;
    Ok(())
}
