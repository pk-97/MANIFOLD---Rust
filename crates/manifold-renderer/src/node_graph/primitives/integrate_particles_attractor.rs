//! `node.integrate_particles_attractor` — RK2 ODE integration of
//! particles through one of five strange-attractor formulas
//! (Lorenz / Rössler / Aizawa / Thomas / Halvorsen).
//!
//! Bit-exact wrap of `generators/shaders/strange_attractor_simulate.wgsl`
//! via include_str. Two pipelines: `cs_simulate` (per-frame
//! integration step, 8 RK2 sub-steps per dispatch) and `cs_seed`
//! (init pass, 50-step warmup + stagger so particles land at
//! different attractor phases).
//!
//! Drives the StrangeAttractor generator family. Pair with
//! node.scatter_particles + node.resolve_accumulator + tone-mapping
//! to assemble a complete attractor visualisation graph; or route
//! the particles through any other downstream consumer.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const ATTRACTOR_TYPES: &[&str] =
    &["Lorenz", "Rössler", "Aizawa", "Thomas", "Halvorsen"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AttractorUniforms {
    attractor_type: u32,
    particle_count: u32,
    frame_count: u32,
    _pad0: u32,
    chaos: f32,
    cam_angle: f32,
    cam_tilt: f32,
    aspect: f32,
    diffusion: f32,
    attractor_dt: f32,
    uv_scale: f32,
    attractor_scale: f32,
    attractor_center: [f32; 3],
    _pad1: f32,
}

crate::primitive! {
    name: IntegrateParticlesAttractor,
    type_id: "node.integrate_particles_attractor",
    purpose: "Integrate an Array<Particle> through a strange-attractor ODE (Lorenz / Rössler / Aizawa / Thomas / Halvorsen) using RK2 sub-stepping with 3D→2D projection. Drives the StrangeAttractor generator family and any \"chaotic trajectory\" graph. Pair with node.scatter_particles → node.resolve_accumulator → tone-mapping for a complete attractor visualisation.",
    inputs: {
        in: Array(Particle) required,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: "attractor_type",
            label: "Attractor",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: ATTRACTOR_TYPES,
        },
        ParamDef {
            name: "particle_count",
            label: "Particle Count",
            ty: ParamType::Int,
            default: ParamValue::Int(500_000),
            range: Some((1024.0, 8_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "chaos",
            label: "Chaos",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_angle",
            label: "Camera Angle",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: "cam_tilt",
            label: "Camera Tilt",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "aspect",
            label: "Aspect Ratio",
            ty: ParamType::Float,
            default: ParamValue::Float(1.777),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "diffusion",
            label: "Diffusion",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: "attractor_dt",
            label: "Attractor dt",
            ty: ParamType::Float,
            default: ParamValue::Float(0.002),
            range: Some((0.0001, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: "uv_scale",
            label: "UV Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "attractor_scale",
            label: "Attractor Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.05),
            range: Some((0.001, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "seed_now",
            label: "Seed Now",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
    ],
    composition_notes: "seed_now=true dispatches the cs_seed kernel for one frame, initialising every particle with 50 RK2 warmup steps + a 0..N stagger so particles land at different attractor phases (visible structure on first render). Toggle off after the first init frame, or pulse it on attractor_type change. Toggle false dispatches cs_simulate (8 RK2 sub-steps per frame). For continuous seed-on-change behaviour, drive seed_now from upstream logic that detects param changes.",
    examples: [],
    picker: { label: "Integrate Particles Attractor", category: Atom },
    extra_fields: {
        seed_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
    },
}

impl Primitive for IntegrateParticlesAttractor {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let attractor_type = match ctx.params.get("attractor_type") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let particle_count = match ctx.params.get("particle_count") {
            Some(ParamValue::Int(n)) => (*n).max(0) as u32,
            _ => 500_000,
        };
        let chaos = match ctx.params.get("chaos") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.3,
        };
        let cam_angle = match ctx.params.get("cam_angle") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let cam_tilt = match ctx.params.get("cam_tilt") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.3,
        };
        let aspect = match ctx.params.get("aspect") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.777,
        };
        let diffusion = match ctx.params.get("diffusion") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let attractor_dt = match ctx.params.get("attractor_dt") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.002,
        };
        let uv_scale = match ctx.params.get("uv_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let attractor_scale = match ctx.params.get("attractor_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.05,
        };
        let seed_now = matches!(ctx.params.get("seed_now"), Some(ParamValue::Bool(true)));

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out_buf;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (in_buf.size / particle_size) as u32;
        let particle_count = particle_count.min(capacity);

        let frame_count = ctx.time.frame_count as u32;

        let gpu = ctx.gpu_encoder();
        let pipeline = if seed_now {
            self.seed_pipeline.get_or_insert_with(|| {
                gpu.device.create_compute_pipeline(
                    include_str!(
                        "../../generators/shaders/strange_attractor_simulate.wgsl"
                    ),
                    "cs_seed",
                    "node.integrate_particles_attractor.seed",
                )
            })
        } else {
            self.pipeline.get_or_insert_with(|| {
                gpu.device.create_compute_pipeline(
                    include_str!(
                        "../../generators/shaders/strange_attractor_simulate.wgsl"
                    ),
                    "cs_simulate",
                    "node.integrate_particles_attractor.simulate",
                )
            })
        };

        let uniforms = AttractorUniforms {
            attractor_type,
            particle_count,
            frame_count,
            _pad0: 0,
            chaos,
            cam_angle,
            cam_tilt,
            aspect,
            diffusion,
            attractor_dt,
            uv_scale,
            attractor_scale,
            attractor_center: [0.0, 0.0, 0.0],
            _pad1: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_buf,
                    offset: 0,
                },
            ],
            [particle_count.div_ceil(256), 1, 1],
            "node.integrate_particles_attractor",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn integrate_attractor_declares_particle_in_and_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType {
            item_size: std::mem::size_of::<Particle>() as u32,
            item_align: std::mem::align_of::<Particle>() as u32,
        };
        assert_eq!(
            IntegrateParticlesAttractor::TYPE_ID,
            "node.integrate_particles_attractor"
        );
        assert_eq!(IntegrateParticlesAttractor::INPUTS.len(), 1);
        assert_eq!(
            IntegrateParticlesAttractor::INPUTS[0].ty,
            PortType::Array(layout)
        );
        assert_eq!(IntegrateParticlesAttractor::OUTPUTS.len(), 1);
        assert_eq!(
            IntegrateParticlesAttractor::OUTPUTS[0].ty,
            PortType::Array(layout)
        );
    }

    #[test]
    fn integrate_attractor_has_five_attractor_options() {
        let p = IntegrateParticlesAttractor::PARAMS
            .iter()
            .find(|p| p.name == "attractor_type")
            .unwrap();
        assert_eq!(p.ty, ParamType::Enum);
        assert_eq!(p.enum_values.len(), 5);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = IntegrateParticlesAttractor::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.integrate_particles_attractor");
    }
}
