//! Depth-masked foam coverage raster for water particles.
use super::particle_surface_depth::{DEFAULT_RADIUS, SurfacePixelUniforms, SurfaceSplatUniforms};
use crate::node_graph::camera::CameraMode;
use crate::node_graph::effect_node::{EffectNodeContext, NodeRequires};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::WaterParticle;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuTextureFormat};
use std::borrow::Cow;
pub const FOAM_DEFAULT_RADIUS: f32 = DEFAULT_RADIUS;

pub const FOAM_SPLAT_WGSL: &str = concat!(
    include_str!("shaders/particle_splat_common.wgsl"),
    "\n",
    include_str!("shaders/particle_foam_splat.wgsl")
);
pub const FOAM_RESOLVE_WGSL: &str = include_str!("shaders/particle_thickness_resolve.wgsl");

/// Compile both raster stages during renderer installation.
pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
    let _ = device.create_compute_pipeline(FOAM_SPLAT_WGSL, "cs_main", "node.particle_foam.splat");
    let _ =
        device.create_compute_pipeline(FOAM_RESOLVE_WGSL, "cs_main", "node.particle_foam.resolve");
}

pub struct FoamScratch {
    width: u32,
    height: u32,
    bits: GpuBuffer,
}

crate::primitive! {
    name: ParticleFoam,
    type_id: "node.particle_foam",
    purpose: "Depth-masked sphere foam coverage raster for water; output R16Float coverage in [0,1].",
    inputs: { particles: Array(WaterParticle) required, foam: Array(f32) required, depth: Texture2D required, camera: Camera required, radius: ScalarF32 optional },
    outputs: { coverage: Texture2D },
    params: [ParamDef { name: Cow::Borrowed("radius"), label: "Impostor Radius", ty: ParamType::Float, default: ParamValue::Float(FOAM_DEFAULT_RADIUS), range: Some((0.001, 1.0)), enum_values: &[] }],
    depth_rule: Terminal,
    composition_notes: "Depth-masked near-surface foam coverage for water shading.",
    examples: [], picker: { label: "Particle Foam", category: Atom }, summary: "Rasterizes near-surface particle foam.", category: Particles3D, role: Filter, aliases: ["water foam"], boundary_reason: Blocked,
    extra_fields: { resolve_pipeline: Option<manifold_gpu::GpuComputePipeline> = None, scratch: Option<FoamScratch> = None, },
}

impl Primitive for ParticleFoam {
    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: false,
            gpu_encoder: true,
        }
    }
    fn output_canvas_scale(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        (port == "coverage").then_some((1, 1))
    }
    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        (port == "coverage").then_some(GpuTextureFormat::R16Float)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(out) = ctx.outputs.texture_2d("coverage") else {
            return;
        };
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(foam) = ctx.inputs.array("foam") else {
            return;
        };
        let Some(depth) = ctx.inputs.texture_2d("depth") else {
            return;
        };
        let Some(cam) = ctx.inputs.camera("camera") else {
            gpu_clear(ctx, out);
            ctx.error("node.particle_foam: missing required `camera` input");
            return;
        };
        let (w, h) = (out.width, out.height);
        if w == 0 || h == 0 {
            return;
        }
        if depth.width != w || depth.height != h {
            gpu_clear(ctx, out);
            ctx.error("node.particle_foam: depth dimensions must match coverage output");
            return;
        }
        let CameraMode::Perspective { fov_y } = cam.mode else {
            gpu_clear(ctx, out);
            ctx.error("node.particle_foam: perspective camera required");
            return;
        };
        if !(cam.near.is_finite() && cam.far.is_finite() && cam.near > 0.0 && cam.near < cam.far) {
            gpu_clear(ctx, out);
            ctx.error("node.particle_foam: camera near/far must be finite with 0 < near < far");
            return;
        }
        if !fov_y.is_finite() || fov_y <= 0.0 || fov_y >= std::f32::consts::PI {
            gpu_clear(ctx, out);
            ctx.error("node.particle_foam: camera fov_y must be finite and in (0, pi)");
            return;
        }
        let radius = ctx.scalar_or_param("radius", FOAM_DEFAULT_RADIUS);
        if !radius.is_finite() || radius <= 0.0 {
            gpu_clear(ctx, out);
            ctx.error("node.particle_foam: radius must be finite and positive");
            return;
        }
        let count = ((particles.size as usize) / std::mem::size_of::<WaterParticle>())
            .min((foam.size as usize) / 4) as u32;
        if count == 0 {
            gpu_clear(ctx, out);
            return;
        }
        let gpu = ctx.gpu_encoder();
        if self
            .scratch
            .as_ref()
            .is_none_or(|s| s.width != w || s.height != h)
        {
            self.scratch = Some(FoamScratch {
                width: w,
                height: h,
                bits: gpu.device.create_buffer(u64::from(w) * u64::from(h) * 4),
            });
        }
        let s = self.scratch.as_ref().unwrap();
        gpu.native_enc.clear_buffer(&s.bits);
        let pipe = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                FOAM_SPLAT_WGSL,
                "cs_main",
                "node.particle_foam.splat",
            )
        });
        let resolve = self.resolve_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                FOAM_RESOLVE_WGSL,
                "cs_main",
                "node.particle_foam.resolve",
            )
        });
        let su = SurfaceSplatUniforms {
            view: cam.view,
            tan_half_fov: (fov_y * 0.5).tan(),
            near: cam.near,
            far: cam.far,
            radius,
            width: w,
            height: h,
            count,
            _pad: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipe,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&su),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: foam,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: depth,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &s.bits,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.particle_foam.splat",
        );
        let pu = SurfacePixelUniforms {
            width: w,
            height: h,
            _pad0: 0,
            _pad1: 0,
        };
        let n = w * h;
        gpu.native_enc.dispatch_compute(
            resolve,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&pu),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &s.bits,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: out,
                },
            ],
            [n.div_ceil(256), 1, 1],
            "node.particle_foam.resolve",
        );
    }
}
fn gpu_clear(ctx: &mut EffectNodeContext<'_, '_>, t: &manifold_gpu::GpuTexture) {
    ctx.gpu_encoder()
        .native_enc
        .clear_texture(t, 0., 0., 0., 0.);
}
