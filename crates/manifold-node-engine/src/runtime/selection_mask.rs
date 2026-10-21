//! The selection mask surface of PresetRuntime (D-102): which scene objects
//! the editor outlines, and the coverage the last render drew for them.
//! Editing chrome only; the output never changes (REALTIME_3D_DESIGN.md D9).

use manifold_gpu::GpuTexture;

use super::PresetRuntime;

impl PresetRuntime {
    /// Outline the objects in render slots `slots` of the scene this
    /// runtime renders, through the camera it renders with. Empty stops it.
    /// A rebuilt graph starts without a request, so hosts set it each frame
    /// while an outline shows.
    pub fn set_selection_mask(&mut self, slots: &[u32]) {
        let Some(slot) = self.effect_nodes.first() else { return };
        for (_, instance) in &slot.node_map {
            if let Some(node) = self.graph.get_node_mut(*instance) {
                node.node.set_selection_mask(slots);
            }
        }
    }

    /// The mask the last render drew, from the node whose camera the
    /// overlay reads (`write_rendered_view`).
    pub fn selection_mask(&self) -> Option<&GpuTexture> {
        let slot = self.effect_nodes.first()?;
        slot.node_map.iter().find_map(|(_, instance)| {
            let node = &self.graph.get_node(*instance)?.node;
            node.rendered_camera()?;
            node.selection_mask()
        })
    }

    /// Outline these render slots in the second view
    /// ([`Self::set_scene_viewport`]); kept across a new viewport target.
    pub fn set_scene_viewport_selection_mask(&mut self, slots: &[u32]) {
        self.executor.set_scene_viewport_selection_mask(slots);
    }

    /// The second view's mask for its latest frame.
    pub fn scene_viewport_selection_mask(&self) -> Option<&GpuTexture> {
        self.executor.scene_viewport_selection_mask()
    }
}

impl PresetRuntime {
    /// Keep the scene's raw depth each frame for the editor's gizmo lines.
    /// A rebuilt graph starts without the request, so hosts set it each
    /// frame while gizmos show.
    pub fn set_gizmo_depth(&mut self, wanted: bool) {
        let Some(slot) = self.effect_nodes.first() else { return };
        for (_, instance) in &slot.node_map {
            if let Some(node) = self.graph.get_node_mut(*instance) {
                node.node.set_gizmo_depth(wanted);
            }
        }
    }

    /// The depth the last render kept, from the node whose camera the
    /// overlay reads.
    pub fn gizmo_depth(&self) -> Option<&GpuTexture> {
        let slot = self.effect_nodes.first()?;
        slot.node_map.iter().find_map(|(_, instance)| {
            let node = &self.graph.get_node(*instance)?.node;
            node.rendered_camera()?;
            node.gizmo_depth()
        })
    }

    /// Keep the second view's depth too ([`Self::set_scene_viewport`]).
    pub fn set_scene_viewport_gizmo_depth(&mut self, wanted: bool) {
        self.executor.set_scene_viewport_gizmo_depth(wanted);
    }

    /// The second view's depth for its latest frame.
    pub fn scene_viewport_gizmo_depth(&self) -> Option<&GpuTexture> {
        self.executor.scene_viewport_gizmo_depth()
    }
}
