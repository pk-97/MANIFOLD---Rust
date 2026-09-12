//! Content-owned modifier actions and structural graph admission.
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphNode};
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_core::{GraphTarget, LayerId, NodeId, PresetTypeId, project::Project};
use manifold_editing::command::Command;
use manifold_editing::commands::graph::{
    DeleteSceneModifierCommand, DuplicateSceneModifiersCommand, InsertSceneModifierCommand,
    MoveSceneModifierCommand, RemoveSceneModifiersCommand, ReorderSceneModifiersCommand,
    SetSceneModifierPreparationParamCommand,
};
use manifold_renderer::node_graph::{
    PrimitiveRegistry, scene_modifier_authoring::prepare_new_scene_modifier,
};

#[derive(Debug)]
pub(crate) enum SceneModifierAction {
    Add(LayerId, String),
    Remove(LayerId, NodeId),
    Move(LayerId, NodeId, usize),
    Reorder(LayerId, Vec<NodeId>),
    Duplicate(LayerId, Vec<NodeId>),
    RemoveMany(LayerId, Vec<NodeId>),
    Retarget(LayerId, NodeId, SceneTargetSelection),
    Toggle(LayerId, NodeId),
    Preparation(LayerId, NodeId, String, f32),
}

pub(crate) fn build_action(
    project: &Project,
    action: SceneModifierAction,
) -> Result<Box<dyn Command>, String> {
    let layer = match &action {
        SceneModifierAction::Add(layer, _)
        | SceneModifierAction::Remove(layer, _)
        | SceneModifierAction::Move(layer, ..)
        | SceneModifierAction::Reorder(layer, _)
        | SceneModifierAction::Duplicate(layer, _)
        | SceneModifierAction::RemoveMany(layer, _)
        | SceneModifierAction::Retarget(layer, ..)
        | SceneModifierAction::Toggle(layer, _)
        | SceneModifierAction::Preparation(layer, ..) => layer,
    };
    let target = GraphTarget::Generator(layer.clone());
    let default = crate::graph_target::owner_default(project, &target)
        .ok_or("Generator graph is no longer available")?;
    let graph = crate::graph_target::resolve(project, &target)
        .ok_or("Generator graph is no longer available")?;
    match action {
        SceneModifierAction::Add(_, preset) => {
            let recipe = manifold_renderer::node_graph::bundled_preset_def(
                &PresetTypeId::from_string(preset),
            )
            .ok_or("Scene modifier preset is no longer available")?;
            let mut scenes = Vec::new();
            collect_scenes(&graph.nodes, &mut Vec::new(), &mut scenes);
            if scenes.len() != 1 {
                return Err("Applying a modifier requires exactly one scene".into());
            }
            let instance = prepare_new_scene_modifier(
                graph,
                recipe,
                NodeId::new(manifold_core::short_id()),
                scenes.remove(0),
                SceneTargetSelection::AllObjects,
            )
            .map_err(|e| e.to_string())?;
            InsertSceneModifierCommand::new(
                project,
                target,
                &default,
                graph.scene_modifiers.len(),
                instance,
            )
            .map(|command| Box::new(command) as Box<dyn Command>)
            .map_err(|e| e.to_string())
        }
        SceneModifierAction::Remove(_, id) => {
            DeleteSceneModifierCommand::new(project, target, &default, id)
                .map(|command| Box::new(command) as Box<dyn Command>)
                .map_err(|e| e.to_string())
        }
        SceneModifierAction::Move(_, id, index) => {
            MoveSceneModifierCommand::new(project, target, &default, id, index)
                .map(|command| Box::new(command) as Box<dyn Command>)
                .map_err(|e| e.to_string())
        }
        SceneModifierAction::Reorder(_, order) => {
            ReorderSceneModifiersCommand::new(project, target, &default, order)
                .map(|command| Box::new(command) as Box<dyn Command>)
                .map_err(|e| e.to_string())
        }
        SceneModifierAction::Duplicate(_, selected) => {
            DuplicateSceneModifiersCommand::new(project, target, &default, selected)
                .map(|command| Box::new(command) as Box<dyn Command>)
                .map_err(|e| e.to_string())
        }
        SceneModifierAction::RemoveMany(_, selected) => {
            RemoveSceneModifiersCommand::new(project, target, &default, selected)
                .map(|command| Box::new(command) as Box<dyn Command>)
                .map_err(|e| e.to_string())
        }
        SceneModifierAction::Toggle(_, id) => {
            let instance = graph
                .scene_modifiers
                .iter()
                .find(|m| m.id == id)
                .ok_or("Modifier is no longer present")?;
            let enabled = &instance
                .graph
                .preset_metadata
                .as_ref()
                .and_then(|m| m.scene_modifier.as_ref())
                .ok_or("Modifier metadata is missing")?
                .enabled_param;
            let bindings = &graph
                .preset_metadata
                .as_ref()
                .ok_or("Generator bindings are missing")?
                .bindings;
            let mut matches = bindings.iter().filter(|binding| matches!(&binding.target,
                BindingTarget::SceneModifier { modifier_id, param_id } if modifier_id == &id && param_id == enabled));
            let binding = matches
                .next()
                .ok_or("Modifier enabled control is missing")?;
            if matches.next().is_some() {
                return Err(
                    "Modifier enabled control is ambiguous; edit its explicit macro".into(),
                );
            }
            let param = project
                .graph_target_owner(&target)
                .and_then(|host| host.params.get(&binding.id))
                .ok_or("Modifier enabled parameter is missing")?;
            let local_value = manifold_core::effects::apply_card_reshape(
                param.base,
                param.spec.min,
                param.spec.max,
                param.spec.invert,
                param.spec.curve,
                binding.scale,
                binding.offset,
            );
            if !local_value.is_finite() {
                return Err("Modifier enabled control has an invalid mapped value".into());
            }
            let next_local = if local_value > 0.5 { 0.0 } else { 1.0 };
            let next_base = manifold_core::effects::invert_card_reshape(
                next_local,
                param.spec.min,
                param.spec.max,
                param.spec.invert,
                param.spec.curve,
                binding.scale,
                binding.offset,
            )
            .ok_or("Modifier enabled control has a non-invertible mapping")?;
            let round_trip = manifold_core::effects::apply_card_reshape(
                next_base,
                param.spec.min,
                param.spec.max,
                param.spec.invert,
                param.spec.curve,
                binding.scale,
                binding.offset,
            );
            if !next_base.is_finite()
                || next_base < param.spec.min
                || next_base > param.spec.max
                || (round_trip - next_local).abs() > 1e-4
            {
                return Err("Modifier enabled target is outside its mapped range".into());
            }
            Ok(Box::new(
                manifold_editing::commands::effects::ChangeGraphParamCommand::new(
                    target,
                    binding.id.clone(),
                    param.base,
                    next_base,
                ),
            ))
        }
        SceneModifierAction::Retarget(_, id, targets) => {
            let mut instance = graph
                .scene_modifiers
                .iter()
                .find(|m| m.id == id)
                .ok_or("Modifier is no longer present")?
                .clone();
            instance.targets = targets.clone();
            let frames =
                manifold_renderer::node_graph::scene_modifier_expand::resolve_modifier_mesh_frames(
                    graph, &instance,
                )
                .map_err(|error| error.to_string())?;
            manifold_editing::commands::graph::RetargetSceneModifierCommand::new(
                project, target, &default, id, targets, frames,
            )
            .map(|command| Box::new(command) as Box<dyn Command>)
            .map_err(|error| error.to_string())
        }
        SceneModifierAction::Preparation(_, id, param_id, value) => {
            SetSceneModifierPreparationParamCommand::new(
                project, target, &default, id, param_id, value,
            )
            .map(|command| Box::new(command) as Box<dyn Command>)
            .map_err(|error| error.to_string())
        }
    }
}

/// Source selectors and render mode cannot change under captured geometry.
/// This cheap authored-identity check also guards live UI writes before they
/// reach the ordinary scalar path.
pub(crate) fn node_parameter_lock_reason(
    project: &Project,
    target: &GraphTarget,
    scope: &[u32],
    doc_id: u32,
    param: &str,
) -> Option<&'static str> {
    if matches!(target, GraphTarget::SceneModifier { .. }) {
        return None;
    }
    let graph = crate::graph_target::resolve(project, target)?;
    let node = descend_nodes(&graph.nodes, scope)?
        .iter()
        .find(|node| node.id == doc_id)?;
    manifold_core::scene_modifier_preset::scene_modifier_parameter_lock_reason(
        graph,
        &node.node_id,
        param,
    )
}

pub(crate) fn macro_parameter_lock_reason(
    project: &Project,
    target: &GraphTarget,
    param: &str,
) -> Option<&'static str> {
    if matches!(target, GraphTarget::SceneModifier { .. }) {
        return None;
    }
    let graph = crate::graph_target::resolve(project, target)?;
    manifold_core::scene_modifier_preset::scene_modifier_macro_lock_reason(graph, param)
}

/// The local node-face address for a preparation-only modifier control.
/// Preparation values belong to the modifier recipe and never acquire a host
/// macro. The caller uses the returned reshape to invert the displayed node
/// value before sending the content-owned preparation command.
#[derive(Debug, Clone)]
pub(crate) struct PreparationTarget {
    pub modifier_id: NodeId,
    pub param_id: String,
    pub baseline: f32,
    pub min: f32,
    pub max: f32,
    pub invert: bool,
    pub curve: manifold_core::macro_bank::MacroCurve,
    pub scale: f32,
    pub offset: f32,
}

pub(crate) fn preparation_target_for_node_param(
    project: &Project,
    target: &GraphTarget,
    scope_path: &[u32],
    node_doc_id: u32,
    param_name: &str,
) -> Result<Option<PreparationTarget>, String> {
    let GraphTarget::SceneModifier { modifier_id, .. } = target else {
        return Ok(None);
    };
    let local = crate::graph_target::resolve(project, target)
        .ok_or_else(|| "Scene modifier graph is no longer available".to_string())?;
    let recipe = local
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_modifier.as_ref());
    let Some(recipe) = recipe else {
        return Ok(None);
    };
    let nodes = descend_nodes(&local.nodes, scope_path)
        .ok_or_else(|| "Scene modifier node scope is no longer available".to_string())?;
    let node = nodes
        .iter()
        .find(|node| node.id == node_doc_id)
        .ok_or_else(|| format!("Scene modifier node {node_doc_id} is no longer available"))?;
    if node.node_id.is_empty() {
        return Err("Preparation control has no stable local node id".into());
    }
    let metadata = local
        .preset_metadata
        .as_ref()
        .expect("recipe metadata was present above");
    let bindings: Vec<_> = metadata
        .bindings
        .iter()
        .filter(|binding| {
            matches!(
                &binding.target,
                BindingTarget::Node { node_id, param }
                    if node_id == &node.node_id && param == param_name
            )
        })
        .collect();
    let Some(binding) = select_preparation_binding(
        &bindings,
        &recipe.preparation_params,
        &node.node_id,
        param_name,
    )?
    else {
        if recipe.preparation_params.iter().any(|id| id == param_name) {
            return Err(format!(
                "Preparation parameter {param_name} has no direct local node binding"
            ));
        }
        return Ok(None);
    };
    let spec = metadata
        .params
        .iter()
        .find(|param| param.id == binding.id)
        .ok_or_else(|| {
            format!(
                "Preparation parameter {} has no numeric metadata",
                binding.id
            )
        })?;
    if !spec.min.is_finite() || !spec.max.is_finite() || !spec.default_value.is_finite() {
        return Err(format!(
            "Preparation parameter {} has invalid bounds",
            binding.id
        ));
    }
    Ok(Some(PreparationTarget {
        modifier_id: modifier_id.clone(),
        param_id: binding.id.clone(),
        baseline: spec.default_value,
        min: spec.min,
        max: spec.max,
        invert: spec.invert,
        curve: spec.curve,
        scale: binding.scale,
        offset: binding.offset,
    }))
}

fn descend_nodes<'a>(
    nodes: &'a [EffectGraphNode],
    scope_path: &[u32],
) -> Option<&'a [EffectGraphNode]> {
    let Some(group_id) = scope_path.first() else {
        return Some(nodes);
    };
    let group = nodes
        .iter()
        .find(|node| node.id == *group_id)?
        .group
        .as_ref()?;
    descend_nodes(&group.nodes, &scope_path[1..])
}

fn select_preparation_binding<'a>(
    bindings: &[&'a manifold_core::effect_graph_def::BindingDef],
    preparation_params: &[String],
    node_id: &NodeId,
    param_name: &str,
) -> Result<Option<&'a manifold_core::effect_graph_def::BindingDef>, String> {
    let preparation: Vec<_> = bindings
        .iter()
        .filter(|binding| preparation_params.iter().any(|id| id == &binding.id))
        .copied()
        .collect();
    if preparation.len() > 1 || (preparation.len() == 1 && bindings.len() != 1) {
        return Err(format!(
            "Preparation control {}/{} is ambiguous",
            node_id, param_name
        ));
    }
    Ok(preparation.first().copied())
}

fn collect_scenes(nodes: &[EffectGraphNode], scope: &mut Vec<NodeId>, out: &mut Vec<SceneNodeRef>) {
    for node in nodes {
        if node.type_id == "node.render_scene" {
            out.push(SceneNodeRef {
                scope: scope.clone(),
                node: node.node_id.clone(),
            });
        }
        if let Some(group) = &node.group {
            scope.push(node.node_id.clone());
            collect_scenes(&group.nodes, scope, out);
            scope.pop();
        }
    }
}

/// Only declared structural edits take a private project snapshot. Numeric
/// gestures have no admission targets and keep the existing direct path.
pub(crate) fn with_admission(command: Box<dyn Command>) -> Box<dyn Command> {
    let mut targets = Vec::new();
    command.graph_admission_targets(&mut targets);
    let mut clips = Vec::new();
    command.graph_admission_clips(&mut clips);
    let mut owners = Vec::new();
    for target in targets {
        if let Some(owner @ GraphTarget::Generator(_)) = target.host_target()
            && !owners.contains(owner)
        {
            owners.push(owner.clone());
        }
    }
    if owners.is_empty() && clips.is_empty() {
        return command;
    }
    Box::new(AdmittedGraphCommand {
        command,
        owners,
        clips,
        applied: false,
        rejection: None,
        frame_changes: Vec::new(),
        frames_captured: false,
    })
}

#[derive(Debug)]
struct AdmittedGraphCommand {
    command: Box<dyn Command>,
    owners: Vec<GraphTarget>,
    clips: Vec<manifold_core::ClipId>,
    applied: bool,
    rejection: Option<String>,
    frame_changes: Vec<FrameChange>,
    frames_captured: bool,
}

#[derive(Debug, Clone)]
struct FrameChange {
    owner: GraphTarget,
    modifier_id: NodeId,
    before: Vec<manifold_core::scene_modifier_preset::SceneMeshReferenceFrame>,
    after: Vec<manifold_core::scene_modifier_preset::SceneMeshReferenceFrame>,
}

impl Command for AdmittedGraphCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        let mut candidate = project.clone();
        self.command.execute(&mut candidate);
        let mut owners = self.owners.clone();
        for layer in &candidate.timeline.layers {
            if layer.clips.iter().any(|clip| self.clips.contains(&clip.id)) {
                let target = GraphTarget::Generator(layer.layer_id.clone());
                if !owners.contains(&target) {
                    owners.push(target);
                }
            }
        }
        if !self.command.was_applied() {
            self.rejection = self.command.rejection_reason().map(str::to_string);
            return;
        }
        if !self.frames_captured {
            match capture_frame_changes(&candidate, &owners) {
                Ok(changes) => {
                    self.frame_changes = changes;
                    self.frames_captured = true;
                }
                Err(error) => {
                    self.command.undo(&mut candidate);
                    self.rejection = Some(error);
                    return;
                }
            }
        }
        if let Err(error) = apply_frame_changes(&mut candidate, &self.frame_changes, true) {
            restore_frame_changes(&mut candidate, &self.frame_changes);
            self.command.undo(&mut candidate);
            self.rejection = Some(error);
            return;
        }
        let registry = PrimitiveRegistry::with_builtin();
        for owner in &owners {
            if let Err(error) = validate_owner(&candidate, owner, &registry) {
                // Reset the command's reverse state as well, so a rejected redo
                // can be retried after the source is restored.
                restore_frame_changes(&mut candidate, &self.frame_changes);
                self.command.undo(&mut candidate);
                self.rejection = Some(error);
                return;
            }
        }
        *project = candidate;
        self.applied = true;
    }
    fn undo(&mut self, project: &mut Project) {
        if self.applied {
            restore_frame_changes(project, &self.frame_changes);
            self.command.undo(project);
            self.applied = false;
        }
    }
    fn description(&self) -> &str {
        self.command.description()
    }
    fn was_applied(&self) -> bool {
        self.applied
    }
    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
}

fn capture_frame_changes(
    project: &Project,
    owners: &[GraphTarget],
) -> Result<Vec<FrameChange>, String> {
    let mut changes = Vec::new();
    for owner in owners {
        let owner_graph = crate::graph_target::resolve(project, owner)
            .ok_or_else(|| format!("Generator owner {} is no longer available", owner.label()))?;
        for instance in &owner_graph.scene_modifiers {
            let before = instance.mesh_frames.clone();
            let after =
                manifold_renderer::node_graph::scene_modifier_expand::resolve_modifier_mesh_frames(
                    owner_graph,
                    instance,
                )
                .map_err(|error| format!("Scene modifier frame admission rejected: {error}"))?;
            if before != after {
                changes.push(FrameChange {
                    owner: owner.clone(),
                    modifier_id: instance.id.clone(),
                    before,
                    after,
                });
            }
        }
    }
    Ok(changes)
}

fn apply_frame_changes(
    project: &mut Project,
    changes: &[FrameChange],
    after: bool,
) -> Result<(), String> {
    // Validate every target before changing any frame array, so a malformed
    // redo cannot partially apply its staged reconciliation.
    for change in changes {
        let graph = project
            .graph_target_owner(&change.owner)
            .and_then(|owner| owner.graph.as_ref())
            .ok_or_else(|| format!("Generator owner {} has no graph", change.owner.label()))?;
        let mut matches = graph
            .scene_modifiers
            .iter()
            .filter(|instance| instance.id == change.modifier_id);
        if matches.next().is_none() {
            return Err(format!(
                "Scene modifier {} is no longer available",
                change.modifier_id
            ));
        }
        if matches.next().is_some() {
            return Err(format!(
                "Scene modifier {} has duplicate identities",
                change.modifier_id
            ));
        }
    }
    for change in changes {
        let graph = project
            .graph_target_owner_mut(&change.owner)
            .and_then(|owner| owner.graph.as_mut())
            .expect("frame target was validated above");
        let instance = graph
            .scene_modifiers
            .iter_mut()
            .find(|instance| instance.id == change.modifier_id)
            .expect("frame target was validated above");
        instance.mesh_frames = if after {
            change.after.clone()
        } else {
            change.before.clone()
        };
    }
    Ok(())
}

fn restore_frame_changes(project: &mut Project, changes: &[FrameChange]) {
    if let Err(error) = apply_frame_changes(project, changes, false) {
        log::error!("[scene modifier] failed to restore mesh frames: {error}");
    }
}

fn validate_owner(
    project: &Project,
    target: &GraphTarget,
    registry: &PrimitiveRegistry,
) -> Result<(), String> {
    let Some(graph) = crate::graph_target::resolve(project, target) else {
        return Ok(());
    };
    if graph.scene_modifiers.is_empty() {
        return Ok(());
    }
    let host = project
        .graph_target_owner(target)
        .ok_or("Generator owner is no longer present")?;
    let mut runtime = manifold_renderer::preset_runtime::PresetRuntime::from_def(
        graph.clone(),
        registry,
        Some(&host.params),
    )
    .map_err(|e| format!("Scene modifier edit rejected: {e}"))?;
    let width = u32::try_from(project.settings.output_width)
        .ok()
        .filter(|&value| value > 0)
        .ok_or("Scene modifier edit rejected: output width must be positive")?;
    let height = u32::try_from(project.settings.output_height)
        .ok()
        .filter(|&value| value > 0)
        .ok_or("Scene modifier edit rejected: output height must be positive")?;
    runtime
        .prepared_modifier_buffer_usage((width, height))
        .map_err(|e| format!("Scene modifier edit rejected: {e}"))?;
    if let GraphTarget::Generator(layer) = target {
        let (_, layer) = project
            .timeline
            .find_layer_by_id(layer)
            .ok_or("Generator layer is no longer present")?;
        for clip in &layer.clips {
            runtime.apply_string_params(clip.string_params.as_ref());
            if let Some((node, param)) = runtime.graph.prepared_param_violation() {
                return Err(format!(
                    "Scene modifier edit rejected: clip {} changes calibrated source {node}.{param}",
                    clip.id
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod routing_tests {
    use super::select_preparation_binding;
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::{BindingDef, BindingTarget};

    fn binding(id: &str) -> BindingDef {
        BindingDef {
            id: id.into(),
            label: id.into(),
            default_value: 0.0,
            target: BindingTarget::Node {
                node_id: NodeId::new("node"),
                param: "value".into(),
            },
            convert: Default::default(),
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        }
    }

    #[test]
    fn ordinary_aliases_do_not_become_preparation_controls() {
        let first = binding("live_a");
        let second = binding("live_b");
        let bindings = vec![&first, &second];
        assert!(
            select_preparation_binding(&bindings, &["prep".into()], &NodeId::new("node"), "value",)
                .expect("ordinary aliases are valid")
                .is_none()
        );
    }

    #[test]
    fn preparation_aliases_are_rejected_when_ambiguous() {
        let first = binding("prep");
        let second = binding("prep");
        let bindings = vec![&first, &second];
        let error =
            select_preparation_binding(&bindings, &["prep".into()], &NodeId::new("node"), "value")
                .expect_err("duplicate preparation aliases are ambiguous");
        assert!(error.contains("ambiguous"));
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod frame_tests;

#[cfg(test)]
mod clip_tests;
