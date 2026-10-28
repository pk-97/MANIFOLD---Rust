//! Caustics: the sun's light focused by the water onto what lies under it
//! (`caustics.wgsl`). After the shadow maps, a GPU step narrows the sun's view
//! to the water's bounds and the water's surface mesh is drawn in that view
//! into a depth map; one photon per texel of it refracts through the surface
//! onto the opaque scene in the sun's shadow map and adds the light that gets
//! through to the caustic map; scene shading reads the map, in the same
//! narrowed view, for the sun's light on anything under the water
//! (`caustic_light` in `scene_shading.wgsl`). Runs only with a sun whose
//! shadow map is drawn this frame and a meshed water that lets light through;
//! otherwise it holds no targets and reads off.
use std::sync::LazyLock;

use manifold_gpu::*;

use super::ripple_detail::RippleDetail;
use super::water_shade::ripple_uniforms;
use super::{FIELD_TILES_WGSL, FieldObject, mat4_inverse, mat4_mul};

/// The field's table reads, the ripple detail and the foam cover it fades
/// by, then the pass.
static SHADER: LazyLock<String> = LazyLock::new(|| {
    [
        manifold_node_engine::scene::world_tiles::READ_SIDE,
        FIELD_TILES_WGSL,
        include_str!("../shaders/look_ripple_detail.wgsl"),
        include_str!("../shaders/foam_field.wgsl"),
        include_str!("../shaders/caustics.wgsl"),
    ]
    .concat()
});

/// The caustic map's largest side; the sun's shadow resolution when smaller.
const MAX_SIDE: u32 = 1024;

/// How far under the water's surface, in metres along the sun's light, a
/// point must lie to take caustics: the water's own surface, which the
/// march places within a display cell of the mesh, stays plainly lit.
const SURFACE_BIAS_METRES: f32 = 0.02;

/// Field for field with `Splat` in `caustics.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Splat {
    sun_view_proj: [[f32; 4]; 4],
    inverse_sun_view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    toward_sun: [f32; 4],
    extinction: [f32; 4],
    sizes: [u32; 4],
    bias: [f32; 4],
}

/// The size of `CausticFit` in `caustics.wgsl` and `scene_shading.wgsl`: two
/// matrices, the slot, the bias and two pads. All zeros reads off.
const FIT_BYTES: u64 = 144;

/// The sun the caustics follow: its caster slot and entry, the shadow maps
/// holding its layer, drawn this frame, and the depth state they draw with.
pub(super) struct Sun<'a> {
    pub slot: usize,
    pub caster: &'a ShadowCaster,
    pub shadow_maps: &'a GpuTexture,
    pub depth_state: &'a GpuDepthStencilState,
}

/// The first caster that is a lit sun, if any.
pub(super) fn sun_slot(casters: &[ShadowCaster]) -> Option<usize> {
    casters
        .iter()
        .position(|c| c.color[3] == 0.0 && c.color[..3].iter().any(|&channel| channel > 0.0))
}

struct Targets {
    side: u32,
    surface: GpuTexture,
    sums: GpuBuffer,
    map: GpuTexture,
    /// The narrowed view, written on the GPU each frame.
    fit: GpuBuffer,
}

#[derive(Default)]
pub(super) struct Caustics {
    fit: Option<GpuComputePipeline>,
    surface: Option<GpuRenderPipeline>,
    splat: Option<GpuComputePipeline>,
    resolve: Option<GpuComputePipeline>,
    targets: Option<Targets>,
    /// The view scene shading reads when no caustics run: zeros, so off.
    off: Option<GpuBuffer>,
    ran: bool,
}

impl Caustics {
    /// The view scene shading reads: this frame's narrowed view when the
    /// caustics ran, else one that reads off. None only before the first
    /// `encode`.
    pub(super) fn fit(&self) -> Option<&GpuBuffer> {
        match &self.targets {
            Some(targets) if self.ran => Some(&targets.fit),
            _ => self.off.as_ref(),
        }
    }

    /// The caustic map and the water's surface in its depth, when this
    /// frame's caustics ran.
    pub(super) fn maps(&self) -> Option<(&GpuTexture, &GpuTexture)> {
        self.targets
            .as_ref()
            .filter(|_| self.ran)
            .map(|t| (&t.map, &t.surface))
    }

    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        sun: Option<Sun<'_>>,
        fields: &[FieldObject<'_>],
        ripples: &RippleDetail,
        seconds: f32,
    ) -> Result<(), String> {
        self.ran = false;
        self.off.get_or_insert_with(|| {
            let off = device.create_buffer_shared(FIT_BYTES);
            off.zero_fill();
            off
        });
        let water = fields
            .iter()
            .find(|f| f.mesh.is_some() && f.uniforms.transmission_volume_params[0] > 0.0);
        let (Some(sun), Some(water), Some(mesh)) =
            (sun, water, water.and_then(|w| w.mesh.as_ref()))
        else {
            self.targets = None;
            return Ok(());
        };
        let vp = sun.caster.view_proj;
        // 1 / the sun's shadow resolution, which a caster with a layer has.
        let texel = sun.caster.shadow[2];
        let side = ((1.0 / texel).round() as u32).min(MAX_SIDE);
        if self.targets.as_ref().is_none_or(|t| t.side != side) {
            self.targets = Some(Targets {
                side,
                surface: device.try_create_texture(&GpuTextureDesc {
                    width: side,
                    height: side,
                    depth: 1,
                    mip_levels: 1,
                    format: GpuTextureFormat::Depth32Float,
                    dimension: GpuTextureDimension::D2,
                    usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
                    label: "node.render_scene caustic surface",
                })?,
                sums: device.try_create_buffer(3 * 4 * u64::from(side) * u64::from(side))?,
                map: device.try_create_texture(&GpuTextureDesc {
                    width: side,
                    height: side,
                    depth: 1,
                    mip_levels: 1,
                    format: GpuTextureFormat::Rgba16Float,
                    dimension: GpuTextureDimension::D2,
                    usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ,
                    label: "node.render_scene caustic map",
                })?,
                fit: device.try_create_buffer(FIT_BYTES)?,
            });
        }
        let targets = self.targets.as_ref().expect("ensured");

        // Water's extinction per metre, as `volume_attenuation_coefficient`.
        let colour = water.uniforms.volume_attenuation_color;
        let distance = water.uniforms.transmission_volume_params[2].max(1e-4);
        let extinction = [0, 1, 2].map(|c| -colour[c].max(1e-6).ln() / distance);
        // The sun's depth per metre along its light: the length of its
        // projection's depth row, which the narrowed view keeps.
        let depth_per_metre =
            (vp[0][2] * vp[0][2] + vp[1][2] * vp[1][2] + vp[2][2] * vp[2][2]).sqrt();
        let splat = Splat {
            sun_view_proj: vp,
            inverse_sun_view_proj: mat4_inverse(vp)
                .ok_or_else(|| "Caustics: singular sun matrix".to_owned())?,
            // As the water's first-hit draw places it, one copy at the origin.
            model: mat4_mul(water.model, water.uniforms.instance_inner),
            toward_sun: [
                sun.caster.source[0],
                sun.caster.source[1],
                sun.caster.source[2],
                water.uniforms.pbr_metallic_roughness[2].max(1.0),
            ],
            extinction: [extinction[0], extinction[1], extinction[2], texel],
            sizes: [side, sun.slot as u32, sun.slot as u32 + 1, 0],
            bias: [SURFACE_BIAS_METRES * depth_per_metre, 0.0, 0.0, 0.0],
        };
        // The splat passes its texel's size in metres as the eye distance, so
        // the ripples are taken at one texel of the narrowed view.
        let tile = ripples.tile(water.slot);
        let ripple = ripple_uniforms(
            water,
            tile.map(|tile| tile.tile_metres),
            water.foam.is_some(),
            seconds,
            [1.0, 0.0],
        )?;
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&splat),
            },
            // A water without a tile reads no ripples, so the stand-ins are
            // never sampled.
            GpuBinding::Texture {
                binding: 62,
                texture: tile.map_or(ripples.dummy_slopes(), |tile| &tile.texture),
            },
            GpuBinding::Sampler {
                binding: 63,
                sampler: ripples.sampler(),
            },
            GpuBinding::Bytes {
                binding: 64,
                data: bytemuck::bytes_of(&ripple),
            },
            GpuBinding::Buffer {
                binding: 65,
                buffer: water.velocity.unwrap_or(ripples.dummy_velocity()),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 66,
                buffer: water.foam.unwrap_or(ripples.dummy_velocity()),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 70,
                buffer: water.tiles,
                offset: 0,
            },
            GpuBinding::Texture {
                binding: 1,
                texture: &targets.surface,
            },
            GpuBinding::Texture {
                binding: 2,
                texture: sun.shadow_maps,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &targets.sums,
                offset: 0,
            },
            GpuBinding::Texture {
                binding: 4,
                texture: &targets.map,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: &targets.fit,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: &targets.fit,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 71,
                buffer: water.extent,
                offset: 0,
            },
        ];
        let make = |entry| {
            device.create_compute_pipeline(SHADER.as_str(), entry, "node.render_scene caustics")
        };
        let fit_pipeline = self.fit.get_or_insert_with(|| make("cs_fit"));
        encoder.dispatch_compute(
            fit_pipeline,
            &bindings,
            [1, 1, 1],
            "node.render_scene caustic fit",
        );

        // The water's surface in the narrowed view.
        let surface_pipeline = self.surface.get_or_insert_with(|| {
            device.create_render_pipeline_depth_only(
                SHADER.as_str(),
                "vs_surface",
                "fs_surface",
                GpuTextureFormat::Depth32Float,
                "node.render_scene caustic surface",
            )
        });
        let surface_bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&splat),
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: &targets.fit,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 7,
                buffer: mesh.vertices,
                offset: 0,
            },
        ];
        let surface_draw =
            GpuEncoder::depth_msaa_draw(surface_pipeline, &surface_bindings, mesh.vertex_count, 1);
        let surface_draw = match &mesh.live_args {
            Some((args, offset)) => surface_draw.indirect(args, *offset),
            None => surface_draw,
        };
        encoder.draw_instanced_depth_only_batch(
            &targets.surface,
            sun.depth_state,
            std::slice::from_ref(&surface_draw),
            "node.render_scene caustic surface",
        );

        let splat_pipeline = self.splat.get_or_insert_with(|| make("cs_splat"));
        encoder.clear_buffer(&targets.sums);
        let groups = [side.div_ceil(16), side.div_ceil(16), 1];
        encoder.dispatch_compute(
            splat_pipeline,
            &bindings,
            groups,
            "node.render_scene caustic splat",
        );
        encoder.compute_memory_barrier_buffers();
        let resolve_pipeline = self.resolve.get_or_insert_with(|| make("cs_resolve"));
        encoder.dispatch_compute(
            resolve_pipeline,
            &bindings,
            groups,
            "node.render_scene caustic resolve",
        );
        self.ran = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caustics_shader_validates() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("caustics parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("caustics validate");
        assert_eq!(std::mem::size_of::<Splat>(), 256);
        // Both sides' `CausticFit` are the size the stand-in is made.
        for source in [SHADER.as_str(), super::super::RENDER_SCENE_WGSL.as_str()] {
            let module = naga::front::wgsl::parse_str(source).expect("parses");
            let (_, fit) = module
                .types
                .iter()
                .find(|(_, t)| t.name.as_deref() == Some("CausticFit"))
                .expect("names its CausticFit");
            assert_eq!(u64::from(fit.inner.size(module.to_ctx())), FIT_BYTES);
        }
    }
}
