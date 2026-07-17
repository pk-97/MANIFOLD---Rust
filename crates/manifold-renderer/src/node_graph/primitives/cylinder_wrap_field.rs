//! `node.cylinder_wrap_field` — lift an `Array<vec2<f32>>` of UVs
//! onto a cylindrical surface, emit `Array<InstanceTransform>`.
//!
//! Per UV idx:
//! ```text
//! theta = uv.x * TAU
//! r     = base_radius * pow(max(1 - uv.y, 0), taper) + radius_disp[idx]
//! y     = (uv.y - 0.5) * height_scale
//! pos   = (r * cos(theta), y, r * sin(theta))
//! ```
//!
//! The taper curve `pow(1 - uv.y, taper)` narrows the radius toward
//! `uv.y = 1` — Christmas-tree stems, vases, tapered tubes, conifer
//! columns. With `taper = 0` you get a straight cylinder; `taper > 1`
//! makes the top sharper.
//!
//! Optional `radius_disp` is a per-instance `Array<f32>` added to the
//! tapered radius — drive from `node.simplex_noise_per_copy` (organic
//! stem noise) or any other Array<f32> source.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    count: u32,
    base_radius: f32,
    height_scale: f32,
    taper: f32,
    instance_scale: f32,
    has_radius_disp: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: CylinderWrapField,
    type_id: "node.cylinder_wrap_field",
    purpose: "Lift an Array<vec2<f32>> of UVs onto a cylindrical surface and emit Array<InstanceTransform>. For each UV: theta = uv.x * TAU, r = base_radius * pow(max(1 - uv.y, 0), taper) + radius_disp, y = (uv.y - 0.5) * height_scale, pos = (r·cos θ, y, r·sin θ). The taper curve narrows the radius toward uv.y=1 — produces stem / vase / cone / conifer shapes. Optional radius_disp Array<f32> adds per-instance radial noise (typically driven by node.simplex_noise_per_copy + shaping). All scalar params are port-shadow so the cylinder can be animated by control wires.",
    inputs: {
        uv: Array([f32; 2]) required,
        radius_disp: Array(f32) optional,
        base_radius: ScalarF32 optional,
        height_scale: ScalarF32 optional,
        taper: ScalarF32 optional,
        instance_scale: ScalarF32 optional,
    },
    outputs: {
        instances: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("base_radius"),
            label: "Base Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height_scale"),
            label: "Height",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("taper"),
            label: "Taper",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("instance_scale"),
            label: "Instance Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity follows the `uv` input. `instance_scale` is written into the .w of pos_scale on every emitted InstanceTransform — pair with node.torus_wrap_field downstream of node.switch_array<InstanceTransform> and feed the SAME scale wire into both so the .w stays continuous across a cyl↔tor morph. Rotation is left at zero on every output; pair with node.rotation_jitter downstream for hash-driven per-instance rotation. Taper = 0 disables tapering (straight cylinder); larger values sharpen the top.",
    examples: [],
    picker: { label: "Cylinder Wrap Field", category: Atom },
    summary: "Wraps a flat grid of points around a cylinder, placing copies on a curved surface. Part of the digital-plants geometry.",
    category: Geometry3D,
    role: Map,
    aliases: ["cylinder wrap", "curved surface", "wrap"],
    boundary_reason: FusedBundle,
}

impl Primitive for CylinderWrapField {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "instances" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(p, _)| *p == "uv")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let base_radius = ctx.scalar_or_param("base_radius", 1.0);
        let height_scale = ctx.scalar_or_param("height_scale", 2.0);
        let taper = ctx.scalar_or_param("taper", 2.0);
        let instance_scale = ctx.scalar_or_param("instance_scale", 1.0);

        let Some(uv_buf) = ctx.inputs.array("uv") else {
            return;
        };
        let radius_disp_wired = ctx.inputs.array("radius_disp");
        let radius_disp_buf = radius_disp_wired.unwrap_or(uv_buf);
        let Some(out_buf) = ctx.outputs.array("instances") else {
            return;
        };

        let vec2_size = std::mem::size_of::<[f32; 2]>() as u64;
        let inst_size = std::mem::size_of::<InstanceTransform>() as u64;
        let in_capacity = (uv_buf.size / vec2_size) as u32;
        let out_capacity = (out_buf.size / inst_size) as u32;
        let count = in_capacity.min(out_capacity);
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/cylinder_wrap_field.wgsl"),
                "cs_main",
                "node.cylinder_wrap_field",
            )
        });

        let uniforms = Uniforms {
            count,
            base_radius,
            height_scale,
            taper,
            instance_scale,
            has_radius_disp: u32::from(radius_disp_wired.is_some()),
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
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: radius_disp_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.cylinder_wrap_field",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn cylinder_wrap_field_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();
        let f32_layout = ArrayType::of_known::<f32>();
        let inst_layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(CylinderWrapField::TYPE_ID, "node.cylinder_wrap_field");

        let uv_in = CylinderWrapField::INPUTS.iter().find(|p| p.name == "uv").unwrap();
        assert!(uv_in.required);
        assert_eq!(uv_in.ty, PortType::Array(vec2_layout));

        let disp_in = CylinderWrapField::INPUTS
            .iter()
            .find(|p| p.name == "radius_disp")
            .unwrap();
        assert!(!disp_in.required);
        assert_eq!(disp_in.ty, PortType::Array(f32_layout));

        for name in ["base_radius", "height_scale", "taper", "instance_scale"] {
            let port = CylinderWrapField::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(CylinderWrapField::OUTPUTS.len(), 1);
        assert_eq!(CylinderWrapField::OUTPUTS[0].name, "instances");
        assert_eq!(
            CylinderWrapField::OUTPUTS[0].ty,
            PortType::Array(inst_layout),
        );
    }

    #[test]
    fn cylinder_wrap_field_output_follows_uv_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = CylinderWrapField::new();
        let params = ParamValues::default();
        let inputs = [("uv", 160_000_u32), ("radius_disp", 160_000_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "instances", &params, &inputs),
            Some(160_000),
        );
        // Unwired radius_disp still gives full capacity.
        let inputs_minimal = [("uv", 160_000_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "instances", &params, &inputs_minimal),
            Some(160_000),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CylinderWrapField::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.cylinder_wrap_field");
    }
}
