//! Optional geometry-based subsurface pass; see SUBSURFACE_MATERIAL_DESIGN.md.
use super::*;
use crate::node_graph::material::SubsurfaceMode;
use manifold_gpu::raytrace::{SubsurfaceMaterial, SubsurfaceParams};

#[derive(Default)]
pub(super) struct SubsurfacePass {
    pub(super) output: Option<manifold_gpu::GpuTexture>,
    raw: Option<manifold_gpu::GpuTexture>,
    guide: Option<manifold_gpu::GpuTexture>,
    filter_scratch: Option<manifold_gpu::GpuTexture>,
    history: [Option<manifold_gpu::GpuTexture>; 2],
    history_count: [Option<manifold_gpu::GpuTexture>; 2],
    history_ping: usize,
    needs_reset: bool,
    scene_key: Option<u64>,
    size: [u32; 2],
    materials: Option<manifold_gpu::GpuBuffer>,
    capacity: usize,
    allocation_error: Option<String>,
}

impl SubsurfacePass {
    pub(super) fn reset(&mut self) {
        self.history_ping = 0;
        self.needs_reset = true;
        self.scene_key = None;
    }

    pub(super) fn ensure(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
        count: usize,
    ) {
        self.allocation_error = self.try_ensure(device, w, h, count).err();
    }

    fn try_ensure(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
        count: usize,
    ) -> Result<(), String> {
        let resize = self.output.is_none() || self.size != [w, h];
        let grow = self.capacity < count;
        if !resize && !grow {
            return Ok(());
        }
        let table_bytes = (count * std::mem::size_of::<SubsurfaceMaterial>()) as u64;
        let additional_bytes = if resize {
            // Five Rgba16Float radiance textures (8 B/px), one Rgba32Float
            // guide (16 B/px), and two R16Float history-count textures (2
            // B/px): 60 B/px total. Counts are capped at 64 below.
            u64::from(w) * u64::from(h) * 60
        } else {
            0
        } + if grow { table_bytes } else { 0 };
        crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            additional_bytes,
        )
        .map_err(|error| format!("Subsurface resource admission failed: {error}"))?;
        let make_texture = |format: manifold_gpu::GpuTextureFormat, label: &'static str| {
            device.try_create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::SHADER_WRITE
                    | manifold_gpu::GpuTextureUsage::COPY_SRC,
                label,
                mip_levels: 1,
            })
        };
        let output = if resize {
            Some(make_texture(
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                "node.render_scene subsurface radiance",
            )?)
        } else {
            None
        };
        let raw = if resize {
            Some(make_texture(
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                "node.render_scene subsurface raw",
            )?)
        } else {
            None
        };
        let guide = if resize {
            Some(make_texture(
                manifold_gpu::GpuTextureFormat::Rgba32Float,
                "node.render_scene subsurface guide",
            )?)
        } else {
            None
        };
        let filter_scratch = if resize {
            Some(make_texture(
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                "node.render_scene subsurface filter scratch",
            )?)
        } else {
            None
        };
        let history = if resize {
            [
                Some(make_texture(
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                    "node.render_scene subsurface history A",
                )?),
                Some(make_texture(
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                    "node.render_scene subsurface history B",
                )?),
            ]
        } else {
            [None, None]
        };
        let history_count = if resize {
            [
                Some(make_texture(
                    manifold_gpu::GpuTextureFormat::R16Float,
                    "node.render_scene subsurface history count A",
                )?),
                Some(make_texture(
                    manifold_gpu::GpuTextureFormat::R16Float,
                    "node.render_scene subsurface history count B",
                )?),
            ]
        } else {
            [None, None]
        };
        let materials = if grow {
            Some(device.try_create_buffer(table_bytes)?)
        } else {
            None
        };
        // Publish the complete replacement only after both allocations succeed.
        if let Some(output) = output {
            self.output = Some(output);
            self.raw = raw;
            self.guide = guide;
            self.filter_scratch = filter_scratch;
            self.history = history;
            self.history_count = history_count;
            self.history_ping = 0;
            self.needs_reset = true;
            self.size = [w, h];
        }
        if let Some(materials) = materials {
            self.materials = Some(materials);
            self.capacity = count;
            self.needs_reset = true;
        }
        Ok(())
    }
}

impl RenderScene {
    pub(super) fn subsurface_trace<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        draws: &[&ObjectDraw<'ctx>],
        objects: &[manifold_gpu::raytrace::RtObjectGeometry<'ctx>],
        gi_materials: &[manifold_gpu::raytrace::GiMaterial],
        textures: &[&manifold_gpu::GpuTexture],
    ) -> bool {
        if let Some(error) = &self.subsurface_pass.allocation_error {
            ctx.error(error);
            return false;
        }
        let Some(inv_view_proj) = mat4_inverse(pre.view_proj) else {
            ctx.error("Subsurface scattering requires an invertible camera projection.");
            return false;
        };
        let rows: arrayvec::ArrayVec<SubsurfaceMaterial, { OBJECT_SAFETY_MAX as usize }> = draws
            .iter()
            .map(|draw| {
                let s = draw.subsurface;
                SubsurfaceMaterial {
                    color_weight: [
                        s.color[0],
                        s.color[1],
                        s.color[2],
                        draw.subsurface_binding[0],
                    ],
                    radius_phase: [s.radius[0], s.radius[1], s.radius[2], s.anisotropy],
                    config: [
                        u32::from(s.mode == SubsurfaceMode::RandomWalk),
                        s.samples,
                        0,
                        0,
                    ],
                }
            })
            .collect();
        // Static accumulation is deliberately invalidated on any semantic
        // change. Unknown producer versions cannot promise a still scene.
        use std::hash::{Hash, Hasher};
        let mut key = ahash::AHasher::default();
        key.write(bytemuck::cast_slice(&pre.view_proj));
        key.write(bytemuck::cast_slice(&pre.light_data));
        key.write(bytemuck::cast_slice(rows.as_slice()));
        self.rt_accel_key.hash(&mut key);
        self.rt_accel_topo_key.hash(&mut key);
        let env_content = ctx.inputs.content_version("envmap");
        env_content.hash(&mut key);
        for draw in draws {
            draw.vertices_content.hash(&mut key);
            draw.instances_content.hash(&mut key);
            draw.weights_content.hash(&mut key);
            key.write(&draw.gain.to_ne_bytes());
            key.write(bytemuck::cast_slice(&draw.map_uniforms));
            key.write(bytemuck::bytes_of(&draw.uniforms.alpha_params));
            key.write(bytemuck::bytes_of(&draw.uniforms.normal_uv_t));
            for content in draw.rt_texture_content { content.hash(&mut key); }
        }
        let material_bytes = unsafe {
            std::slice::from_raw_parts(gi_materials.as_ptr().cast::<u8>(),
                std::mem::size_of_val(gi_materials))
        };
        key.write(material_bytes);
        let key = key.finish();
        let known = draws.iter().all(|d| d.geometry_content_known && d.appearance_content_known)
            && (ctx.inputs.texture_2d("envmap").is_none() || env_content.is_some());
        if !known || self.subsurface_pass.scene_key != Some(key) {
            self.subsurface_pass.reset();
        }
        self.subsurface_pass.scene_key = Some(key);
        let params = SubsurfaceParams {
            inv_view_proj,
            camera_pos: [pre.cam.pos[0], pre.cam.pos[1], pre.cam.pos[2], 0.0],
            render_size: [pre.width, pre.height],
            frame_index: self.jitter_frame_index,
            slot_row_base: objects.len() as u32,
            light_count: pre.light_count,
            material_count: rows.len() as u32,
            query_units_per_pixel: rows
                .iter()
                .filter(|row| row.color_weight[3] > 0.0)
                .map(|row| {
                    let events = if row.config[0] == 1 { 256 } else { 32 };
                    1 + row.config[1] * (events + pre.light_count + 1)
                })
                .max()
                .unwrap_or(1),
            _pad: 0,
        };
        let environment = ctx
            .inputs
            .texture_2d("envmap")
            .unwrap_or(self.dummy_texture.as_ref().expect("ensured"));
        let gpu = ctx.gpu_encoder();
        let materials = self.subsurface_pass.materials.as_ref().expect("ensured");
        manifold_gpu::raytrace::encode_inline_copy(
            gpu.device,
            gpu.native_enc,
            materials,
            0,
            bytemuck::cast_slice(rows.as_slice()),
        );
        // SSS can be the only ray consumer. Publish canonical emission rows
        // here; the regular RT pass additionally publishes its per-slot rows.
        let gi_buffer = self
            .rt_gi_materials
            .as_ref()
            .expect("current-frame acceleration tables");
        let bytes = unsafe {
            std::slice::from_raw_parts(
                gi_materials.as_ptr().cast::<u8>(),
                std::mem::size_of_val(gi_materials),
            )
        };
        manifold_gpu::raytrace::encode_inline_copy(gpu.device, gpu.native_enc, gi_buffer, 0, bytes);
        let read_idx = self.subsurface_pass.history_ping;
        let write_idx = 1 - read_idx;
        let reset = self.subsurface_pass.needs_reset;
        let raw = self.subsurface_pass.raw.as_ref().expect("ensured");
        let guide = self.subsurface_pass.guide.as_ref().expect("ensured");
        let history_read = self.subsurface_pass.history[read_idx]
            .as_ref()
            .expect("ensured");
        let history_write = self.subsurface_pass.history[write_idx]
            .as_ref()
            .expect("ensured");
        let count_read = self.subsurface_pass.history_count[read_idx]
            .as_ref()
            .expect("ensured");
        let count_write = self.subsurface_pass.history_count[write_idx]
            .as_ref()
            .expect("ensured");
        let filter_scratch = self
            .subsurface_pass
            .filter_scratch
            .as_ref()
            .expect("ensured");
        let output = self.subsurface_pass.output.as_ref().expect("ensured");
        self.rt_tracer
            .as_ref()
            .expect("ensured")
            .dispatch_subsurface(
                gpu.native_enc,
                gpu.device,
                &params,
                self.rt_accel.as_ref().expect("current-frame acceleration"),
                self.rt_normal_sources
                    .as_ref()
                    .expect("current-frame normal sources"),
                materials,
                gi_buffer,
                &self.light_buffers[self.light_frame],
                objects,
                textures,
                self.opaque_depth_snapshot.as_ref().expect("ensured"),
                environment,
                raw,
                guide,
                history_read,
                history_write,
                count_read,
                count_write,
                filter_scratch,
                output,
                reset,
            );
        gpu.rt_dispatches += 1;
        self.subsurface_pass.history_ping = write_idx;
        self.subsurface_pass.needs_reset = false;
        true
    }
}
