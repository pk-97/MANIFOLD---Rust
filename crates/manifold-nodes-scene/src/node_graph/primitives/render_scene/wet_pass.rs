//! The wet pass: the water's wet marks (`node.wet_map`) drawn over the opaque
//! depth into the per-pixel wet mask the surface shading reads
//! (`look_wet_surface.wgsl`), after the depth prepass and before any colour.
//! One triangle per decal in the decal's own plane, counted on the GPU by the
//! map's own draw arguments; each pixel sums the decals over it, held at 1. A
//! scene with no wet map draws nothing and holds no mask.
use std::sync::LazyLock;

use manifold_gpu::*;

use super::{FieldObject, mat4_inverse, mat4_mul};

// The map's layout, then the pass.
static SHADER: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n{}",
        manifold_node_engine::scene::wet_map::layout(),
        include_str!("../shaders/wet_pass.wgsl")
    )
});

/// Render pixels per wet mask texel along each axis, `WET_MASK_SCALE` in
/// `look_wet_surface.wgsl`.
const MASK_SCALE: u32 = 2;

/// Field for field with `Uniforms` in `wet_pass.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    clip_to_local: [[f32; 4]; 4],
    mask_scale: [f32; 4],
}

#[derive(Default)]
pub(super) struct WetPass {
    pipeline: Option<GpuRenderPipeline>,
    mask: Option<GpuTexture>,
}

impl WetPass {
    /// The mask this frame's pass drew; None when no water has a wet map.
    pub(super) fn mask(&self) -> Option<&GpuTexture> {
        self.mask.as_ref()
    }

    /// Draws the first water's wet map that has one (one wet map per scene)
    /// into the mask, at 1 / `MASK_SCALE` of the opaque depth's size.
    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        fields: &[FieldObject<'_>],
        view_proj: [[f32; 4]; 4],
        opaque_depth: &GpuTexture,
    ) -> Result<(), &'static str> {
        let Some((map, model)) = fields
            .iter()
            .find_map(|field| Some((field.wet_map?, field.model)))
        else {
            self.mask = None;
            return Ok(());
        };
        let inverse_view_proj =
            mat4_inverse(view_proj).ok_or("Wet pass: singular camera matrix")?;
        let world_to_local = mat4_inverse(model).ok_or("Wet pass: singular water model matrix")?;
        let uniforms = Uniforms {
            view_proj,
            model,
            clip_to_local: mat4_mul(world_to_local, inverse_view_proj),
            mask_scale: [MASK_SCALE as f32, 0.0, 0.0, 0.0],
        };
        let pipeline = self.pipeline.get_or_insert_with(|| {
            device.create_render_pipeline(
                SHADER.as_str(),
                "vs_decal",
                "fs_decal",
                GpuTextureFormat::R8Unorm,
                Some(GpuBlendState {
                    src_factor: GpuBlendFactor::One,
                    dst_factor: GpuBlendFactor::One,
                    operation: GpuBlendOp::Add,
                    src_alpha_factor: GpuBlendFactor::One,
                    dst_alpha_factor: GpuBlendFactor::One,
                    alpha_operation: GpuBlendOp::Add,
                }),
                "wet pass",
            )
        });
        let (width, height) = (
            opaque_depth.width.div_ceil(MASK_SCALE),
            opaque_depth.height.div_ceil(MASK_SCALE),
        );
        if self
            .mask
            .as_ref()
            .is_none_or(|mask| mask.width != width || mask.height != height)
        {
            self.mask = Some(device.create_texture(&GpuTextureDesc {
                width,
                height,
                depth: 1,
                mip_levels: 1,
                format: GpuTextureFormat::R8Unorm,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
                label: "node.render_scene wet mask",
            }));
        }
        encoder.draw_instanced(
            pipeline,
            self.mask.as_ref().expect("ensured"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: map,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: opaque_depth,
                },
            ],
            DrawCount::Indirect {
                args: map,
                offset: 0,
            },
            GpuLoadAction::Clear,
            "node.render_scene wet pass",
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wet_pass_shader_validates() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("wet pass parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("wet pass validates");
        assert_eq!(std::mem::size_of::<Uniforms>(), 208);
    }

    /// The surface shading reads the mask at the scale the pass draws it.
    #[test]
    fn the_read_side_scales_the_mask_as_the_pass_draws_it() {
        let module = naga::front::wgsl::parse_str(include_str!("../shaders/look_wet_surface.wgsl"))
            .expect("the wet look parses alone");
        let (_, scale) = module
            .constants
            .iter()
            .find(|(_, c)| c.name.as_deref() == Some("WET_MASK_SCALE"))
            .expect("the wet look names its mask scale");
        let naga::Expression::Literal(naga::Literal::F32(value)) =
            module.global_expressions[scale.init]
        else {
            panic!("the mask scale is an f32 literal");
        };
        assert_eq!(value, MASK_SCALE as f32);
    }

    /// The soaked albedo of `look_wet_surface.wgsl`, in f64.
    fn soaked(albedo: f64) -> f64 {
        let (n, r_e) = (1.333f64, 0.066f64);
        let r_i = 1.0 - (1.0 - r_e) / (n * n);
        (1.0 - r_e) * (1.0 - r_i) * albedo / (1.0 - r_i * albedo)
    }

    /// Lekner and Dorf's own check of the wet look: a film always darkens,
    /// and a darker surface darkens by more.
    #[test]
    fn wet_albedo_is_darker_and_more_so_for_darker_surfaces() {
        let mut last_ratio = 0.0;
        for i in 1..=100 {
            let albedo = f64::from(i) / 100.0;
            let ratio = soaked(albedo) / albedo;
            assert!(ratio < 1.0, "albedo {albedo} became {}", soaked(albedo));
            assert!(
                ratio > last_ratio,
                "albedo {albedo}: ratio {ratio} not rising"
            );
            last_ratio = ratio;
        }
        assert!(soaked(0.2) / 0.2 < soaked(0.8) / 0.8);
    }
}
