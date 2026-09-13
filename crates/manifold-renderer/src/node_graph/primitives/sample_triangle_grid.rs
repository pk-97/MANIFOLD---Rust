//! Bounded sparse triangle samples for Math View overlays.

use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::{Primitive, PrimitiveSpec};
use manifold_gpu::GpuBinding;
use std::borrow::Cow;

pub const SAMPLE_TRIANGLE_GRID_CAPACITY: u32 = 8 * 8 * 8 * 3;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SampleUniforms {
    density: i32,
    radius: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: SampleTriangleGrid,
    type_id: "node.sample_triangle_grid",
    purpose: "Emit a bounded sparse lattice of tiny independent MeshVertex triangles for Math View presentation. Coordinates are deterministic cell centres scaled by radius and translated by source_offset; this source contains no modifier or evaluation math.",
    inputs: { density: ScalarF32 optional, radius: ScalarF32 optional, source_offset_x: ScalarF32 optional, source_offset_y: ScalarF32 optional, source_offset_z: ScalarF32 optional },
    outputs: { vertices: Array(MeshVertex) },
    params: [
        ParamDef { name: Cow::Borrowed("density"), label: "Density", ty: ParamType::Int, default: ParamValue::Float(4.0), range: Some((2.0, 8.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("radius"), label: "Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.001, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Fixed output capacity is 1536 vertices (512 triangles). Density is clamped to 2..8 at runtime; inactive slots are degenerate. Parameters are port-shadowed and only position the samples.",
    examples: [], picker: { label: "Sample Triangle Grid", category: Atom },
    summary: "Creates a small bounded lattice of triangle samples for spatial overlays.",
    category: Geometry3D, role: Source, aliases: ["sample grid", "triangle samples", "math view samples"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/sample_triangle_grid_body.wgsl"),
}

impl Primitive for SampleTriangleGrid {
    fn array_output_capacity(
        &self,
        port: &str,
        _p: &crate::node_graph::effect_node::ParamValues,
        _i: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "vertices").then_some(SAMPLE_TRIANGLE_GRID_CAPACITY)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let cap = (dst.size / std::mem::size_of::<MeshVertex>() as u64) as u32;
        if cap == 0 {
            return;
        }
        let density = ctx.scalar_or_param("density", 4.0).round().clamp(2.0, 8.0) as i32;
        let u = SampleUniforms {
            density,
            radius: ctx.scalar_or_param("radius", 1.0),
            source_offset_x: ctx.scalar_or_param("source_offset_x", 0.0),
            source_offset_y: ctx.scalar_or_param("source_offset_y", 0.0),
            source_offset_z: ctx.scalar_or_param("source_offset_z", 0.0),
            dispatch_count: cap,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        let p = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            p,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [cap.div_ceil(256), 1, 1],
            Self::TYPE_ID,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;
    #[test]
    fn fixed_capacity_and_ports() {
        assert_eq!(SAMPLE_TRIANGLE_GRID_CAPACITY, 1536);
        assert_eq!(SampleTriangleGrid::TYPE_ID, "node.sample_triangle_grid");
        assert_eq!(SampleTriangleGrid::OUTPUTS.len(), 1);
        assert!(
            crate::node_graph::PrimitiveRegistry::with_builtin()
                .contains(SampleTriangleGrid::TYPE_ID)
        );
    }

    #[test]
    fn source_body_keeps_capacity_and_calibration_contract() {
        let body = <SampleTriangleGrid as PrimitiveSpec>::WGSL_BODY.expect("source body");
        assert!(body.contains("source_offset_x"));
        assert!(body.contains("- vec3<f32>(source_offset_x"));
        assert!(
            SampleTriangleGrid::INPUTS
                .iter()
                .any(|p| p.name == "density")
        );
    }
}
