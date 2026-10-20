//! The scene viewport's content side (REALTIME_3D_DESIGN.md D7a,
//! D9): one request, owned by the graph editor's viewport or the output
//! preview, rendered beside the output and copied into its own bridge sized
//! from the request. The output and the workspace preview bridge never see
//! it.

use std::sync::Arc;

use manifold_compositor::generator_renderer::GeneratorRenderer;
use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_node_engine::scene::scene_viewport::SceneViewportHostError;

use super::ContentPipeline;
use crate::scene_viewport::{SceneViewportFrames, SceneViewportObservations, SceneViewportRequest, ViewportHost};

/// The request, what each output surface's frame was rendered with, and the
/// bridge its pixels travel through.
#[derive(Default)]
pub(crate) struct SceneViewportCapture {
    pub request: Option<Arc<SceneViewportRequest>>,
    pub observations: SceneViewportObservations,
    #[cfg(target_os = "macos")]
    pub textures: [Option<manifold_gpu::GpuTexture>; crate::shared_texture::SURFACE_COUNT],
    #[cfg(target_os = "macos")]
    pub bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
}

/// One frame's copy into the viewport bridge: the rendered view (`None`
/// clears the surface) and the bridge surface it lands on.
#[cfg(target_os = "macos")]
pub(super) type ViewportCopy = (Option<manifold_gpu::GpuTexture>, manifold_gpu::GpuTexture);

impl SceneViewportCapture {
    /// Record a generator scene's frame for surface `slot`; what to copy
    /// when this frame has a request for one.
    #[cfg(target_os = "macos")]
    pub(super) fn observe_generator(
        &mut self,
        generators: Option<&GeneratorRenderer>,
        slot: usize,
        frame: u64,
    ) -> Option<ViewportCopy> {
        let request = self.request.clone()?;
        let Some(manifold_core::GraphTarget::Generator(layer)) = request.target.host_target() else { return None };
        self.observations.scratch.clear();
        if let Some(generators) = generators {
            generators.write_scene_viewport_fluid_domains(layer, &mut self.observations.scratch);
        }
        let status = generators.map_or(Err(SceneViewportHostError::MissingRuntime), |g| g.scene_viewport_status(layer));
        let texture = generators.and_then(|g| g.scene_viewport_texture(layer)).cloned();
        self.record(request.clone(), slot, frame, status, texture)
    }

    /// The effect-scene counterpart of [`Self::observe_generator`].
    #[cfg(target_os = "macos")]
    pub(super) fn observe_effect(
        &mut self,
        compositor: &dyn manifold_compositor::compositor::Compositor,
        slot: usize,
        frame: u64,
    ) -> Option<ViewportCopy> {
        let request = self.request.clone().filter(|request| matches!(request.target, manifold_core::GraphTarget::Effect(_)))?;
        self.observations.scratch.clear();
        compositor.write_scene_viewport_fluid_domains(&mut self.observations.scratch);
        self.record(request, slot, frame, compositor.scene_viewport_status(), compositor.scene_viewport_texture().cloned())
    }

    #[cfg(target_os = "macos")]
    fn record(
        &mut self,
        request: Arc<SceneViewportRequest>,
        slot: usize,
        frame: u64,
        status: Result<FrameRenderStatus, SceneViewportHostError>,
        texture: Option<manifold_gpu::GpuTexture>,
    ) -> Option<ViewportCopy> {
        let generation = self.bridge.as_ref().map_or(0, |bridge| bridge.generation());
        self.observations.record(slot, frame, generation, request, status);
        Some((texture, self.textures[slot].clone()?))
    }

    /// The bridge takes the owning host's view size (D7a). Both sides
    /// re-import on the generation bump.
    #[cfg(target_os = "macos")]
    fn fit(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        let Some(bridge) = self.bridge.as_ref() else { return };
        let (width, height) = (width.max(1), height.max(1));
        if bridge.width() == width && bridge.height() == height {
            return;
        }
        bridge.resize(width, height);
        self.textures = std::array::from_fn(|i| Some(unsafe { bridge.import_texture_native(device, i) }));
        self.observations.frames.fill(None);
    }
}

impl ContentPipeline {
    /// Install the IOSurface textures + bridge the viewport's frames travel
    /// through.
    #[cfg(target_os = "macos")]
    pub fn set_scene_viewport_textures(
        &mut self,
        textures: [manifold_gpu::GpuTexture; crate::shared_texture::SURFACE_COUNT],
        bridge: Arc<crate::shared_texture::SharedTextureBridge>,
    ) {
        self.scene_viewport.textures = textures.map(Some);
        self.scene_viewport.bridge = Some(bridge);
    }

    pub fn set_scene_viewport_request(&mut self, request: Option<Arc<SceneViewportRequest>>) {
        if request.is_none() {
            self.scene_viewport.observations.frames.fill(None);
        }
        #[cfg(target_os = "macos")]
        if let (Some(request), Some(device)) = (request.as_ref(), self.native_device.as_ref()) {
            self.scene_viewport.fit(device, request.config.width, request.config.height);
        }
        self.scene_viewport.request = request;
    }

    /// The host owning the viewport request, if any.
    pub fn scene_viewport_host(&self) -> Option<ViewportHost> {
        self.scene_viewport.request.as_ref().map(|request| request.host)
    }

    pub fn scene_viewport_frames(&self) -> &SceneViewportFrames {
        &self.scene_viewport.observations.frames
    }

    /// Copy one frame's view into the viewport bridge, display-mapped like
    /// the node preview's colour path.
    #[cfg(target_os = "macos")]
    pub(super) fn copy_scene_viewport(
        enc: &mut manifold_gpu::GpuEncoder,
        (source, target): ViewportCopy,
        pipelines: &super::PreviewPipelines<'_>,
        sampler: Option<&manifold_gpu::GpuSampler>,
    ) {
        match source {
            Some(source) => Self::update_node_preview(enc, &source, Some(&target), false, Default::default(), pipelines, sampler),
            None => enc.clear_texture(&target, 0.0, 0.0, 0.0, 1.0),
        }
    }
}
