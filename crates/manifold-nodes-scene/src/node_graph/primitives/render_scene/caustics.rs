//! Caustics: the sun's light focused by the water onto what lies under it
//! (`caustics.wgsl`). After the shadow maps, the water's surface mesh is drawn
//! from the sun into a depth map; one photon per texel of it refracts through
//! the surface onto the opaque scene in the sun's shadow map and adds the
//! light that gets through to the caustic map; scene shading reads the map
//! for the sun's light on anything under the water (`caustic_light` in
//! `scene_shading.wgsl`). Runs only with a sun whose shadow map is drawn this
//! frame and a meshed water that lets light through; otherwise it holds no
//! targets and reads off.
use manifold_gpu::*;

use super::{FieldObject, ShadowUniforms, mat4_inverse};

const SHADER: &str = include_str!("../shaders/caustics.wgsl");

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
    light_view_proj: [[f32; 4]; 4],
    inverse_light_view_proj: [[f32; 4]; 4],
    toward_sun: [f32; 4],
    extinction: [f32; 4],
    sizes: [u32; 4],
}

/// Field for field with `CausticRead` in `scene_shading.wgsl`: all zeros
/// reads off.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct CausticRead {
    slot: u32,
    bias: f32,
    pad: [u32; 2],
}

/// The sun the caustics follow: its caster slot and entry, and the shadow
/// maps holding its layer, drawn this frame.
pub(super) struct Sun<'a> {
    pub slot: usize,
    pub caster: &'a ShadowCaster,
    pub shadow_maps: &'a GpuTexture,
}

/// The first caster that is a lit sun, if any.
pub(super) fn sun_slot(casters: &[ShadowCaster]) -> Option<usize> {
    casters
        .iter()
        .position(|c| c.color[3] == 0.0 && c.color[..3].iter().any(|&channel| channel > 0.0))
}

/// The depth pipeline, its state and the stand-ins the surface draw binds,
/// as the water's first-hit depth draw binds them.
pub(super) struct SurfaceDraw<'a> {
    pub pipeline: &'a GpuRenderPipeline,
    pub depth_state: &'a GpuDepthStencilState,
    pub identity_stub: &'a GpuBuffer,
    pub dummy: &'a GpuTexture,
    pub sampler: &'a GpuSampler,
}

struct Targets {
    side: u32,
    surface: GpuTexture,
    sums: GpuBuffer,
    map: GpuTexture,
}

#[derive(Default)]
pub(super) struct Caustics {
    splat: Option<GpuComputePipeline>,
    resolve: Option<GpuComputePipeline>,
    targets: Option<Targets>,
    read: Option<CausticRead>,
}

impl Caustics {
    /// What scene shading reads: off unless this frame's caustics ran.
    pub(super) fn read(&self) -> CausticRead {
        self.read.unwrap_or(bytemuck::Zeroable::zeroed())
    }

    /// The caustic map and the water's surface in the sun's depth, when this
    /// frame's caustics ran.
    pub(super) fn maps(&self) -> Option<(&GpuTexture, &GpuTexture)> {
        self.read?;
        self.targets.as_ref().map(|t| (&t.map, &t.surface))
    }

    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        sun: Option<(Sun<'_>, SurfaceDraw<'_>)>,
        fields: &[FieldObject<'_>],
    ) -> Result<(), String> {
        self.read = None;
        let water = fields
            .iter()
            .find(|f| f.mesh.is_some() && f.uniforms.transmission_volume_params[0] > 0.0);
        let (Some((sun, draw)), Some(water), Some(mesh)) =
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
            });
        }
        let targets = self.targets.as_ref().expect("ensured");

        // The water's surface from the sun: Gain 1, no weights, not a cutout.
        let surface_uniforms = ShadowUniforms {
            light_view_proj: vp,
            model: water.model,
            appearance: [1.0, 0.0, 0.0, 0.0],
            alpha: [0.0, 0.0, 1.0, 0.0],
            uv_m: [0.0; 4],
            uv_t: [0.0; 4],
            instance_inner: water.uniforms.instance_inner,
        };
        let surface_bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&surface_uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: mesh.vertices,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: draw.identity_stub,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: mesh.vertices,
                offset: 0,
            },
            GpuBinding::Texture {
                binding: 4,
                texture: draw.dummy,
            },
            GpuBinding::Sampler {
                binding: 5,
                sampler: draw.sampler,
            },
        ];
        let surface_draw =
            GpuEncoder::depth_msaa_draw(draw.pipeline, &surface_bindings, mesh.vertex_count, 1);
        let surface_draw = match &mesh.live_args {
            Some((args, offset)) => surface_draw.indirect(args, *offset),
            None => surface_draw,
        };
        encoder.draw_instanced_depth_only_batch(
            &targets.surface,
            draw.depth_state,
            std::slice::from_ref(&surface_draw),
            "node.render_scene caustic surface",
        );

        // Water's extinction per metre, as `volume_attenuation_coefficient`.
        let colour = water.uniforms.volume_attenuation_color;
        let distance = water.uniforms.transmission_volume_params[2].max(1e-4);
        let extinction = [0, 1, 2].map(|c| -colour[c].max(1e-6).ln() / distance);
        let splat = Splat {
            light_view_proj: vp,
            inverse_light_view_proj: mat4_inverse(vp)
                .ok_or_else(|| "Caustics: singular sun matrix".to_owned())?,
            toward_sun: [
                sun.caster.source[0],
                sun.caster.source[1],
                sun.caster.source[2],
                water.uniforms.pbr_metallic_roughness[2].max(1.0),
            ],
            extinction: [extinction[0], extinction[1], extinction[2], texel],
            sizes: [side, sun.slot as u32, 0, 0],
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&splat),
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
        ];
        let make =
            |entry| device.create_compute_pipeline(SHADER, entry, "node.render_scene caustics");
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

        // The sun's depth per metre along its light: the length of its
        // projection's depth row.
        let depth_per_metre =
            (vp[0][2] * vp[0][2] + vp[1][2] * vp[1][2] + vp[2][2] * vp[2][2]).sqrt();
        self.read = Some(CausticRead {
            slot: sun.slot as u32 + 1,
            bias: SURFACE_BIAS_METRES * depth_per_metre,
            pad: [0; 2],
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caustics_shader_validates() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("caustics parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("caustics validate");
        assert_eq!(std::mem::size_of::<Splat>(), 176);
        assert_eq!(std::mem::size_of::<CausticRead>(), 16);
    }
}
