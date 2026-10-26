//! The selection mask (D-102): the selected objects' coverage through the
//! camera this frame rendered with, into an `R8Unorm` target the editor
//! outlines over the picture. It draws only while slots are set and never
//! touches the output (REALTIME_3D_DESIGN.md D9).

use super::*;

/// The mask's state on one renderer.
#[derive(Default)]
pub(super) struct SelectionMask {
    /// Render slots the mask draws, kept so a steady selection allocates
    /// nothing. Empty draws no mask.
    slots: Vec<u32>,
    /// The target, at the scene's native render size; kept while slots are set.
    texture: Option<manifold_gpu::GpuTexture>,
    pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// This frame drew the mask; cleared at the start of every evaluate.
    pub(super) drawn: bool,
    /// Dirty-cache key for the last drawn mask.
    cache_key: Option<u64>,
}

impl SelectionMask {
    pub(super) fn set(&mut self, slots: &[u32]) {
        if self.slots != slots {
            self.cache_key = None;
        }
        self.slots.clear();
        self.slots.extend_from_slice(slots);
        if slots.is_empty() {
            self.texture = None;
        }
    }

    pub(super) fn reset(&mut self) {
        self.cache_key = None;
    }

    /// The mask this frame drew, if it drew one.
    pub(super) fn texture(&self) -> Option<&manifold_gpu::GpuTexture> {
        self.texture.as_ref().filter(|_| self.drawn)
    }
}

impl RenderScene {
    /// Draw every draw whose render slot is selected into the mask, sized
    /// to the native render. Runs after the main draw; one pass per selected draw,
    /// the first clearing, so a group's parts union.
    pub(super) fn selection_mask_pass<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        draws: &[ObjectDraw<'ctx>],
    ) {
        if self.selection.slots.is_empty() {
            return;
        }
        let (width, height) = (pre.native_width.max(1), pre.native_height.max(1));
        let rebuild_epoch = ctx.rebuild_epoch;
        let gpu = ctx.gpu_encoder();
        let recreated = self
            .selection
            .texture
            .as_ref()
            .is_none_or(|mask| mask.width != width || mask.height != height);
        if recreated {
            self.selection.texture =
                Some(gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                    width,
                    height,
                    depth: 1,
                    format: manifold_gpu::GpuTextureFormat::R8Unorm,
                    dimension: manifold_gpu::GpuTextureDimension::D2,
                    usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                        | manifold_gpu::GpuTextureUsage::SHADER_READ,
                    label: "node.render_scene selection mask",
                    mip_levels: 1,
                }));
            self.selection.cache_key = None;
        }
        let pipeline = self
            .selection
            .pipeline
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
        let mask = self.selection.texture.as_ref().expect("ensured above");
        let identity_stub = self.identity_instance_stub.as_ref().expect("ensured");
        let dummy = self.dummy_texture.as_ref().expect("ensured");
        // Unjittered: the editor projects through the same camera.
        let view_proj = pre.cam.view_proj(pre.aspect);
        let selected_draws: Vec<&ObjectDraw<'ctx>> = draws
            .iter()
            .filter(|d| d.instance_count > 0 && self.selection.slots.contains(&d.slot))
            .collect();
        use std::hash::{Hash, Hasher};
        let mut hasher = ahash::AHasher::default();
        hasher.write_u32(width);
        hasher.write_u32(height);
        hasher.write_u64(rebuild_epoch);
        hasher.write(bytemuck::bytes_of(&view_proj));
        self.selection.slots.hash(&mut hasher);
        hasher.write_usize(selected_draws.len());
        for d in &selected_draws {
            hasher.write_u32(d.slot);
            hasher.write(bytemuck::bytes_of(&ShadowUniforms::for_draw(view_proj, d)));
            d.vertices_content.hash(&mut hasher);
            d.indices_content.hash(&mut hasher);
            d.instances_content.hash(&mut hasher);
            d.weights_content.hash(&mut hasher);
            d.rt_texture_content[0].hash(&mut hasher);
            hasher.write_u32(Self::sampler_cache_key(d.sampler_descs[0]));
            hasher.write_u32(d.vertex_count);
            hasher.write_u32(d.instance_count);
            hasher.write_u8(u8::from(d.live_args.is_some()));
        }
        let selection_key = hasher.finish();
        let cacheable = selected_draws.iter().all(|d| {
            d.geometry_content_known
                && d.appearance_content_known
                && d.live_args.is_none()
                && (d.base_color_map.is_none() || d.rt_texture_content[0].is_some())
        });
        if cacheable && self.selection.cache_key == Some(selection_key) {
            self.selection.drawn = true;
            return;
        }
        self.selection.cache_key = cacheable.then_some(selection_key);

        if selected_draws.is_empty() {
            // Nothing selected drew this frame: an empty mask, not last frame's.
            gpu.native_enc.clear_texture(mask, 0.0, 0.0, 0.0, 0.0);
        } else {
            let mut load = manifold_gpu::GpuLoadAction::Clear;
            for d in selected_draws {
                let uniforms = ShadowUniforms::for_draw(view_proj, d);
                let bindings = [
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: d.vertices,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: d.instances.unwrap_or(identity_stub),
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: d.weights.unwrap_or(d.vertices),
                        offset: 0,
                    },
                    GpuBinding::Texture {
                        binding: 4,
                        texture: d.base_color_map.unwrap_or(dummy),
                    },
                    GpuBinding::Sampler {
                        binding: 5,
                        sampler: &self.material_samplers[&Self::sampler_cache_key(d.sampler_descs[0])],
                    },
                ];
                gpu.native_enc.draw_instanced(
                    &pipeline,
                    mask,
                    &bindings,
                    d.draw_count(),
                    load,
                    "node.render_scene selection mask",
                );
                load = manifold_gpu::GpuLoadAction::Load;
            }
        }
        self.selection.drawn = true;
    }
}
