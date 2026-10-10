//! Render-only viewport pass owned by the scene renderer.

use manifold_gpu::GpuTexture;
#[cfg(test)]
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::scene::viewport_outputs::ViewportOutputs;
use manifold_node_engine::runtime::frame_status::{FrameRenderFailure, FrameRenderStatus};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext};
use manifold_node_engine::scene::scene_viewport::{SceneViewportConfig, SceneViewportError, ViewportPass};
use super::RenderScene;
#[cfg(test)]
use manifold_node_engine::exec::effect_node::ParamValues;
#[cfg(test)]
use manifold_node_engine::scene::viewport_camera::ViewportCamera;

/// A persistent, render-only `node.render_scene` pass.
pub(crate) struct SceneViewportPass {
    renderer: RenderScene,
    outputs: ViewportOutputs,
    errors: Vec<String>,
    status: FrameRenderStatus,
    last_attempt_valid: bool,
}

impl SceneViewportPass {
    pub(crate) fn new() -> Self {
        Self {
            renderer: RenderScene::new(),
            outputs: ViewportOutputs::default(),
            errors: Vec::new(),
            status: FrameRenderStatus::Complete,
            last_attempt_valid: false,
        }
    }

    /// Render the resolved scene through the viewport camera. The source
    /// context is used only as a read view plus the shared native encoder.
    pub(crate) fn render(
        &mut self,
        source: &mut EffectNodeContext<'_, '_>,
        config: SceneViewportConfig,
    ) {
        self.errors.clear();
        self.status = FrameRenderStatus::Complete;
        self.last_attempt_valid = false;

        if !config.is_valid() {
            self.errors
                .push(SceneViewportError::InvalidConfiguration.to_string());
            self.status = FrameRenderStatus::Failed(FrameRenderFailure::SurfaceAllocation);
            return;
        }

        self.renderer.reconfigure(source.params);
        let source_time = source.time;
        let source_params = source.params;
        let source_inputs = source.inputs;
        let source_node_id = source.node_id;
        let source_owner_key = source.owner_key;
        let source_rebuild_epoch = source.rebuild_epoch;
        let source_rt_quality = source.rt_quality;
        let source_layer_skin_registry = source.layer_skin_registry;

        let Some(parent_gpu) = source.gpu.as_deref_mut() else {
            self.errors
                .push("viewport render requires a native Metal encoder".to_string());
            self.status = FrameRenderStatus::Failed(FrameRenderFailure::SurfaceAllocation);
            return;
        };

        let device = parent_gpu.device;
        if let Err(error) = self.outputs.ensure(device, config, source_params, &self.renderer) {
            self.errors.push(error);
            self.status = FrameRenderStatus::Failed(FrameRenderFailure::SurfaceAllocation);
            return;
        }

        let mut viewport_gpu = GpuEncoder::new(&mut *parent_gpu.native_enc, parent_gpu.device);
        viewport_gpu.pool = parent_gpu.pool;
        viewport_gpu.uniform_arena = parent_gpu.uniform_arena;
        viewport_gpu.chunking_enabled = parent_gpu.chunking_enabled;
        viewport_gpu.preparing = parent_gpu.preparing;
        viewport_gpu.audio_visuals = parent_gpu.audio_visuals;

        let Some(outputs) = self.outputs.outputs() else {
            self.errors.push("viewport output storage is unavailable".to_string());
            self.status = FrameRenderStatus::Failed(FrameRenderFailure::SurfaceAllocation);
            return;
        };
        let inputs = source_inputs.with_camera_override("camera", config.camera.to_camera());
        let pending = {
            let mut viewport_ctx = EffectNodeContext::with_state(
                source_time,
                source_params,
                inputs,
                outputs,
                Some(&mut viewport_gpu),
                None,
                source_node_id,
                source_owner_key,
                source_rebuild_epoch,
                source_rt_quality,
                source_layer_skin_registry,
            )
            .with_errors(&mut self.errors);

            self.renderer.evaluate(&mut viewport_ctx);
            viewport_ctx.outputs_pending
        };

        self.status = viewport_gpu.frame_status();
        if pending || self.renderer.warmup_pending() {
            self.status.merge(FrameRenderStatus::PendingGeometry);
        }
        if !self.errors.is_empty() {
            self.status.merge(FrameRenderStatus::Failed(
                FrameRenderFailure::InvalidGeometry,
            ));
        }
        self.last_attempt_valid = self.status == FrameRenderStatus::Complete;
    }

    /// The successful color output of the most recent pass.
    pub(crate) fn texture(&self) -> Option<&GpuTexture> {
        if !self.last_attempt_valid {
            return None;
        }
        self.outputs.texture()
    }

    pub(crate) fn status(&self) -> FrameRenderStatus {
        self.status
    }

    pub(crate) fn errors(&self) -> &[String] {
        &self.errors
    }

    pub(crate) fn clear_state(&mut self) {
        self.renderer.clear_state();
        self.errors.clear();
        self.status = FrameRenderStatus::PendingGeometry;
        self.last_attempt_valid = false;
    }

    #[cfg(test)]
    fn output_shape(&self, config: SceneViewportConfig, params: &ParamValues, port: &'static str) -> (u32, u32, GpuTextureFormat) {
        ViewportOutputs::output_shape(&self.renderer, config, params, port)
    }
}

impl ViewportPass for SceneViewportPass {
    fn render(&mut self, ctx: &mut EffectNodeContext<'_, '_>, config: SceneViewportConfig) {
        SceneViewportPass::render(self, ctx, config);
    }

    fn texture(&self) -> Option<&GpuTexture> { SceneViewportPass::texture(self) }
    fn status(&self) -> FrameRenderStatus { SceneViewportPass::status(self) }
    fn errors(&self) -> &[String] { SceneViewportPass::errors(self) }
    fn clear_state(&mut self) { SceneViewportPass::clear_state(self); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_rejects_invalid_canvas_and_camera() {
        let config = SceneViewportConfig {
            camera: ViewportCamera::default(),
            width: 320,
            height: 200,
        };
        assert!(config.is_valid());
        for (width, height) in [(0, 200), (320, 0), (4097, 200), (320, 4097)] {
            assert!(
                !SceneViewportConfig {
                    width,
                    height,
                    ..config
                }
                .is_valid()
            );
        }
        for camera in [
            ViewportCamera {
                target: [0.0, f32::NAN, 0.0],
                ..config.camera
            },
            ViewportCamera {
                yaw: f32::INFINITY,
                ..config.camera
            },
            ViewportCamera {
                pitch: f32::NAN,
                ..config.camera
            },
            ViewportCamera {
                distance: 0.0,
                ..config.camera
            },
            ViewportCamera {
                fov_y: std::f32::consts::PI,
                ..config.camera
            },
            ViewportCamera {
                near: 0.0,
                ..config.camera
            },
            ViewportCamera {
                far: config.camera.near,
                ..config.camera
            },
        ] {
            assert!(!SceneViewportConfig { camera, ..config }.is_valid());
        }
    }

    #[test]
    fn viewport_buffers_follow_scene_formats_and_temporal_resolution() {
        let pass = SceneViewportPass::new();
        let config = SceneViewportConfig {
            camera: ViewportCamera::default(),
            width: 321,
            height: 201,
        };
        let mut params = ParamValues::default();
        assert_eq!(
            pass.output_shape(config, &params, "color"),
            (321, 201, GpuTextureFormat::Rgba16Float)
        );
        assert_eq!(
            pass.output_shape(config, &params, "depth"),
            (321, 201, GpuTextureFormat::R32Float)
        );
        params.insert(
            "temporal_upscale".into(),
            manifold_node_engine::parameters::ParamValue::Bool(true),
        );
        params.insert("rt_denoise_feed".into(), manifold_node_engine::parameters::ParamValue::Bool(true));
        let scale = pass.renderer.output_canvas_scale("depth", &params).unwrap();
        let dimensions = (321 * scale.0 / scale.1, 201 * scale.0 / scale.1);
        for (port, format) in [
            ("depth", GpuTextureFormat::R32Float),
            ("velocity", GpuTextureFormat::Rg16Float),
            ("normals", GpuTextureFormat::Rgba16Float),
            ("roughness", GpuTextureFormat::R16Float),
        ] {
            assert!(
                pass.renderer
                    .force_consumed_outputs(&params)
                    .contains(&port)
            );
            assert_eq!(
                pass.output_shape(config, &params, port),
                (dimensions.0, dimensions.1, format)
            );
        }
        assert_eq!(
            pass.output_shape(config, &params, "color"),
            (321, 201, GpuTextureFormat::Rgba16Float)
        );
    }
}
