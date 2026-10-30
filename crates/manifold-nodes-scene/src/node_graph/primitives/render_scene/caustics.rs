//! Caustics: the sun's light bent by the clear surfaces onto what lies under
//! them (`caustics.wgsl`). A clear surface is a meshed water with caustics, or
//! a clear mesh that casts shadows and draws once (no copies): goo, jelly,
//! glass. After the shadow maps, a GPU step narrows the sun's view to the
//! clear surfaces' bounds. In that view every clear surface draws its side
//! toward the sun, which also tells shading what lies under one, and every
//! clear mesh its far side. One photon per texel of the sides toward the sun
//! refracts through its surface onto the opaque scene in the sun's shadow map
//! and adds the light that gets through to the caustic map. Scene shading
//! reads the map, in the same narrowed view, for the sun's light on anything
//! under a clear surface (`caustic_light` in `scene_shading.wgsl`). The shadow
//! maps hold no clear surface, so the map is its shadow too. Runs only with a
//! sun whose shadow map is drawn this frame and a clear surface; otherwise it
//! holds no targets and reads off.
use manifold_gpu::BufferUse;
use std::sync::LazyLock;

use manifold_gpu::*;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::scene::live_extent::LiveExtent;

use super::ripple_detail::RippleDetail;
use super::water_shade::{RippleUniforms, ripple_uniforms};
use super::{FIELD_TILES_WGSL, FieldObject, ObjectDraw, mat4_inverse, mat4_mul};

/// The field's table reads and the ripple detail, then the pass.
static SHADER: LazyLock<String> = LazyLock::new(|| {
    [
        manifold_node_engine::scene::world_tiles::READ_SIDE,
        FIELD_TILES_WGSL,
        include_str!("../shaders/look_ripple_detail.wgsl"),
        include_str!("../shaders/caustics.wgsl"),
    ]
    .concat()
});

/// The caustic map's largest side; the sun's shadow resolution when smaller.
const MAX_SIDE: u32 = 1024;

/// How far under a clear surface, in metres along the sun's light, a point
/// must lie to take caustics: the surface itself, which a water's march
/// places within a display cell of its mesh, stays plainly lit.
const SURFACE_BIAS_METRES: f32 = 0.02;

/// `cs_bounds`' group size, and the most groups it runs: each thread steps
/// through the vertices past them.
const BOUNDS_GROUP: u64 = 256;
const MAX_BOUNDS_GROUPS: u64 = 1024;

/// The most clear surfaces one frame lights (`CA_MAX_CLEARS` in
/// `caustics.wgsl`); past them in draw order the rest cast no light or
/// shadow.
const MAX_CLEARS: usize = 32;

/// Field for field with `Splat` in `caustics.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Splat {
    sun_view_proj: [[f32; 4]; 4],
    inverse_sun_view_proj: [[f32; 4]; 4],
    toward_sun: [f32; 4],
    sizes: [u32; 4],
    bias: [f32; 4],
}

/// Field for field with `Clear` in `caustics.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Clear {
    model: [[f32; 4]; 4],
    medium: [f32; 4],
    through: [f32; 4],
    live: [u32; 4],
}

/// The size of `CausticFit` in `caustics.wgsl` and `scene_shading.wgsl`: two
/// matrices, the slot, the bias, the intensity and a pad. All zeros reads off.
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
        .position(|c| c.is_sun() && c.color[..3].iter().any(|&channel| channel > 0.0))
}

/// One clear surface this frame: its mesh, how it draws, and what it does to
/// the light.
struct Surface<'a> {
    vertices: &'a GpuBuffer,
    count: DrawCount<'a>,
    live: Option<&'a LiveExtent>,
    clear: Clear,
}

/// Extinction per metre from a volume's attenuation colour and distance
/// (`volume_attenuation_coefficient`), for each colour.
fn extinction(colour: [f32; 4], distance: f32) -> [f32; 3] {
    let distance = distance.max(1e-4);
    [0, 1, 2].map(|c| -colour[c].max(1e-6).ln() / distance)
}

/// A water that casts caustics: meshed, letting light through, with its
/// caustics on.
fn casts_caustics(field: &FieldObject) -> bool {
    field.caustics > 0.0
        && field.mesh.is_some()
        && field.uniforms.transmission_volume_params[0] > 0.0
}

/// The clear surfaces in order, at most [`MAX_CLEARS`]: every water that
/// casts caustics (the first takes the ripple detail), then every clear mesh
/// that casts a shadow and draws one copy.
fn surfaces<'a>(
    fields: &'a [FieldObject<'_>],
    draws: &'a [ObjectDraw<'_>],
) -> impl Iterator<Item = Surface<'a>> + 'a {
    let waters = fields
        .iter()
        .filter(|field| casts_caustics(field))
        .enumerate()
        .filter_map(|(index, water)| {
            let mesh = water.mesh.as_ref()?;
            let u = &water.uniforms;
            let [r, g, b] = extinction(u.volume_attenuation_color, u.transmission_volume_params[2]);
            Some(Surface {
                vertices: mesh.vertices,
                count: match &mesh.live_args {
                    Some((args, offset)) => DrawCount::Indirect {
                        args,
                        offset: *offset,
                    },
                    None => DrawCount::Direct {
                        vertices: mesh.vertex_count,
                        instances: 1,
                    },
                },
                live: mesh.live_extent.as_ref(),
                clear: Clear {
                    // As the water's first-hit draw places it, one copy at
                    // the origin.
                    model: mat4_mul(water.model, u.instance_inner),
                    medium: [r, g, b, u.pbr_metallic_roughness[2].max(1.0)],
                    through: [1.0, 1.0, 1.0, if index == 0 { 1.0 } else { 2.0 }],
                    live: [0; 4],
                },
            })
        });
    let meshes = draws
        .iter()
        .filter(|draw| {
            draw.is_transmissive
                && draw.cast_shadows
                && !draw.points
                && draw.fill_mode == GpuTriangleFillMode::Fill
                && draw.instances.is_none()
                && draw.instance_count == 1
        })
        .map(|draw| {
            let u = &draw.uniforms;
            // A thin wall (no volume thickness) neither bends the light nor
            // absorbs it on the way.
            let thin = u.transmission_volume_params[1] <= 0.0;
            let [r, g, b] = if thin {
                [0.0; 3]
            } else {
                extinction(u.volume_attenuation_color, u.transmission_volume_params[2])
            };
            let ior = if thin {
                1.0
            } else {
                u.pbr_metallic_roughness[2].max(1.0)
            };
            let passed = u.transmission_volume_params[0] * (1.0 - u.pbr_metallic_roughness[0]);
            Surface {
                vertices: draw.vertices,
                count: draw.draw_count(),
                live: draw.live_extent.as_ref(),
                clear: Clear {
                    model: mat4_mul(u.model, u.instance_inner),
                    medium: [r, g, b, ior],
                    through: [
                        passed * u.base_color[0],
                        passed * u.base_color[1],
                        passed * u.base_color[2],
                        0.0,
                    ],
                    live: [0; 4],
                },
            }
        });
    waters
        .chain(meshes)
        .take(MAX_CLEARS)
        .enumerate()
        .map(|(index, mut surface)| {
            if let Some(live) = surface.live {
                surface.clear.live = [1, (live.offset / 4) as u32, live.per_item, 0];
            }
            surface.clear.live[3] = index as u32;
            surface
        })
}

/// What every pass of one frame binds beside the surface it is on.
struct Shared<'a> {
    splat: &'a Splat,
    clears: &'a [Clear; MAX_CLEARS],
    targets: &'a Targets,
    shadow_maps: &'a GpuTexture,
    /// Stands in for an array the pass binds and never reads.
    off: &'a GpuBuffer,
    // The ripple detail of the water that takes it; stand-ins without one,
    // which no photon reads.
    slopes: &'a GpuTexture,
    sampler: &'a GpuSampler,
    ripple: &'a RippleUniforms,
    velocity: &'a GpuBuffer,
    tiles: &'a GpuBuffer,
    extent: &'a GpuBuffer,
}

impl<'a> Shared<'a> {
    /// The compute passes' bindings for `surface`.
    fn bindings<'b>(&'b self, surface: &'b Surface<'_>) -> [GpuBinding<'b>; 21] {
        let targets = self.targets;
        [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(self.splat),
            },
            GpuBinding::Texture {
                binding: 1,
                texture: &targets.front,
            },
            GpuBinding::Texture {
                binding: 2,
                texture: self.shadow_maps,
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
                binding: 7,
                buffer: surface.vertices,
                offset: 0,
            },
            GpuBinding::Bytes {
                binding: 8,
                data: bytemuck::bytes_of(&surface.clear),
            },
            GpuBinding::Texture {
                binding: 9,
                texture: &targets.front_normal,
            },
            GpuBinding::Texture {
                binding: 10,
                texture: &targets.back,
            },
            GpuBinding::Texture {
                binding: 11,
                texture: &targets.back_normal,
            },
            GpuBinding::Buffer {
                binding: 12,
                buffer: surface.live.map_or(self.off, |live| &live.counts),
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 13,
                buffer: &targets.bounds,
                offset: 0,
            },
            GpuBinding::Bytes {
                binding: 14,
                data: bytemuck::bytes_of(self.clears),
            },
            GpuBinding::Texture {
                binding: 62,
                texture: self.slopes,
            },
            GpuBinding::Sampler {
                binding: 63,
                sampler: self.sampler,
            },
            GpuBinding::Bytes {
                binding: 64,
                data: bytemuck::bytes_of(self.ripple),
            },
            GpuBinding::Buffer {
                binding: 65,
                buffer: self.velocity,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 70,
                buffer: self.tiles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 71,
                buffer: self.extent,
                offset: 0,
            },
        ]
    }
}

/// The surface draws' bindings: the narrowed view, the mesh and where it
/// sits.
fn draw_bindings<'b>(fit: &'b GpuBuffer, surface: &'b Surface<'_>) -> [GpuBinding<'b>; 3] {
    [
        GpuBinding::Buffer {
            binding: 6,
            buffer: fit,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 7,
            buffer: surface.vertices,
            offset: 0,
        },
        GpuBinding::Bytes {
            binding: 8,
            data: bytemuck::bytes_of(&surface.clear),
        },
    ]
}

struct Targets {
    side: u32,
    /// Every clear surface's side toward the sun, which shading reads too,
    /// and every clear mesh's far side, each with its normals and which
    /// surface it is.
    front: GpuTexture,
    front_normal: GpuTexture,
    back: GpuTexture,
    back_normal: GpuTexture,
    sums: GpuBuffer,
    map: GpuTexture,
    /// The clear surfaces' reach in the sun's view, then the narrowed view,
    /// written on the GPU each frame.
    bounds: GpuBuffer,
    fit: GpuBuffer,
}

impl Targets {
    fn new(device: &GpuDevice, side: u32) -> Result<Self, String> {
        let texture = |format, usage, label| {
            device.try_create_texture(&GpuTextureDesc {
                width: side,
                height: side,
                depth: 1,
                mip_levels: 1,
                format,
                dimension: GpuTextureDimension::D2,
                usage,
                label,
            })
        };
        let drawn = GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ;
        Ok(Self {
            side,
            front: texture(
                GpuTextureFormat::Depth32Float,
                drawn,
                "node.render_scene caustic front",
            )?,
            front_normal: texture(
                GpuTextureFormat::Rgba16Float,
                drawn,
                "node.render_scene caustic front normal",
            )?,
            back: texture(
                GpuTextureFormat::Depth32Float,
                drawn,
                "node.render_scene caustic back",
            )?,
            back_normal: texture(
                GpuTextureFormat::Rgba16Float,
                drawn,
                "node.render_scene caustic back normal",
            )?,
            sums: device
                .try_create_buffer(3 * 4 * u64::from(side) * u64::from(side), BufferUse::Gpu)?,
            map: texture(
                GpuTextureFormat::Rgba16Float,
                GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ,
                "node.render_scene caustic map",
            )?,
            bounds: device.try_create_buffer(16, BufferUse::Gpu)?,
            fit: device.try_create_buffer(FIT_BYTES, BufferUse::Gpu)?,
        })
    }
}

#[derive(Default)]
pub(super) struct Caustics {
    bounds: Option<GpuComputePipeline>,
    fit: Option<GpuComputePipeline>,
    sides: Option<GpuRenderPipeline>,
    splat: Option<GpuComputePipeline>,
    resolve: Option<GpuComputePipeline>,
    /// The far side's depth test: the farthest from the sun wins.
    far: Option<GpuDepthStencilState>,
    targets: Option<Targets>,
    /// The view scene shading reads when no caustics run: zeros, so off. It
    /// also stands in for the arrays a pass binds and never reads.
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

    /// The caustic map and the clear surfaces' depth, when this frame's
    /// caustics ran.
    pub(super) fn maps(&self) -> Option<(&GpuTexture, &GpuTexture)> {
        self.targets
            .as_ref()
            .filter(|_| self.ran)
            .map(|t| (&t.map, &t.front))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        sun: Option<Sun<'_>>,
        fields: &[FieldObject<'_>],
        draws: &[ObjectDraw<'_>],
        ripples: &RippleDetail,
        seconds: f32,
    ) -> Result<(), String> {
        self.ran = false;
        self.off.get_or_insert_with(|| {
            let off = device.create_buffer(FIT_BYTES, BufferUse::Upload);
            off.zero_fill();
            off
        });
        let Some(sun) = sun.filter(|_| surfaces(fields, draws).next().is_some()) else {
            self.targets = None;
            return Ok(());
        };
        let vp = sun.caster.view_proj;
        // 1 / the sun's shadow resolution, which a caster with a layer has.
        let texel = sun.caster.shadow[2];
        let side = ((1.0 / texel).round() as u32).min(MAX_SIDE);
        if self.targets.as_ref().is_none_or(|t| t.side != side) {
            self.targets = Some(Targets::new(device, side)?);
        }
        let targets = self.targets.as_ref().expect("ensured");
        let off = self.off.as_ref().expect("ensured");
        let far = self.far.get_or_insert_with(|| {
            device.create_depth_stencil_state(&GpuDepthStencilDesc {
                compare: GpuCompareFunction::Greater,
                write_enabled: true,
            })
        });

        // The water that takes the ripple detail sets how strongly every
        // caustic shows; clear meshes alone show them at their physical 1.
        let water = fields.iter().find(|field| casts_caustics(field));
        // The sun's depth per metre along its light: the length of its
        // projection's depth row, which the narrowed view keeps.
        let depth_per_metre =
            (vp[0][2] * vp[0][2] + vp[1][2] * vp[1][2] + vp[2][2] * vp[2][2]).sqrt();
        let splat = Splat {
            sun_view_proj: vp,
            inverse_sun_view_proj: mat4_inverse(vp)
                .ok_or_else(|| "Caustics: singular sun matrix".to_owned())?,
            toward_sun: [
                sun.caster.source[0],
                sun.caster.source[1],
                sun.caster.source[2],
                texel,
            ],
            sizes: [side, sun.slot as u32, sun.slot as u32 + 1, 0],
            bias: [
                SURFACE_BIAS_METRES * depth_per_metre,
                water.map_or(1.0, |water| water.caustics),
                0.0,
                0.0,
            ],
        };
        // The splat passes its texel's size in metres as the eye distance, so
        // the ripples are taken at one texel of the narrowed view. Without a
        // water the ripple bindings are stand-ins no photon reads.
        let tile = water.and_then(|water| ripples.tile(water.slot));
        let ripple = match water {
            Some(water) => ripple_uniforms(
                water,
                tile.map(|tile| tile.tile_metres),
                false,
                seconds,
                [1.0, 0.0],
            )?,
            None => bytemuck::Zeroable::zeroed(),
        };
        let mut clears = [bytemuck::Zeroable::zeroed(); MAX_CLEARS];
        for surface in surfaces(fields, draws) {
            clears[surface.clear.live[3] as usize] = surface.clear;
        }
        let shared = Shared {
            splat: &splat,
            clears: &clears,
            targets,
            shadow_maps: sun.shadow_maps,
            off,
            slopes: tile.map_or(ripples.dummy_slopes(), |tile| &tile.texture),
            sampler: ripples.sampler(),
            ripple: &ripple,
            velocity: water
                .and_then(|water| water.velocity)
                .unwrap_or(ripples.dummy_velocity()),
            tiles: water.map_or(off, |water| water.tiles),
            extent: water.map_or(off, |water| water.extent),
        };
        let make = |entry| {
            device.create_compute_pipeline(SHADER.as_str(), entry, "node.render_scene caustics")
        };

        // Every clear surface's reach in the sun's view, then the view
        // narrowed round them.
        let bounds_pipeline = self.bounds.get_or_insert_with(|| make("cs_bounds"));
        encoder.clear_buffer(&targets.bounds);
        for surface in surfaces(fields, draws) {
            let vertices = surface.vertices.size() / std::mem::size_of::<MeshVertex>() as u64;
            let groups = vertices.div_ceil(BOUNDS_GROUP).clamp(1, MAX_BOUNDS_GROUPS) as u32;
            encoder.dispatch_compute(
                bounds_pipeline,
                &shared.bindings(&surface),
                [groups, 1, 1],
                "node.render_scene caustic bounds",
            );
        }
        encoder.compute_memory_barrier_buffers();
        let fit_pipeline = self.fit.get_or_insert_with(|| make("cs_fit"));
        let first = surfaces(fields, draws).next().expect("checked above");
        encoder.dispatch_compute(
            fit_pipeline,
            &shared.bindings(&first),
            [1, 1, 1],
            "node.render_scene caustic fit",
        );

        // Every clear surface's side toward the sun, then every clear mesh's
        // far side (a water's light ends under it), in the narrowed view.
        let sides_pipeline = self.sides.get_or_insert_with(|| {
            device.create_render_pipeline(
                &RenderPipelineDesc::new(
                    SHADER.as_str(),
                    "vs_surface",
                    "fs_normal",
                    "node.render_scene caustic sides",
                )
                .color(GpuTextureFormat::Rgba16Float, None)
                .depth(GpuTextureFormat::Depth32Float),
            )
        });
        for far_side in [false, true] {
            let (normal, depth, state) = if far_side {
                (&targets.back_normal, &targets.back, &*far)
            } else {
                (&targets.front_normal, &targets.front, sun.depth_state)
            };
            let drawn = surfaces(fields, draws)
                .filter(|surface| !far_side || surface.clear.through[3] == 0.0);
            for (index, surface) in drawn.enumerate() {
                let load = if index == 0 {
                    GpuLoadAction::Clear
                } else {
                    GpuLoadAction::Load
                };
                let b = draw_bindings(&targets.fit, &surface);
                encoder.draw(
                    &DrawPass::color(normal, load).depth(depth, state, load, DepthStore::Store),
                    &[Draw::counted(sides_pipeline, &b, surface.count)],
                    "node.render_scene caustic sides",
                );
            }
        }

        // One photon per texel of the sides toward the sun.
        let splat_pipeline = self.splat.get_or_insert_with(|| make("cs_splat"));
        encoder.clear_buffer(&targets.sums);
        let groups = [side.div_ceil(16), side.div_ceil(16), 1];
        encoder.dispatch_compute(
            splat_pipeline,
            &shared.bindings(&first),
            groups,
            "node.render_scene caustic splat",
        );
        encoder.compute_memory_barrier_buffers();
        let resolve_pipeline = self.resolve.get_or_insert_with(|| make("cs_resolve"));
        encoder.dispatch_compute(
            resolve_pipeline,
            &shared.bindings(&first),
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

    /// The shader validates, and its uniforms are the sizes the Rust side
    /// writes.
    #[test]
    fn caustics_shader_validates() {
        let module = naga::front::wgsl::parse_str(&SHADER).expect("caustics parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("caustics validate");
        let size = |module: &naga::Module, name: &str| {
            let (_, ty) = module
                .types
                .iter()
                .find(|(_, t)| t.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("names its {name}"));
            u64::from(ty.inner.size(module.to_ctx()))
        };
        assert_eq!(size(&module, "Splat"), std::mem::size_of::<Splat>() as u64);
        assert_eq!(size(&module, "Clear"), std::mem::size_of::<Clear>() as u64);
        // Both sides' `CausticFit` are the size the stand-in is made.
        for source in [SHADER.as_str(), super::super::RENDER_SCENE_WGSL.as_str()] {
            let module = naga::front::wgsl::parse_str(source).expect("parses");
            assert_eq!(size(&module, "CausticFit"), FIT_BYTES);
        }
    }
}
