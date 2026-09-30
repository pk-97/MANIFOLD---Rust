//! Content-owned draft and commit path for fluid-domain gizmo edits.
//!
//! Pointer motion only changes [`FluidDomainDrag`]. The project is read again
//! at commit time and receives one existing undoable graph command after the
//! original target, transform, resolution, and binding mapping have been
//! revalidated.

use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, ParamSpecDef, SerializedParamValue,
};
use manifold_core::effects::{ParamConvert, apply_card_reshape, invert_card_reshape};
use manifold_core::liquid_domain::is_liquid_domain;
use manifold_core::{GraphTarget, LayerId, NodeId, project::Project};
use manifold_editing::command::Command;
use manifold_editing::commands::effects::ChangeGraphParamCommand;
use manifold_editing::commands::graph::SetGraphNodeParamCommand;
use manifold_renderer::node_graph::fluid::{FluidDomainLayout, FluidSettings};
use manifold_renderer::node_graph::scene_vm::{ParamAddr, SceneObjectVm, SceneVm, TransformVm};
use manifold_renderer::node_graph::{
    GizmoAxis, GizmoMode, GizmoTarget, GizmoTargetKind, ParamValue, convert_param_value,
    drag_write, gizmo_target_for,
};

const DEFAULT_FLUID_RESOLUTION: u32 = 24;

#[derive(Debug, Clone, PartialEq)]
struct DomainBinding {
    binding: BindingDef,
    spec: ParamSpecDef,
}

/// Build the authored graph snapshot used by the viewport scene VM.
///
/// A direct scalar card binding is projected into its node literal using the
/// owner's base value. Only editor geometry uses this projection; the viewport
/// runtime receives the original graph and the owner's effective manifest.
pub(crate) fn authored_def(project: &Project, target: &GraphTarget) -> Option<EffectGraphDef> {
    let mut def = crate::graph_target::resolve(project, target)?.clone();
    project_authored_params(&mut def, project, target)?;
    Some(def)
}

pub(crate) fn project_authored_params(
    def: &mut EffectGraphDef,
    project: &Project,
    target: &GraphTarget,
) -> Option<()> {
    if !matches!(target, GraphTarget::Generator(_)) {
        return Some(());
    }
    let Some(metadata) = def.preset_metadata.as_mut() else {
        return Some(());
    };
    let Some(owner) = project.graph_target_owner(target) else {
        return Some(());
    };

    for binding in &mut metadata.bindings {
        let BindingTarget::Node { node_id, param } = &binding.target else {
            continue;
        };
        let Some(spec) = metadata.params.iter().find(|spec| spec.id == binding.id) else {
            continue;
        };
        if binding.convert == ParamConvert::Trigger || spec.is_trigger {
            continue;
        }

        if owner.params.get(&binding.id).is_none() {
            if binding.default_mirrors_node_param {
                continue;
            }
            let effective = apply_card_reshape(
                binding.default_value,
                spec.min,
                spec.max,
                spec.invert,
                spec.curve,
                binding.scale,
                binding.offset,
            );
            let value = scalar_to_serialized(binding.convert, effective)?;
            let node = find_node_by_stable_id_mut(&mut def.nodes, node_id)?;
            node.params.insert(param.clone(), value);
            continue;
        }

        let base = owner.get_base_param(&binding.id);
        if !base.is_finite() {
            return None;
        }
        binding.default_value = base;
        let effective = apply_card_reshape(
            base,
            spec.min,
            spec.max,
            spec.invert,
            spec.curve,
            binding.scale,
            binding.offset,
        );
        let value = scalar_to_serialized(binding.convert, effective)?;
        let node = find_node_by_stable_id_mut(&mut def.nodes, node_id)?;
        node.params.insert(param.clone(), value);
    }
    Some(())
}

/// Match editor bounds/picking to the runtime that rendered the cached frame.
/// Unaccepted domains have no bounds. A driven or changed layout remains
/// selectable, but can't write through stale authored transform values.
pub(crate) fn apply_runtime_domains(
    scene: &mut SceneVm,
    def: &EffectGraphDef,
    domains: &[(NodeId, manifold_renderer::node_graph::fluid::FluidDomainSnapshot)],
) {
    use manifold_renderer::node_graph::fluid::FluidDomainState;
    for object in &mut scene.objects {
        let SceneObjectVm::Known(row) = object else { continue };
        if row.fluid_node_ids.is_empty() {
            continue;
        }
        let accepted = row.fluid_node_ids.iter()
            .filter_map(|id| find_node_by_doc_id(&def.nodes, *id))
            .find(|node| is_liquid_domain(&node.type_id))
            .and_then(|node| domains.iter().find(|(id, _)| id == &node.node_id))
            .and_then(|(_, snapshot)| (snapshot.state == FluidDomainState::Ready)
                .then_some(snapshot.accepted_layout).flatten());
        if accepted.is_none() || row.fluid_domain != accepted {
            row.fluid_domain_transform = None;
        }
        row.fluid_domain = accepted;
    }
}

fn scalar_to_serialized(convert: ParamConvert, value: f32) -> Option<SerializedParamValue> {
    if !value.is_finite() {
        return None;
    }
    match convert_param_value(convert, value) {
        ParamValue::Float(value) if value.is_finite() => {
            Some(SerializedParamValue::Float { value })
        }
        ParamValue::Bool(value) => Some(SerializedParamValue::Bool { value }),
        ParamValue::Enum(value) => Some(SerializedParamValue::Enum { value }),
        _ => None,
    }
}

fn serialized_scalar(value: &SerializedParamValue) -> Option<f32> {
    match value {
        SerializedParamValue::Float { value } => Some(*value),
        SerializedParamValue::Int { value } => Some(*value as f32),
        _ => None,
    }
}

fn find_node_by_stable_id_mut<'a>(
    nodes: &'a mut [EffectGraphNode],
    node_id: &NodeId,
) -> Option<&'a mut EffectGraphNode> {
    for node in nodes {
        if &node.node_id == node_id {
            return Some(node);
        }
        if let Some(group) = node.group.as_deref_mut()
            && let Some(found) = find_node_by_stable_id_mut(&mut group.nodes, node_id)
        {
            return Some(found);
        }
    }
    None
}

fn find_node_by_doc_id(
    nodes: &[EffectGraphNode],
    node_id: u32,
) -> Option<&EffectGraphNode> {
    for node in nodes {
        if node.id == node_id {
            return Some(node);
        }
        if let Some(group) = node.group.as_deref()
            && let Some(found) = find_node_by_doc_id(&group.nodes, node_id)
        {
            return Some(found);
        }
    }
    None
}

fn node_at_scope<'a>(
    nodes: &'a [EffectGraphNode],
    scope: &[u32],
    node_id: u32,
) -> Option<&'a EffectGraphNode> {
    let Some(group_id) = scope.first() else {
        return nodes.iter().find(|node| node.id == node_id);
    };
    let group = nodes
        .iter()
        .find(|node| node.id == *group_id)?
        .group
        .as_deref()?;
    node_at_scope(&group.nodes, &scope[1..], node_id)
}

fn fluid_resolution(
    def: &EffectGraphDef,
    row: &manifold_renderer::node_graph::scene_vm::SceneObjectKnownRow,
) -> Result<u32, String> {
    let fluid = fluid_surface_node(def, row)?;
    let resolution = fluid
        .params
        .get("resolution")
        .and_then(serialized_scalar)
        .unwrap_or(DEFAULT_FLUID_RESOLUTION as f32);
    if !resolution.is_finite() {
        return Err("Fluid resolution is invalid".into());
    }
    let resolution = resolution.round() as u32;
    (8..=96)
        .contains(&resolution)
        .then_some(resolution)
        .ok_or_else(|| "Fluid resolution is outside the supported range".into())
}

fn fluid_surface_node<'a>(
    def: &'a EffectGraphDef,
    row: &manifold_renderer::node_graph::scene_vm::SceneObjectKnownRow,
) -> Result<&'a EffectGraphNode, String> {
    row.fluid_node_ids
        .iter()
        .find_map(|id| {
            find_node_by_doc_id(&def.nodes, *id).filter(|node| is_liquid_domain(&node.type_id))
        })
        .ok_or("Fluid surface node is no longer present".into())
}

fn axis_param(mode: GizmoMode, axis: GizmoAxis) -> &'static str {
    match (mode, axis) {
        (GizmoMode::Move, GizmoAxis::X) => "pos_x",
        (GizmoMode::Move, GizmoAxis::Y) => "pos_y",
        (GizmoMode::Move, GizmoAxis::Z) => "pos_z",
        (GizmoMode::Scale, GizmoAxis::X) => "scale_x",
        (GizmoMode::Scale, GizmoAxis::Y) => "scale_y",
        (GizmoMode::Scale, GizmoAxis::Z) => "scale_z",
        (GizmoMode::Rotate, GizmoAxis::X) => "rot_x",
        (GizmoMode::Rotate, GizmoAxis::Y) => "rot_y",
        (GizmoMode::Rotate, GizmoAxis::Z) => "rot_z",
    }
}

fn transform_scalar(transform: &TransformVm, mode: GizmoMode, axis: GizmoAxis) -> f32 {
    let value = match mode {
        GizmoMode::Move => transform.pos_value,
        GizmoMode::Rotate => transform.rot_value,
        GizmoMode::Scale => transform.scale_value,
    };
    match axis {
        GizmoAxis::X => value.0,
        GizmoAxis::Y => value.1,
        GizmoAxis::Z => value.2,
    }
}

fn set_transform_scalar(transform: &mut TransformVm, mode: GizmoMode, axis: GizmoAxis, value: f32) {
    match (mode, axis) {
        (GizmoMode::Move, GizmoAxis::X) => transform.pos_value.0 = value,
        (GizmoMode::Move, GizmoAxis::Y) => transform.pos_value.1 = value,
        (GizmoMode::Move, GizmoAxis::Z) => transform.pos_value.2 = value,
        (GizmoMode::Rotate, GizmoAxis::X) => transform.rot_value.0 = value,
        (GizmoMode::Rotate, GizmoAxis::Y) => transform.rot_value.1 = value,
        (GizmoMode::Rotate, GizmoAxis::Z) => transform.rot_value.2 = value,
        (GizmoMode::Scale, GizmoAxis::X) => transform.scale_value.0 = value,
        (GizmoMode::Scale, GizmoAxis::Y) => transform.scale_value.1 = value,
        (GizmoMode::Scale, GizmoAxis::Z) => transform.scale_value.2 = value,
    }
}

fn fluid_layout(resolution: u32, transform: &TransformVm) -> Result<FluidDomainLayout, String> {
    FluidSettings {
        resolution,
        domain: Some(manifold_renderer::node_graph::Transform {
            pos: [
                transform.pos_value.0,
                transform.pos_value.1,
                transform.pos_value.2,
            ],
            rot_euler: [
                transform.rot_value.0,
                transform.rot_value.1,
                transform.rot_value.2,
            ],
            scale: [
                transform.scale_value.0,
                transform.scale_value.1,
                transform.scale_value.2,
            ],
            billboard: false,
        }),
        ..FluidSettings::default()
    }
    .domain_layout()
}

fn binding_for(
    def: &EffectGraphDef,
    node_id: &NodeId,
    param: &str,
) -> Result<Option<DomainBinding>, String> {
    let Some(metadata) = def.preset_metadata.as_ref() else {
        return Ok(None);
    };
    let mut matches = metadata.bindings.iter().filter(|binding| {
        matches!(&binding.target, BindingTarget::Node { node_id: id, param: name } if id == node_id && name == param)
    });
    let Some(binding) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(format!(
            "Fluid domain parameter {param} has multiple card bindings"
        ));
    }
    if binding.convert != ParamConvert::Float {
        return Err(format!(
            "Fluid domain parameter {param} requires a float binding"
        ));
    }
    if !binding.scale.is_finite()
        || !binding.offset.is_finite()
        || binding.scale.abs() < f32::EPSILON
    {
        return Err(format!(
            "Fluid domain parameter {param} has a non-invertible mapping"
        ));
    }
    if metadata
        .bindings
        .iter()
        .filter(|candidate| candidate.id == binding.id)
        .count()
        > 1
    {
        return Err(format!(
            "Fluid domain parameter {param} uses a fan-out binding; edit the macro directly"
        ));
    }
    let spec = metadata
        .params
        .iter()
        .find(|spec| spec.id == binding.id)
        .ok_or_else(|| format!("Binding {} has no parameter spec", binding.id))?;
    if spec.is_trigger {
        return Err(format!(
            "Fluid domain parameter {param} cannot use a trigger binding"
        ));
    }
    Ok(Some(DomainBinding {
        binding: binding.clone(),
        spec: spec.clone(),
    }))
}

fn same_transform(a: &TransformVm, b: &TransformVm) -> bool {
    a == b
}

/// Use the same representable range during preview and content commit.
fn mapped_base(mapping: &DomainBinding, value: f32) -> Option<f32> {
    let spec = &mapping.spec;
    let binding = &mapping.binding;
    let base = invert_card_reshape(
        value,
        spec.min,
        spec.max,
        spec.invert,
        spec.curve,
        binding.scale,
        binding.offset,
    )?;
    let round_trip = apply_card_reshape(
        base,
        spec.min,
        spec.max,
        spec.invert,
        spec.curve,
        binding.scale,
        binding.offset,
    );
    (base.is_finite()
        && base >= spec.min
        && base <= spec.max
        && round_trip.is_finite()
        && (round_trip - value).abs() <= 1e-4)
        .then_some(base)
}

/// Ephemeral fluid-domain gizmo draft. `update` only changes this value and
/// its derived target/layout; it never writes to a [`Project`].
#[derive(Debug, Clone)]
pub(crate) struct FluidDomainDrag {
    pub layer_id: LayerId,
    pub object_node_id: u32,
    pub mode: GizmoMode,
    pub axis: GizmoAxis,
    pub target: GizmoTarget,
    pub layout: FluidDomainLayout,
    pub value: f32,
    addr: ParamAddr,
    domain_node_id: NodeId,
    original_transform: TransformVm,
    initial_layout: FluidDomainLayout,
    original_resolution: u32,
    fluid_surface_node_id: NodeId,
    initial_value: f32,
    binding: Option<DomainBinding>,
}

impl FluidDomainDrag {
    pub fn begin(
        project: &Project,
        layer_id: LayerId,
        object_node_id: u32,
        mode: GizmoMode,
        axis: GizmoAxis,
    ) -> Result<Self, String> {
        let target_graph = GraphTarget::Generator(layer_id.clone());
        let def = authored_def(project, &target_graph).ok_or("Generator graph is unavailable")?;
        let scene = SceneVm::from_def(&def).ok_or("Scene graph is unavailable")?;
        let target =
            gizmo_target_for(&scene, object_node_id).ok_or("Fluid domain is unavailable")?;
        if target.kind != GizmoTargetKind::FluidDomain || !target.supports_mode(mode) {
            return Err("Fluid domain gizmos support Move and Scale only".into());
        }
        let (addr, value, driven) =
            drag_write(mode, axis, &target).ok_or("Fluid domain parameter is unavailable")?;
        if driven {
            return Err("Fluid domain parameter is driven".into());
        }
        if addr.param_id != axis_param(mode, axis) {
            return Err("Fluid domain parameter address does not match the selected axis".into());
        }
        let row = scene
            .objects
            .iter()
            .find_map(|object| match object {
                SceneObjectVm::Known(row) if row.object_node_id == object_node_id => {
                    Some(row.as_ref())
                }
                _ => None,
            })
            .ok_or("Fluid domain object is unavailable")?;
        let layout = row
            .fluid_domain
            .ok_or("Fluid domain bounds are unavailable")?;
        let transform = row
            .fluid_domain_transform
            .clone()
            .ok_or("Fluid domain transform is unavailable")?;
        if transform.rot_value.0.abs() > 1e-6
            || transform.rot_value.1.abs() > 1e-6
            || transform.rot_value.2.abs() > 1e-6
        {
            return Err("Fluid domain rotation is unsupported".into());
        }
        let fluid_surface = fluid_surface_node(&def, row)?;
        let resolution = fluid_resolution(&def, row)?;
        let node = node_at_scope(&def.nodes, &addr.scope_path, addr.node_doc_id)
            .ok_or("Fluid domain transform node is unavailable")?;
        let domain_node_id = node.node_id.clone();
        let binding = if domain_node_id.is_empty() {
            None
        } else {
            binding_for(&def, &domain_node_id, &addr.param_id)?
        };
        if !value.is_finite()
            || !same_transform(
                &transform,
                target
                    .transform
                    .as_ref()
                    .ok_or("Fluid domain transform is unavailable")?,
            )
        {
            return Err("Fluid domain transform is invalid".into());
        }
        Ok(Self {
            layer_id,
            object_node_id,
            mode,
            axis,
            target,
            layout,
            value,
            addr,
            domain_node_id,
            original_transform: transform,
            initial_layout: layout,
            original_resolution: resolution,
            fluid_surface_node_id: fluid_surface.node_id.clone(),
            initial_value: value,
            binding,
        })
    }

    pub fn update(&mut self, value: f32) -> bool {
        let Some(value) = self.target.constrain_value(self.mode, value) else {
            return false;
        };
        if self
            .binding
            .as_ref()
            .is_some_and(|mapping| mapped_base(mapping, value).is_none())
        {
            return false;
        }
        if value == self.value {
            return false;
        }
        let Some(transform) = self.target.transform.as_mut() else {
            return false;
        };
        let old_pos = transform.pos_value;
        let old_rot = transform.rot_value;
        let old_scale = transform.scale_value;
        set_transform_scalar(transform, self.mode, self.axis, value);
        let Ok(layout) = fluid_layout(self.original_resolution, transform) else {
            transform.pos_value = old_pos;
            transform.rot_value = old_rot;
            transform.scale_value = old_scale;
            return false;
        };
        self.target.origin = layout.transform().pos;
        self.layout = layout;
        self.value = value;
        true
    }

    pub fn changed(&self) -> bool {
        self.value != self.initial_value
    }
}

fn candidate_scalar(drag: &FluidDomainDrag) -> Result<f32, String> {
    let transform = drag
        .target
        .transform
        .as_ref()
        .ok_or("Fluid domain transform is unavailable")?;
    let value = transform_scalar(transform, drag.mode, drag.axis);
    if !value.is_finite() {
        return Err("Fluid domain value is invalid".into());
    }
    let layout = fluid_layout(drag.original_resolution, transform)?;
    if layout != drag.layout {
        return Err("Fluid domain draft layout is invalid".into());
    }
    Ok(value)
}

/// Revalidate a draft against current content and produce one undoable graph
/// command. Any topology, address, resolution, transform, or binding change
/// during the gesture rejects the commit.
pub(crate) fn build_action(
    project: &Project,
    drag: FluidDomainDrag,
) -> Result<Box<dyn Command>, String> {
    if !drag.changed() {
        return Err("Fluid domain gesture made no change".into());
    }
    let current = FluidDomainDrag::begin(
        project,
        drag.layer_id.clone(),
        drag.object_node_id,
        drag.mode,
        drag.axis,
    )?;
    if current.addr != drag.addr
        || current.domain_node_id != drag.domain_node_id
        || current.fluid_surface_node_id != drag.fluid_surface_node_id
        || current.original_resolution != drag.original_resolution
        || current.initial_layout != drag.initial_layout
        || !same_transform(&current.original_transform, &drag.original_transform)
        || current.binding != drag.binding
        || current.initial_value != drag.initial_value
    {
        return Err("Fluid domain changed during the gesture".into());
    }
    let new_value = candidate_scalar(&drag)?;
    let target = GraphTarget::Generator(drag.layer_id.clone());
    if let Some(mapping) = drag.binding {
        let owner = project
            .graph_target_owner(&target)
            .ok_or("Generator owner is unavailable")?;
        let param = owner
            .params
            .get(&mapping.binding.id)
            .ok_or("Fluid domain card parameter is unavailable")?;
        let old_base = owner.get_base_param(&mapping.binding.id);
        let new_base = mapped_base(&mapping, new_value)
            .ok_or("Fluid domain card mapping cannot represent the edit")?;
        if !old_base.is_finite() || new_base < param.spec.min || new_base > param.spec.max {
            return Err("Fluid domain card mapping cannot represent the edit".into());
        }
        return Ok(Box::new(ChangeGraphParamCommand::new(
            target,
            mapping.binding.id,
            old_base,
            new_base,
        )));
    }

    let owner_default = crate::graph_target::owner_default(project, &target)
        .ok_or("Generator graph default is unavailable")?;
    let current_def =
        crate::graph_target::resolve(project, &target).ok_or("Generator graph is unavailable")?;
    let previous = node_at_scope(
        &current_def.nodes,
        &drag.addr.scope_path,
        drag.addr.node_doc_id,
    )
    .and_then(|node| node.params.get(&drag.addr.param_id).cloned());
    let value = scalar_to_serialized(ParamConvert::Float, new_value)
        .ok_or("Fluid domain value is invalid")?;
    Ok(Box::new(
        SetGraphNodeParamCommand::new(
            target,
            drag.addr.node_doc_id,
            drag.addr.param_id,
            value,
            owner_default,
        )
        .with_scope(drag.addr.scope_path)
        .with_previous(previous),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::{BindingDef, EffectGraphWire};
    use manifold_core::types::LayerType;

    const WATER_BASIN: &str =
        include_str!("../../manifold-renderer/assets/generator-presets/WaterBasin.json");

    #[test]
    fn runtime_domain_bounds_hide_unaccepted_layouts_and_lock_driven_edits() {
        use manifold_renderer::node_graph::fluid::{FluidDomainSnapshot, FluidDomainState};
        let (project, layer, object_id) = fluid_project(true);
        let def = authored_def(&project, &GraphTarget::Generator(layer)).unwrap();
        let authored = SceneVm::from_def(&def).unwrap();
        let row = authored.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(row) if row.object_node_id == object_id => Some(row),
            _ => None,
        }).unwrap();
        let layout = row.fluid_domain.unwrap();
        let fluid = row.fluid_node_ids.iter().filter_map(|id| find_node_by_doc_id(&def.nodes, *id))
            .find(|node| node.type_id == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID).unwrap().node_id.clone();

        for state in [FluidDomainState::Initializing, FluidDomainState::PendingInputs, FluidDomainState::Failed] {
            let mut scene = authored.clone();
            // Even a malformed non-ready observation must not expose old bounds.
            apply_runtime_domains(&mut scene, &def, &[(fluid.clone(), FluidDomainSnapshot {
                epoch: 1, state, accepted_layout: Some(layout),
            })]);
            assert!(gizmo_target_for(&scene, object_id).is_none());
            assert!(scene.objects.iter().all(|object| !matches!(object,
                SceneObjectVm::Known(row) if row.object_node_id == object_id && row.fluid_domain.is_some())));
        }
        let mut missing = authored.clone();
        apply_runtime_domains(&mut missing, &def, &[]);
        assert!(gizmo_target_for(&missing, object_id).is_none());

        let mut ready = authored.clone();
        let observation = FluidDomainSnapshot { epoch: 2, state: FluidDomainState::Ready, accepted_layout: Some(layout) };
        apply_runtime_domains(&mut ready, &def, &[(fluid.clone(), observation)]);
        assert!(gizmo_target_for(&ready, object_id).is_some());

        let mut changed = layout;
        changed.min[0] += 3.0;
        apply_runtime_domains(&mut ready, &def, &[(fluid, FluidDomainSnapshot {
            accepted_layout: Some(changed), ..observation
        })]);
        assert!(gizmo_target_for(&ready, object_id).is_none(), "driven layout cannot write stale base values");
        assert!(ready.objects.iter().any(|object| matches!(object,
            SceneObjectVm::Known(row) if row.object_node_id == object_id && row.fluid_domain == Some(changed))));
    }

    #[test]
    fn runtime_domain_bounds_match_grouped_fluid_by_stable_identity() {
        use manifold_renderer::node_graph::fluid::{FluidDomainSnapshot, FluidDomainState};
        let (project, layer, object_id) = added_fluid_project();
        let def = authored_def(&project, &GraphTarget::Generator(layer)).unwrap();
        let mut scene = SceneVm::from_def(&def).unwrap();
        let row = scene.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(row) if row.object_node_id == object_id => Some(row),
            _ => None,
        }).unwrap();
        let layout = row.fluid_domain.unwrap();
        let fluid = row.fluid_node_ids.iter().filter_map(|id| find_node_by_doc_id(&def.nodes, *id))
            .find(|node| node.type_id == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID).unwrap();
        assert!(!def.nodes.iter().any(|node| node.node_id == fluid.node_id));
        apply_runtime_domains(&mut scene, &def, &[(fluid.node_id.clone(), FluidDomainSnapshot {
            epoch: 4, state: FluidDomainState::Ready, accepted_layout: Some(layout),
        })]);
        assert!(gizmo_target_for(&scene, object_id).is_some());
    }

    fn fluid_project(bound: bool) -> (Project, LayerId, u32) {
        let mut project = Project::default();
        let index = project.timeline.add_layer(
            "Fluid",
            LayerType::Generator,
            PresetTypeId::from_string("WaterBasin".to_string()),
        );
        let layer_id = project.timeline.layers[index].layer_id.clone();
        let mut def: EffectGraphDef = serde_json::from_str(WATER_BASIN).unwrap();
        def.nodes.push(EffectGraphNode {
            id: 40,
            node_id: NodeId::new("domain_transform"),
            type_id: "node.transform_3d".into(),
            handle: Some("Domain".into()),
            params: BTreeMap::from([
                ("pos_y".into(), SerializedParamValue::Float { value: 2.0 }),
                ("scale_x".into(), SerializedParamValue::Float { value: 4.0 }),
                ("scale_y".into(), SerializedParamValue::Float { value: 4.0 }),
                ("scale_z".into(), SerializedParamValue::Float { value: 4.0 }),
            ]),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        });
        def.wires.push(EffectGraphWire {
            from_node: 40,
            from_port: "transform".into(),
            to_node: 4,
            to_port: "domain".into(),
        });
        if bound {
            def.preset_metadata
                .as_mut()
                .unwrap()
                .params
                .push(ParamSpecDef {
                    id: "domain_x".into(),
                    name: "Domain X".into(),
                    min: -10.0,
                    max: 10.0,
                    default_value: 0.0,
                    ..ParamSpecDef::default()
                });
            def.preset_metadata
                .as_mut()
                .unwrap()
                .bindings
                .push(BindingDef {
                    id: "domain_x".into(),
                    label: "Domain X".into(),
                    default_value: 0.0,
                    target: BindingTarget::Node {
                        node_id: NodeId::new("domain_transform"),
                        param: "pos_x".into(),
                    },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                    default_mirrors_node_param: false,
                });
        }
        let layer = &mut project.timeline.layers[index];
        let generator = layer.gen_params_mut().unwrap();
        generator.graph = Some(def.clone());
        generator.refresh_manifest_from_graph();
        if bound {
            generator.set_base_param("domain_x", 1.5);
        }
        let scene = SceneVm::from_def(&def).unwrap();
        let object_id = scene
            .objects
            .iter()
            .find_map(|object| match object {
                SceneObjectVm::Known(row) if row.fluid_domain_transform.is_some() => {
                    Some(row.object_node_id)
                }
                _ => None,
            })
            .expect("fixture domain object");
        (project, layer_id, object_id)
    }

    fn added_fluid_project() -> (Project, LayerId, u32) {
        use manifold_editing::commands::graph::AddSceneFluidCommand;
        use manifold_renderer::node_graph::scene_exposure::metadata_for_node_type;
        let mut project = Project::default();
        let index = project.timeline.add_layer(
            "Scene",
            LayerType::Generator,
            PresetTypeId::new("Scene"),
        );
        let layer_id = project.timeline.layers[index].layer_id.clone();
        let target = GraphTarget::Generator(layer_id.clone());
        let default = crate::graph_target::owner_default(&project, &target).unwrap();
        let scene_id = default
            .nodes
            .iter()
            .find(|node| node.type_id == "node.render_scene")
            .unwrap()
            .id;
        let mut command = AddSceneFluidCommand::new(
            target.clone(),
            scene_id,
            metadata_for_node_type(manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID),
            metadata_for_node_type("node.transform_3d"),
            metadata_for_node_type("node.pbr_material"),
            metadata_for_node_type("node.scene_object"),
            default,
        )
        .with_role_metadata(metadata_for_node_type("node.fluid_role_source"))
        .with_world_metadata(metadata_for_node_type("node.physics_world"));
        command.execute(&mut project);
        assert!(command.was_applied(), "{:?}", command.rejection_reason());
        let scene =
            SceneVm::from_def(crate::graph_target::resolve(&project, &target).unwrap()).unwrap();
        let object_id = scene
            .objects
            .iter()
            .find_map(|object| match object {
                SceneObjectVm::Known(row) if !row.fluid_node_ids.is_empty() => {
                    Some(row.object_node_id)
                }
                _ => None,
            })
            .unwrap();
        (project, layer_id, object_id)
    }

    #[test]
    fn added_nested_domain_uses_inverse_mapping_and_one_undo_unit() {
        let (mut project, layer_id, object_id) = added_fluid_project();
        let target = GraphTarget::Generator(layer_id.clone());
        let initial = FluidDomainDrag::begin(
            &project,
            layer_id.clone(),
            object_id,
            GizmoMode::Scale,
            GizmoAxis::X,
        )
        .unwrap();
        assert!(!initial.addr.scope_path.is_empty());
        let outer_id = initial.binding.as_ref().unwrap().binding.id.clone();
        let old_base = project
            .graph_target_owner(&target)
            .unwrap()
            .get_base_param(&outer_id);
        let owner = project.graph_target_owner_mut(&target).unwrap();
        let mapping = owner
            .graph
            .as_mut()
            .unwrap()
            .preset_metadata
            .as_mut()
            .unwrap()
            .bindings
            .iter_mut()
            .find(|binding| binding.id == outer_id)
            .unwrap();
        mapping.scale = 2.0;
        mapping.offset = 1.0;
        // Exercise mirrored defaults too: a live manifest value still owns
        // this node, while the authored literal must remain untouched.
        mapping.default_mirrors_node_param = true;
        let before = serde_json::to_vec(&project).unwrap();
        let mut drag = FluidDomainDrag::begin(
            &project,
            layer_id.clone(),
            object_id,
            GizmoMode::Scale,
            GizmoAxis::X,
        )
        .unwrap();
        assert_eq!(drag.value, old_base * 2.0 + 1.0);
        for width in [8.0, 6.0, 7.0] {
            assert!(drag.update(width));
        }
        assert!(
            !drag.update(1.0),
            "mapped minimum must constrain the draft too"
        );
        assert_eq!(drag.value, 7.0);
        assert_eq!(
            serde_json::to_vec(&project).unwrap(),
            before,
            "pointer packets are drafts"
        );
        let addr = drag.addr.clone();
        let mut edits = manifold_editing::service::EditingService::new();
        edits.execute(build_action(&project, drag).unwrap(), &mut project);
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .get_base_param(&outer_id),
            3.0
        );
        let node = node_at_scope(
            &crate::graph_target::resolve(&project, &target)
                .unwrap()
                .nodes,
            &addr.scope_path,
            addr.node_doc_id,
        )
        .unwrap();
        assert_eq!(
            serialized_scalar(node.params.get("scale_x").unwrap()),
            Some(4.0),
            "no duplicate def write"
        );
        assert!(edits.undo(&mut project));
        assert!(!edits.can_undo(), "a whole drag creates one undo entry");
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .get_base_param(&outer_id),
            old_base
        );
        assert!(edits.redo(&mut project));
        let reloaded: Project =
            serde_json::from_slice(&serde_json::to_vec(&project).unwrap()).unwrap();
        let restored = FluidDomainDrag::begin(
            &reloaded,
            layer_id,
            object_id,
            GizmoMode::Scale,
            GizmoAxis::X,
        )
        .unwrap();
        assert_eq!(restored.value, 7.0);
    }

    #[test]
    fn nested_unbound_domain_preserves_scope_and_undo() {
        let (mut project, layer_id, object_id) = added_fluid_project();
        let target = GraphTarget::Generator(layer_id.clone());
        let initial = FluidDomainDrag::begin(
            &project,
            layer_id.clone(),
            object_id,
            GizmoMode::Move,
            GizmoAxis::X,
        )
        .unwrap();
        let outer_id = initial.binding.as_ref().unwrap().binding.id.clone();
        project
            .graph_target_owner_mut(&target)
            .unwrap()
            .graph
            .as_mut()
            .unwrap()
            .preset_metadata
            .as_mut()
            .unwrap()
            .bindings
            .retain(|binding| binding.id != outer_id);
        let before = crate::graph_target::resolve(&project, &target)
            .unwrap()
            .clone();
        let mut drag = FluidDomainDrag::begin(
            &project,
            layer_id.clone(),
            object_id,
            GizmoMode::Move,
            GizmoAxis::X,
        )
        .unwrap();
        assert!(drag.binding.is_none());
        assert!(drag.update(2.5));
        let mut command = build_action(&project, drag).unwrap();
        command.execute(&mut project);
        assert!(command.was_applied());
        assert_eq!(
            FluidDomainDrag::begin(&project, layer_id, object_id, GizmoMode::Move, GizmoAxis::X)
                .unwrap()
                .value,
            2.5
        );
        command.undo(&mut project);
        assert_eq!(
            crate::graph_target::resolve(&project, &target).unwrap(),
            &before
        );
    }

    #[test]
    fn authored_projection_updates_node_and_empty_runtime_binding_default() {
        let (project, layer_id, object_id) = fluid_project(true);
        let target = GraphTarget::Generator(layer_id);
        let before = crate::graph_target::resolve(&project, &target)
            .unwrap()
            .clone();
        let projected = authored_def(&project, &target).unwrap();
        let domain = projected
            .nodes
            .iter()
            .find(|node| node.node_id == NodeId::new("domain_transform"))
            .unwrap();
        assert_eq!(
            serialized_scalar(domain.params.get("pos_x").unwrap()),
            Some(1.5)
        );
        assert_eq!(
            projected
                .preset_metadata
                .as_ref()
                .unwrap()
                .bindings
                .iter()
                .find(|binding| binding.id == "domain_x")
                .unwrap()
                .default_value,
            1.5
        );
        assert_eq!(
            crate::graph_target::resolve(&project, &target).unwrap(),
            &before
        );
        assert!(gizmo_target_for(&SceneVm::from_def(&projected).unwrap(), object_id).is_some());
    }

    #[test]
    fn bound_drag_commits_once_and_undoes_to_original_base() {
        let (mut project, layer_id, object_id) = fluid_project(true);
        let mut drag = FluidDomainDrag::begin(
            &project,
            layer_id.clone(),
            object_id,
            GizmoMode::Move,
            GizmoAxis::X,
        )
        .unwrap();
        assert!(drag.update(2.0));
        let mut command = build_action(&project, drag).unwrap();
        command.execute(&mut project);
        let target = GraphTarget::Generator(layer_id);
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .get_base_param("domain_x"),
            2.0
        );
        command.undo(&mut project);
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .get_base_param("domain_x"),
            1.5
        );
    }

    #[test]
    fn draft_update_does_not_mutate_project_and_stale_base_rejects_commit() {
        let (mut project, layer_id, object_id) = fluid_project(true);
        let target = GraphTarget::Generator(layer_id.clone());
        let before = serde_json::to_vec(&project).unwrap();
        let mut drag = FluidDomainDrag::begin(
            &project,
            layer_id,
            object_id,
            GizmoMode::Scale,
            GizmoAxis::Y,
        )
        .unwrap();
        assert!(drag.update(6.0));
        assert_eq!(serde_json::to_vec(&project).unwrap(), before);
        project
            .graph_target_owner_mut(&target)
            .unwrap()
            .set_base_param("domain_x", 3.0);
        assert!(build_action(&project, drag).is_err());
    }

    #[test]
    fn fanout_and_noninvertible_bindings_are_rejected_at_begin() {
        let (mut project, layer_id, object_id) = fluid_project(true);
        let target = GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        let graph = owner.graph.as_mut().unwrap();
        graph
            .preset_metadata
            .as_mut()
            .unwrap()
            .bindings
            .push(BindingDef {
                id: "domain_x".into(),
                label: "Fanout".into(),
                default_value: 0.0,
                target: BindingTarget::Node {
                    node_id: NodeId::new("domain_transform"),
                    param: "scale_x".into(),
                },
                convert: ParamConvert::Float,
                user_added: false,
                scale: 1.0,
                offset: 0.0,
                default_mirrors_node_param: false,
            });
        assert!(
            FluidDomainDrag::begin(
                &project,
                layer_id.clone(),
                object_id,
                GizmoMode::Move,
                GizmoAxis::X
            )
            .unwrap_err()
            .contains("fan-out")
        );
        let metadata = project
            .graph_target_owner_mut(&target)
            .unwrap()
            .graph
            .as_mut()
            .unwrap()
            .preset_metadata
            .as_mut()
            .unwrap();
        metadata.bindings.pop();
        metadata
            .bindings
            .iter_mut()
            .find(|binding| binding.id == "domain_x")
            .unwrap()
            .scale = 0.0;
        assert!(
            FluidDomainDrag::begin(&project, layer_id, object_id, GizmoMode::Move, GizmoAxis::X)
                .unwrap_err()
                .contains("non-invertible")
        );
    }

    #[test]
    fn invalid_scale_update_keeps_last_valid_draft() {
        let (project, layer_id, object_id) = fluid_project(false);
        let mut drag = FluidDomainDrag::begin(
            &project,
            layer_id,
            object_id,
            GizmoMode::Scale,
            GizmoAxis::X,
        )
        .unwrap();
        assert!(!drag.update(f32::NAN));
        assert_eq!(drag.value, 4.0);
        assert!(drag.update(20.1));
        assert_eq!(drag.value, 20.0);
        assert!(drag.update(6.0));
    }
}
