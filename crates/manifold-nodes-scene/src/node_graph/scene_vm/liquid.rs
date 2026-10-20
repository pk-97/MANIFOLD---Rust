//! Where a stable node reference sits in the authored graph. Which rows are
//! water, and what each owns, is the scene object model's
//! (`manifold_core::scene_object_model`).

use manifold_core::SceneNodeRef;

use super::Level;
use super::shapes::NodeAt;

/// Find `reference` in the authored graph from the root.
pub(super) fn locate_node<'a>(root: &Level<'a>, reference: &SceneNodeRef) -> Option<NodeAt<'a>> {
    let mut level = Level { nodes: root.nodes, wires: root.wires };
    let mut parent = None;
    let mut scope = Vec::with_capacity(reference.scope.len());
    for group_id in &reference.scope {
        let group = level.nodes.iter().find(|node| &node.node_id == group_id)?;
        let body = group.group.as_deref()?;
        scope.push(group.id);
        parent = Some((Level { nodes: level.nodes, wires: level.wires }, group.id));
        level = Level { nodes: &body.nodes, wires: &body.wires };
    }
    let node = level.nodes.iter().find(|node| node.node_id == reference.node)?;
    Some(NodeAt { level, scope, node, parent })
}
