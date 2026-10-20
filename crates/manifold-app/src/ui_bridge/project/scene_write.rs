//! The scene panel's one param write path and the reads it shares with the
//! eye and gizmo projections: node and binding lookup by group scope, the
//! panel's base value, and which node is a Water family's object.

use manifold_core::LayerId;
use manifold_core::project::Project;

use super::generator_catalog_default;

/// Find the `EffectGraphNode` with doc id `node_doc_id` at `scope_path`
/// (a path of group-node doc ids to descend into, empty = document root) —
/// the same addressing every graph command's `.with_scope` takes.
/// `inspector.rs`'s scene modulation redirect reads the node's stable
/// identity through it when it materializes an exposure on first arm
/// (BUG-249 (scene modulation redirect)).
pub(crate) fn find_node_by_scope<'a>(
    def: &'a manifold_core::effect_graph_def::EffectGraphDef,
    scope_path: &[u32],
    node_doc_id: u32,
) -> Option<&'a manifold_core::effect_graph_def::EffectGraphNode> {
    let mut nodes = def.nodes.as_slice();
    for group_id in scope_path {
        let group_node = nodes.iter().find(|n| n.id == *group_id)?;
        nodes = &group_node.group.as_ref()?.nodes;
    }
    nodes.iter().find(|n| n.id == node_doc_id)
}

/// Resolve a row binding through its group scope. Recipe-local numeric ids
/// may repeat in nested groups; stable binding targets do not.
pub(crate) fn scene_binding_id(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    scope_path: &[u32], node_doc_id: u32, param_key: &str,
) -> Option<String> {
    let Some(node) = find_node_by_scope(def, scope_path, node_doc_id) else {
        // Existing scene actions also accept an empty scope for imported
        // nodes with document-wide ids. Preserve that established address
        // form; an explicit group scope must always resolve exactly.
        return scope_path.is_empty().then(||
            manifold_core::effects::binding_id_for_node_param_in(def, node_doc_id, param_key)
        ).flatten();
    };
    let identity = if node.node_id.is_empty() {
        manifold_core::NodeId::new(node.handle.clone().unwrap_or_else(|| format!("node{node_doc_id}")))
    } else { node.node_id.clone() };
    def.preset_metadata.as_ref()?.bindings.iter().find(|binding| matches!(&binding.target,
        manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
            if node_id == &identity && param == param_key)).map(|binding| binding.id.clone())
}

/// The ONE write path every scene-panel param change takes. Bound param →
/// edit the binding's instance slot, never the def — a def write on a bound
/// param is re-seeded over on rebuild (the importer-camera deadness this
/// guards against). Unbound → def-level `SetGraphNodeParamCommand`. Returns
/// the command for the content thread — sent singly by
/// `SceneSetupParamChanged`, or batched under one `CompositeCommand` by
/// frame-selected so a camera frame is one undo unit.
/// `None` when the layer has no generator default or the bound value is
/// unchanged (the epsilon no-change guard).
pub(crate) fn apply_scene_param_write(
    project: &Project,
    layer_id: &LayerId,
    scope_path: Vec<u32>,
    node_doc_id: u32,
    param_id: &str,
    value: f32,
) -> Option<Box<dyn manifold_editing::command::Command + Send>> {
    // A water's eye is its family flag, but a water can also be hidden by
    // its object's own `visible` (saved before it read as Water): showing
    // it raises both, as one undo step.
    if param_id == "parent_visible" && value > 0.5 {
        let default = generator_catalog_default(project, layer_id)?;
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let def = project.graph_target_owner(&target).and_then(|instance| instance.graph.as_ref()).unwrap_or(&default);
        let inst = project.graph_target_owner(&target);
        if is_water_object(def, &scope_path, node_doc_id)
            && scene_param_value(def, inst, &scope_path, node_doc_id, "visible") < 0.5
        {
            let own = scene_param_write(project, layer_id, scope_path.clone(), node_doc_id, "visible", 1.0);
            let family = scene_param_write(project, layer_id, scope_path, node_doc_id, param_id, value);
            let writes: Vec<Box<dyn manifold_editing::command::Command>> =
                own.into_iter().chain(family).map(|cmd| cmd as Box<dyn manifold_editing::command::Command>).collect();
            return Some(Box::new(manifold_editing::command::CompositeCommand::new(writes, "Show water".into())));
        }
    }
    scene_param_write(project, layer_id, scope_path, node_doc_id, param_id, value)
}

/// Whether the node at `scope_path`/`node_doc_id` is a Water family's real
/// object: the parent `water_family` finds in its enclosing group.
pub(crate) fn is_water_object(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    scope_path: &[u32],
    node_doc_id: u32,
) -> bool {
    let Some((group, outer)) = scope_path.split_last() else { return false };
    find_node_by_scope(def, outer, *group)
        .and_then(|group| group.group.as_deref())
        .and_then(manifold_core::scene_object_model::water_family)
        .is_some_and(|family| family.parent == node_doc_id)
}

/// A scene param's base value as the panel reads it: the binding's instance
/// slot for a bound param, else the node's own value (default on).
pub(crate) fn scene_param_value(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    inst: Option<&manifold_core::effects::PresetInstance>,
    scope_path: &[u32],
    node_doc_id: u32,
    param: &str,
) -> f32 {
    if let Some(inst) = inst
        && let Some(id) = scene_binding_id(def, scope_path, node_doc_id, param)
        && inst.params.contains(id.as_str())
    {
        return inst.get_base_param(id.as_str());
    }
    find_node_by_scope(def, scope_path, node_doc_id)
        .and_then(|node| node.params.get(param))
        .and_then(crate::editor_bridge::serialized_value_as_f32)
        .unwrap_or(1.0)
}

fn scene_param_write(
    project: &Project,
    layer_id: &LayerId,
    scope_path: Vec<u32>,
    node_doc_id: u32,
    param_id: &str,
    value: f32,
) -> Option<Box<dyn manifold_editing::command::Command + Send>> {
    let default = generator_catalog_default(project, layer_id)?;
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let effective = project.graph_target_owner(&target)
        .and_then(|instance| instance.graph.as_ref()).unwrap_or(&default);
    let bound = scene_binding_id(effective, &scope_path, node_doc_id, param_id);
    if let Some(id) = bound {
        let pid = manifold_core::effects::ParamId::from(id);
        let old_val = project
            .graph_target_owner(&target)
            .and_then(|inst| {
                inst.params
                    .contains(pid.as_ref())
                    .then(|| inst.get_base_param(pid.as_ref()))
            })?;
        if (old_val - value).abs() <= f32::EPSILON {
            return None;
        }
        return Some(Box::new(
            manifold_editing::commands::effects::ChangeGraphParamCommand::new(
                target, pid, old_val, value,
            ),
        ));
    }
    let cmd: Box<dyn manifold_editing::command::Command + Send> = Box::new(
        manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
            target,
            node_doc_id,
            param_id.to_string(),
            manifold_core::effect_graph_def::SerializedParamValue::Float { value },
            default,
        )
        .with_scope(scope_path),
    );
    Some(cmd)
}
