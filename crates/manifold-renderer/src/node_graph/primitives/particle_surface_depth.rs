//! `node.particle_surface_depth` — S6 sphere-impostor depth/coverage raster.
//!
//! Renders the water surface as sphere impostors (one per live particle,
//! radius default 0.75*h): per pixel, the exact front ray-sphere hit
//! becomes a raw [0,1] clip depth through the shared projection convention
//! (`raw = range * (near/view_z - 1)`, the inverse of
//! `depth_common.wgsl`'s `linearize_depth`). Depth testing is `atomicMin`
//! on the f32 bit pattern — nearer impostors win, this is a real surface
//! raster, not additive point energy. `depth` is R32Float clip depth
//! (empty = 1), `coverage` is R8Unorm (0 empty, 1 occupied).
//!
//! Codegen gap (reported to the lead, S6 — same documented escape as the
//! S4 atomic stages): the splat is a scatter/atomic raster; a barrier-free
//! per-element body cannot express per-particle bounding-box emission with
//! cross-pixel depth testing. Hand-authored standalone kernels
//! (`particle_splat_common.wgsl` is the single source of the view/bbox/
//! ray-sphere math shared with `node.particle_thickness`).
//!
//! Camera and output dimensions are declared explicitly
//! (`output_canvas_scale` (1,1), `output_format` per port) — no texture
//! map can size the raster target. Perspective cameras only; the V1
//! rejects (near-plane-intersecting spheres, a camera inside a sphere —
//! an underwater view — and beyond-far spheres) are skipped in-kernel and
//! camera-level invalidity is an explicit `ctx.error` with empty outputs,
//! never constructed garbage (docs/WATER_SIMULATION_DESIGN.md section 7).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuTextureFormat};

use crate::node_graph::camera::CameraMode;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::{GRID_SPACING, WaterParticle};

use super::CUBE_HALF;

/// Shared splat helpers — see the module doc (single source with
/// `node.particle_thickness`).
pub const SURFACE_DEPTH_SPLAT_WGSL: &str = concat!(
    include_str!("shaders/particle_splat_common.wgsl"),
    "\n",
    include_str!("shaders/particle_surface_depth_splat.wgsl"),
);
pub const SURFACE_DEPTH_CLEAR_WGSL: &str =
    include_str!("shaders/particle_surface_depth_clear.wgsl");
pub const SURFACE_DEPTH_RESOLVE_WGSL: &str =
    include_str!("shaders/particle_surface_depth_resolve.wgsl");

/// Default impostor radius: 0.75 * h (design section 7).
pub const DEFAULT_RADIUS: f32 = 0.75 * GRID_SPACING;

/// Splat kernel uniform: the SplatView struct in `particle_splat_common.wgsl`,
/// 96 bytes (mat4 + 4 f32 + 4 u32, no padding traps).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SurfaceSplatUniforms {
    pub view: [[f32; 4]; 4],
    pub tan_half_fov: f32,
    pub near: f32,
    pub far: f32,
    pub radius: f32,
    pub width: u32,
    pub height: u32,
    pub count: u32,
    pub _pad: u32,
}

const _: () = assert!(core::mem::size_of::<SurfaceSplatUniforms>() == 96);

/// Optional solid clip for the reconstructed surface. `camera_to_world`
/// converts the splat kernel's +z-forward view frame back to world space;
/// `collider_center.w` is 1 when clipping is enabled. The collider matches
/// the solver's translating, axis-aligned box contract.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SurfaceColliderUniforms {
    pub camera_to_world: [[f32; 4]; 4],
    pub collider_center: [f32; 4],
    pub collider_half: [f32; 4],
}

const _: () = assert!(core::mem::size_of::<SurfaceColliderUniforms>() == 96);

/// Per-pixel kernel uniforms (clear/resolve): one u32 word each.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SurfacePixelUniforms {
    pub width: u32,
    pub height: u32,
    pub _pad0: u32,
    pub _pad1: u32,
}

const _: () = assert!(core::mem::size_of::<SurfacePixelUniforms>() == 16);

/// Cached scratch buffers, one entry per canvas size this node has seen.
pub struct SurfaceDepthScratch {
    width: u32,
    height: u32,
    depth_bits: GpuBuffer,
    coverage: GpuBuffer,
}

crate::primitive! {
    name: ParticleSurfaceDepth,
    type_id: "node.particle_surface_depth",
    purpose: "Sphere-impostor depth/coverage raster for water surface reconstruction (design section 7): splats each live WaterParticle as a sphere of radius `radius` (default 0.75*h) with per-pixel depth testing (atomicMin on the f32 bit pattern — nearer impostors win), writing raw [0,1] clip depth (R32Float, empty=1) and 0/1 coverage (R8Unorm). An optional collider Transform clips reconstructed hits inside the same translating AABB used by the solver, preventing smooth splats from protruding through the solid. Perspective camera only; near-plane-intersecting spheres, a camera inside a sphere (underwater view) and beyond-far spheres are rejected, never garbage depths. Pair with node.bilateral_blur (ClipDepth mode + coverage) and node.normals_from_depth for the smoothed surface.",
    inputs: {
        particles: Array(WaterParticle) required,
        shapes: Channels["surface_center_radius": Vec4F, "surface_axis_x": Vec4F, "surface_axis_y": Vec4F, "surface_axis_z": Vec4F] optional,
        camera: Camera required,
        collider: Transform optional,
        radius: ScalarF32 optional,
    },
    outputs: {
        depth: Texture2D,
        coverage: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Impostor Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_RADIUS),
            range: Some((0.001, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cube_half_x"),
            label: "Collider Half X",
            ty: ParamType::Float,
            default: ParamValue::Float(CUBE_HALF[0]),
            range: Some((0.001, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cube_half_y"),
            label: "Collider Half Y",
            ty: ParamType::Float,
            default: ParamValue::Float(CUBE_HALF[1]),
            range: Some((0.001, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("cube_half_z"),
            label: "Collider Half Z",
            ty: ParamType::Float,
            default: ParamValue::Float(CUBE_HALF[2]),
            range: Some((0.001, 2.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "First stage of the S6 water surface graph: particles + the shared Camera in; depth + coverage out. When the simulation uses node.water_collide_box, wire the accepted collider Transform here and keep cube_half_x/y/z identical so the reconstructed surface respects the physical AABB. Wire coverage + depth into node.bilateral_blur (value_space=ClipDepth) for the H/V smoothed surface, then node.normals_from_depth (depth + coverage + camera) for view normals. Output is always full canvas resolution (design section 7: no half-res before correctness).",
    examples: [],
    picker: { label: "Particle Surface Depth", category: Atom },
    summary: "Renders water particles as depth-tested sphere impostors — the surface depth map the rest of the water shading builds on.",
    category: Particles3D,
    role: Filter,
    aliases: ["water surface depth", "particle depth raster", "sphere splat depth", "impostor depth", "surface raster"],
    boundary_reason: Blocked,
    extra_fields: {
        clear_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        resolve_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        anisotropic_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        scratch: Option<SurfaceDepthScratch> = None,
    },
}

impl Primitive for ParticleSurfaceDepth {
    /// Explicit output declarations: canvas-sized (no texture input may
    /// size the raster target) and per-port formats.
    fn output_canvas_scale(
        &self,
        port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        match port {
            "depth" | "coverage" => Some((1, 1)),
            _ => None,
        }
    }

    fn output_format(&self, port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
        match port {
            "depth" => Some(GpuTextureFormat::R32Float),
            "coverage" => Some(GpuTextureFormat::R8Unorm),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(depth_tex) = ctx.outputs.texture_2d("depth") else {
            return;
        };
        let Some(coverage_tex) = ctx.outputs.texture_2d("coverage") else {
            return;
        };
        let (w, h) = (depth_tex.width, depth_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let radius = ctx.scalar_or_param("radius", DEFAULT_RADIUS);
        let cam = ctx.inputs.camera("camera");

        // Camera-level validation: explicit diagnostic + empty outputs
        // (depth clear = 1, coverage clear = 0), never garbage.
        let invalid = match &cam {
            None => Some("node.particle_surface_depth: missing required `camera` input"),
            Some(c) => {
                if !matches!(c.mode, CameraMode::Perspective { .. }) {
                    Some(
                        "node.particle_surface_depth: perspective camera required (orthographic rejected in V1)",
                    )
                } else if !(c.near.is_finite()
                    && c.far.is_finite()
                    && c.near > 0.0
                    && c.near < c.far)
                {
                    Some(
                        "node.particle_surface_depth: camera near/far must be finite with 0 < near < far",
                    )
                } else {
                    None
                }
            }
        };
        if !radius.is_finite() || radius <= 0.0 {
            invalid_camera_clear(ctx, depth_tex, coverage_tex);
            ctx.error(format!(
                "node.particle_surface_depth: radius must be finite and positive, got {radius}"
            ));
            return;
        }
        if let Some(msg) = invalid {
            invalid_camera_clear(ctx, depth_tex, coverage_tex);
            ctx.error(msg);
            return;
        }
        let cam = cam.expect("validated above");
        let CameraMode::Perspective { fov_y } = cam.mode else {
            unreachable!("validated perspective above");
        };
        if !fov_y.is_finite() || fov_y <= 0.0 || fov_y >= std::f32::consts::PI {
            invalid_camera_clear(ctx, depth_tex, coverage_tex);
            ctx.error(format!(
                "node.particle_surface_depth: camera fov_y must be finite and in (0, pi), got {fov_y}"
            ));
            return;
        }

        let Some(particles) = ctx.inputs.array("particles") else {
            // Required port unwired: empty surface (all-empty clear).
            invalid_camera_clear(ctx, depth_tex, coverage_tex);
            return;
        };
        let capacity = (particles.size / std::mem::size_of::<WaterParticle>() as u64) as u32;
        if capacity == 0 {
            invalid_camera_clear(ctx, depth_tex, coverage_tex);
            return;
        }
        let shapes = ctx.inputs.array("shapes");
        if let Some(s) = shapes
            && s.size < u64::from(capacity) * 64
        {
            invalid_camera_clear(ctx, depth_tex, coverage_tex);
            ctx.error("node.particle_surface_depth: shapes capacity must match particles");
            return;
        }
        let collider = ctx.inputs.transform("collider");
        let read_half = |name: &str, default: f32| match ctx.params.get(name) {
            Some(ParamValue::Float(value)) => *value,
            _ => default,
        };
        let collider_half = [
            read_half("cube_half_x", CUBE_HALF[0]),
            read_half("cube_half_y", CUBE_HALF[1]),
            read_half("cube_half_z", CUBE_HALF[2]),
        ];
        if collider.is_some()
            && collider_half
                .iter()
                .any(|half| !half.is_finite() || *half <= 0.0)
        {
            invalid_camera_clear(ctx, depth_tex, coverage_tex);
            ctx.error(
                "node.particle_surface_depth: collider half-extents must be finite and positive",
            );
            return;
        }

        let gpu = ctx.gpu_encoder();

        // Scratch buffers, recreated when the canvas size changes.
        let needs_new = self
            .scratch
            .as_ref()
            .is_none_or(|s| s.width != w || s.height != h);
        if needs_new {
            let pixels = u64::from(w) * u64::from(h) * 4;
            self.scratch = Some(SurfaceDepthScratch {
                width: w,
                height: h,
                depth_bits: gpu.device.create_buffer(pixels),
                coverage: gpu.device.create_buffer(pixels),
            });
        }
        let scratch = self.scratch.as_ref().expect("scratch created above");

        let clear_pipeline = self.clear_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                SURFACE_DEPTH_CLEAR_WGSL,
                "cs_main",
                "node.particle_surface_depth.clear",
            )
        });
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                SURFACE_DEPTH_SPLAT_WGSL,
                "cs_main",
                "node.particle_surface_depth.splat",
            )
        });
        let anisotropic = self.anisotropic_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                SURFACE_DEPTH_SPLAT_WGSL,
                "cs_anisotropic",
                "node.particle_surface_depth.splat.anisotropic",
            )
        });
        let resolve_pipeline = self.resolve_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                SURFACE_DEPTH_RESOLVE_WGSL,
                "cs_main",
                "node.particle_surface_depth.resolve",
            )
        });

        let pixel_count = w * h;
        let pixel_uniforms = SurfacePixelUniforms {
            width: w,
            height: h,
            _pad0: 0,
            _pad1: 0,
        };
        let splat_uniforms = SurfaceSplatUniforms {
            view: cam.view,
            tan_half_fov: (fov_y * 0.5).tan(),
            near: cam.near,
            far: cam.far,
            radius,
            width: w,
            height: h,
            count: capacity,
            _pad: 0,
        };
        let collider_uniforms = surface_collider_uniforms(&cam, collider, collider_half);

        // 1. Clear: depth-bits scratch to clip-depth 1.0; coverage to 0.
        gpu.native_enc.dispatch_compute(
            clear_pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&pixel_uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &scratch.depth_bits,
                    offset: 0,
                },
            ],
            [pixel_count.div_ceil(256), 1, 1],
            "node.particle_surface_depth.clear",
        );
        gpu.native_enc.clear_buffer(&scratch.coverage);

        // 2. Splat: one thread per particle slot; rejected/ inactive slots
        // exit early.
        let legacy = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&splat_uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: particles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &scratch.depth_bits,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &scratch.coverage,
                offset: 0,
            },
            GpuBinding::Bytes {
                binding: 5,
                data: bytemuck::bytes_of(&collider_uniforms),
            },
        ];
        let shaped = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&splat_uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: particles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &scratch.depth_bits,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &scratch.coverage,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: shapes.unwrap_or(particles),
                offset: 0,
            },
            GpuBinding::Bytes {
                binding: 5,
                data: bytemuck::bytes_of(&collider_uniforms),
            },
        ];
        if shapes.is_some() {
            gpu.native_enc.dispatch_compute(
                anisotropic,
                &shaped,
                [capacity.div_ceil(256), 1, 1],
                "node.particle_surface_depth.splat.anisotropic",
            );
        } else {
            gpu.native_enc.dispatch_compute(
                pipeline,
                &legacy,
                [capacity.div_ceil(256), 1, 1],
                "node.particle_surface_depth.splat",
            );
        }

        // 3. Resolve: scratch -> R32Float depth + R8Unorm coverage.
        gpu.native_enc.dispatch_compute(
            resolve_pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&pixel_uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &scratch.depth_bits,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &scratch.coverage,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: depth_tex,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: coverage_tex,
                },
            ],
            [pixel_count.div_ceil(256), 1, 1],
            "node.particle_surface_depth.resolve",
        );
    }
}

fn surface_collider_uniforms(
    cam: &crate::node_graph::camera::Camera,
    collider: Option<crate::node_graph::transform::Transform>,
    half: [f32; 3],
) -> SurfaceColliderUniforms {
    let center = collider.map(|value| value.pos).unwrap_or([0.0; 3]);
    SurfaceColliderUniforms {
        // Columns of an affine view-frame -> world transform. The splat
        // frame uses +z along Camera::fwd.
        camera_to_world: [
            [cam.right[0], cam.right[1], cam.right[2], 0.0],
            [cam.up[0], cam.up[1], cam.up[2], 0.0],
            [cam.fwd[0], cam.fwd[1], cam.fwd[2], 0.0],
            [cam.pos[0], cam.pos[1], cam.pos[2], 1.0],
        ],
        collider_center: [
            center[0],
            center[1],
            center[2],
            if collider.is_some() { 1.0 } else { 0.0 },
        ],
        collider_half: [half[0], half[1], half[2], 0.0],
    }
}

/// Error fallback: empty surface — depth cleared to clip depth 1 (empty),
/// coverage to 0 (the section 7 "no garbage" convention).
fn invalid_camera_clear(
    ctx: &mut EffectNodeContext<'_, '_>,
    depth_tex: &manifold_gpu::GpuTexture,
    coverage_tex: &manifold_gpu::GpuTexture,
) {
    let gpu = ctx.gpu_encoder();
    gpu.native_enc.clear_texture(depth_tex, 1.0, 0.0, 0.0, 1.0);
    gpu.native_enc
        .clear_texture(coverage_tex, 0.0, 0.0, 0.0, 0.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::ports::PortType;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn water_surface_depth_declares_explicit_outputs() {
        assert_eq!(ParticleSurfaceDepth::TYPE_ID, "node.particle_surface_depth");
        let names: Vec<&str> = ParticleSurfaceDepth::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec!["particles", "shapes", "camera", "collider", "radius"]
        );
        assert!(ParticleSurfaceDepth::INPUTS[0].required);
        assert!(!ParticleSurfaceDepth::INPUTS[1].required);
        assert!(ParticleSurfaceDepth::INPUTS[2].required);
        assert!(!ParticleSurfaceDepth::INPUTS[3].required);
        assert!(!ParticleSurfaceDepth::INPUTS[4].required);
        let out_names: Vec<&str> = ParticleSurfaceDepth::OUTPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(out_names, vec!["depth", "coverage"]);

        // Output dims are canvas-declared (never sized by a texture map)
        // and formats are pinned per port.
        let prim = ParticleSurfaceDepth::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        assert_eq!(
            Primitive::output_canvas_scale(&prim, "depth", &params),
            Some((1, 1))
        );
        assert_eq!(
            Primitive::output_canvas_scale(&prim, "coverage", &params),
            Some((1, 1))
        );
        assert_eq!(
            Primitive::output_format(&prim, "depth"),
            Some(GpuTextureFormat::R32Float)
        );
        assert_eq!(
            Primitive::output_format(&prim, "coverage"),
            Some(GpuTextureFormat::R8Unorm)
        );
        assert_eq!(Primitive::output_format(&prim, "other"), None);
    }

    #[test]
    fn water_surface_depth_particles_wire_is_water_particle() {
        let particles = ParticleSurfaceDepth::INPUTS
            .iter()
            .find(|p| p.name == "particles")
            .expect("particles input");
        assert_eq!(
            particles.ty,
            PortType::Array(crate::node_graph::ports::ArrayType::of_known::<WaterParticle>())
        );
    }

    #[test]
    fn water_surface_depth_radius_default_is_three_quarters_h() {
        let radius = ParticleSurfaceDepth::PARAMS
            .iter()
            .find(|p| p.name == "radius")
            .expect("radius param");
        assert_eq!(radius.default, ParamValue::Float(0.75 * GRID_SPACING));
        assert_eq!(DEFAULT_RADIUS, 0.046875);
    }

    #[test]
    fn water_surface_depth_registers_as_palette_atom() {
        let prim = ParticleSurfaceDepth::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.particle_surface_depth");
    }
}
