//! Caustics: every shadow-casting light's light bent by the clear surfaces
//! onto what lies under them (`caustics.wgsl`). A clear surface is a meshed
//! water with caustics, or a clear mesh that casts shadows and draws once (no
//! copies): goo, jelly, glass. Each lit light whose view may hold a clear
//! surface runs once, after the shadow maps, in its own view: a sun's or a
//! spot's shadow view, or for a point light one cone toward the clear
//! surfaces across its cube faces. A GPU step narrows that view to the clear
//! surfaces' bounds; in it every clear surface draws its side toward the
//! light, which also tells shading what lies under one, and every clear mesh
//! its far side. One photon per texel of the sides toward the light refracts
//! through its surface onto the opaque scene in the light's shadow maps and
//! adds the light that gets through to that light's layer of the caustic
//! map. Scene shading reads the layer of each light's caster slot, in its
//! narrowed view, for that light on anything under a clear surface
//! (`caustic_light` in `scene_shading.wgsl`). The shadow maps hold no clear
//! surface, so the map is its shadow too. A light that cannot reach a clear
//! mesh is skipped here; one with no clear surface found on the GPU writes
//! nothing and reads off.
use manifold_gpu::BufferUse;
use std::sync::LazyLock;

use manifold_gpu::*;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::scene::live_extent::LiveExtent;

use super::ripple_detail::RippleDetail;
use super::water_shade::{RippleUniforms, ripple_uniforms};
use manifold_node_engine::scene::light::{Light, LightMode};
use manifold_node_engine::scene::mesh_bounds::MeshBounds;

use super::frustum::box_outside_clip;
use super::{
    FIELD_TILES_WGSL, FieldObject, ObjectDraw, mat4_inverse, mat4_mul, shadow_map_resolution,
};

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

/// The caustic map's largest side; the lights' shadow resolution when smaller.
const MAX_SIDE: u32 = 1024;

/// How far under a clear surface, in metres along the light, a point
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
    view_proj: [[f32; 4]; 4],
    inverse_view_proj: [[f32; 4]; 4],
    light: [f32; 4],
    sizes: [u32; 4],
    bias: [f32; 4],
    shadow_views: [[[f32; 4]; 4]; 6],
    inverse_shadow_views: [[[f32; 4]; 4]; 6],
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
/// matrices, the slot, the bias, the intensity and the layer. All zeros
/// reads off.
const FIT_BYTES: u64 = 144;

/// The lights the caustics follow: one shadow view per caster slot, the
/// shadow maps holding their layers, drawn this frame, and the depth state
/// they draw with.
pub(super) struct Lights<'a> {
    pub views: &'a [Light],
    pub shadow_maps: &'a GpuTexture,
    pub depth_state: &'a GpuDepthStencilState,
}

/// Half the cone a point light's caustic view spans: the clear surfaces
/// within 75 degrees of their centre, seen from the light.
const POINT_HALF_ANGLE: f32 = 75.0 * std::f32::consts::PI / 180.0;

/// The middle of this frame's clear surfaces: a clear mesh's box centre, a
/// water's origin.
fn clear_centre(fields: &[FieldObject<'_>], draws: &[ObjectDraw<'_>]) -> [f32; 3] {
    let mut sum = [0.0; 3];
    let mut count = 0.0_f32;
    for surface in surfaces(fields, draws) {
        let (m, local) = match surface.bounds {
            Some((model, b)) => (model, std::array::from_fn(|k| 0.5 * (b.min[k] + b.max[k]))),
            None => (surface.clear.model, [0.0; 3]),
        };
        for (k, total) in sum.iter_mut().enumerate() {
            *total += m[0][k] * local[0] + m[1][k] * local[1] + m[2][k] * local[2] + m[3][k];
        }
        count += 1.0;
    }
    sum.map(|total| total / count.max(1.0))
}

/// A point light's caustic view: a cone from the light toward the clear
/// surfaces' `centre`, as deep as its cube faces (`face` is the first).
fn toward_clear(face: &Light, centre: [f32; 3]) -> Light {
    let to: [f32; 3] = std::array::from_fn(|k| centre[k] - face.pos[k]);
    let length = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
    let dir = if length > 1e-4 {
        to.map(|c| c / length)
    } else {
        [0.0, -1.0, 0.0]
    };
    let depth = (0..3)
        .map(|k| (face.aim[k] - face.pos[k]).powi(2))
        .sum::<f32>()
        .sqrt();
    Light {
        mode: LightMode::Spot,
        aim: std::array::from_fn(|k| face.pos[k] + dir[k] * depth),
        dir,
        outer_cone_angle: POINT_HALF_ANGLE,
        ..*face
    }
}

/// One clear surface this frame: its mesh, how it draws, what it does to the
/// light, and its box where the CPU knows it (a clear mesh's, not a water's).
struct Surface<'a> {
    vertices: &'a GpuBuffer,
    count: DrawCount<'a>,
    live: Option<&'a LiveExtent>,
    clear: Clear,
    bounds: Option<([[f32; 4]; 4], &'a MeshBounds)>,
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
                bounds: None,
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
                bounds: draw.bounds.as_ref().map(|b| (u.model, b)),
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

/// What every pass of one view binds beside the surface it is on.
struct Shared<'a> {
    splat: &'a Splat,
    clears: &'a [Clear; MAX_CLEARS],
    targets: &'a Targets,
    /// This view's layers of the surface depth and the caustic map, and the
    /// views of every caster slot.
    front: &'a GpuTexture,
    map: &'a GpuTexture,
    fits: &'a GpuBuffer,
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
                texture: self.front,
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
                texture: self.map,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: self.fits,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: self.fits,
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

/// The surface draws' bindings: the view, its narrowing, the mesh and where
/// it sits.
fn draw_bindings<'b>(
    splat: &'b Splat,
    fits: &'b GpuBuffer,
    surface: &'b Surface<'_>,
) -> [GpuBinding<'b>; 4] {
    [
        GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(splat),
        },
        GpuBinding::Buffer {
            binding: 6,
            buffer: fits,
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
    /// Every clear surface's side toward each view's light, one layer per
    /// view, which shading reads too.
    front: GpuTexture,
    fronts: Vec<GpuTexture>,
    /// The light each view puts through, one layer per view.
    map: GpuTexture,
    maps: Vec<GpuTexture>,
    /// Scratch one view at a time: the sides' normals, every clear mesh's
    /// far side, the photon sums, and the clear surfaces' reach in the view.
    front_normal: GpuTexture,
    back: GpuTexture,
    back_normal: GpuTexture,
    sums: GpuBuffer,
    bounds: GpuBuffer,
}

impl Targets {
    fn new(device: &GpuDevice, side: u32, layers: u32) -> Result<Self, String> {
        let texture = |format, usage, layers, label| {
            device.try_create_texture(&GpuTextureDesc {
                width: side,
                height: side,
                depth: layers,
                mip_levels: 1,
                format,
                dimension: if layers > 1 {
                    GpuTextureDimension::D2Array
                } else {
                    GpuTextureDimension::D2
                },
                usage,
                label,
            })
        };
        let array = |format, usage, label| {
            device.try_create_texture(&GpuTextureDesc {
                width: side,
                height: side,
                depth: layers,
                mip_levels: 1,
                format,
                dimension: GpuTextureDimension::D2Array,
                usage,
                label,
            })
        };
        let drawn = GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ;
        let front = array(
            GpuTextureFormat::Depth32Float,
            drawn,
            "node.render_scene caustic front",
        )?;
        let map = array(
            GpuTextureFormat::Rgba16Float,
            GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ,
            "node.render_scene caustic map",
        )?;
        Ok(Self {
            side,
            fronts: (0..layers).map(|layer| front.layer_view(layer)).collect(),
            maps: (0..layers).map(|layer| map.layer_view(layer)).collect(),
            front,
            map,
            front_normal: texture(
                GpuTextureFormat::Rgba16Float,
                drawn,
                1,
                "node.render_scene caustic front normal",
            )?,
            back: texture(
                GpuTextureFormat::Depth32Float,
                drawn,
                1,
                "node.render_scene caustic back",
            )?,
            back_normal: texture(
                GpuTextureFormat::Rgba16Float,
                drawn,
                1,
                "node.render_scene caustic back normal",
            )?,
            sums: device
                .try_create_buffer(3 * 4 * u64::from(side) * u64::from(side), BufferUse::Gpu)?,
            bounds: device.try_create_buffer(16, BufferUse::Gpu)?,
        })
    }
}

/// What shading binds when no caustics ran: one zeroed view, which reads
/// off, and one-layer stand-ins for the map and the surface.
struct Off {
    fits: GpuBuffer,
    map: GpuTexture,
    surface: GpuTexture,
}

#[derive(Default)]
pub(super) struct Caustics {
    bounds: Option<GpuComputePipeline>,
    fit: Option<GpuComputePipeline>,
    sides: Option<GpuRenderPipeline>,
    splat: Option<GpuComputePipeline>,
    resolve: Option<GpuComputePipeline>,
    /// The far side's depth test: the farthest from the light wins.
    far: Option<GpuDepthStencilState>,
    targets: Option<Targets>,
    /// One view per caster slot, written on the GPU each frame.
    fits: Option<GpuBuffer>,
    /// Also stands in for the arrays a pass binds and never reads.
    off: Option<Off>,
    ran: bool,
}

impl Caustics {
    /// What scene shading binds: the caustic map, the clear surfaces' depth
    /// and every caster slot's view when this frame's caustics ran, else
    /// stand-ins that read off. None only before the first `encode`.
    pub(super) fn bindings(&self) -> Option<(&GpuTexture, &GpuTexture, &GpuBuffer)> {
        match (&self.targets, &self.fits) {
            (Some(targets), Some(fits)) if self.ran => Some((&targets.map, &targets.front, fits)),
            _ => self
                .off
                .as_ref()
                .map(|off| (&off.map, &off.surface, &off.fits)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        lights: Option<Lights<'_>>,
        fields: &[FieldObject<'_>],
        draws: &[ObjectDraw<'_>],
        ripples: &RippleDetail,
        seconds: f32,
    ) -> Result<(), String> {
        self.ran = false;
        if self.off.is_none() {
            let fits = device.create_buffer(FIT_BYTES, BufferUse::Upload);
            fits.zero_fill();
            let stand_in = |format, usage, label| {
                device.create_texture(&GpuTextureDesc {
                    width: 1,
                    height: 1,
                    depth: 1,
                    mip_levels: 1,
                    format,
                    dimension: GpuTextureDimension::D2Array,
                    usage,
                    label,
                })
            };
            self.off = Some(Off {
                fits,
                map: stand_in(
                    GpuTextureFormat::Rgba16Float,
                    GpuTextureUsage::SHADER_READ,
                    "node.render_scene caustic map off",
                ),
                surface: stand_in(
                    GpuTextureFormat::Depth32Float,
                    GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
                    "node.render_scene caustic surface off",
                ),
            });
        }
        let Some(lights) = lights.filter(|_| surfaces(fields, draws).next().is_some()) else {
            self.targets = None;
            return Ok(());
        };
        // Every lit light whose view may hold a clear surface: a water's
        // reach is known only on the GPU, a clear mesh's box here. A point
        // light's six cube faces share one view toward the clear surfaces,
        // so light bent across a face's edge still lands.
        let water_drawn = surfaces(fields, draws).any(|surface| surface.bounds.is_none());
        let centre = clear_centre(fields, draws);
        let views = || {
            let mut slot = 0;
            std::iter::from_fn(move || {
                let light = lights.views.get(slot)?;
                let (view, faces) = if light.mode == LightMode::Point {
                    let faces = lights.views.get(slot..slot + 6)?;
                    (toward_clear(light, centre), Some(faces))
                } else {
                    (*light, None)
                };
                let at = slot;
                slot += faces.map_or(1, <[Light]>::len);
                Some((at, light, view, faces))
            })
            .filter(move |(_, light, view, _)| {
                let vp = view.shadow_view_proj();
                light.color[..3].iter().any(|&channel| channel > 0.0)
                    && (water_drawn
                        || surfaces(fields, draws).any(|surface| {
                            surface
                                .bounds
                                .is_some_and(|(model, b)| !box_outside_clip(&vp, &model, b))
                        }))
            })
            .map(|(slot, _, view, faces)| (slot, view, faces))
        };
        let layers = views().count() as u32;
        if layers == 0 {
            self.targets = None;
            return Ok(());
        }
        let side = views()
            .map(|(_, view, _)| shadow_map_resolution(view.shadow_resolution))
            .max()
            .unwrap_or(MAX_SIDE)
            .min(MAX_SIDE);
        if self
            .targets
            .as_ref()
            .is_none_or(|t| t.side != side || (t.maps.len() as u32) < layers)
        {
            self.targets = Some(Targets::new(device, side, layers)?);
        }
        let fit_bytes = FIT_BYTES * lights.views.len() as u64;
        if self
            .fits
            .as_ref()
            .is_none_or(|fits| fits.size() < fit_bytes)
        {
            self.fits = Some(device.try_create_buffer(fit_bytes, BufferUse::Gpu)?);
        }
        let targets = self.targets.as_ref().expect("ensured");
        let fits = self.fits.as_ref().expect("ensured");
        let off = &self.off.as_ref().expect("ensured").fits;
        let far = self.far.get_or_insert_with(|| {
            device.create_depth_stencil_state(&GpuDepthStencilDesc {
                compare: GpuCompareFunction::Greater,
                write_enabled: true,
            })
        });
        // Slots with no view read off.
        encoder.clear_buffer(fits);

        // The water that takes the ripple detail sets how strongly every
        // caustic shows; clear meshes alone show them at their physical 1.
        let water = fields.iter().find(|field| casts_caustics(field));
        // The ripple bindings: without a water they are stand-ins no photon
        // reads.
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
        let make = |entry| {
            device.create_compute_pipeline(SHADER.as_str(), entry, "node.render_scene caustics")
        };
        let bounds_pipeline = &*self.bounds.get_or_insert_with(|| make("cs_bounds"));
        let fit_pipeline = &*self.fit.get_or_insert_with(|| make("cs_fit"));
        let splat_pipeline = &*self.splat.get_or_insert_with(|| make("cs_splat"));
        let resolve_pipeline = &*self.resolve.get_or_insert_with(|| make("cs_resolve"));
        let sides_pipeline = &*self.sides.get_or_insert_with(|| {
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

        for (layer, (slot, view, faces)) in views().enumerate() {
            let inverse =
                |vp| mat4_inverse(vp).ok_or_else(|| "Caustics: singular light matrix".to_owned());
            let vp = view.shadow_view_proj();
            let texel = 1.0 / shadow_map_resolution(view.shadow_resolution) as f32;
            let toward = match (view.mode, faces) {
                (LightMode::Sun, _) => [-view.dir[0], -view.dir[1], -view.dir[2], 0.0],
                (_, Some(_)) => [view.pos[0], view.pos[1], view.pos[2], 2.0],
                (_, None) => [view.pos[0], view.pos[1], view.pos[2], 1.0],
            };
            // The scene's shadow views: a point light's cube faces, else the
            // light's own.
            let mut shadow_views = [[[0.0; 4]; 4]; 6];
            let mut inverse_shadow_views = [[[0.0; 4]; 4]; 6];
            for (face, light) in faces
                .unwrap_or(std::slice::from_ref(&view))
                .iter()
                .enumerate()
            {
                shadow_views[face] = light.shadow_view_proj();
                inverse_shadow_views[face] = inverse(shadow_views[face])?;
            }
            let splat = Splat {
                view_proj: vp,
                inverse_view_proj: inverse(vp)?,
                light: toward,
                sizes: [side, slot as u32, slot as u32 + 1, layer as u32],
                bias: [
                    SURFACE_BIAS_METRES,
                    water.map_or(1.0, |water| water.caustics),
                    texel,
                    0.0,
                ],
                shadow_views,
                inverse_shadow_views,
            };
            let shared = Shared {
                splat: &splat,
                clears: &clears,
                targets,
                front: &targets.fronts[layer],
                map: &targets.maps[layer],
                fits,
                shadow_maps: lights.shadow_maps,
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

            // Every clear surface's reach in the light's view, then the view
            // narrowed round them.
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
            let first = surfaces(fields, draws).next().expect("checked above");
            encoder.dispatch_compute(
                fit_pipeline,
                &shared.bindings(&first),
                [1, 1, 1],
                "node.render_scene caustic fit",
            );

            // Every clear surface's side toward the light, then every clear
            // mesh's far side (a water's light ends under it), in the
            // narrowed view.
            for far_side in [false, true] {
                let (normal, depth, state) = if far_side {
                    (&targets.back_normal, &targets.back, &*far)
                } else {
                    (&targets.front_normal, shared.front, lights.depth_state)
                };
                let drawn = surfaces(fields, draws)
                    .filter(|surface| !far_side || surface.clear.through[3] == 0.0);
                for (index, surface) in drawn.enumerate() {
                    let load = if index == 0 {
                        GpuLoadAction::Clear
                    } else {
                        GpuLoadAction::Load
                    };
                    let b = draw_bindings(&splat, fits, &surface);
                    encoder.draw(
                        &DrawPass::color(normal, load).depth(depth, state, load, DepthStore::Store),
                        &[Draw::counted(sides_pipeline, &b, surface.count)],
                        "node.render_scene caustic sides",
                    );
                }
            }

            // One photon per texel of the sides toward the light.
            encoder.clear_buffer(&targets.sums);
            let groups = [side.div_ceil(16), side.div_ceil(16), 1];
            encoder.dispatch_compute(
                splat_pipeline,
                &shared.bindings(&first),
                groups,
                "node.render_scene caustic splat",
            );
            encoder.compute_memory_barrier_buffers();
            encoder.dispatch_compute(
                resolve_pipeline,
                &shared.bindings(&first),
                groups,
                "node.render_scene caustic resolve",
            );
        }
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
