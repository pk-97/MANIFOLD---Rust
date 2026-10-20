//! The selection mask (D-102): the selected objects' coverage through the
//! camera this frame rendered with, into an `R8Unorm` target the editor
//! outlines over the picture. It draws only while slots are set and never
//! touches the output (REALTIME_3D_DESIGN.md D9).

use super::*;

impl RenderScene {
    /// Draw every draw whose render slot is selected into the mask, sized
    /// to the output. Runs after the main draw; one pass per selected draw,
    /// the first clearing, so a group's parts union.
    pub(super) fn selection_mask_pass<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        draws: &[ObjectDraw<'ctx>],
    ) {
        if self.selection_slots.is_empty() {
            return;
        }
        let (width, height) = (pre.native_width.max(1), pre.native_height.max(1));
        let gpu = ctx.gpu_encoder();
        if self.selection_mask.as_ref().is_none_or(|mask| mask.width != width || mask.height != height) {
            self.selection_mask = Some(gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                width,
                height,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::R8Unorm,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET | manifold_gpu::GpuTextureUsage::SHADER_READ,
                label: "node.render_scene selection mask",
                mip_levels: 1,
            }));
        }
        let pipeline = self
            .selection_mask_pipeline
            .get_or_insert_with(|| {
                gpu.device.create_render_pipeline(
                    include_str!("../shaders/shadow_depth.wgsl"),
                    "vs_main",
                    "fs_mask",
                    manifold_gpu::GpuTextureFormat::R8Unorm,
                    None,
                    "node.render_scene selection mask",
                )
            })
            .clone();
        let mask = self.selection_mask.as_ref().expect("ensured above");
        let identity_stub = self.identity_instance_stub.as_ref().expect("ensured");
        let dummy = self.dummy_texture.as_ref().expect("ensured");
        // Unjittered: the editor projects through the same camera.
        let view_proj = pre.cam.view_proj(pre.aspect);
        let mut load = manifold_gpu::GpuLoadAction::Clear;
        for d in draws.iter().filter(|d| d.instance_count > 0 && self.selection_slots.contains(&d.slot)) {
            let uniforms = ShadowUniforms::for_draw(view_proj, d);
            let bindings = [
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: d.vertices, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: d.instances.unwrap_or(identity_stub), offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: d.weights.unwrap_or(d.vertices), offset: 0 },
                GpuBinding::Texture { binding: 4, texture: d.base_color_map.unwrap_or(dummy) },
                GpuBinding::Sampler { binding: 5, sampler: &self.material_samplers[&Self::sampler_cache_key(d.sampler_descs[0])] },
            ];
            gpu.native_enc.draw_instanced(&pipeline, mask, &bindings, d.draw_count(), load, "node.render_scene selection mask");
            load = manifold_gpu::GpuLoadAction::Load;
        }
        if load == manifold_gpu::GpuLoadAction::Clear {
            // Nothing selected drew this frame: an empty mask, not last frame's.
            gpu.native_enc.clear_texture(mask, 0.0, 0.0, 0.0, 0.0);
        }
        self.selection_mask_drawn = true;
    }
}
