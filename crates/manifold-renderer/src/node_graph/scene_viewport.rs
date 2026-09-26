//! Isolated render-only viewport execution for an already-resolved scene.
//!
//! The viewport owns a renderer and its output bindings, but borrows the
//! source node's resolved inputs and the current native Metal encoder. It
//! never runs the graph executor and never shares the source node's output
//! storage or state store.

use std::fmt;

use manifold_gpu::{GpuTexture, GpuTextureFormat};

use crate::frame_status::{FrameRenderFailure, FrameRenderStatus};
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::backend::Backend;
use crate::node_graph::bindings::{NodeOutputs, Slot};
use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, ParamValues};
use crate::node_graph::execution_plan::ResourceId;
use crate::node_graph::metal_backend::MetalBackend;
use crate::node_graph::primitives::RenderScene;
use crate::node_graph::viewport_camera::ViewportCamera;
use crate::render_target::RenderTarget;

/// The camera and canvas used by one render-only viewport pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneViewportConfig {
    pub camera: ViewportCamera,
    pub width: u32,
    pub height: u32,
}

impl SceneViewportConfig {
    /// Reject dimensions and camera values that could make the native pass
    /// issue an invalid allocation or projection.
    pub fn is_valid(self) -> bool {
        let camera = self.camera;
        self.width > 0
            && self.width <= 4096
            && self.height > 0
            && self.height <= 4096
            && camera.target.iter().all(|value| value.is_finite())
            && camera.yaw.is_finite()
            && camera.pitch.is_finite()
            && camera.distance.is_finite()
            && camera.distance > 0.0
            && camera.fov_y.is_finite()
            && camera.fov_y > 0.0
            && camera.fov_y < std::f32::consts::PI
            && camera.near.is_finite()
            && camera.near > 0.0
            && camera.far.is_finite()
            && camera.near < camera.far
    }
}

/// Errors reported by the viewport host before or during target selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneViewportError {
    TargetNotFound,
    NotSceneRenderer,
    InvalidConfiguration,
}

impl fmt::Display for SceneViewportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetNotFound => f.write_str("viewport target not found"),
            Self::NotSceneRenderer => f.write_str("viewport target is not a scene renderer"),
            Self::InvalidConfiguration => f.write_str("viewport configuration is invalid"),
        }
    }
}

impl std::error::Error for SceneViewportError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputLayout {
    port: &'static str,
    width: u32,
    height: u32,
    format: GpuTextureFormat,
}

/// A persistent, render-only `node.render_scene` pass.
pub(crate) struct SceneViewportPass {
    renderer: RenderScene,
    output_backend: Option<MetalBackend>,
    output_bindings: Vec<(&'static str, Slot)>,
    output_layout: Vec<OutputLayout>,
    required_outputs: Vec<&'static str>,
    color_slot: Option<Slot>,
    errors: Vec<String>,
    status: FrameRenderStatus,
    last_attempt_valid: bool,
    pending_scalar_writes: Vec<(Slot, crate::node_graph::ParamValue)>,
    pending_camera_writes: Vec<(Slot, crate::node_graph::camera::Camera)>,
    pending_light_writes: Vec<(Slot, crate::node_graph::light::Light)>,
    pending_material_writes: Vec<(Slot, crate::node_graph::material::Material)>,
    pending_transform_writes: Vec<(Slot, crate::node_graph::transform::Transform)>,
    pending_atmosphere_writes: Vec<(Slot, crate::node_graph::atmosphere::Atmosphere)>,
    pending_render_mode_writes: Vec<(Slot, crate::node_graph::render_mode::RenderMode)>,
    pending_object_writes: Vec<(Slot, crate::node_graph::scene_object::SceneObject)>,
}

impl SceneViewportPass {
    pub(crate) fn new() -> Self {
        Self {
            renderer: RenderScene::new(),
            output_backend: None,
            output_bindings: Vec::new(),
            output_layout: Vec::new(),
            required_outputs: Vec::new(),
            color_slot: None,
            errors: Vec::new(),
            status: FrameRenderStatus::Complete,
            last_attempt_valid: false,
            pending_scalar_writes: Vec::new(),
            pending_camera_writes: Vec::new(),
            pending_light_writes: Vec::new(),
            pending_material_writes: Vec::new(),
            pending_transform_writes: Vec::new(),
            pending_atmosphere_writes: Vec::new(),
            pending_render_mode_writes: Vec::new(),
            pending_object_writes: Vec::new(),
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
        if let Err(error) = self.ensure_outputs(device, config, source_params) {
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

        let Some(output_backend) = self.output_backend.as_ref() else {
            self.errors
                .push("viewport output storage is unavailable".to_string());
            self.status = FrameRenderStatus::Failed(FrameRenderFailure::SurfaceAllocation);
            return;
        };
        let inputs = source_inputs.with_camera_override("camera", config.camera.to_camera());
        let outputs = NodeOutputs::new(
            &self.output_bindings,
            output_backend,
            &mut self.pending_scalar_writes,
            &mut self.pending_camera_writes,
            &mut self.pending_light_writes,
            &mut self.pending_material_writes,
            &mut self.pending_transform_writes,
            &mut self.pending_atmosphere_writes,
            &mut self.pending_render_mode_writes,
            &mut self.pending_object_writes,
        );
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
        let backend = self.output_backend.as_ref()?;
        backend.texture_2d(self.color_slot?)
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

    fn ensure_outputs(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        config: SceneViewportConfig,
        params: &ParamValues,
    ) -> Result<(), String> {
        self.required_outputs.clear();
        self.required_outputs.push("color");
        for &port in self.renderer.force_consumed_outputs(params) {
            if !self.required_outputs.contains(&port) {
                self.required_outputs.push(port);
            }
        }

        let mut changed = self.output_backend.is_none()
            || self.output_layout.len() != self.required_outputs.len();
        if !changed {
            for (index, &port) in self.required_outputs.iter().enumerate() {
                let (width, height, format) = self.output_shape(config, params, port);
                let old = self.output_layout[index];
                if old
                    != (OutputLayout {
                        port,
                        width,
                        height,
                        format,
                    })
                {
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            return Ok(());
        }

        let mut backend = MetalBackend::without_device(
            config.width,
            config.height,
            GpuTextureFormat::Rgba16Float,
        );
        // Allocation may fail partway through a resized layout. Do not reuse
        // its partial bindings against the previous backend on the next try.
        self.output_backend = None;
        self.output_bindings.clear();
        self.output_layout.clear();
        self.color_slot = None;
        for (index, &port) in self.required_outputs.iter().enumerate() {
            let (width, height, format) = self.output_shape(config, params, port);
            let target =
                RenderTarget::try_new(device, width, height, format, "scene viewport output")?;
            let slot = backend.pre_bind_texture_2d(ResourceId(index as u32), target);
            self.output_bindings.push((port, slot));
            self.output_layout.push(OutputLayout {
                port,
                width,
                height,
                format,
            });
            if port == "color" {
                self.color_slot = Some(slot);
            }
        }
        self.output_backend = Some(backend);
        Ok(())
    }

    fn output_shape(
        &self,
        config: SceneViewportConfig,
        params: &ParamValues,
        port: &'static str,
    ) -> (u32, u32, GpuTextureFormat) {
        let (num, den) = self
            .renderer
            .output_canvas_scale(port, params)
            .unwrap_or((1, 1));
        let width = scaled_dimension(config.width, num, den);
        let height = scaled_dimension(config.height, num, den);
        let format = self
            .renderer
            .output_format(port)
            .unwrap_or(GpuTextureFormat::Rgba16Float);
        (width, height, format)
    }
}

fn scaled_dimension(value: u32, numerator: u32, denominator: u32) -> u32 {
    if numerator == 0 || denominator == 0 {
        return 1;
    }
    // Match execution::resolve_dims and RenderScene's render-scale policy.
    let scaled = u64::from(value) * u64::from(numerator) / u64::from(denominator);
    scaled.clamp(1, u64::from(u32::MAX)) as u32
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
            crate::node_graph::ParamValue::Bool(true),
        );
        params.insert("rt_denoise_feed".into(), crate::node_graph::ParamValue::Bool(true));
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
