//! `node.scatter_particles` — atomic-add splat of particles into a
//! `u32` fixed-point accumulator.
//!
//! Phase A.7 of `BUFFER_PORT_PLAN`. Reads particle positions from
//! an Array(Particle) input and writes to an Array(u32) accumulator
//! buffer sized `width × height`. Each live particle adds the
//! configured `scaled_energy` to its nearest texel via `atomicAdd`.
//!
//! The accumulator is cleared at dispatch time (one full-grid
//! `atomicStore(0)` pass) before the splat — so the consumer
//! reads a fresh frame each tick. Pair with
//! [`crate::node_graph::primitives::ResolveAccumulator`] to lift
//! the u32 grid into a float texture for downstream texture ops.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScatterUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    scaled_energy: u32,
}

crate::primitive! {
    name: ScatterParticles,
    type_id: "node.scatter_particles",
    purpose: "Atomic-add splat of particles into a u32 fixed-point accumulator buffer sized width×height. Each live particle contributes `scaled_energy` to its nearest texel; the buffer is cleared at the start of each dispatch. Pair with `node.resolve_accumulator` to read the result as a float texture.",
    inputs: {
        particles: Array(Particle) required,
    },
    outputs: {
        accum: Array(u32),
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
            name: "width",
            label: "Accumulator Width",
            ty: ParamType::Int,
            default: ParamValue::Int(960),
            range: Some((16.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "height",
            label: "Accumulator Height",
            ty: ParamType::Int,
            default: ParamValue::Int(540),
            range: Some((16.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scaled_energy",
            label: "Energy per Particle",
            ty: ParamType::Int,
            default: ParamValue::Int(4096),
            range: Some((1.0, 65536.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output accumulator buffer is u32 fixed-point. `scaled_energy = 4096` ≈ 1.0 in float after Resolve divides by FIXED_POINT_SCALE — matching the FluidSim convention. Width × height should match the velocity/density grid resolution used by Integrate.",
    examples: [],
    picker: { label: "Scatter Particles", category: Atom },
    extra_fields: {
        splat_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
    },
}

impl Primitive for ScatterParticles {
    /// Accumulator buffer is `width × height` u32 cells. Read both
    /// params, multiply, return the cell count. The pre-allocator
    /// multiplies by `item_size` (4) to get bytes.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "accum" {
            return None;
        }
        let read_dim = |name| match params.get(name) {
            Some(ParamValue::Int(n)) => Some((*n).max(1) as u32),
            Some(ParamValue::Float(f)) => Some(f.round().max(1.0) as u32),
            _ => None,
        };
        Some(read_dim("width")? * read_dim("height")?)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = match ctx.params.get("active_count") {
            Some(ParamValue::Int(n)) => (*n).max(0) as u32,
            _ => 100_000,
        };
        let width = match ctx.params.get("width") {
            Some(ParamValue::Int(n)) => (*n).max(1) as u32,
            _ => 960,
        };
        let height = match ctx.params.get("height") {
            Some(ParamValue::Int(n)) => (*n).max(1) as u32,
            _ => 540,
        };
        let scaled_energy = match ctx.params.get("scaled_energy") {
            Some(ParamValue::Int(n)) => (*n).max(0) as u32,
            _ => 4096,
        };

        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(accum) = ctx.outputs.array("accum") else {
            return;
        };

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let active_count = active_count.min(particle_capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline_clear = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/scatter_particles.wgsl"),
                "clear_main",
                "node.scatter_particles.clear",
            )
        });

        let uniforms = ScatterUniforms {
            active_count,
            width,
            height,
            scaled_energy,
        };

        // Pass 1: zero the accumulator. 16×16 workgroups cover the grid.
        gpu.native_enc.dispatch_compute(
            pipeline_clear,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: accum,
                    offset: 0,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.scatter_particles.clear",
        );

        let pipeline_splat = self.splat_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/scatter_particles.wgsl"),
                "splat_main",
                "node.scatter_particles.splat",
            )
        });

        // Pass 2: atomic-add splat. 256-particle workgroups along x.
        gpu.native_enc.dispatch_compute(
            pipeline_splat,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: accum,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.scatter_particles.splat",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scatter_particles_declares_array_in_and_array_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType {
            item_size: std::mem::size_of::<Particle>() as u32,
            item_align: std::mem::align_of::<Particle>() as u32,
        };
        let u32_layout = ArrayType {
            item_size: 4,
            item_align: 4,
        };

        assert_eq!(ScatterParticles::TYPE_ID, "node.scatter_particles");
        assert_eq!(ScatterParticles::INPUTS.len(), 1);
        assert_eq!(ScatterParticles::INPUTS[0].name, "particles");
        assert_eq!(
            ScatterParticles::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );

        assert_eq!(ScatterParticles::OUTPUTS.len(), 1);
        assert_eq!(ScatterParticles::OUTPUTS[0].name, "accum");
        assert_eq!(ScatterParticles::OUTPUTS[0].ty, PortType::Array(u32_layout));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ScatterParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scatter_particles");
    }
}
