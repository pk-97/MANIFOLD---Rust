//! Optional geometry-based subsurface pass; see SUBSURFACE_MATERIAL_DESIGN.md.
use super::*;
use crate::node_graph::material::SubsurfaceMode;
use manifold_gpu::raytrace::{SubsurfaceMaterial, SubsurfaceParams};

#[derive(Default)]
pub(super) struct SubsurfacePass {
    pub(super) output: Option<manifold_gpu::GpuTexture>,
    size: [u32; 2],
    materials: Option<manifold_gpu::GpuBuffer>,
    capacity: usize,
    allocation_error: Option<String>,
}

impl SubsurfacePass {
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
            u64::from(w) * u64::from(h) * 8
        } else {
            0
        } + if grow { table_bytes } else { 0 };
        crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(),
            additional_bytes,
        )
        .map_err(|error| format!("Subsurface resource admission failed: {error}"))?;
        let output = if resize {
            Some(device.try_create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::SHADER_WRITE
                    | manifold_gpu::GpuTextureUsage::COPY_SRC,
                label: "node.render_scene subsurface radiance",
                mip_levels: 1,
            })?)
        } else {
            None
        };
        let materials = if grow {
            Some(device.try_create_buffer(table_bytes)?)
        } else {
            None
        };
        // Publish the complete replacement only after both allocations succeed.
        if let Some(output) = output {
            self.output = Some(output);
            self.size = [w, h];
        }
        if let Some(materials) = materials {
            self.materials = Some(materials);
            self.capacity = count;
        }
        Ok(())
    }
}

impl RenderScene {
    pub(super) fn subsurface_trace<'ctx, 'gpu>(
        &self,
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
                self.subsurface_pass.output.as_ref().expect("ensured"),
            );
        gpu.rt_dispatches += 1;
        true
    }
}
