//! `node.particle_thickness` — S6 additive sphere-chord thickness raster.
//!
//! Companion to `node.particle_surface_depth`: same sphere-impostor
//! rasterisation (`particle_splat_common.wgsl` is the single source of the
//! view/bbox/ray-sphere math), but the per-pixel emit accumulates the
//! ray's chord length through every impostor it crosses
//! (2 * sqrt(discriminant), in metres). This is an APPROXIMATE optical
//! thickness by construction — sphere-splat chord sums, not an exact volume
//! integral (documented in docs/WATER_SIMULATION_DESIGN.md section 7) — the
//! refraction pass in S7 consumes it as Beer-Lambert path length.
//! Accumulation is a bounded CAS loop on the non-negative f32 bit pattern.
//!
//! Output is `thickness` R16Float (empty = 0), full canvas resolution only
//! (design section 7: no half-res before correctness). Codegen gap: same
//! documented scatter/atomic-raster escape as `node.particle_surface_depth`.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuTextureFormat};

use crate::node_graph::camera::CameraMode;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::WaterParticle;

use super::particle_surface_depth::{DEFAULT_RADIUS, SurfacePixelUniforms, SurfaceSplatUniforms};

/// Shared splat helpers — see `node.particle_surface_depth` (single source).
pub const THICKNESS_SPLAT_WGSL: &str = concat!(
    include_str!("shaders/particle_splat_common.wgsl"),
    "\n",
    include_str!("shaders/particle_thickness_splat.wgsl"),
);
pub const THICKNESS_RESOLVE_WGSL: &str = include_str!("shaders/particle_thickness_resolve.wgsl");

pub struct ThicknessScratch {
    width: u32,
    height: u32,
    thickness: GpuBuffer,
}

crate::primitive! {
    name: ParticleThickness,
    type_id: "node.particle_thickness",
    purpose: "Additive sphere-chord thickness raster for water (design section 7): splats each live WaterParticle as a sphere of radius `radius` (default 0.75*h) and accumulates the length of each pixel ray's chord through every impostor (2*sqrt(discriminant), metres). Approximate optical thickness by construction — a sphere-splat chord sum, not an exact volume integral; S7's refraction pass consumes it as Beer-Lambert path length. Output R16Float, empty=0, full canvas resolution. Perspective camera only; same V1 rejects as node.particle_surface_depth (near-plane cross, camera inside a sphere, beyond far are skipped, never garbage).",
    inputs: {
        particles: Array(WaterParticle) required,
        shapes: Channels["surface_center_radius": Vec4F, "surface_axis_x": Vec4F, "surface_axis_y": Vec4F, "surface_axis_z": Vec4F] optional,
        camera: Camera required,
        radius: ScalarF32 optional,
    },
    outputs: {
        thickness: Texture2D,
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
    ],
    depth_rule: Terminal,
    composition_notes: "Second raster of the S6 water surface graph: same particles + camera as node.particle_surface_depth (same shared Camera wire), thickness out. The smoothed depth comes from node.bilateral_blur; this raw per-ray optical depth feeds S7's refractive shading.",
    examples: [],
    picker: { label: "Particle Thickness", category: Atom },
    summary: "Accumulates how much water each view ray passes through — the approximate optical thickness refraction needs.",
    category: Particles3D,
    role: Filter,
    aliases: ["water thickness", "particle thickness", "optical thickness", "chord raster"],
    boundary_reason: Blocked,
    extra_fields: {
        resolve_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        anisotropic_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        scratch: Option<ThicknessScratch> = None,
    },
}

impl Primitive for ParticleThickness {
    fn output_canvas_scale(
        &self,
        port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        match port {
            "thickness" => Some((1, 1)),
            _ => None,
        }
    }

    fn output_format(&self, port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
        match port {
            "thickness" => Some(GpuTextureFormat::R16Float),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(thickness_tex) = ctx.outputs.texture_2d("thickness") else {
            return;
        };
        let (w, h) = (thickness_tex.width, thickness_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let radius = ctx.scalar_or_param("radius", DEFAULT_RADIUS);
        let cam = ctx.inputs.camera("camera");

        let invalid = match &cam {
            None => Some("node.particle_thickness: missing required `camera` input"),
            Some(c) => {
                if !matches!(c.mode, CameraMode::Perspective { .. }) {
                    Some(
                        "node.particle_thickness: perspective camera required (orthographic rejected in V1)",
                    )
                } else if !(c.near.is_finite()
                    && c.far.is_finite()
                    && c.near > 0.0
                    && c.near < c.far)
                {
                    Some(
                        "node.particle_thickness: camera near/far must be finite with 0 < near < far",
                    )
                } else {
                    None
                }
            }
        };
        let clear = |ctx: &mut EffectNodeContext<'_, '_>| {
            let gpu = ctx.gpu_encoder();
            gpu.native_enc
                .clear_texture(thickness_tex, 0.0, 0.0, 0.0, 0.0);
        };
        if !radius.is_finite() || radius <= 0.0 {
            clear(ctx);
            ctx.error(format!(
                "node.particle_thickness: radius must be finite and positive, got {radius}"
            ));
            return;
        }
        if let Some(msg) = invalid {
            clear(ctx);
            ctx.error(msg);
            return;
        }
        let cam = cam.expect("validated above");
        let CameraMode::Perspective { fov_y } = cam.mode else {
            unreachable!("validated perspective above");
        };
        if !fov_y.is_finite() || fov_y <= 0.0 || fov_y >= std::f32::consts::PI {
            clear(ctx);
            ctx.error(format!(
                "node.particle_thickness: camera fov_y must be finite and in (0, pi), got {fov_y}"
            ));
            return;
        }

        let Some(particles) = ctx.inputs.array("particles") else {
            clear(ctx);
            return;
        };
        let capacity = (particles.size / std::mem::size_of::<WaterParticle>() as u64) as u32;
        if capacity == 0 {
            clear(ctx);
            return;
        }
        let shapes = ctx.inputs.array("shapes");
        if let Some(s) = shapes
            && s.size < u64::from(capacity) * 64
        {
            ctx.gpu_encoder()
                .native_enc
                .clear_texture(thickness_tex, 0., 0., 0., 0.);
            ctx.error("node.particle_thickness: shapes capacity must match particles");
            return;
        }

        let gpu = ctx.gpu_encoder();

        let needs_new = self
            .scratch
            .as_ref()
            .is_none_or(|s| s.width != w || s.height != h);
        if needs_new {
            let pixels = u64::from(w) * u64::from(h) * 4;
            self.scratch = Some(ThicknessScratch {
                width: w,
                height: h,
                thickness: gpu.device.create_buffer(pixels),
            });
        }
        let scratch = self.scratch.as_ref().expect("scratch created above");

        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                THICKNESS_SPLAT_WGSL,
                "cs_main",
                "node.particle_thickness.splat",
            )
        });
        let anisotropic = self.anisotropic_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                THICKNESS_SPLAT_WGSL,
                "cs_anisotropic",
                "node.particle_thickness.splat.anisotropic",
            )
        });
        let resolve_pipeline = self.resolve_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                THICKNESS_RESOLVE_WGSL,
                "cs_main",
                "node.particle_thickness.resolve",
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

        // 1. Clear: additive scratch zeroes (0.0f32 bits).
        gpu.native_enc.clear_buffer(&scratch.thickness);

        // 2. Splat: bounded CAS chord accumulation.
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
                buffer: &scratch.thickness,
                offset: 0,
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
                buffer: &scratch.thickness,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: shapes.unwrap_or(particles),
                offset: 0,
            },
        ];
        if shapes.is_some() {
            gpu.native_enc.dispatch_compute(
                anisotropic,
                &shaped,
                [capacity.div_ceil(256), 1, 1],
                "node.particle_thickness.splat.anisotropic",
            );
        } else {
            gpu.native_enc.dispatch_compute(
                pipeline,
                &legacy,
                [capacity.div_ceil(256), 1, 1],
                "node.particle_thickness.splat",
            );
        }

        // 3. Resolve: scratch -> R16Float thickness.
        gpu.native_enc.dispatch_compute(
            resolve_pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&pixel_uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &scratch.thickness,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: thickness_tex,
                },
            ],
            [pixel_count.div_ceil(256), 1, 1],
            "node.particle_thickness.resolve",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::ports::PortType;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn water_thickness_declares_explicit_output() {
        assert_eq!(ParticleThickness::TYPE_ID, "node.particle_thickness");
        let out_names: Vec<&str> = ParticleThickness::OUTPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(out_names, vec!["thickness"]);

        let prim = ParticleThickness::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        assert_eq!(
            Primitive::output_canvas_scale(&prim, "thickness", &params),
            Some((1, 1))
        );
        assert_eq!(
            Primitive::output_format(&prim, "thickness"),
            Some(GpuTextureFormat::R16Float)
        );
        assert_eq!(Primitive::output_format(&prim, "depth"), None);
    }

    #[test]
    fn water_thickness_particles_wire_is_water_particle() {
        let particles = ParticleThickness::INPUTS
            .iter()
            .find(|p| p.name == "particles")
            .expect("particles input");
        assert_eq!(
            particles.ty,
            PortType::Array(crate::node_graph::ports::ArrayType::of_known::<WaterParticle>())
        );
        let camera = ParticleThickness::INPUTS
            .iter()
            .find(|p| p.name == "camera")
            .expect("camera input");
        assert_eq!(camera.ty, PortType::Camera);
        assert!(camera.required);
    }

    #[test]
    fn water_thickness_registers_as_palette_atom() {
        let prim = ParticleThickness::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.particle_thickness");
    }
}
