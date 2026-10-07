//! Scene viewport and RT probe surface of PresetRuntime.

use super::*;

impl PresetRuntime {
    /// Render a second view from the selected scene's already-resolved inputs.
    /// The main graph, camera, simulation and render history are untouched.
    /// A pruned scene produces no viewport image; requesting a view doesn't
    /// activate a hidden physics branch.
    pub fn set_scene_viewport(
        &mut self,
        effect_id: &EffectId,
        node_id: &NodeId,
        config: crate::node_graph::scene_viewport::SceneViewportConfig,
    ) -> Result<(), crate::node_graph::scene_viewport::SceneViewportError> {
        use crate::node_graph::scene_viewport::SceneViewportError;
        let resolved = self.effect_nodes.iter()
            .find(|slot| &slot.effect_id == effect_id)
            .and_then(|slot| slot.node_map.iter().find(|(id, _)| id == node_id))
            .and_then(|(_, instance)| self.graph.get_node(*instance));
        let result = if !config.is_valid() {
            Err(SceneViewportError::InvalidConfiguration)
        } else if let Some(node) = resolved {
            Ok(node)
        } else {
            Err(SceneViewportError::TargetNotFound)
        };
        match result {
            Ok(node) => {
                if self.executor.set_scene_viewport(node.id, config, || node.node.viewport_pass()) {
                    Ok(())
                } else {
                    self.clear_scene_viewport();
                    Err(SceneViewportError::NotSceneRenderer)
                }
            }
            Err(error) => {
                self.clear_scene_viewport();
                Err(error)
            }
        }
    }

    /// Single-generator counterpart of [`Self::set_scene_viewport`].
    pub fn set_scene_viewport_watched(
        &mut self,
        node_id: &NodeId,
        config: crate::node_graph::scene_viewport::SceneViewportConfig,
    ) -> Result<(), crate::node_graph::scene_viewport::SceneViewportError> {
        let Some(effect_id) = self.effect_nodes.first().map(|slot| slot.effect_id.clone()) else {
            self.clear_scene_viewport();
            return Err(crate::node_graph::scene_viewport::SceneViewportError::TargetNotFound);
        };
        self.set_scene_viewport(&effect_id, node_id, config)
    }

    pub fn clear_scene_viewport(&mut self) {
        self.executor.clear_scene_viewport();
    }

    /// Valid for the latest evaluated frame only. No capture/readback or graph
    /// evaluation occurs here; the host copies this into its preview surface.
    pub fn scene_viewport_texture(&self) -> Option<&GpuTexture> {
        self.executor.scene_viewport_texture()
    }

    pub fn scene_viewport_status(&self) -> Option<crate::frame_status::FrameRenderStatus> {
        self.executor.scene_viewport_status()
    }

    pub fn scene_viewport_errors(&self) -> &[String] {
        self.executor.scene_viewport_errors()
    }

    #[cfg(feature = "gpu-proofs")]
    pub fn rt_probe_rays(
        &self,
        device: &manifold_gpu::GpuDevice,
        encoder: &mut manifold_gpu::GpuEncoder,
        rays: &[manifold_gpu::raytrace::DebugRayQueryRay],
    ) -> Option<manifold_gpu::GpuBuffer> {
        self.graph.nodes().find_map(|node| node.node.rt_probe_rays(device, encoder, rays))
    }
}
