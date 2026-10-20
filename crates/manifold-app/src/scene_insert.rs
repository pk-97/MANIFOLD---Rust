//! Preparing a scene template insertion for callers that know the render
//! scene by document id (the graph editor's buttons, the snapshot harness).

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::project::Project;
use manifold_core::scene_template::SceneTemplate;
use manifold_core::{GraphTarget, SceneNodeRef};
use manifold_editing::scene_template::InsertSceneTemplateCommand;

/// The scene reference of the render scene with document id `render` under
/// the groups with document ids `scope`, outermost first.
pub(crate) fn scene_ref(def: &EffectGraphDef, scope: &[u32], render: u32) -> Option<SceneNodeRef> {
    let mut nodes = &def.nodes;
    let mut ids = Vec::new();
    for doc in scope {
        let group = nodes.iter().find(|node| node.id == *doc)?;
        ids.push(group.node_id.clone());
        nodes = &group.group.as_deref()?.nodes;
    }
    let node = nodes.iter().find(|node| node.id == render)?.node_id.clone();
    Some(SceneNodeRef { scope: ids, node })
}

/// The insert command for `template`, prepared against the current graph.
pub(crate) fn prepare(
    project: &Project,
    target: &GraphTarget,
    default: &EffectGraphDef,
    scope: &[u32],
    render: u32,
    template: SceneTemplate,
) -> Result<InsertSceneTemplateCommand, String> {
    let scene = project
        .graph_for_target(target, Some(default))
        .and_then(|def| scene_ref(def, scope, render))
        .ok_or_else(|| "The scene is no longer available".to_string())?;
    InsertSceneTemplateCommand::new(project, target.clone(), scene, template, default).map_err(|error| error.message)
}
