//! `node.mac_resolve` — resolve a padded MAC accumulator into face velocity
//! and validity channels.
//!
//! Each output entry corresponds to one coordinate in the common 65³ padded
//! lattice.  `mac_velocity.xyz` contains the normalized x/y/z face momenta
//! where their corresponding mass is positive; `mac_valid.xyz` is 1 for those
//! faces and 0 otherwise.  Both channel `.w` values are always zero.

use manifold_gpu::GpuBinding;

use super::mac_scatter_mass_momentum::{MAC_ACCUM_SLOTS, MAC_GRID_ENTRIES};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

pub const MAC_RESOLVED_CELL_BYTES: u64 = 32;

/// Generated buffer-codegen uniform layout: the injected element count and
/// padding to the same 16-byte ABI used by other array-domain atoms.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MacResolveUniforms {
    pub dispatch_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub _pad2: u32,
}

/// Rust-side ABI witness for the generated `Element` struct.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MacResolvedCell {
    pub mac_velocity: [f32; 4],
    pub mac_valid: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<MacResolvedCell>() == MAC_RESOLVED_CELL_BYTES as usize);

pub fn shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<MacResolve>()
        .expect("node.mac_resolve codegen")
}

crate::primitive! {
    name: MacResolve,
    type_id: "node.mac_resolve",
    purpose: "Resolve the padded 65³ APIC MAC accumulator. Each entry reads [mass_x, momentum_x, mass_y, momentum_y, mass_z, momentum_z] from the signed Q=2^20 accumulator and emits Channels[mac_velocity: Vec4F, mac_valid: Vec4F]. A component is momentum/mass when mass is positive and finite, otherwise zero; validity is 1 for a positive-mass component and 0 otherwise. Both channel w values are zero.",
    inputs: {
        accumulator: Channels["water_grid_accum": I32] required,
    },
    outputs: {
        out: Channels["mac_velocity": Vec4F, "mac_valid": Vec4F],
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Consumes node.mac_scatter_mass_momentum's six-slot padded accumulator. The output allocation is fixed at exactly 65³ entries, with a 32-byte std430 element: mac_velocity.xyzw followed by mac_valid.xyzw. Validity is face mass > 0 and is independent per axis; unused tangential padding faces naturally resolve to zero.",
    examples: [],
    picker: { label: "MAC Resolve", category: Atom },
    summary: "Turns fixed-point MAC face mass and momentum into normalized velocity and per-face validity channels.",
    category: Particles3D,
    role: Filter,
    aliases: ["mac resolve", "mac velocity", "face velocity", "apic resolve"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/mac_resolve_body.wgsl"),
    input_access: [BufferGather],
    wgsl_includes: [include_str!("shaders/water_common.wgsl")],
    extra_fields: { source: String = shader_source(), },
}

impl Primitive for MacResolve {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out").then_some(MAC_GRID_ENTRIES)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(accumulator) = ctx.inputs.array("accumulator") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let accumulator_capacity = (accumulator.size / std::mem::size_of::<i32>() as u64) as u32;
        let output_capacity = (out.size / std::mem::size_of::<MacResolvedCell>() as u64) as u32;
        if accumulator_capacity < MAC_ACCUM_SLOTS {
            ctx.error("node.mac_resolve: accumulator requires the full padded 65-cubed grid");
            return;
        }
        if output_capacity < MAC_GRID_ENTRIES {
            ctx.error("node.mac_resolve: output requires the full padded 65-cubed grid");
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(&self.source, "cs_main", "node.mac_resolve")
        });
        let uniforms = MacResolveUniforms {
            dispatch_count: MAC_GRID_ENTRIES,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        // uniform(0), accumulator(1), resolved channels(2).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: accumulator,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out,
                    offset: 0,
                },
            ],
            [MAC_GRID_ENTRIES.div_ceil(256), 1, 1],
            "node.mac_resolve",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn mac_resolve_declares_32_byte_velocity_validity_output() {
        use crate::node_graph::ports::{ChannelElementType, PortType};
        assert_eq!(MacResolve::TYPE_ID, "node.mac_resolve");
        let PortType::Array(input) = &MacResolve::INPUTS[0].ty else {
            panic!("accumulator input must be an array");
        };
        assert_eq!(input.item_size, 4);
        assert_eq!(input.specs[0].ty, ChannelElementType::I32);
        let PortType::Array(output) = &MacResolve::OUTPUTS[0].ty else {
            panic!("resolve output must be an array");
        };
        assert_eq!(output.item_size, MAC_RESOLVED_CELL_BYTES as u32);
        assert_eq!(output.specs.len(), 2);
        assert_eq!(output.specs[0].ty, ChannelElementType::Vec4F);
        assert_eq!(output.specs[1].ty, ChannelElementType::Vec4F);
    }

    #[test]
    fn mac_resolve_codegen_is_per_element_and_reads_six_slots() {
        let wgsl = shader_source();
        assert!(wgsl.contains("var<storage, read> buf_accumulator: array<i32>"));
        assert!(wgsl.contains("struct Element"));
        assert!(wgsl.contains("mac_velocity: vec4<f32>"));
        assert!(wgsl.contains("mac_valid: vec4<f32>"));
        assert!(wgsl.contains("idx * 6u"));
    }

    #[test]
    fn mac_resolve_generated_wgsl_validates() {
        let source = shader_source();
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("node.mac_resolve generated WGSL must validate");
    }

    #[test]
    fn mac_resolve_allocates_exactly_one_padded_lattice() {
        let prim = MacResolve::new();
        assert_eq!(
            prim.array_output_capacity("out", &Default::default(), &[]),
            Some(MAC_GRID_ENTRIES)
        );
    }
}
