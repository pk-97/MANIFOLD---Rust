//! The graph editor viewport's drawn gizmo selection: rebuilt only when its
//! scene or its picked ids change, so a steady frame allocates nothing.

use std::sync::Arc;

use manifold_nodes_scene::node_graph::scene_vm::SceneVm;
use manifold_nodes_scene::node_graph::viewport_gizmo::{GizmoSelection, GizmoTargetId};
use manifold_ui::panels::scene_setup_panel::SceneSelection;

/// The editor viewport's drawn selection and what it was built from.
pub struct ViewportSelection {
    pub scene: Arc<SceneVm>,
    pub ids: Vec<GizmoTargetId>,
    pub selection: GizmoSelection,
}

impl ViewportSelection {
    /// The selection of `ids` over `scene`, rebuilt into `cache` when either
    /// changed. `None` when the viewport has no scene.
    pub fn refresh<'a>(cache: &'a mut Option<Self>, scene: Option<&Arc<SceneVm>>, ids: &[GizmoTargetId]) -> Option<&'a Self> {
        let scene = scene?;
        let stale = cache.as_ref().is_none_or(|shown| !Arc::ptr_eq(&shown.scene, scene) || shown.ids != ids);
        if stale {
            let selection = GizmoSelection::of_ids(scene, ids);
            *cache = Some(Self { scene: scene.clone(), ids: ids.to_vec(), selection });
        }
        cache.as_ref()
    }

    /// The outline slots of every selected scene object.
    pub fn outline(&self, out: &mut Vec<u32>) {
        let objects: Vec<_> = self.selection.selected.iter().filter_map(|target| match crate::scene_gizmo::scene_selection(&self.scene, target) {
            Some(SceneSelection::Object(object)) => Some(object),
            _ => None,
        }).collect();
        crate::selection_outline::outline_slots(&self.scene, &objects, out);
    }
}
