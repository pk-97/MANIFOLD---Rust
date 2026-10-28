//! Full-screen pass that shades the water the march found, with the scene's
//! own lighting and glass-style light through the water. One pipeline per
//! colour format, compiled with the scene's other pipelines so the first water
//! frame is a cache hit.
use std::sync::LazyLock;

use manifold_gpu::*;
use manifold_node_engine::scene::world_tiles::{READ_SIDE, table_entries};

use super::ripple_detail::FLOW_PERIOD_SECONDS;
use super::{FIELD_TILES_WGSL, FieldObject, mat4_inverse};

/// The world-tile read side and the field's table, the scene shading, the five
/// look hooks and the field helpers, then the water pass itself.
pub(super) static WATER_SHADE_WGSL: LazyLock<String> = LazyLock::new(|| {
    [
        READ_SIDE,
        FIELD_TILES_WGSL,
        include_str!("../shaders/scene_shading.wgsl"),
        include_str!("../shaders/look_ripple_detail.wgsl"),
        include_str!("../shaders/foam_field.wgsl"),
        include_str!("../shaders/look_foam.wgsl"),
        include_str!("../shaders/look_mist.wgsl"),
        include_str!("../shaders/look_thin_water_light.wgsl"),
        manifold_node_engine::scene::wet_map::LAYOUT,
        include_str!("../shaders/look_wet_surface.wgsl"),
        include_str!("../shaders/water_field_common.wgsl"),
        include_str!("../shaders/water_shade.wgsl"),
    ]
    .concat()
});

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ShadeUniforms {
    inv_view_proj: [[f32; 4]; 4],
    target_size: [f32; 4],
    slot: [u32; 4],
}

/// Uniforms for shading the hits of one water slot at the given target size.
pub(super) fn shade_uniforms(
    view_proj: [[f32; 4]; 4],
    width: u32,
    height: u32,
    slot: u32,
) -> Result<ShadeUniforms, &'static str> {
    let inv_view_proj = mat4_inverse(view_proj).ok_or("Water shade: singular camera matrix")?;
    Ok(ShadeUniforms {
        inv_view_proj,
        target_size: [width as f32, height as f32, 0.0, 0.0],
        slot: [slot + 1, 0, 0, 0],
    })
}

/// The ripple detail's uniforms, field for field with `RippleUniforms` in
/// `look_ripple_detail.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct RippleUniforms {
    world_to_local: [[f32; 4]; 4],
    local_to_world: [[f32; 4]; 4],
    /// x pool tiles, y table entries, z 1 when a foam field is bound.
    tiles: [u32; 4],
    /// x display cell size in metres, y tile side in metres (0: no ripples),
    /// z strength override.
    field: [f32; 4],
    time: [f32; 4],
}

/// Uniforms for the ripple detail on one field. `tile` is the slope tile's
/// side in metres, `None` for a field without ripples. `footprint` is the
/// size of a pixel on the water: metres per pixel per metre of distance, then
/// metres per pixel flat (perspective and orthographic cameras). `has_foam` is
/// whether a foam field is bound, which the foam sampling reads from `tiles.z`.
pub(super) fn ripple_uniforms(
    field: &FieldObject,
    tile: Option<f32>,
    has_foam: bool,
    seconds: f32,
    footprint: [f32; 2],
) -> Result<RippleUniforms, &'static str> {
    let world_to_local =
        mat4_inverse(field.model).ok_or("Water shade: singular water model matrix")?;
    if !(field.cell_size > 0.0 && field.cell_size.is_finite()) || field.pool_tiles == 0 {
        return Err("Water shade: the field needs a cell size above zero and a pool of tiles");
    }
    Ok(RippleUniforms {
        world_to_local,
        local_to_world: field.model,
        tiles: [
            field.pool_tiles,
            table_entries(field.pool_tiles),
            u32::from(has_foam),
            0,
        ],
        // 0 is the shader's off value for the tile side.
        field: [
            field.cell_size,
            tile.unwrap_or(0.0),
            field.ripple_detail,
            0.0,
        ],
        time: [seconds, FLOW_PERIOD_SECONDS, footprint[0], footprint[1]],
    })
}

/// The look inputs one water draw binds at 62-66 and 70.
pub(super) struct LookBindings<'a> {
    pub uniforms: &'a RippleUniforms,
    pub slopes: &'a GpuTexture,
    pub sampler: &'a GpuSampler,
    pub velocity: &'a GpuBuffer,
    pub foam: &'a GpuBuffer,
    /// The field's world-tile table.
    pub tiles: &'a GpuBuffer,
}

/// The mist composites over the colour with `dst * (1 - a) + colour`, the
/// colour premultiplied by the mist's coverage `a`.
const MIST_BLEND: GpuBlendState = GpuBlendState {
    src_factor: GpuBlendFactor::One,
    dst_factor: GpuBlendFactor::OneMinusSrcAlpha,
    operation: GpuBlendOp::Add,
    src_alpha_factor: GpuBlendFactor::One,
    dst_alpha_factor: GpuBlendFactor::OneMinusSrcAlpha,
    alpha_operation: GpuBlendOp::Add,
};

#[derive(Default)]
pub(super) struct WaterShade {
    pipeline: Option<(GpuRenderPipeline, GpuTextureFormat)>,
    mist_pipeline: Option<(GpuRenderPipeline, GpuTextureFormat)>,
    /// Always passes and writes nothing: the mist's full-screen triangle sits at
    /// depth 0, and the march already stopped the mist at the nearest surface.
    mist_depth_state: Option<GpuDepthStencilState>,
}

impl WaterShade {
    /// The shading pipeline for one colour format, through the device's
    /// shared render-pipeline cache.
    fn build_pipeline(device: &GpuDevice, color_format: GpuTextureFormat) -> GpuRenderPipeline {
        device.create_render_pipeline_depth(
            &WATER_SHADE_WGSL,
            "vs_water",
            "fs_water",
            color_format,
            GpuTextureFormat::Depth32Float,
            None,
            1,
            "node.render_scene water shade",
        )
    }

    /// The mist pipeline for one colour format, through the same shared cache.
    fn build_mist_pipeline(
        device: &GpuDevice,
        color_format: GpuTextureFormat,
    ) -> GpuRenderPipeline {
        device.create_render_pipeline_depth(
            &WATER_SHADE_WGSL,
            "vs_water",
            "fs_mist",
            color_format,
            GpuTextureFormat::Depth32Float,
            Some(MIST_BLEND),
            1,
            "node.render_scene water mist",
        )
    }

    fn pipeline(
        &mut self,
        device: &GpuDevice,
        color_format: GpuTextureFormat,
    ) -> &GpuRenderPipeline {
        if !matches!(&self.pipeline, Some((_, f)) if *f == color_format) {
            self.pipeline = Some((Self::build_pipeline(device, color_format), color_format));
        }
        &self.pipeline.as_ref().expect("built above").0
    }

    /// Builds the pipeline into the device's shared cache ahead of the first draw.
    pub(super) fn prewarm(device: &GpuDevice, color_format: GpuTextureFormat) {
        Self::build_pipeline(device, color_format);
        Self::build_mist_pipeline(device, color_format);
    }

    /// Shades one slot's water over `target`, writing its depth into `depth`.
    /// `scene` is the include's scene-wide bindings as a mesh draw builds
    /// them; `object_uniforms` is the water object's `RenderSceneUniforms`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        target: &GpuTexture,
        depth: &GpuTexture,
        depth_state: &GpuDepthStencilState,
        scene: &[GpuBinding; 16],
        object_uniforms: &[u8],
        hits: &GpuBuffer,
        shade: &ShadeUniforms,
        look: &LookBindings,
    ) {
        let pipeline = self.pipeline(device, target.format);
        let mut bindings = [scene[0]; 25];
        bindings[..16].copy_from_slice(scene);
        bindings[16] = GpuBinding::Bytes {
            binding: 0,
            data: object_uniforms,
        };
        bindings[17] = GpuBinding::Buffer {
            binding: 60,
            buffer: hits,
            offset: 0,
        };
        bindings[18] = GpuBinding::Bytes {
            binding: 61,
            data: bytemuck::bytes_of(shade),
        };
        bindings[19] = GpuBinding::Texture {
            binding: 62,
            texture: look.slopes,
        };
        bindings[20] = GpuBinding::Sampler {
            binding: 63,
            sampler: look.sampler,
        };
        bindings[21] = GpuBinding::Bytes {
            binding: 64,
            data: bytemuck::bytes_of(look.uniforms),
        };
        bindings[22] = GpuBinding::Buffer {
            binding: 65,
            buffer: look.velocity,
            offset: 0,
        };
        bindings[23] = GpuBinding::Buffer {
            binding: 66,
            buffer: look.foam,
            offset: 0,
        };
        bindings[24] = GpuBinding::Buffer {
            binding: 70,
            buffer: look.tiles,
            offset: 0,
        };
        let draw = GpuEncoder::depth_msaa_draw(pipeline, &bindings, 3, 1);
        enc.draw_instanced_depth_batch(
            target,
            depth,
            depth_state,
            &[draw],
            GpuLoadAction::Load,
            GpuLoadAction::Load,
            "node.render_scene water shade",
        );
    }

    /// Draws the mist the march integrated into `mist` over `target`: one
    /// full-screen blend that attenuates what is there and adds the in-scattered
    /// light. `object_uniforms` is the first mist-bearing water object's, which
    /// carries the scene's fog and exposure. The look inputs at 62-66 and 70 are
    /// bound as the shade draw binds them, because the module declares them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn encode_mist(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        target: &GpuTexture,
        depth: &GpuTexture,
        scene: &[GpuBinding; 16],
        object_uniforms: &[u8],
        mist: &GpuBuffer,
        shade: &ShadeUniforms,
        look: &LookBindings,
    ) {
        if !matches!(&self.mist_pipeline, Some((_, f)) if *f == target.format) {
            self.mist_pipeline = Some((
                Self::build_mist_pipeline(device, target.format),
                target.format,
            ));
        }
        let pipeline = &self.mist_pipeline.as_ref().expect("built above").0;
        let depth_state = self.mist_depth_state.get_or_insert_with(|| {
            device.create_depth_stencil_state(&GpuDepthStencilDesc {
                compare: GpuCompareFunction::Always,
                write_enabled: false,
            })
        });
        let mut bindings = [scene[0]; 25];
        bindings[..16].copy_from_slice(scene);
        bindings[16] = GpuBinding::Bytes {
            binding: 0,
            data: object_uniforms,
        };
        bindings[17] = GpuBinding::Bytes {
            binding: 61,
            data: bytemuck::bytes_of(shade),
        };
        bindings[18] = GpuBinding::Texture {
            binding: 62,
            texture: look.slopes,
        };
        bindings[19] = GpuBinding::Sampler {
            binding: 63,
            sampler: look.sampler,
        };
        bindings[20] = GpuBinding::Bytes {
            binding: 64,
            data: bytemuck::bytes_of(look.uniforms),
        };
        bindings[21] = GpuBinding::Buffer {
            binding: 65,
            buffer: look.velocity,
            offset: 0,
        };
        bindings[22] = GpuBinding::Buffer {
            binding: 66,
            buffer: look.foam,
            offset: 0,
        };
        bindings[23] = GpuBinding::Buffer {
            binding: 67,
            buffer: mist,
            offset: 0,
        };
        bindings[24] = GpuBinding::Buffer {
            binding: 70,
            buffer: look.tiles,
            offset: 0,
        };
        let draw = GpuEncoder::depth_msaa_draw(pipeline, &bindings, 3, 1);
        enc.draw_instanced_depth_batch(
            target,
            depth,
            depth_state,
            &[draw],
            GpuLoadAction::Load,
            GpuLoadAction::Load,
            "node.render_scene water mist",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::mesh::pipeline::mat4_mul;
    use manifold_node_engine::scene::camera::Camera;

    #[test]
    fn water_shade_shader_validates() {
        let module = naga::front::wgsl::parse_str(&WATER_SHADE_WGSL)
            .expect("water shade shader must naga-parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = validator.validate(&module) {
            panic!(
                "water shade validation failed:\n{}",
                e.emit_to_string(&WATER_SHADE_WGSL)
            );
        }
    }

    #[test]
    fn ripple_uniforms_are_176_bytes() {
        assert_eq!(std::mem::size_of::<RippleUniforms>(), 176);
    }

    #[test]
    fn shade_uniforms_are_96_bytes() {
        assert_eq!(std::mem::size_of::<ShadeUniforms>(), 96);
    }

    #[test]
    fn shade_uniforms_invert_the_camera_and_carry_size_and_slot() {
        let cam = Camera {
            pos: [1.5, 2.0, -3.0],
            ..Camera::default_perspective()
        };
        let vp = cam.view_proj(16.0 / 9.0);
        let u = shade_uniforms(vp, 1280, 720, 3).expect("a real camera inverts");
        let product = mat4_mul(u.inv_view_proj, vp);
        for (c, column) in product.iter().enumerate() {
            for (r, &value) in column.iter().enumerate() {
                let expected = if c == r { 1.0 } else { 0.0 };
                assert!(
                    (value - expected).abs() < 1e-4,
                    "inv*vp[{c}][{r}] = {value}, expected {expected}"
                );
            }
        }
        assert_eq!(u.target_size, [1280.0, 720.0, 0.0, 0.0]);
        assert_eq!(u.slot, [4, 0, 0, 0]);
    }

    #[test]
    fn shade_uniforms_reject_a_singular_matrix() {
        assert!(shade_uniforms([[0.0; 4]; 4], 1280, 720, 0).is_err());
    }
}
