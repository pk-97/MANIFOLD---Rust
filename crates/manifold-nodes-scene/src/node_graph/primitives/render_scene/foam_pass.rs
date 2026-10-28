//! The foam pass: each water's foam particles drawn over the march's hits
//! into the per-pixel foam mask the water shading reads (`foam_mask.wgsl`),
//! after the march and before the shade. One screen-aligned square per
//! particle slot, an empty slot drawing nothing; each water pixel sums the
//! foam over it (blend add), so foam is as fine as its particles up close
//! instead of the display lattice's node spacing. A frame with no foam draws
//! nothing and holds no mask.
use std::sync::LazyLock;

use manifold_gpu::*;
use manifold_node_engine::particles::FluidParticle;

use super::{FieldObject, mat4_inverse, mat4_mul};

// The hit record, then the pass.
static SHADER: LazyLock<String> = LazyLock::new(|| {
    [
        include_str!("../shaders/water_field_common.wgsl"),
        include_str!("../shaders/foam_pass.wgsl"),
    ]
    .concat()
});

/// The mask's format: tau sums past 1 where foam piles up, so a float.
const MASK_FORMAT: GpuTextureFormat = GpuTextureFormat::R16Float;

/// Field for field with `Uniforms` in `foam_pass.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    clip_to_local: [[f32; 4]; 4],
    frame: [f32; 4],
    footprint: [f32; 4],
}

/// What the pass reads of the frame: the camera, the march's hits and the
/// size of a pixel on the water (`footprint`, as the ripple detail takes it:
/// metres per pixel per metre of depth, and flat).
pub(super) struct FoamView<'a> {
    pub view_proj: [[f32; 4]; 4],
    pub width: u32,
    pub height: u32,
    pub hits: &'a GpuBuffer,
    pub footprint: [f32; 2],
}

#[derive(Default)]
pub(super) struct FoamPass {
    pipeline: Option<GpuRenderPipeline>,
    mask: Option<GpuTexture>,
}

impl FoamPass {
    /// The mask this frame's pass drew; None when no water has foam.
    pub(super) fn mask(&self) -> Option<&GpuTexture> {
        self.mask.as_ref()
    }

    /// Draws every water's foam into the mask, the size of the hits.
    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        fields: &[FieldObject<'_>],
        view: &FoamView<'_>,
    ) -> Result<(), &'static str> {
        if !fields.iter().any(|field| field.foam.is_some()) {
            self.mask = None;
            return Ok(());
        }
        let inverse_view_proj =
            mat4_inverse(view.view_proj).ok_or("Foam pass: singular camera matrix")?;
        let pipeline = self.pipeline.get_or_insert_with(|| {
            device.create_render_pipeline(
                &manifold_gpu::RenderPipelineDesc::new(
                    SHADER.as_str(),
                    "vs_foam",
                    "fs_foam",
                    "foam pass",
                )
                .color(
                    MASK_FORMAT,
                    Some(GpuBlendState {
                        src_factor: GpuBlendFactor::One,
                        dst_factor: GpuBlendFactor::One,
                        operation: GpuBlendOp::Add,
                        src_alpha_factor: GpuBlendFactor::One,
                        dst_alpha_factor: GpuBlendFactor::One,
                        alpha_operation: GpuBlendOp::Add,
                    }),
                ),
            )
        });
        if self
            .mask
            .as_ref()
            .is_none_or(|mask| mask.width != view.width || mask.height != view.height)
        {
            self.mask = Some(device.create_texture(&GpuTextureDesc {
                width: view.width,
                height: view.height,
                depth: 1,
                mip_levels: 1,
                format: MASK_FORMAT,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
                label: "node.render_scene foam mask",
            }));
        }
        let mask = self.mask.as_ref().expect("ensured");
        let mut load = GpuLoadAction::Clear;
        for field in fields {
            let Some(particles) = field.foam else {
                continue;
            };
            let slots = particles.size / size_of::<FluidParticle>() as u64;
            let Some(vertices) = u32::try_from(slots)
                .ok()
                .and_then(|slots| slots.checked_mul(6))
            else {
                return Err("Foam pass: more foam particles than a draw can count");
            };
            if vertices == 0 {
                continue;
            }
            let world_to_local =
                mat4_inverse(field.model).ok_or("Foam pass: singular water model matrix")?;
            let uniforms = Uniforms {
                view_proj: view.view_proj,
                model: field.model,
                clip_to_local: mat4_mul(world_to_local, inverse_view_proj),
                frame: [
                    view.width as f32,
                    view.height as f32,
                    (field.slot + 1) as f32,
                    slots as f32,
                ],
                footprint: [
                    view.footprint[0],
                    view.footprint[1],
                    field.foam_coverage,
                    0.0,
                ],
            };
            encoder.draw_instanced(
                pipeline,
                mask,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: particles,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: view.hits,
                        offset: 0,
                    },
                ],
                DrawCount::Direct {
                    vertices,
                    instances: 1,
                },
                load,
                "node.render_scene foam pass",
            );
            load = GpuLoadAction::Load;
        }
        // No water drew: the mask still has to read as no foam.
        if load == GpuLoadAction::Clear {
            self.mask = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foam_pass_shader_validates() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("foam pass parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("foam pass validates");
        assert_eq!(std::mem::size_of::<Uniforms>(), 224);
    }
}
