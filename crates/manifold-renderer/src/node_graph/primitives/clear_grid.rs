//! `node.clear_grid` — S4 solver stage: zero the grid accumulation wire.
//!
//! One grid clear dispatch per substep (design step 2): the flat i32
//! mass/momentum accumulation wire (`4*nx*ny*nz` items, `4*g+0..2` momentum
//! xyz, `4*g+3` mass) is zeroed before the scatter stages run. Sticky fault
//! status is never touched here — only reset clears it.
//!
//! The `step_dt` input is an ordering dependency, not a numerical input:
//! wiring the region's step clock into it guarantees the region compiler
//! keeps the clear inside the repeated region (plan section 2.2).
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 3 and 5;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Wire capacity of the default 64^3 accumulation grid: 4 i32 slots per
/// cell (momentum xyz + mass).
pub const ACCUM_ITEMS: u32 = 4 * 64 * 64 * 64;

/// Generated-codegen uniform layout: the allocation-only `max_capacity`
/// param (Int -> i32), then the codegen-injected `dispatch_count` (u32),
/// padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ClearGridUniforms {
    pub max_capacity: i32,
    pub dispatch_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
}

crate::primitive! {
    name: ClearGrid,
    type_id: "node.clear_grid",
    purpose: "Zero the Live Water grid accumulation wire (4*nx*ny*nz signed i32 slots: 4*g+0..2 grid momentum xyz, 4*g+3 grid mass in Q=2^20 fixed point) at the start of every solver substep. Runs before the two mpm_scatter stages each iteration; the scatter output aliases this wire, so the clear is what separates one substep's accumulation from the last. `step_dt` is an ordering dependency (every step-dependent atom must consume a region output) and is not used numerically. Sticky fault status is never cleared here — only reset does that.",
    inputs: {
        step_dt: ScalarF32 required,
    },
    outputs: {
        out: Channels["water_grid_accum": I32],
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Accumulator Items",
            ty: ParamType::Int,
            default: ParamValue::Float(ACCUM_ITEMS as f32),
            range: Some((4.0, 16_000_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "First stage of the repeated water region body: `clear_grid -> mpm_scatter_mass_momentum -> mpm_scatter_stress -> mpm_grid_velocity -> mpm_gather_advect -> water_validate -> water_commit`. Capacity must be exactly 4 * nx * ny * nz for the configured domain (default 64^3 -> 4,194,304 items).",
    examples: [],
    picker: { label: "Clear Water Grid", category: Atom },
    summary: "Blanks the water solver's momentum-and-mass scratch grid between substeps.",
    category: Particles3D,
    role: Filter,
    aliases: ["clear grid", "clear water grid", "zero accum"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/clear_grid_body.wgsl"),
}

impl Primitive for ClearGrid {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        // 4-byte i32 items.
        let capacity = (out_buf.size / 4) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.clear_grid standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.clear_grid",
            )
        });

        let uniforms = ClearGridUniforms {
            max_capacity: capacity as i32,
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
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
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.clear_grid",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn clear_grid_declares_i32_accumulator_source() {
        use crate::node_graph::ports::{ChannelElementType, PortType};
        assert_eq!(ClearGrid::TYPE_ID, "node.clear_grid");
        assert_eq!(ClearGrid::INPUTS.len(), 1);
        assert_eq!(ClearGrid::INPUTS[0].name, "step_dt");
        assert!(ClearGrid::INPUTS[0].required);
        assert_eq!(ClearGrid::OUTPUTS.len(), 1);
        assert_eq!(ClearGrid::OUTPUTS[0].name, "out");
        let PortType::Array(at) = &ClearGrid::OUTPUTS[0].ty else {
            panic!("out must be an array wire");
        };
        assert_eq!(at.specs.len(), 1);
        assert_eq!(at.specs[0].ty, ChannelElementType::I32);
        assert_eq!(at.item_size, 4);
    }

    #[test]
    fn clear_grid_registers_as_palette_atom() {
        let prim = ClearGrid::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.clear_grid");
    }
}
