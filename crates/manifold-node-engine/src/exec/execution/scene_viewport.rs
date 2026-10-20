//! The executor side of the scene viewport (REALTIME_3D_DESIGN.md D7a, D9):
//! one extra view of a scene node, rendered from its resolved inputs beside
//! the frame, never into it, and the selection mask it draws (D-102).

use super::Executor;
use crate::exec::effect_node::NodeInstanceId;

impl Executor {
    pub(crate) fn set_scene_viewport(
        &mut self,
        node: NodeInstanceId,
        config: crate::scene::scene_viewport::SceneViewportConfig,
        make: impl FnOnce() -> Option<Box<dyn crate::scene::scene_viewport::ViewportPass>>,
    ) -> bool {
        if let Some((target, current, _)) = self.scene_viewport.as_mut()
            && *target == node
        {
            *current = config;
        } else {
            let Some(mut pass) = make() else { return false; };
            pass.set_selection_mask(&self.scene_viewport_selection);
            self.scene_viewport = Some((node, config, pass));
        }
        self.scene_viewport_captured = false;
        true
    }

    pub(crate) fn clear_scene_viewport(&mut self) {
        self.scene_viewport = None;
        self.scene_viewport_captured = false;
    }

    pub(crate) fn reset_scene_viewport_state(&mut self) {
        if let Some((_, _, pass)) = self.scene_viewport.as_mut() {
            pass.clear_state();
        }
        self.scene_viewport_captured = false;
    }

    pub(crate) fn scene_viewport_texture(&self) -> Option<&manifold_gpu::GpuTexture> {
        self.scene_viewport.as_ref()
            .filter(|_| self.scene_viewport_captured)
            .and_then(|(_, _, pass)| pass.texture())
    }

    /// Outline these render slots in the viewport (D-102); empty stops it.
    pub(crate) fn set_scene_viewport_selection_mask(&mut self, slots: &[u32]) {
        if self.scene_viewport_selection == slots {
            return;
        }
        self.scene_viewport_selection.clear();
        self.scene_viewport_selection.extend_from_slice(slots);
        if let Some((_, _, pass)) = self.scene_viewport.as_mut() {
            pass.set_selection_mask(slots);
        }
    }

    pub(crate) fn scene_viewport_selection_mask(&self) -> Option<&manifold_gpu::GpuTexture> {
        self.scene_viewport.as_ref()
            .filter(|_| self.scene_viewport_captured)
            .and_then(|(_, _, pass)| pass.selection_mask())
    }

    pub(crate) fn scene_viewport_status(&self) -> Option<crate::runtime::frame_status::FrameRenderStatus> {
        self.scene_viewport.as_ref().map(|(_, _, pass)| {
            if self.scene_viewport_captured { pass.status() }
            else { crate::runtime::frame_status::FrameRenderStatus::PendingGeometry }
        })
    }

    pub(crate) fn scene_viewport_errors(&self) -> &[String] {
        self.scene_viewport.as_ref()
            .filter(|_| self.scene_viewport_captured)
            .map(|(_, _, pass)| pass.errors()).unwrap_or(&[])
    }
}
