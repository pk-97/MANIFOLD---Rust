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
/// `None` when the layer has no generator default, the value is not finite
/// (a NaN or infinity is never written), an unbound param is one its node
/// does not declare, or the bound value is unchanged (the epsilon no-change
/// guard).
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
    if !value.is_finite() {
        return None;
    }
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
    // The def keeps the param's declared shape: a toggle such as a body's
    // `enabled` stays a Bool, which the graph loader and the readers that
    // match on it require. A param the node does not declare, as the loader
    // builds it, is refused: the loader would reject the layer's graph.
    let node = find_node_by_scope(effective, &scope_path, node_doc_id)?;
    let declared = manifold_nodes_scene::node_graph::scene_exposure::declared_params_for_node(node)?
        .into_iter()
        .find(|meta| meta.name == param_id)?;
    let serialized = manifold_editing::commands::graph::effective_value_to_serialized(declared.convert, value);
    let cmd: Box<dyn manifold_editing::command::Command + Send> = Box::new(
        manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
            target,
            node_doc_id,
            param_id.to_string(),
            serialized,
            default,
        )
        .with_scope(scope_path),
    );
    Some(cmd)
}

/// "Aim Sun at HDRI": move the first Sun light so it shines from the HDRI's
/// brightest region, keeping its target and its distance from it. The three
/// position writes are one undo step. Reads the file's decoded bytes once, on
/// the click, and writes nothing per frame.
pub(crate) fn aim_sun_at_hdri(
    project: &Project,
    layer_id: &LayerId,
    hdri_file: &str,
) -> Result<Box<dyn manifold_editing::command::Command + Send>, String> {
    let default = generator_catalog_default(project, layer_id).ok_or("There is no scene to aim")?;
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let inst = project.graph_target_owner(&target);
    let def = inst.and_then(|instance| instance.graph.as_ref()).unwrap_or(&default);
    let read = |sun: u32, param: &str| scene_param_value(def, inst, &[], sun, param);
    let sun = def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.light" && read(node.id, "mode") < 0.5)
        .map(|node| node.id)
        .ok_or("This scene has no Sun light to aim")?;
    let toward = manifold_nodes_scene::node_graph::hdri_sun::hdri_sun_direction(std::path::Path::new(hdri_file))
        .map_err(|reason| format!("Could not aim the Sun: {reason}"))?;

    let aim = ["aim_x", "aim_y", "aim_z"].map(|param| read(sun, param));
    let pos = ["pos_x", "pos_y", "pos_z"].map(|param| read(sun, param));
    let apart = (0..3).map(|k| (pos[k] - aim[k]).powi(2)).sum::<f32>().sqrt();
    let distance = if apart > 1e-3 { apart } else { 5.0 };
    let writes: Vec<Box<dyn manifold_editing::command::Command>> = ["pos_x", "pos_y", "pos_z"]
        .into_iter()
        .enumerate()
        .filter_map(|(k, param)| {
            scene_param_write(project, layer_id, Vec::new(), sun, param, aim[k] + toward[k] * distance)
        })
        .map(|cmd| cmd as Box<dyn manifold_editing::command::Command>)
        .collect();
    if writes.is_empty() {
        return Err("The Sun already points at the brightest part of the HDRI".to_string());
    }
    Ok(Box::new(manifold_editing::command::CompositeCommand::new(writes, "Aim Sun at HDRI".into())))
}

#[cfg(test)]
mod aim_sun_tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::types::LayerType;

    fn scene_project() -> (Project, LayerId) {
        let mut project = Project::default();
        let preset = PresetTypeId::from_string("Scene".to_string());
        let idx = project.timeline.add_layer("Scene", LayerType::Generator, preset);
        let layer_id = project.timeline.layers[idx].layer_id.clone();
        (project, layer_id)
    }

    fn sun_vectors(project: &Project, layer_id: &LayerId) -> ([f32; 3], [f32; 3]) {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let inst = project.graph_target_owner(&target);
        let default = generator_catalog_default(project, layer_id).unwrap();
        let def = inst.and_then(|instance| instance.graph.as_ref()).unwrap_or(&default);
        let sun = def
            .nodes
            .iter()
            .find(|node| node.type_id == "node.light" && scene_param_value(def, inst, &[], node.id, "mode") < 0.5)
            .expect("the Scene preset has a Sun")
            .id;
        let read = |params: [&str; 3]| params.map(|param| scene_param_value(def, inst, &[], sun, param));
        (read(["pos_x", "pos_y", "pos_z"]), read(["aim_x", "aim_y", "aim_z"]))
    }

    fn hdri_fixture() -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/hdri/hdri_float32.exr")
            .to_string_lossy()
            .into_owned()
    }

    /// The Sun ends up on the HDRI's bright side of its target at the same
    /// distance, and one undo puts it back.
    #[test]
    fn aiming_moves_the_sun_toward_the_hdri_peak_and_undoes_in_one_step() {
        let (mut project, layer_id) = scene_project();
        let (pos_before, aim) = sun_vectors(&project, &layer_id);
        let toward = manifold_nodes_scene::node_graph::hdri_sun::hdri_sun_direction(std::path::Path::new(&hdri_fixture()))
            .expect("the fixture has a bright region");
        let distance = (0..3).map(|k| (pos_before[k] - aim[k]).powi(2)).sum::<f32>().sqrt();

        let mut command = aim_sun_at_hdri(&project, &layer_id, &hdri_fixture()).expect("aim");
        command.execute(&mut project);
        let (pos_after, aim_after) = sun_vectors(&project, &layer_id);
        assert_eq!(aim_after, aim, "the target stays");
        for k in 0..3 {
            let want = aim[k] + toward[k] * distance;
            assert!((pos_after[k] - want).abs() < 1e-3, "axis {k}: {} vs {want}", pos_after[k]);
        }
        command.undo(&mut project);
        assert_eq!(sun_vectors(&project, &layer_id).0, pos_before);
    }

    #[test]
    fn aiming_at_a_missing_file_says_why() {
        let (project, layer_id) = scene_project();
        let Err(reason) = aim_sun_at_hdri(&project, &layer_id, "/nonexistent/sky.exr") else { panic!("a missing file is refused") };
        assert!(reason.starts_with("Could not aim the Sun"), "{reason}");
    }
}
