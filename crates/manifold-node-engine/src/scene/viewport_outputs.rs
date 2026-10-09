//! Persistent output storage for a render-only viewport pass.
use super::scene_viewport::SceneViewportConfig;
use crate::bindings::{NodeOutputs, Slot};
use crate::exec::backend::Backend;
use crate::exec::effect_node::{EffectNode, ParamValues};
use crate::exec::execution_plan::ResourceId;
use crate::exec::metal_backend::MetalBackend;
use crate::gpu::render_target::RenderTarget;
use manifold_gpu::{GpuTexture, GpuTextureFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputLayout {
    port: &'static str,
    width: u32,
    height: u32,
    format: GpuTextureFormat,
}

/// Engine-owned viewport targets and reusable output scratch.
#[derive(Default)]
pub struct ViewportOutputs {
    output_backend: Option<MetalBackend>,
    output_bindings: Vec<(&'static str, Slot)>,
    output_layout: Vec<OutputLayout>,
    required_outputs: Vec<&'static str>,
    color_slot: Option<Slot>,
    pending_scalar_writes: Vec<(Slot, crate::parameters::ParamValue)>,
    pending_camera_writes: Vec<(Slot, crate::scene::camera::Camera)>,
    pending_light_writes: Vec<(Slot, crate::scene::light::Light)>,
    pending_material_writes: Vec<(Slot, crate::scene::material::Material)>,
    pending_transform_writes: Vec<(Slot, crate::scene::transform::Transform)>,
    pending_atmosphere_writes: Vec<(Slot, crate::scene::atmosphere::Atmosphere)>,
    pending_render_mode_writes: Vec<(Slot, crate::scene::render_mode::RenderMode)>,
    pending_object_writes: Vec<(Slot, crate::scene::scene_object::SceneObject)>,
}

impl ViewportOutputs {
    pub fn ensure(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        config: SceneViewportConfig,
        params: &ParamValues,
        renderer: &dyn EffectNode,
    ) -> Result<(), String> {
        self.required_outputs.clear();
        self.required_outputs.push("color");
        for &port in renderer.force_consumed_outputs(params) {
            if !self.required_outputs.contains(&port) {
                self.required_outputs.push(port);
            }
        }

        let mut changed = self.output_backend.is_none()
            || self.output_layout.len() != self.required_outputs.len();
        if !changed {
            for (index, &port) in self.required_outputs.iter().enumerate() {
                let (width, height, format) = Self::output_shape(renderer, config, params, port);
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
            let (width, height, format) = Self::output_shape(renderer, config, params, port);
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

    manifold_core::testkit_visible! {
    pub(crate) fn output_shape(
        renderer: &dyn EffectNode,
        config: SceneViewportConfig,
        params: &ParamValues,
        port: &'static str,
    ) -> (u32, u32, GpuTextureFormat) {
        let (num, den) = renderer
            .output_canvas_scale(port, params)
            .unwrap_or((1, 1));
        let width = scaled_dimension(config.width, num, den);
        let height = scaled_dimension(config.height, num, den);
        let format = renderer
            .output_format(port)
            .unwrap_or(GpuTextureFormat::Rgba16Float);
        (width, height, format)
    }
    }
    /// Bind the prepared targets for one pass without allocating scratch.
    pub fn outputs(&mut self) -> Option<NodeOutputs<'_>> {
        let backend = self.output_backend.as_ref()?;
        Some(NodeOutputs::new(
            &self.output_bindings,
            backend,
            &mut self.pending_scalar_writes,
            &mut self.pending_camera_writes,
            &mut self.pending_light_writes,
            &mut self.pending_material_writes,
            &mut self.pending_transform_writes,
            &mut self.pending_atmosphere_writes,
            &mut self.pending_render_mode_writes,
            &mut self.pending_object_writes,
        ))
    }
    /// Color target from the last successfully prepared layout.
    pub fn texture(&self) -> Option<&GpuTexture> {
        self.output_backend.as_ref()?.texture_2d(self.color_slot?)
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
