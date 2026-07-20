//! `node.grid_uv_field` — emit an `Array<vec2<f32>>` of UV positions
//! on an N×N grid in `[0, 1]²` space.
//!
//! Foundational producer for per-instance noise samplers
//! (`node.simplex_noise_per_copy`, `node.fractal_noise_per_copy`) and
//! topology-wrap primitives (`node.cylinder_wrap_field`,
//! `node.torus_wrap_field`). Each cell is sampled at its center:
//! for `idx = row*N + col`, `uv = ((col+0.5)/N, (row+0.5)/N)`.
//!
//! Output capacity is `grid_size²` and is pre-allocated at chain
//! build time from the `grid_size` param. The dispatch is a cheap
//! write-only pass (8 bytes × N² per frame); no state is carried.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: `grid_size` (Int → i32) then the
/// codegen-injected `dispatch_count` element count, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridUniforms {
    grid_size: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: GridUvField,
    type_id: "node.grid_uv_field",
    purpose: "Emit an Array<vec2<f32>> of UV positions on an N×N grid in [0,1]² space, sampling each cell at its centre: for idx = row*N + col, uv = ((col+0.5)/N, (row+0.5)/N). Foundational producer for per-instance noise samplers (simplex/fbm per_instance) and topology-wrap primitives (cylinder/torus wrap field). Output capacity equals grid_size² and is pre-allocated at chain-build time from the `grid_size` param.",
    inputs: {},
    outputs: {
        uv: Array([f32; 2]),
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
    ],
    depth_rule: Terminal,
    composition_notes: "DigitalPlants uses grid_size = 400 (160K UVs). Output is dispatched fresh each frame — cheap (8 bytes × N² write per frame), no persistent state. Editor changes to grid_size trigger a chain rebuild because the buffer capacity comes from this param. Pair downstream with node.simplex_noise_per_copy / node.fractal_noise_per_copy to sample noise at each UV, or with node.cylinder_wrap_field / node.torus_wrap_field to lift the UV grid onto a 3D surface as Array<InstanceTransform>.",
    examples: [],
    picker: { label: "Grid UV Field", category: Atom },
    summary: "Outputs a grid of sample points across the frame as a list, used to drive instanced shapes or sample a field at regular spots.",
    category: FieldsAndCoordinates,
    role: Source,
    aliases: ["grid uv", "sample grid", "points"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/grid_uv_field_body.wgsl"),
}

impl Primitive for GridUvField {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "uv" {
            return None;
        }
        let grid_size = match params.get("grid_size") {
            Some(ParamValue::Float(n)) => n.round().max(2.0) as u32,
            _ => 400,
        };
        Some(grid_size * grid_size)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid_size = match ctx.params.get("grid_size") {
            Some(ParamValue::Float(n)) => n.round().max(2.0) as u32,
            _ => 400,
        };

        let Some(uv_buf) = ctx.outputs.array("uv") else {
            return;
        };
        let item_size = std::mem::size_of::<[f32; 2]>() as u64;
        let capacity = (uv_buf.size / item_size) as u32;
        let count = (grid_size as u64 * grid_size as u64).min(capacity as u64) as u32;
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer source
            // path, 0 array inputs).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.grid_uv_field standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.grid_uv_field",
            )
        });

        let uniforms = GridUniforms {
            grid_size: grid_size as i32,
            dispatch_count: count,
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
                    buffer: uv_buf,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.grid_uv_field",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn grid_uv_field_declares_vec2_array_output_only() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<[f32; 2]>();
        assert_eq!(GridUvField::TYPE_ID, "node.grid_uv_field");
        assert!(GridUvField::INPUTS.is_empty());
        assert_eq!(GridUvField::OUTPUTS.len(), 1);
        assert_eq!(GridUvField::OUTPUTS[0].name, "uv");
        assert_eq!(GridUvField::OUTPUTS[0].ty, PortType::Array(layout));
        assert_eq!(layout.item_size, 8);
    }

    #[test]
    fn grid_uv_field_capacity_is_grid_size_squared() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = GridUvField::new();
        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("grid_size"), ParamValue::Float(400.0));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "uv", &params, &[]),
            Some(160_000),
        );
        params.insert(std::borrow::Cow::Borrowed("grid_size"), ParamValue::Float(64.0));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "uv", &params, &[]),
            Some(4_096),
        );
    }

    #[test]
    fn grid_uv_field_capacity_unknown_port_returns_none() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = GridUvField::new();
        let params = ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "other", &params, &[]),
            None,
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GridUvField::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.grid_uv_field");
    }
}

