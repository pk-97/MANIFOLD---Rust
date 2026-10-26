//! The item-id buffer (SCENE_DOCUMENT_DESIGN.md section 7.14): on a pick,
//! every draw writes its render slot plus one into a small depth-tested
//! target that covers the picked point and a few pixels around it, through
//! the camera this frame rendered with, so a click gets exactly what is
//! drawn there, a part inside a group or a water surface included (field
//! water draws its cover there). It draws only on a frame with a pick
//! waiting, reads back on the next render, and never touches the output
//! (REALTIME_3D_DESIGN.md D9).

use manifold_node_engine::scene::pick::{
    PICK_REGION, PickHit, PickRequest, nearest_slot, pick_view_proj,
};

use super::*;

/// Bytes per row of the read-back region; blits want rows 256-aligned.
const ROW_BYTES: u32 = 256;

/// One renderer's pick state and its few small resources, made on the first
/// pick.
#[derive(Default)]
pub(super) struct ItemPick {
    /// A pick waiting for the next drawn frame.
    waiting: Option<PickRequest>,
    /// The pick whose ids were copied into `readback` this frame or before.
    in_flight: Option<u64>,
    /// The answer, until taken.
    answered: Option<PickHit>,
    ids: Option<manifold_gpu::GpuTexture>,
    depth: Option<manifold_gpu::GpuTexture>,
    readback: Option<manifold_gpu::GpuBuffer>,
    pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// The cover draw that puts field water into the pick region.
    field_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
}

impl ItemPick {
    pub(super) fn request(&mut self, pick: PickRequest) {
        self.waiting = Some(pick);
    }

    pub(super) fn take(&mut self) -> Option<PickHit> {
        self.answered.take()
    }

    /// Read the last frame's ids back. The host waits for the previous frame
    /// before encoding the next (`gpu_readback.rs`), so they are written.
    pub(super) fn read_back(&mut self) {
        let (Some(id), Some(buffer)) = (self.in_flight.take(), self.readback.as_ref()) else {
            return;
        };
        let ptr = buffer
            .mapped_ptr()
            .expect("the pick readback is shared memory")
            .cast::<u8>();
        let mut ids = [0.0_f32; (PICK_REGION * PICK_REGION) as usize];
        for (row, chunk) in ids.chunks_exact_mut(PICK_REGION as usize).enumerate() {
            // SAFETY: the buffer holds PICK_REGION rows of ROW_BYTES, written
            // by a completed blit; each row starts with PICK_REGION f32s.
            let src = unsafe {
                std::slice::from_raw_parts(
                    ptr.add(row * ROW_BYTES as usize).cast::<f32>(),
                    chunk.len(),
                )
            };
            chunk.copy_from_slice(src);
        }
        self.answered = Some(PickHit {
            id,
            slot: nearest_slot(&ids),
        });
    }
}

impl RenderScene {
    /// Draw a waiting pick's item ids and copy them for the next render to
    /// read. Unjittered, through the camera the picture was rendered with.
    pub(super) fn item_pick_pass<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        draws: &[ObjectDraw<'ctx>],
        fields: &[FieldObject<'ctx>],
    ) {
        let Some(pick) = self.pick.waiting.take() else {
            return;
        };
        let gpu = ctx.gpu_encoder();
        let region = |label: &'static str, format: manifold_gpu::GpuTextureFormat| {
            gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: PICK_REGION,
                height: PICK_REGION,
                depth: 1,
                format,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                    | manifold_gpu::GpuTextureUsage::SHADER_READ,
                label,
                mip_levels: 1,
            })
        };
        let ids = self
            .pick
            .ids
            .get_or_insert_with(|| {
                region(
                    "node.render_scene pick ids",
                    manifold_gpu::GpuTextureFormat::R32Float,
                )
            })
            .clone();
        let depth = self
            .pick
            .depth
            .get_or_insert_with(|| {
                region(
                    "node.render_scene pick depth",
                    manifold_gpu::GpuTextureFormat::Depth32Float,
                )
            })
            .clone();
        let readback = self
            .pick
            .readback
            .get_or_insert_with(|| {
                gpu.device
                    .create_buffer_shared(u64::from(ROW_BYTES * PICK_REGION))
            })
            .clone();
        let pipeline = self
            .pick
            .pipeline
            .get_or_insert_with(|| {
                gpu.device.create_render_pipeline_depth(
                    include_str!("../shaders/shadow_depth.wgsl"),
                    "vs_main",
                    "fs_id",
                    manifold_gpu::GpuTextureFormat::R32Float,
                    manifold_gpu::GpuTextureFormat::Depth32Float,
                    None,
                    1,
                    "node.render_scene pick",
                )
            })
            .clone();
        let field_pipeline = (!fields.is_empty()).then(|| {
            self.pick
                .field_pipeline
                .get_or_insert_with(|| {
                    gpu.device.create_render_pipeline_depth(
                        water_cover::COVER_WGSL,
                        "vs_cover",
                        "fs_cover_id",
                        manifold_gpu::GpuTextureFormat::R32Float,
                        manifold_gpu::GpuTextureFormat::Depth32Float,
                        None,
                        1,
                        "node.render_scene pick water",
                    )
                })
                .clone()
        });
        let depth_stencil = self
            .depth_stencil
            .as_ref()
            .expect("ensured before the draw");
        let identity_stub = self.identity_instance_stub.as_ref().expect("ensured");
        let dummy = self.dummy_texture.as_ref().expect("ensured");
        let view_proj = pick_view_proj(
            pre.cam.view_proj(pre.aspect),
            pick.uv,
            pre.native_width,
            pre.native_height,
        );
        let drawn: Vec<&ObjectDraw<'ctx>> = draws.iter().filter(|d| d.instance_count > 0).collect();
        let uniforms: Vec<ShadowUniforms> = drawn
            .iter()
            .map(|d| {
                let mut uniforms = ShadowUniforms::for_draw(view_proj, d);
                uniforms.appearance[2] = (d.slot + 1) as f32;
                uniforms
            })
            .collect();
        let bindings: Vec<[GpuBinding; 6]> = drawn
            .iter()
            .zip(&uniforms)
            .map(|(d, uniforms)| {
                [
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(uniforms),
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
                        sampler: &self.material_samplers
                            [&Self::sampler_cache_key(d.sampler_descs[0])],
                    },
                ]
            })
            .collect();
        // A field whose uniforms fail is skipped: Pass B already failed the
        // frame on the same matrices.
        let cover_uniforms: Vec<(&FieldObject<'ctx>, water_march::MarchUniforms)> = fields
            .iter()
            .filter_map(|field| {
                water_cover::cover_uniforms(view_proj, PICK_REGION, PICK_REGION, field)
                    .ok()
                    .map(|uniforms| (field, uniforms))
            })
            .collect();
        let cover_bindings: Vec<[GpuBinding; 2]> = cover_uniforms
            .iter()
            .map(|(field, uniforms)| water_cover::cover_bindings(uniforms, field))
            .collect();
        let mut batch: Vec<manifold_gpu::DepthMsaaDraw> = drawn
            .iter()
            .zip(&bindings)
            .map(|(d, bindings)| {
                d.live(manifold_gpu::GpuEncoder::depth_msaa_draw(
                    &pipeline,
                    bindings,
                    d.vertex_count,
                    d.instance_count,
                ))
            })
            .collect();
        // After the meshes, so the one depth-tested pass decides between
        // meshes and water.
        if let Some(field_pipeline) = &field_pipeline {
            batch.extend(cover_bindings.iter().map(|bindings| {
                manifold_gpu::GpuEncoder::depth_msaa_draw(field_pipeline, bindings, 3, 1)
            }));
        }
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.draw_instanced_depth_batch(
            &ids,
            &depth,
            depth_stencil,
            &batch,
            manifold_gpu::GpuLoadAction::Clear,
            manifold_gpu::GpuLoadAction::Clear,
            "node.render_scene pick",
        );
        gpu.native_enc
            .copy_texture_to_buffer(&ids, &readback, PICK_REGION, PICK_REGION, ROW_BYTES);
        self.pick.in_flight = Some(pick.id);
    }
}
