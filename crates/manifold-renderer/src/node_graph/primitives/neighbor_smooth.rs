//! `node.neighbor_smooth` — 5-point cross-neighborhood smoothing
//! over an `Array<InstanceTransform>` with grid topology.
//!
//! Extracted from `generators/shaders/digital_plants_smooth.wgsl`.
//! Smooths the xyz position component of each instance with its
//! 4 grid neighbors; scale (.w) and rotation pass through unchanged.
//! Border instances fall back to self for missing neighbors.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout (NOT the hand `neighbor_smooth.wgsl`
/// order): scalar params in PARAMS order — `grid_size` (Int → i32),
/// `center_weight` (f32) — then the codegen-injected `dispatch_count` element
/// count, padded to 16 bytes. The dispatch is keyed on `dispatch_count` (= the
/// live instance count); the body casts `grid_size` i32 → u32.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SmoothUniforms {
    grid_size: i32,
    center_weight: f32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: NeighborSmooth,
    type_id: "node.neighbor_smooth",
    purpose: "5-point cross-neighborhood smoothing of an Array<InstanceTransform> arranged as an NxN grid. Smooths the xyz position; scale and rotation pass through. Border instances fall back to self. Drives plant-stalk-style smoothed motion in instanced renderers — pair upstream with a procedural compute that emits noisy positions, then this primitive cleans them.",
    inputs: {
        in: Array(InstanceTransform) required,
    },
    outputs: {
        out: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("grid_size"),
            label: "Grid Size",
            ty: ParamType::Int,
            default: ParamValue::Float(400.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("center_weight"),
            label: "Center Weight",
            ty: ParamType::Float,
            default: ParamValue::Float(0.6),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "grid_size must match the producer upstream (DigitalPlants uses 400×400 = 160k instances). Total active = grid_size² capped at buffer capacity. center_weight=0.6 matches the DigitalPlants default; 1.0 disables smoothing; 0.2 is heavy smoothing. The 4 neighbor weights are uniformly (1-center)/4 each.",
    examples: [],
    picker: { label: "Smooth (neighbors)", category: Atom },
    summary: "Averages each point with its neighbours on a grid, smoothing out a bumpy field of values or positions.",
    category: FieldsAndCoordinates,
    role: Filter,
    aliases: ["smooth", "neighbor average", "blur grid"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/neighbor_smooth_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for NeighborSmooth {
    /// Output `out` is sized to match input `in` — neighbor smoothing
    /// over an Array<InstanceTransform> produces the same number of
    /// transforms.
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
        let grid_size = match ctx.params.get("grid_size") {
            Some(ParamValue::Float(n)) => n.round().max(2_f32) as u32,
            _ => 400,
        };
        let center_weight = match ctx.params.get("center_weight") {
            Some(ParamValue::Float(f)) => f.clamp(0.0, 1.0),
            _ => 0.6,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let item_size = std::mem::size_of::<InstanceTransform>() as u64;
        let capacity = (in_buf.size.min(out_buf.size) / item_size) as u32;
        let instance_count = (grid_size as u64 * grid_size as u64).min(capacity as u64) as u32;
        if instance_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: the kernel is generated from the `wgsl_body` (buffer
            // standalone codegen). `neighbor_smooth.wgsl` (the hand-kernel parity
            // oracle) was deleted 2026-07-20 (W1-B, migration scaffolding
            // retired). Bindings match: uniform(0), buf_in(1), buf_out(2).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.neighbor_smooth standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.neighbor_smooth",
            )
        });

        let uniforms = SmoothUniforms {
            grid_size: grid_size as i32,
            center_weight,
            dispatch_count: instance_count,
            _pad0: 0,
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
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [instance_count.div_ceil(256), 1, 1],
            "node.neighbor_smooth",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn neighbor_smooth_declares_instance_array_in_and_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(NeighborSmooth::TYPE_ID, "node.neighbor_smooth");
        assert_eq!(NeighborSmooth::INPUTS.len(), 1);
        assert_eq!(NeighborSmooth::INPUTS[0].name, "in");
        assert!(NeighborSmooth::INPUTS[0].required);
        assert_eq!(NeighborSmooth::INPUTS[0].ty, PortType::Array(layout));
        assert_eq!(NeighborSmooth::OUTPUTS.len(), 1);
        assert_eq!(NeighborSmooth::OUTPUTS[0].name, "out");
        assert_eq!(NeighborSmooth::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn neighbor_smooth_has_grid_and_center_weight_params() {
        let names: Vec<&str> = NeighborSmooth::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["grid_size", "center_weight"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = NeighborSmooth::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.neighbor_smooth");
    }
}

