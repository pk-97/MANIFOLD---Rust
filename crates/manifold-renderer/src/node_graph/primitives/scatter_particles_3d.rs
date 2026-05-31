//! `node.scatter_particles_3d` — atomic-add splat of particles into
//! a 3D `u32` fixed-point accumulator buffer.
//!
//! Bit-exact wrap of `generators/shaders/fluid_scatter_3d.wgsl`'s
//! `splat_3d` entry point via `include_str!`. Each live particle's
//! `position.xyz` indexes a voxel in the `vol_res × vol_res ×
//! vol_depth` grid and contributes `scaled_energy` via `atomicAdd`.
//! Pair with `node.resolve_3d_accumulator` to lift the u32 grid into
//! a float Texture3D for downstream volumetric primitives.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Splat3DUniforms {
    active_count: u32,
    vol_res: u32,
    vol_depth: u32,
    scaled_energy: u32,
    // Naga requires every @binding(2) uniform in the shader module to
    // have the same size as the largest one (ProjectedUniforms = 112B).
    _pad0: [u32; 4],
    _pad1: [u32; 4],
    _pad2: [u32; 4],
    _pad3: [u32; 4],
    _pad4: [u32; 4],
    _pad5: [u32; 4],
}

crate::primitive! {
    name: ScatterParticles3D,
    type_id: "node.scatter_particles_3d",
    purpose: "Atomic-add splat of an Array<Particle> into a u32 3D accumulator buffer sized vol_res × vol_res × vol_depth. Each live particle's nearest-voxel cell receives `scaled_energy` via atomicAdd. Pair with node.resolve_3d_accumulator to lift the u32 grid into a float Texture3D for downstream volumetric primitives like blur_3d, gradient_curl_3d, project_particles_3d.",
    inputs: {
        particles: Array(Particle) required,
        active_count: ScalarF32 optional,
        scaled_energy: ScalarF32 optional,
    },
    outputs: {
        accum: Array(u32),
    },
    params: [
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "vol_res",
            label: "Volume Resolution",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "vol_depth",
            label: "Volume Depth",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scaled_energy",
            label: "Energy per Particle",
            ty: ParamType::Int,
            default: ParamValue::Float(4096.0),
            range: Some((1.0, 65536.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "vol_res × vol_res × vol_depth = total cells; accumulator buffer must be sized vol_res² × vol_depth × 4 bytes upstream. Particle positions are in [0,1]³; cells outside that range are toroidally wrapped (% vr / % vd). scaled_energy = 4096 ≈ 1.0 in float density after Resolve divides by 4096 (matches FluidSim3D convention).",
    examples: [],
    picker: { label: "Draw Particles (3D scatter)", category: Atom },
    summary: "Splats 3D particles into a volume buffer, building up a 3D density field from where they land. The 3D version of Draw Particles.",
    category: Particles3D,
    role: Filter,
    aliases: ["draw particles 3d", "scatter 3d", "splat", "volume"],
}

impl Primitive for ScatterParticles3D {
    /// Volume accumulator is `vol_res × vol_res × vol_depth` u32 cells.
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
            Some(ParamValue::Float(f)) => Some(f.round().max(1.0) as u32),
            _ => None,
        };
        let r = read_dim("vol_res")?;
        let d = read_dim("vol_depth")?;
        Some(r.saturating_mul(r).saturating_mul(d))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count =
            ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;
        let vol_res = match ctx.params.get("vol_res") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };
        let vol_depth = match ctx.params.get("vol_depth") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };
        let scaled_energy =
            ctx.scalar_or_param("scaled_energy", 4096.0).round().max(0.0) as u32;

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
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_scatter_3d.wgsl"),
                "splat_3d",
                "node.scatter_particles_3d",
            )
        });

        let uniforms = Splat3DUniforms {
            active_count,
            vol_res,
            vol_depth,
            scaled_energy,
            _pad0: [0; 4],
            _pad1: [0; 4],
            _pad2: [0; 4],
            _pad3: [0; 4],
            _pad4: [0; 4],
            _pad5: [0; 4],
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: accum,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.scatter_particles_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scatter_3d_declares_particle_in_and_u32_array_out() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let u32_layout = ArrayType::of_known::<u32>();

        assert_eq!(ScatterParticles3D::TYPE_ID, "node.scatter_particles_3d");
        assert_eq!(ScatterParticles3D::INPUTS[0].name, "particles");
        assert_eq!(
            ScatterParticles3D::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(ScatterParticles3D::INPUTS[0].required);
        // Port-shadow inputs let the JSON drive active_count + scaled_energy
        // from the outer-card particle-count / energy chains.
        for name in ["active_count", "scaled_energy"] {
            let port = ScatterParticles3D::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional port-shadow");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(ScatterParticles3D::OUTPUTS.len(), 1);
        assert_eq!(ScatterParticles3D::OUTPUTS[0].name, "accum");
        assert_eq!(
            ScatterParticles3D::OUTPUTS[0].ty,
            PortType::Array(u32_layout)
        );
    }

    #[test]
    fn scatter_3d_uniform_struct_matches_naga_padding() {
        // The shader's Splat3DUniforms is padded to match the largest
        // @binding(2) uniform (ProjectedUniforms = 112 bytes). The
        // Rust struct must match exactly.
        assert_eq!(std::mem::size_of::<Splat3DUniforms>(), 112);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ScatterParticles3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scatter_particles_3d");
    }
}
