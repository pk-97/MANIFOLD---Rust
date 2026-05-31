//! `node.generate_instance_transforms` — emit an
//! `Array<InstanceTransform>` filled with a procedural layout.
//!
//! Phase B of `BUFFER_PORT_PLAN`. The instance-side companion
//! to `node.generate_grid_mesh` — produces N transforms in one
//! of grid / ring / spiral / random patterns, paired with
//! `node.render_instanced_3d_mesh` to draw N copies of a base
//! mesh. Unlocks NestedCubes, DigitalPlants, and any future
//! "many small objects in a pattern" generator.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const INSTANCE_LAYOUTS: &[&str] = &["Grid", "Ring", "Spiral", "Random"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceUniforms {
    active_count: u32,
    capacity: u32,
    layout: u32,
    seed: u32,
    extent_x: f32,
    extent_y: f32,
    extent_z: f32,
    base_scale: f32,
    rot_x: f32,
    rot_y: f32,
    rot_z: f32,
    _pad: f32,
}

crate::primitive! {
    name: GenerateInstanceTransforms,
    type_id: "node.generate_instance_transforms",
    purpose: "Emit an Array<InstanceTransform> filled with a procedural layout (grid / ring / spiral / random). Pair with node.render_instanced_3d_mesh to draw N copies of a base mesh. The unlock for NestedCubes / DigitalPlants-shaped graphs.",
    inputs: {},
    outputs: {
        instances: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(65_536.0),
            range: Some((1.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(64.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "layout",
            label: "Layout",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: INSTANCE_LAYOUTS,
        },
        ParamDef {
            name: "seed",
            label: "Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "extent_x",
            label: "Extent X",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "extent_y",
            label: "Extent Y",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "extent_z",
            label: "Extent Z",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "base_scale",
            label: "Base Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "rot_x",
            label: "Rotation X",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: "rot_y",
            label: "Rotation Y",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: "rot_z",
            label: "Rotation Z",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    composition_notes: "max_capacity is chain-build allocation ceiling — pre-allocates max_capacity × 32 bytes. active_count is a free slider. Rotation params apply to every instance uniformly — for per-instance varying rotation, write a downstream transform primitive that perturbs `rot_pad`.",
    examples: [],
    picker: { label: "Arrange Copies", category: Atom },
    summary: "Lays out a field of copies in a grid, ring, spiral, or random spread, giving each one a position to render at. Pair it with Render Copies.",
    category: Geometry3D,
    role: Source,
    aliases: ["arrange copies", "instance layout", "scatter", "place"],
}

impl Primitive for GenerateInstanceTransforms {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = match ctx.params.get("active_count") {
            Some(ParamValue::Float(n)) => n.round().max(0_f32) as u32,
            _ => 64,
        };
        let layout = match ctx.params.get("layout") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let seed = match ctx.params.get("seed") {
            Some(ParamValue::Float(n)) => n.round() as u32,
            _ => 0,
        };
        let extent_x = match ctx.params.get("extent_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 4.0,
        };
        let extent_y = match ctx.params.get("extent_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 4.0,
        };
        let extent_z = match ctx.params.get("extent_z") {
            Some(ParamValue::Float(f)) => *f,
            _ => 4.0,
        };
        let base_scale = match ctx.params.get("base_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let rot_x = match ctx.params.get("rot_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let rot_y = match ctx.params.get("rot_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let rot_z = match ctx.params.get("rot_z") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let Some(out_buf) = ctx.outputs.array("instances") else {
            return;
        };
        let item_size = std::mem::size_of::<InstanceTransform>() as u64;
        let capacity = (out_buf.size / item_size) as u32;
        let active_count = active_count.min(capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/generate_instance_transforms.wgsl"),
                "cs_main",
                "node.generate_instance_transforms",
            )
        });

        let uniforms = InstanceUniforms {
            active_count,
            capacity,
            layout,
            seed,
            extent_x,
            extent_y,
            extent_z,
            base_scale,
            rot_x,
            rot_y,
            rot_z,
            _pad: 0.0,
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
            [capacity.div_ceil(64), 1, 1],
            "node.generate_instance_transforms",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn generate_instance_transforms_declares_zero_inputs_and_instance_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(
            GenerateInstanceTransforms::TYPE_ID,
            "node.generate_instance_transforms"
        );
        assert!(GenerateInstanceTransforms::INPUTS.is_empty());
        assert_eq!(GenerateInstanceTransforms::OUTPUTS.len(), 1);
        assert_eq!(GenerateInstanceTransforms::OUTPUTS[0].name, "instances");
        assert_eq!(
            GenerateInstanceTransforms::OUTPUTS[0].ty,
            PortType::Array(layout)
        );
    }

    #[test]
    fn layout_enum_has_four_options() {
        let layout_param = GenerateInstanceTransforms::PARAMS
            .iter()
            .find(|p| p.name == "layout")
            .expect("layout param");
        assert_eq!(layout_param.ty, ParamType::Enum);
        assert_eq!(layout_param.enum_values.len(), 4);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GenerateInstanceTransforms::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.generate_instance_transforms");
    }
}
