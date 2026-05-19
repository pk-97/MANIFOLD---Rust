//! `node.neighbor_smooth` — 5-point cross-neighborhood smoothing
//! over an `Array<InstanceTransform>` with grid topology.
//!
//! Extracted from `generators/shaders/digital_plants_smooth.wgsl`.
//! Smooths the xyz position component of each instance with its
//! 4 grid neighbors; scale (.w) and rotation pass through unchanged.
//! Border instances fall back to self for missing neighbors.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SmoothUniforms {
    grid_size: u32,
    instance_count: u32,
    center_weight: f32,
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
            name: "grid_size",
            label: "Grid Size",
            ty: ParamType::Int,
            default: ParamValue::Int(400),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "center_weight",
            label: "Center Weight",
            ty: ParamType::Float,
            default: ParamValue::Float(0.6),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "grid_size must match the producer upstream (DigitalPlants uses 400×400 = 160k instances). Total active = grid_size² capped at buffer capacity. center_weight=0.6 matches the DigitalPlants default; 1.0 disables smoothing; 0.2 is heavy smoothing. The 4 neighbor weights are uniformly (1-center)/4 each.",
    examples: [],
    picker: { label: "Neighbor Smooth", category: Atom },
}

impl Primitive for NeighborSmooth {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid_size = match ctx.params.get("grid_size") {
            Some(ParamValue::Int(n)) => (*n).max(2) as u32,
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
            gpu.device.create_compute_pipeline(
                include_str!("shaders/neighbor_smooth.wgsl"),
                "cs_main",
                "node.neighbor_smooth",
            )
        });

        let uniforms = SmoothUniforms {
            grid_size,
            instance_count,
            center_weight,
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
        let layout = ArrayType {
            item_size: std::mem::size_of::<InstanceTransform>() as u32,
            item_align: std::mem::align_of::<InstanceTransform>() as u32,
        };
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
        let names: Vec<&str> = NeighborSmooth::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["grid_size", "center_weight"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = NeighborSmooth::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.neighbor_smooth");
    }
}
