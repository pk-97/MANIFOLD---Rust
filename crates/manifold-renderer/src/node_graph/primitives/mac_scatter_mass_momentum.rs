//! `node.mac_scatter_mass_momentum` — APIC particle-to-MAC-face transfer.
//!
//! This is Jiang et al. 2015, section 6, equations 12–13: each particle's
//! mass and axis component of affine velocity are scattered to the eight
//! trilinear faces surrounding each particle.  The padded 65³ entry layout is
//! six signed Q20 integers per entry, `[mass_x, momentum_x, mass_y,
//! momentum_y, mass_z, momentum_z]`.  Invalid particles and incomplete face
//! stencils set the existing sticky water status bits; no contribution is
//! clipped at the domain boundary.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::{PARTICLE_CAPACITY, WaterParticle};

/// MAC entries are padded to one common 65³ lattice.  The component-specific
/// face domains use only the legal subset of that lattice (normal coordinate
/// 0..=64 and tangential coordinates 0..63).
pub const MAC_GRID_EDGE: u32 = 65;
pub const MAC_GRID_ENTRIES: u32 = MAC_GRID_EDGE * MAC_GRID_EDGE * MAC_GRID_EDGE;
pub const MAC_ACCUM_SLOTS: u32 = MAC_GRID_ENTRIES * 6;

/// Hand-authored standalone kernel.  The scatter has two aliased atomic
/// outputs, which the generated buffer wrapper intentionally rejects.
pub const WGSL: &str = concat!(
    include_str!("shaders/water_common.wgsl"),
    "\n",
    include_str!("shaders/mac_scatter_mass_momentum.wgsl"),
);

/// Hand-kernel uniform layout: the live particle count and padding to a
/// 16-byte Metal uniform block.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MacScatterUniforms {
    pub active_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub _pad2: u32,
}

crate::primitive! {
    name: MacScatterMassMomentum,
    type_id: "node.mac_scatter_mass_momentum",
    purpose: "APIC particle-to-MAC transfer (Jiang et al. 2015, equations 12–13): scatter each live WaterParticle's mass and axis affine velocity onto the eight trilinear faces around it. The padded 65³ accumulator stores six signed Q=2^20 i32 slots per entry in [mass_x, momentum_x, mass_y, momentum_y, mass_z, momentum_z] order. Incomplete face stencils, nonfinite particle state, quantisation failure, checked-add overflow, and bounded CAS exhaustion set the existing sticky water status bits; no boundary contribution is clipped. The atomic output aliases the accumulator wire and status_out aliases the status wire.",
    inputs: {
        particles: Array(WaterParticle) required,
        accumulator: Channels["water_grid_accum": I32] required,
        status: Array(u32) required,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Channels["water_grid_accum": I32],
        status_out: Array(u32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("active_count"),
            label: "Active Particles",
            ty: ParamType::Int,
            default: ParamValue::Float(PARTICLE_CAPACITY as f32),
            range: Some((0.0, 2_000_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "APIC replacement for node.mpm_scatter_mass_momentum when the solver uses a staggered MAC grid. The six accumulator slots per padded entry are [mass_x, momentum_x, mass_y, momentum_y, mass_z, momentum_z]. Run after a clear of the complete MAC accumulator and before node.mac_resolve. `active_count` is port-shadowed and only bounds the particle dispatch; zero-mass records remain inactive.",
    examples: [],
    picker: { label: "MAC APIC Scatter", category: Atom },
    summary: "Scatters particle mass and affine momentum onto the staggered MAC faces around each particle.",
    category: Particles3D,
    role: Filter,
    aliases: ["mac scatter", "apic scatter", "mac p2g", "particle to mac"],
    boundary_reason: Blocked,

}

impl Primitive for MacScatterMassMomentum {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let from = match port_name {
            "out" => "accumulator",
            "status_out" => "status",
            _ => return None,
        };
        input_capacities
            .iter()
            .find(|(p, _)| *p == from)
            .map(|(_, n)| *n)
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("accumulator", "out"), ("status", "status_out")]
    }

    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(particles) = ctx.inputs.array("particles") else {
            ctx.mark_gpu_accessed();
            return;
        };
        let Some(accum) = ctx.outputs.array("out") else {
            ctx.mark_gpu_accessed();
            return;
        };
        let Some(status_out) = ctx.outputs.array("status_out") else {
            ctx.mark_gpu_accessed();
            return;
        };

        let accumulator_capacity = (accum.size / std::mem::size_of::<i32>() as u64) as u32;
        if accumulator_capacity < MAC_ACCUM_SLOTS || status_out.size < 4 {
            // There is no safe way to report a capacity fault through a
            // possibly undersized alias.  Refuse the dispatch before any
            // particle can partially update the wire and report it through
            // the node error path.
            ctx.error(
                "node.mac_scatter_mass_momentum: accumulator and status buffers are undersized",
            );
            ctx.mark_gpu_accessed();
            return;
        }
        let particle_capacity =
            (particles.size / std::mem::size_of::<WaterParticle>() as u64) as u32;
        let active_count_value = ctx.scalar_or_param("active_count", PARTICLE_CAPACITY as f32);
        if !active_count_value.is_finite() {
            ctx.error("node.mac_scatter_mass_momentum: active_count must be finite");
            ctx.mark_gpu_accessed();
            return;
        }
        let active_count = active_count_value.round().max(0.0) as u32;
        let active_count = active_count.min(particle_capacity);
        if active_count == 0 {
            ctx.mark_gpu_accessed();
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(WGSL, "cs_main", "node.mac_scatter_mass_momentum")
        });
        let uniforms = MacScatterUniforms {
            active_count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // uniform(0), particles(1), accumulator atomic(2), status atomic(3).
        gpu.native_enc.dispatch_compute(
            pipeline,
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
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: status_out,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.mac_scatter_mass_momentum",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn mac_scatter_declares_mpm_status_alias_shape() {
        use crate::node_graph::ports::{ArrayType, ChannelElementType, PortType};
        let particles = ArrayType::of_known::<WaterParticle>();
        let status = ArrayType::of_known::<u32>();
        assert_eq!(
            MacScatterMassMomentum::TYPE_ID,
            "node.mac_scatter_mass_momentum"
        );
        assert_eq!(
            MacScatterMassMomentum::INPUTS[0].ty,
            PortType::Array(particles)
        );
        assert_eq!(MacScatterMassMomentum::INPUTS[1].name, "accumulator");
        assert_eq!(
            MacScatterMassMomentum::INPUTS[2].ty,
            PortType::Array(status)
        );
        assert_eq!(MacScatterMassMomentum::OUTPUTS.len(), 2);
        let PortType::Array(accum) = &MacScatterMassMomentum::OUTPUTS[0].ty else {
            panic!("out must be an accumulator array");
        };
        assert_eq!(accum.item_size, 4);
        assert_eq!(accum.specs[0].ty, ChannelElementType::I32);
    }

    #[test]
    fn mac_scatter_aliases_accumulator_and_status() {
        let prim = MacScatterMassMomentum::new();
        assert_eq!(
            prim.aliased_array_io(),
            &[("accumulator", "out"), ("status", "status_out")]
        );
        assert_eq!(
            prim.array_output_capacity(
                "out",
                &Default::default(),
                &[("accumulator", MAC_ACCUM_SLOTS)]
            ),
            Some(MAC_ACCUM_SLOTS)
        );
    }

    #[test]
    fn mac_scatter_uses_checked_atomic_abi() {
        assert!(WGSL.contains("atomicCompareExchangeWeak"));
        assert!(WGSL.contains("MAC_ACCUM_SLOTS") || WGSL.contains("water_quantise"));
        let prim = MacScatterMassMomentum::new();
        let node: &dyn crate::node_graph::EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mac_scatter_mass_momentum");
    }
}
