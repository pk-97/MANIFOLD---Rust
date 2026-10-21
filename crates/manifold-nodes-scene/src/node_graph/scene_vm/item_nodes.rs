//! The nodes each scene item owns, read off the traced scene
//! (SCENE_SURFACES_DESIGN.md section 3.5 (scene items)). `scene_items`
//! turns these lists into items; nothing else reads them.

use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::{NodeId, SceneNodeRef};
use manifold_core::effect_graph_def::EffectGraphNode;

use super::{AtmosphereVm, CameraVm, EnvironmentVm, Level};

/// Stable identities of `ids` in `level`; nodes without one own no controls.
fn stable_ids(level: &Level, ids: impl IntoIterator<Item = u32>) -> Vec<NodeId> {
    ids.into_iter()
        .filter_map(|id| level.node(id))
        .map(|node| node.node_id.clone())
        .filter(|id| !id.is_empty())
        .collect()
}

/// The camera atom, its lens, and the effects connected to this scene/camera.
pub(super) fn camera_nodes(level: &Level, camera: &CameraVm, scene: &EffectGraphNode, index: Option<&FlatSceneIndex>) -> Vec<NodeId> {
    let (atom, lens) = match camera {
        CameraVm::Orbit(c) => (Some(c.node_doc_id), c.lens.as_ref()),
        CameraVm::Free(c) => (Some(c.node_doc_id), c.lens.as_ref()),
        CameraVm::LookAt(c) => (Some(c.node_doc_id), c.lens.as_ref()),
        CameraVm::Loop(c) => (None, c.lens.as_ref()),
        CameraVm::Custom { lens, .. } => (None, lens.as_ref()),
        CameraVm::None => (None, None),
    };
    let mut controls = stable_ids(level, atom.into_iter().chain(lens.map(|lens| lens.node_doc_id)));
    if let Some(index) = index {
        controls.extend(super::super::scene_camera::camera_effect_controls(index, &SceneNodeRef {
            scope: Vec::new(), node: scene.node_id.clone(),
        }));
    }
    controls
}

/// The render_scene root (its Rendering toggles), the environment and the
/// atmosphere.
pub(super) fn world_nodes(
    level: &Level,
    scene_node: &EffectGraphNode,
    environment: &EnvironmentVm,
    atmosphere: &AtmosphereVm,
) -> Vec<NodeId> {
    let mut ids = vec![scene_node.id];
    match environment {
        EnvironmentVm::Importer(e) => {
            ids.push(e.bake_node_id);
            ids.extend(e.background_node_ids.iter().copied());
        }
        EnvironmentVm::Bare(e) => ids.push(e.node_doc_id),
        EnvironmentVm::Custom { .. } | EnvironmentVm::None => {}
    }
    if let AtmosphereVm::Wired(a) = atmosphere {
        ids.push(a.node_doc_id);
    }
    stable_ids(level, ids)
}

/// Each physics world with the values wired into it. A copy group's body is
/// not the world's: the object that draws it owns its Count, Layout and
/// Spacing.
/// A shared value wired into the world is the world's control too: Add Fluid
/// routes gravity, speed and reset through values that also drive the water.
pub(super) fn physics_nodes(level: &Level) -> Vec<Vec<NodeId>> {
    level
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.physics_world")
        .map(|world| {
            let mut ids = vec![world.id];
            ids.extend(
                level
                    .wires
                    .iter()
                    .filter(|wire| wire.to_node == world.id)
                    .filter(|wire| level.node(wire.from_node).is_some_and(|node| node.type_id == "node.value"))
                    .map(|wire| wire.from_node),
            );
            stable_ids(level, ids)
        })
        .collect()
}
