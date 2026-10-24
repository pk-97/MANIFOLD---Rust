//! The graph editor viewport's drawn gizmo selection: the Scene panel's
//! selected items over the viewport's scene, rebuilt only when either
//! changes, so a steady frame allocates nothing and a press reads it without
//! rebuilding the scene.

use std::sync::Arc;

use manifold_nodes_scene::node_graph::scene_vm::SceneVm;
use manifold_nodes_scene::node_graph::viewport_gizmo::GizmoSelection;
use manifold_ui::panels::scene_setup_panel::SceneSelection;

/// The editor viewport's drawn selection and what it was built from.
pub struct ViewportSelection {
    pub scene: Arc<SceneVm>,
    pub items: Vec<SceneSelection>,
    pub selection: GizmoSelection,
}

impl ViewportSelection {
    /// The selection of `items` over `scene`, rebuilt into `cache` when
    /// either changed. `None` when the viewport has no scene.
    pub fn refresh<'a>(
        cache: &'a mut Option<Self>,
        scene: Option<&Arc<SceneVm>>,
        items: &[SceneSelection],
    ) -> Option<&'a Self> {
        let Some(scene) = scene else {
            *cache = None;
            return None;
        };
        let stale = cache
            .as_ref()
            .is_none_or(|shown| !Arc::ptr_eq(&shown.scene, scene) || shown.items != items);
        if stale {
            let selection =
                GizmoSelection::new(scene, crate::scene_gizmo::selected_targets(scene, items));
            *cache = Some(Self {
                scene: scene.clone(),
                items: items.to_vec(),
                selection,
            });
        }
        cache.as_ref()
    }

    /// The outline slots of every selected scene object.
    pub fn outline(&self, out: &mut Vec<u32>) {
        let objects = self
            .selection
            .selected
            .iter()
            .filter_map(|target| crate::scene_gizmo::selected_object(target));
        self.scene.outline_slots(objects, out);
    }
}
