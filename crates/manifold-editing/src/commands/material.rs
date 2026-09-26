//! Atomic material-inspector edits.
//!
//! Material actions address the existing host parameter slots.  The command
//! validates the complete batch before changing any slot, then keeps the
//! captured bases for a single undo/redo unit.  Graph topology and renderer
//! state remain untouched.

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode};
use manifold_core::effects::{ParamConvert, ParamId};
use manifold_core::graph_target::GraphTarget;
use manifold_core::material_inspector::{MaterialGroup, MaterialParamRole};
use manifold_core::project::Project;
use manifold_core::scene_modifier_preset::SceneNodeRef;

use crate::command::Command;

const EPSILON: f32 = 1.0e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialEditKind {
    Feature,
    Look,
    Placement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialEditContext {
    pub object: SceneNodeRef,
    pub material: SceneNodeRef,
    pub kind: MaterialEditKind,
    pub expected_preset_id: manifold_core::PresetTypeId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaterialParamChange {
    pub param_id: ParamId,
    pub expected: f32,
    pub value: f32,
}

/// Validate a prepared material edit against the current content snapshot.
///
/// The app uses this same function when deciding whether to enable a compound
/// affordance.  The command calls it again on the content thread, since a
/// graph, binding, or parameter may have changed since the UI prepared it.
pub fn validate_material_edit(
    project: &Project,
    target: &GraphTarget,
    context: &MaterialEditContext,
    changes: &[MaterialParamChange],
    catalog_default: Option<&EffectGraphDef>,
) -> Result<(), String> {
    if matches!(target, GraphTarget::SceneModifier { .. }) {
        return Err("material edits require an effect or generator graph target".to_string());
    }
    if changes.is_empty() {
        return Err("material edit contains no parameter changes".to_string());
    }

    let Some(owner) = project.graph_target_owner(target) else {
        return Err(format!("graph target {} no longer exists", target.label()));
    };
    if owner.effect_type() != &context.expected_preset_id {
        return Err("material edit targets a different preset instance".to_string());
    }
    if let Some(default) = catalog_default
        && let Some(metadata) = default.preset_metadata.as_ref()
        && metadata.id != context.expected_preset_id
    {
        return Err("catalog material graph belongs to a different preset".to_string());
    }

    let mut seen = std::collections::BTreeSet::new();
    for change in changes {
        let id = change.param_id.as_ref();
        if !seen.insert(id.to_string()) {
            return Err(format!("material edit contains duplicate parameter {id}"));
        }
        let Some(param) = owner.params.get(id) else {
            return Err(format!("parameter {id} no longer exists"));
        };
        if !change.expected.is_finite() || !change.value.is_finite() {
            return Err(format!("parameter {id} has a non-finite material value"));
        }
        if change.expected < param.spec.min - EPSILON
            || change.expected > param.spec.max + EPSILON
            || change.value < param.spec.min - EPSILON
            || change.value > param.spec.max + EPSILON
        {
            return Err(format!("parameter {id} is outside its declared range"));
        }
        let enum_param = param.spec.whole_numbers || !param.spec.value_labels.is_empty();
        if enum_param && (change.expected.round() - change.expected).abs() > EPSILON {
            return Err(format!("parameter {id} has a non-native enum value"));
        }
        if enum_param && (change.value.round() - change.value).abs() > EPSILON {
            return Err(format!("parameter {id} has a non-native enum value"));
        }
        if !param.spec.value_labels.is_empty()
            && [change.expected, change.value]
                .into_iter()
                .any(|value| value < 0.0 || value >= param.spec.value_labels.len() as f32)
        {
            return Err(format!("parameter {id} has a non-native enum value"));
        }
        let actual = if owner.base_tracked {
            param.base
        } else {
            param.value
        };
        if !actual.is_finite() || (actual - change.expected).abs() > EPSILON {
            return Err(format!(
                "parameter {id} changed before the material edit was applied"
            ));
        }
    }

    let Some(graph) = project
        .graph_for_target(target, None)
        .or_else(|| project.graph_for_target(target, catalog_default))
    else {
        return Err("material graph is unavailable for this preset instance".to_string());
    };

    validate_graph_ownership(owner, graph, catalog_default, context, changes)
}

fn validate_graph_ownership(
    owner: &manifold_core::effects::PresetInstance,
    graph: &EffectGraphDef,
    catalog_default: Option<&EffectGraphDef>,
    context: &MaterialEditContext,
    changes: &[MaterialParamChange],
) -> Result<(), String> {
    let Some((object_doc_id, object, object_wires)) = resolve_node(graph, &context.object) else {
        return Err("selected object is no longer present in the material graph".to_string());
    };
    let Some((material_doc_id, material, material_wires)) = resolve_node(graph, &context.material)
    else {
        return Err("selected material is no longer present in the material graph".to_string());
    };
    if object.type_id != "node.scene_object" {
        return Err("selected object is not a scene object".to_string());
    }
    let Some((scope_nodes, _)) = scope_level(&graph.nodes, &graph.wires, &context.object.scope)
    else {
        return Err("selected object scope is no longer present in the material graph".to_string());
    };
    if material.type_id != "node.pbr_material"
        || !object_references_material(
            scope_nodes,
            object_wires,
            object_doc_id,
            material_doc_id,
            context,
        )
    {
        return Err("selected object no longer references the selected PBR material".to_string());
    }
    if count_node_id(&graph.nodes, &context.material.node) != 1 {
        return Err("selected material identity is ambiguous across graph scopes".to_string());
    }

    let metadata = graph
        .preset_metadata
        .as_ref()
        .or_else(|| catalog_default.and_then(|default| default.preset_metadata.as_ref()));
    for change in changes {
        let id = change.param_id.as_ref();
        let bindings: Vec<_> = metadata
            .into_iter()
            .flat_map(|meta| meta.bindings.iter())
            .filter(|binding| binding.id == id)
            .collect();

        if bindings.is_empty() {
            return Err(format!("parameter {id} has no material binding"));
        }
        for binding in &bindings {
            let BindingTarget::Node { node_id, param } = &binding.target else {
                return Err(format!("parameter {id} is not owned by a material node"));
            };
            if node_id != &context.material.node {
                return Err(format!(
                    "parameter {id} is bound outside the selected material"
                ));
            }
            let Some(slot) = owner.params.get(id) else {
                return Err(format!("parameter {id} is missing from the live manifest"));
            };
            validate_descriptor_identity(
                slot.spec.material_role,
                binding.convert,
                binding,
                &slot.spec,
                id,
            )?;
            validate_param_ownership(
                owner,
                material,
                material_wires,
                slot.spec.material_role,
                param,
                bindings.len(),
                context.kind,
                id,
            )?;
        }
    }

    if context.kind == MaterialEditKind::Look
        && skin_temporarily_owns_object(object_wires, object_doc_id, scope_nodes)
    {
        return Err("material look is blocked while emissive Skin owns this object".to_string());
    }
    Ok(())
}

/// Match the direct or one-group re-export supported by the scene projection.
fn object_references_material(
    nodes: &[EffectGraphNode],
    wires: &[manifold_core::effect_graph_def::EffectGraphWire],
    object: u32,
    material: u32,
    context: &MaterialEditContext,
) -> bool {
    let mut inputs = wires
        .iter()
        .filter(|wire| wire.to_node == object && wire.to_port == "material");
    let Some(input) = inputs.next() else {
        return false;
    };
    if inputs.next().is_some() {
        return false;
    }
    if context.object.scope == context.material.scope {
        return input.from_node == material;
    }
    let Some(extra) = context
        .material
        .scope
        .strip_prefix(context.object.scope.as_slice())
    else {
        return false;
    };
    let [group_id] = extra else { return false };
    let mut matches = nodes
        .iter()
        .filter(|node| node.node_id == *group_id && node.id == input.from_node);
    let Some(group_node) = matches.next() else {
        return false;
    };
    if matches.next().is_some() {
        return false;
    }
    let Some(group) = group_node.group.as_ref() else {
        return false;
    };
    let mut outputs = group
        .nodes
        .iter()
        .filter(|node| node.type_id == manifold_core::effect_graph_def::GROUP_OUTPUT_TYPE_ID);
    let Some(output) = outputs.next() else {
        return false;
    };
    if outputs.next().is_some() {
        return false;
    }
    let mut exports = group
        .wires
        .iter()
        .filter(|wire| wire.to_node == output.id && wire.to_port == input.from_port);
    exports
        .next()
        .is_some_and(|wire| wire.from_node == material)
        && exports.next().is_none()
}

fn validate_param_ownership(
    owner: &manifold_core::effects::PresetInstance,
    material: &EffectGraphNode,
    wires: &[manifold_core::effect_graph_def::EffectGraphWire],
    role: Option<MaterialParamRole>,
    inner_param: &str,
    binding_count: usize,
    kind: MaterialEditKind,
    outer_id: &str,
) -> Result<(), String> {
    let external_wire = wires
        .iter()
        .any(|wire| wire.to_node == material.id && wire.to_port == inner_param);
    let attached = has_attachment(owner, outer_id);
    let is_mode = matches!(role, Some(MaterialParamRole::FeatureMode(_)));
    let is_feature_member = matches!(
        role,
        Some(MaterialParamRole::Scalar(MaterialGroup::Feature(_)))
            | Some(MaterialParamRole::Colour(MaterialGroup::Feature(_), _, _))
            | Some(MaterialParamRole::FeatureMode(_))
    );
    let is_placement = matches!(role, Some(MaterialParamRole::Placement(_, _)));

    match kind {
        MaterialEditKind::Feature => {
            if !is_feature_member {
                return Err(format!(
                    "parameter {outer_id} is not a material feature role"
                ));
            }
            if binding_count > 1 {
                return Err(format!(
                    "feature parameter {outer_id} has multiple material owners"
                ));
            }
            if external_wire {
                return Err(format!("feature parameter {outer_id} is wire-driven"));
            }
            if attached && !is_mode {
                return Err(format!("feature factor {outer_id} is externally driven"));
            }
        }
        MaterialEditKind::Placement => {
            if binding_count > 1 || external_wire || attached || !is_placement {
                return Err(format!(
                    "placement parameter {outer_id} has external ownership"
                ));
            }
        }
        MaterialEditKind::Look => {
            if binding_count > 1 || external_wire || attached {
                return Err(format!(
                    "material look target {outer_id} has external ownership"
                ));
            }
        }
    }
    Ok(())
}

fn validate_descriptor_identity(
    role: Option<MaterialParamRole>,
    convert: ParamConvert,
    binding: &manifold_core::effect_graph_def::BindingDef,
    spec: &manifold_core::effect_graph_def::ParamSpecDef,
    id: &str,
) -> Result<(), String> {
    let Some(role) = role else {
        return Err(format!("parameter {id} has no material role"));
    };
    if spec.invert || spec.curve != manifold_core::macro_bank::MacroCurve::Linear {
        return Err(format!("parameter {id} uses a custom card response"));
    }
    if (binding.scale - 1.0).abs() > EPSILON || binding.offset.abs() > EPSILON {
        return Err(format!("parameter {id} uses a custom binding calibration"));
    }
    let expected = if spec.is_toggle {
        ParamConvert::BoolThreshold
    } else if matches!(role, MaterialParamRole::FeatureMode(_)) || !spec.value_labels.is_empty() {
        ParamConvert::EnumRound
    } else {
        ParamConvert::Float
    };
    if convert != expected {
        return Err(format!("parameter {id} uses an unsupported conversion"));
    }
    Ok(())
}

fn has_attachment(owner: &manifold_core::effects::PresetInstance, id: &str) -> bool {
    owner
        .drivers
        .as_ref()
        .is_some_and(|items| items.iter().any(|item| item.param_id.as_ref() == id))
        || owner
            .envelopes
            .as_ref()
            .is_some_and(|items| items.iter().any(|item| item.param_id.as_ref() == id))
        || owner
            .ableton_mappings
            .as_ref()
            .is_some_and(|items| items.iter().any(|item| item.param_id.as_ref() == id))
        || owner
            .audio_mods
            .as_ref()
            .is_some_and(|items| items.iter().any(|item| item.param_id.as_ref() == id))
        || owner
            .automation_lanes
            .as_ref()
            .is_some_and(|items| items.iter().any(|item| item.param_id.as_ref() == id))
}

fn skin_temporarily_owns_object(
    object_wires: &[manifold_core::effect_graph_def::EffectGraphWire],
    object_doc_id: u32,
    scope_nodes: &[EffectGraphNode],
) -> bool {
    const EMISSIVE_PORT: &str = "emissive_map";
    object_wires.iter().any(|wire| {
        wire.to_node == object_doc_id
            && wire.to_port == EMISSIVE_PORT
            && scope_nodes
                .iter()
                .any(|node| node.id == wire.from_node && node.type_id == "node.layer_source")
    })
}

fn count_node_id(nodes: &[EffectGraphNode], id: &manifold_core::NodeId) -> usize {
    nodes
        .iter()
        .map(|node| {
            usize::from(node.node_id == *id)
                + node
                    .group
                    .as_deref()
                    .map_or(0, |group| count_node_id(&group.nodes, id))
        })
        .sum()
}

fn resolve_node<'a>(
    graph: &'a EffectGraphDef,
    reference: &SceneNodeRef,
) -> Option<(
    u32,
    &'a EffectGraphNode,
    &'a [manifold_core::effect_graph_def::EffectGraphWire],
)> {
    let (nodes, wires) = scope_level(&graph.nodes, &graph.wires, &reference.scope)?;
    let mut matching = nodes.iter().filter(|node| node.node_id == reference.node);
    let node = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some((node.id, node, wires))
}

fn scope_level<'a>(
    nodes: &'a [EffectGraphNode],
    wires: &'a [manifold_core::effect_graph_def::EffectGraphWire],
    scope: &[manifold_core::NodeId],
) -> Option<(
    &'a [EffectGraphNode],
    &'a [manifold_core::effect_graph_def::EffectGraphWire],
)> {
    let Some((head, tail)) = scope.split_first() else {
        return Some((nodes, wires));
    };
    let mut matching = nodes.iter().filter(|node| node.node_id == *head);
    let group = matching.next()?.group.as_deref()?;
    if matching.next().is_some() {
        return None;
    }
    scope_level(&group.nodes, &group.wires, tail)
}

#[derive(Debug)]
pub struct ChangeMaterialParamsCommand {
    target: GraphTarget,
    context: MaterialEditContext,
    changes: Vec<MaterialParamChange>,
    catalog_default: Option<EffectGraphDef>,
    description: String,
    old_values: Option<Vec<f32>>,
    rejection: Option<String>,
    applied: bool,
}

impl ChangeMaterialParamsCommand {
    pub fn new(
        target: GraphTarget,
        context: MaterialEditContext,
        changes: Vec<MaterialParamChange>,
        description: String,
        catalog_default: Option<EffectGraphDef>,
    ) -> Self {
        Self {
            target,
            context,
            changes,
            catalog_default,
            description,
            old_values: None,
            rejection: None,
            applied: false,
        }
    }
}

impl Command for ChangeMaterialParamsCommand {
    fn execute(&mut self, project: &mut Project) {
        self.rejection = None;
        self.applied = false;
        self.old_values = None;
        if let Err(reason) = validate_material_edit(
            project,
            &self.target,
            &self.context,
            &self.changes,
            self.catalog_default.as_ref(),
        ) {
            self.rejection = Some(reason);
            return;
        }

        let changes = &self.changes;
        let Some(old_values) = project.with_preset_graph_mut(&self.target, |owner| {
            let old_values: Vec<f32> = changes
                .iter()
                .map(|change| owner.get_base_param(change.param_id.as_ref()))
                .collect();
            if changes
                .iter()
                .any(|change| !owner.params.contains(change.param_id.as_ref()))
            {
                return None;
            }
            for change in changes {
                if !owner.set_base_param_by_id(change.param_id.as_ref(), change.value) {
                    return None;
                }
            }
            Some(old_values)
        }) else {
            self.rejection = Some("graph target disappeared before material edit".to_string());
            return;
        };
        let Some(old_values) = old_values else {
            self.rejection =
                Some("material parameter disappeared before material edit".to_string());
            return;
        };
        self.old_values = Some(old_values);
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let Some(old_values) = self.old_values.as_ref() else {
            return;
        };
        let changes = &self.changes;
        project.with_preset_graph_mut(&self.target, |owner| {
            for (change, old_value) in changes.iter().zip(old_values) {
                owner.set_base_param_by_id(change.param_id.as_ref(), *old_value);
            }
        });
        self.applied = false;
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{
        BindingDef, EFFECT_GRAPH_VERSION, EffectGraphNode, EffectGraphWire, GroupDef,
        GroupInterface, ParamSpecDef, PresetMetadata,
    };
    use manifold_core::effects::{ParamConvert, ParameterDriver, PresetInstance};
    use manifold_core::id::NodeId;
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::types::{BeatDivision, DriverWaveform};

    fn fixture() -> (Project, GraphTarget, MaterialEditContext) {
        let preset_id = manifold_core::PresetTypeId::from_string("material-test".to_string());
        let object = SceneNodeRef {
            scope: Vec::new(),
            node: NodeId::new("object"),
        };
        let material = SceneNodeRef {
            scope: Vec::new(),
            node: NodeId::new("material"),
        };
        let role = MaterialParamRole::Scalar(MaterialGroup::Surface);
        let spec = ParamSpecDef {
            id: "material_roughness".to_string(),
            name: "Roughness".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            material_role: Some(role),
            ..ParamSpecDef::default()
        };
        let spec_metallic = ParamSpecDef {
            id: "material_metallic".to_string(),
            name: "Metallic".to_string(),
            default_value: 0.25,
            material_role: Some(role),
            ..spec.clone()
        };
        let binding = BindingDef {
            id: spec.id.clone(),
            label: spec.name.clone(),
            default_value: spec.default_value,
            target: BindingTarget::Node {
                node_id: material.node.clone(),
                param: "roughness".to_string(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        };
        let binding_metallic = BindingDef {
            id: spec_metallic.id.clone(),
            label: spec_metallic.name.clone(),
            default_value: spec_metallic.default_value,
            target: BindingTarget::Node {
                node_id: material.node.clone(),
                param: "metallic".to_string(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        };
        let graph = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: preset_id.clone(),
                display_name: "Material test".to_string(),
                category: "Test".to_string(),
                osc_prefix: "material_test".to_string(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                layer_types: None,
                params: vec![spec.clone(), spec_metallic.clone()],
                bindings: vec![binding, binding_metallic],
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
                scene_bounds: None,
                scene_modifier: None,
            }),
            scene_modifiers: Vec::new(),
            nodes: vec![
                EffectGraphNode {
                    id: 1,
                    node_id: object.node.clone(),
                    type_id: "node.scene_object".to_string(),
                    handle: None,
                    params: Default::default(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: Default::default(),
                    output_canvas_scales: Default::default(),
                    group: None,
                },
                EffectGraphNode {
                    id: 2,
                    node_id: material.node.clone(),
                    type_id: "node.pbr_material".to_string(),
                    handle: None,
                    params: Default::default(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: Default::default(),
                    output_canvas_scales: Default::default(),
                    group: None,
                },
            ],
            wires: vec![EffectGraphWire {
                from_node: 2,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "material".to_string(),
            }],
        };
        let mut instance = PresetInstance::new(preset_id.clone());
        instance.params =
            ParamManifest::from_params(vec![Param::bundled(spec), Param::bundled(spec_metallic)]);
        instance.base_tracked = true;
        instance.graph = Some(graph);
        let target = GraphTarget::Effect(instance.id.clone());
        let mut project = Project::default();
        project.settings.master_effects.push(instance);
        let context = MaterialEditContext {
            object,
            material,
            kind: MaterialEditKind::Look,
            expected_preset_id: preset_id,
        };
        (project, target, context)
    }

    fn with_graph_mut(
        project: &mut Project,
        target: &GraphTarget,
        edit: impl FnOnce(&mut EffectGraphDef),
    ) {
        let owner = project.preset_instance_mut(target).unwrap();
        edit(owner.graph.as_mut().unwrap());
    }

    #[test]
    fn material_inspector_group_export_resolves_the_actual_material_scope() {
        let (mut project, target, mut context) = fixture();
        let wrapper_id = NodeId::new("material-wrapper");
        context.material.scope.push(wrapper_id.clone());
        with_graph_mut(&mut project, &target, |graph| {
            let material = graph.nodes.remove(1);
            let mut output = material.clone();
            output.id = 3;
            output.node_id = NodeId::new("material-output");
            output.type_id = manifold_core::effect_graph_def::GROUP_OUTPUT_TYPE_ID.into();
            let mut wrapper = material.clone();
            wrapper.id = 10;
            wrapper.node_id = wrapper_id;
            wrapper.type_id = manifold_core::effect_graph_def::GROUP_TYPE_ID.into();
            wrapper.group = Some(Box::new(GroupDef {
                interface: GroupInterface {
                    inputs: vec![],
                    outputs: vec![],
                    params: vec![],
                },
                nodes: vec![material, output],
                wires: vec![EffectGraphWire {
                    from_node: 2,
                    from_port: "out".into(),
                    to_node: 3,
                    to_port: "material".into(),
                }],
                tint: None,
            }));
            graph.nodes.push(wrapper);
            graph.wires[0].from_node = 10;
            graph.wires[0].from_port = "material".into();
        });
        let changes = [MaterialParamChange {
            param_id: "material_roughness".into(),
            expected: 0.5,
            value: 0.8,
        }];
        assert!(validate_material_edit(&project, &target, &context, &changes, None).is_ok());
        with_graph_mut(&mut project, &target, |graph| {
            graph.nodes[1].group.as_mut().unwrap().wires[0].from_node = 99;
        });
        assert!(validate_material_edit(&project, &target, &context, &changes, None).is_err());
    }

    #[test]
    fn material_inspector_material_command_round_trips_one_atomic_edit() {
        let (mut project, target, context) = fixture();
        let mut command = ChangeMaterialParamsCommand::new(
            target.clone(),
            context,
            vec![MaterialParamChange {
                param_id: ParamId::from("material_roughness"),
                expected: 0.5,
                value: 0.8,
            }],
            "Set material roughness".to_string(),
            None,
        );
        command.execute(&mut project);
        let value = project
            .preset_instance(&target)
            .unwrap()
            .get_base_param("material_roughness");
        assert_eq!(value, 0.8);

        command.undo(&mut project);
        assert_eq!(
            project
                .preset_instance(&target)
                .unwrap()
                .get_base_param("material_roughness"),
            0.5
        );
        command.execute(&mut project);
        assert_eq!(
            project
                .preset_instance(&target)
                .unwrap()
                .get_base_param("material_roughness"),
            0.8
        );
    }

    #[test]
    fn material_inspector_material_command_applies_two_writes_as_one_undo_unit() {
        let (mut project, target, context) = fixture();
        let mut command = ChangeMaterialParamsCommand::new(
            target.clone(),
            context,
            vec![
                MaterialParamChange {
                    param_id: ParamId::from("material_roughness"),
                    expected: 0.5,
                    value: 0.8,
                },
                MaterialParamChange {
                    param_id: ParamId::from("material_metallic"),
                    expected: 0.25,
                    value: 0.9,
                },
            ],
            "Set material surface".to_string(),
            None,
        );
        command.execute(&mut project);
        let owner = project.preset_instance(&target).unwrap();
        assert_eq!(owner.get_base_param("material_roughness"), 0.8);
        assert_eq!(owner.get_base_param("material_metallic"), 0.9);
        command.undo(&mut project);
        let owner = project.preset_instance(&target).unwrap();
        assert_eq!(owner.get_base_param("material_roughness"), 0.5);
        assert_eq!(owner.get_base_param("material_metallic"), 0.25);
        command.execute(&mut project);
        let owner = project.preset_instance(&target).unwrap();
        assert_eq!(owner.get_base_param("material_roughness"), 0.8);
        assert_eq!(owner.get_base_param("material_metallic"), 0.9);
    }

    #[test]
    fn material_inspector_stale_batch_applies_zero_writes() {
        let (mut project, target, context) = fixture();
        project
            .preset_instance_mut(&target)
            .unwrap()
            .set_base_param_by_id("material_roughness", 0.7);
        let mut command = ChangeMaterialParamsCommand::new(
            target.clone(),
            context,
            vec![
                MaterialParamChange {
                    param_id: ParamId::from("material_roughness"),
                    expected: 0.5,
                    value: 0.8,
                },
                MaterialParamChange {
                    param_id: ParamId::from("material_metallic"),
                    expected: 0.25,
                    value: 0.9,
                },
            ],
            "Stale surface".to_string(),
            None,
        );
        command.execute(&mut project);
        let owner = project.preset_instance(&target).unwrap();
        assert_eq!(owner.get_base_param("material_roughness"), 0.7);
        assert_eq!(owner.get_base_param("material_metallic"), 0.25);
        assert!(command.rejection_reason().is_some());
    }

    #[test]
    fn material_inspector_rejects_changed_preset_and_missing_catalog_graph() {
        let (project, target, mut context) = fixture();
        context.expected_preset_id = manifold_core::PresetTypeId::from_string("other".to_string());
        let changes = [MaterialParamChange {
            param_id: ParamId::from("material_roughness"),
            expected: 0.5,
            value: 0.8,
        }];
        assert!(validate_material_edit(&project, &target, &context, &changes, None).is_err());

        let (mut project, target, context) = fixture();
        let catalog = project
            .preset_instance(&target)
            .unwrap()
            .graph
            .clone()
            .unwrap();
        project.preset_instance_mut(&target).unwrap().graph = None;
        assert!(
            validate_material_edit(&project, &target, &context, &changes, Some(&catalog)).is_ok()
        );
        assert!(validate_material_edit(&project, &target, &context, &changes, None).is_err());
    }

    #[test]
    fn material_inspector_mr_scope_and_skin_ownership_are_precise() {
        let (mut project, target, context) = fixture();
        with_graph_mut(&mut project, &target, |graph| {
            graph.nodes.push(EffectGraphNode {
                id: 3,
                node_id: NodeId::new("other_object"),
                type_id: "node.scene_object".to_string(),
                handle: None,
                params: Default::default(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: Default::default(),
                group: None,
            });
            graph.nodes.push(EffectGraphNode {
                id: 4,
                node_id: NodeId::new("other_mr"),
                type_id: "node.gltf_texture_source".to_string(),
                handle: None,
                params: Default::default(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: Default::default(),
                group: None,
            });
            graph.wires.push(EffectGraphWire {
                from_node: 4,
                from_port: "out".to_string(),
                to_node: 3,
                to_port: "mr_map".to_string(),
            });
        });
        let changes = [MaterialParamChange {
            param_id: ParamId::from("material_metallic"),
            expected: 0.25,
            value: 0.9,
        }];
        assert!(validate_material_edit(&project, &target, &context, &changes, None).is_ok());
        with_graph_mut(&mut project, &target, |graph| {
            graph.wires.push(EffectGraphWire {
                from_node: 4,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "mr_map".to_string(),
            });
        });
        assert!(validate_material_edit(&project, &target, &context, &changes, None).is_ok());
        with_graph_mut(&mut project, &target, |graph| {
            graph
                .wires
                .retain(|wire| wire.to_node != 1 || wire.to_port == "material");
            graph.wires.push(EffectGraphWire {
                from_node: 4,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "normal_map".to_string(),
            });
        });
        assert!(validate_material_edit(&project, &target, &context, &changes, None).is_ok());
        with_graph_mut(&mut project, &target, |graph| {
            graph.nodes[3].type_id = "node.layer_source".to_string();
            graph
                .wires
                .retain(|wire| wire.to_node != 1 || wire.to_port == "material");
            graph.wires.push(EffectGraphWire {
                from_node: 4,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "emissive_map".to_string(),
            });
        });
        assert!(validate_material_edit(&project, &target, &context, &changes, None).is_err());
        with_graph_mut(&mut project, &target, |graph| {
            graph
                .wires
                .retain(|wire| wire.to_node != 1 || wire.to_port == "material");
            graph.wires.push(EffectGraphWire {
                from_node: 4,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "base_color_map".to_string(),
            });
        });
        assert!(validate_material_edit(&project, &target, &context, &changes, None).is_ok());
    }

    #[test]
    fn material_inspector_rejects_identity_fanout_driven_and_ambiguous_members() {
        let (mut project, target, context) = fixture();
        with_graph_mut(&mut project, &target, |graph| {
            graph.preset_metadata.as_mut().unwrap().bindings[0].scale = 2.0;
        });
        let change = [MaterialParamChange {
            param_id: ParamId::from("material_roughness"),
            expected: 0.5,
            value: 0.8,
        }];
        assert!(
            validate_material_edit(&project, &target, &context, &change, None)
                .expect_err("custom identity binding")
                .contains("custom binding")
        );
        with_graph_mut(&mut project, &target, |graph| {
            let metadata = graph.preset_metadata.as_mut().unwrap();
            metadata.bindings[0].scale = 1.0;
            let duplicate = metadata.bindings[0].clone();
            metadata.bindings.push(duplicate);
        });
        assert!(
            validate_material_edit(&project, &target, &context, &change, None)
                .expect_err("fan-out binding")
                .contains("external ownership")
        );
        with_graph_mut(&mut project, &target, |graph| {
            graph.preset_metadata.as_mut().unwrap().bindings.pop();
        });
        project.preset_instance_mut(&target).unwrap().drivers = Some(vec![ParameterDriver::new(
            "material_roughness",
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        )]);
        assert!(
            validate_material_edit(&project, &target, &context, &change, None)
                .expect_err("driven seed")
                .contains("external ownership")
        );
        project.preset_instance_mut(&target).unwrap().drivers = None;
        with_graph_mut(&mut project, &target, |graph| {
            graph.nodes.push(EffectGraphNode {
                id: 5,
                node_id: NodeId::new("nested_group"),
                type_id: "node.group".to_string(),
                handle: None,
                params: Default::default(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: Default::default(),
                group: Some(Box::new(GroupDef {
                    interface: GroupInterface {
                        inputs: Vec::new(),
                        outputs: Vec::new(),
                        params: Vec::new(),
                    },
                    nodes: vec![EffectGraphNode {
                        id: 6,
                        node_id: NodeId::new("material"),
                        type_id: "node.pbr_material".to_string(),
                        handle: None,
                        params: Default::default(),
                        exposed_params: Default::default(),
                        editor_pos: None,
                        wgsl_source: None,
                        title: None,
                        output_formats: Default::default(),
                        output_canvas_scales: Default::default(),
                        group: None,
                    }],
                    wires: Vec::new(),
                    tint: None,
                })),
            });
        });
        assert!(
            validate_material_edit(&project, &target, &context, &change, None)
                .expect_err("reused material identity")
                .contains("ambiguous")
        );
    }
}
