//! `node.fluid_simulate` — main per-frame integrator for the
//! FluidSim family. Bit-exact wrap of
//! `generators/shaders/fluid_simulate.wgsl` via include_str.
//!
//! Reads particle positions from an Array<Particle>, samples the
//! blurred vector field at each particle's UV, adds simplex-noise
//! advection (density-adaptive), per-particle diffusion, and an
//! optional injection disturbance — integrates one Euler step with
//! toroidal wrap. Dead+visible particles get revived; excess
//! particles outside the wrap region die.
//!
//! Wire upstream: node.fluid_seed (initialise on clip-trigger), then
//! [particles] → node.fluid_simulate every frame.
//! Wire field/density inputs from node.fluid_gradient_rotate and
//! node.resolve_accumulator (or whatever produces the density
//! texture).

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FluidSimUniforms {
    active_count: u32,
    field_width: u32,
    field_height: u32,
    speed: f32,
    noise_amplitude: f32,
    density_noise_gain: f32,
    diffusion: f32,
    frame_count: u32,
    inject_point_x: f32,
    inject_point_y: f32,
    inject_force: f32,
    inject_phase: f32,
    time_val: f32,
    dt: f32,
    visible_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: FluidSimulate,
    type_id: "node.fluid_simulate",
    purpose: "Per-frame FluidSim integrator. Samples a vector force field + density texture at each particle's UV, applies simplex-noise advection (density-adaptive), per-particle diffusion, and an optional injection disturbance — integrates one Euler step with toroidal wrap. Pair upstream with node.fluid_seed (init / clip-trigger) and node.fluid_gradient_rotate (field). Excess particles (i >= visible_count) die at the wrap boundary; dead+visible particles get revived.",
    inputs: {
        in: Array(Particle) required,
        field: Texture2D required,
        density: Texture2D required,
        speed: ScalarF32 optional,
        inject_force: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Int(100_000),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "visible_count",
            label: "Visible Count",
            ty: ParamType::Int,
            default: ParamValue::Int(100_000),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "speed",
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "noise_amplitude",
            label: "Noise Amplitude",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0005),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: "density_noise_gain",
            label: "Density-Noise Gain",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.0, 20.0)),
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
            name: "inject_point_x",
            label: "Inject X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "inject_point_y",
            label: "Inject Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "inject_force",
            label: "Inject Force",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "inject_phase",
            label: "Inject Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "speed and inject_force are port-shadowed (can be driven by upstream scalar wires). Vector field sampling uses bilinear; density uses bilinear. field_width / field_height are read from the field texture's dimensions automatically — no need to pass as params. For full FluidSim parity: wire node.fluid_gradient_rotate's output to `field` and node.resolve_accumulator's output to `density`.",
    examples: [],
    picker: { label: "Fluid Simulate", category: Atom },
    extra_fields: {
        density_sampler: Option<manifold_gpu::GpuSampler> = None,
    },
}

impl Primitive for FluidSimulate {
    /// Output `out` is sized to match input `in` — particle simulation
    /// is in-place (one particle in, one particle out).
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = match ctx.params.get("active_count") {
            Some(ParamValue::Int(n)) => (*n).max(0) as u32,
            _ => 100_000,
        };
        let visible_count = match ctx.params.get("visible_count") {
            Some(ParamValue::Int(n)) => (*n).max(0) as u32,
            _ => 100_000,
        };
        let speed = match ctx.inputs.scalar("speed") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("speed") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };
        let noise_amplitude = match ctx.params.get("noise_amplitude") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0005,
        };
        let density_noise_gain = match ctx.params.get("density_noise_gain") {
            Some(ParamValue::Float(f)) => *f,
            _ => 2.0,
        };
        let diffusion = match ctx.params.get("diffusion") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let inject_point_x = match ctx.params.get("inject_point_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };
        let inject_point_y = match ctx.params.get("inject_point_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };
        let inject_force = match ctx.inputs.scalar("inject_force") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("inject_force") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let inject_phase = match ctx.params.get("inject_phase") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(field) = ctx.inputs.texture_2d("field") else {
            return;
        };
        let Some(density) = ctx.inputs.texture_2d("density") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        // Output shares the input's slot for in-place mutation (same
        // pattern as integrate_particles).
        let _ = out_buf;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (in_buf.size / particle_size) as u32;
        let active_count = active_count.min(capacity);
        let visible_count = visible_count.min(active_count);

        let time_val = ctx.time.seconds.0 as f32;
        let dt = ctx.time.delta.0 as f32;
        let frame_count = ctx.time.frame_count as u32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_simulate.wgsl"),
                "main",
                "node.fluid_simulate",
            )
        });
        let field_sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let density_sampler = self
            .density_sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = FluidSimUniforms {
            active_count,
            field_width: field.width,
            field_height: field.height,
            speed,
            noise_amplitude,
            density_noise_gain,
            diffusion,
            frame_count,
            inject_point_x,
            inject_point_y,
            inject_force,
            inject_phase,
            time_val,
            dt,
            visible_count,
            _pad0: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: field,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: field_sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: density,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler: density_sampler,
                },
                GpuBinding::Bytes {
                    binding: 5,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.fluid_simulate",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn fluid_simulate_declares_three_required_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType {
            item_size: std::mem::size_of::<Particle>() as u32,
            item_align: std::mem::align_of::<Particle>() as u32,
        };

        assert_eq!(FluidSimulate::TYPE_ID, "node.fluid_simulate");
        assert_eq!(FluidSimulate::INPUTS.len(), 5);
        assert_eq!(FluidSimulate::INPUTS[0].name, "in");
        assert!(FluidSimulate::INPUTS[0].required);
        assert_eq!(
            FluidSimulate::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert_eq!(FluidSimulate::INPUTS[1].name, "field");
        assert_eq!(FluidSimulate::INPUTS[1].ty, PortType::Texture2D);
        assert!(FluidSimulate::INPUTS[1].required);
        assert_eq!(FluidSimulate::INPUTS[2].name, "density");
        assert_eq!(FluidSimulate::INPUTS[2].ty, PortType::Texture2D);
        assert!(FluidSimulate::INPUTS[2].required);
        assert_eq!(FluidSimulate::INPUTS[3].name, "speed");
        assert!(!FluidSimulate::INPUTS[3].required);
        assert_eq!(FluidSimulate::INPUTS[4].name, "inject_force");
        assert!(!FluidSimulate::INPUTS[4].required);
    }

    #[test]
    fn fluid_simulate_has_full_param_surface() {
        let names: Vec<&str> = FluidSimulate::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "active_count",
                "visible_count",
                "speed",
                "noise_amplitude",
                "density_noise_gain",
                "diffusion",
                "inject_point_x",
                "inject_point_y",
                "inject_force",
                "inject_phase",
            ]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FluidSimulate::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.fluid_simulate");
    }
}
