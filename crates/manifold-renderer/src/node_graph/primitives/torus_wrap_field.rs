//! `node.torus_wrap_field` — lift an `Array<vec2<f32>>` of UVs onto
//! a torus surface, emit `Array<InstanceTransform>`.
//!
//! Per UV idx:
//! ```text
//! theta = uv.x * TAU, phi = uv.y * TAU
//! pos   = ((R + r·cos φ)·cos θ, r·sin φ, (R + r·cos φ)·sin θ)
//! normal_outward = (cos φ · cos θ, sin φ, cos φ · sin θ)
//! pos += normal_outward * normal_disp[idx]     (optional)
//! pos  = rotate_x(pos, fold_angle)
//! ```
//!
//! Generic across rings, halos, donuts, flower discs, gateways. The
//! `R` (major) and `r` (tube) radii pin the torus geometry; `fold_angle`
//! rotates the whole field around the X axis (drive from a time wire
//! for continuous fold animation).

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
    torus_radius: f32,
    fold_angle: f32,
    instance_scale: f32,
    has_normal_disp: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: TorusWrapField,
    type_id: "node.torus_wrap_field",
    purpose: "Lift an Array<vec2<f32>> of UVs onto a torus surface, emit Array<InstanceTransform>. For each UV: theta = uv.x * TAU, phi = uv.y * TAU, pos = ((R + r·cos φ)·cos θ, r·sin φ, (R + r·cos φ)·sin θ). Optional Array<f32> normal_disp pushes each instance along the outward surface normal — drive from node.fractal_noise_per_copy × petal-amplitude for flower-style petal displacement. `fold_angle` rotates the whole field around the X axis (port-shadow, drive from time for continuous animation). Generic across rings, halos, donuts, flower discs, gateways.",
    inputs: {
        uv: Array([f32; 2]) required,
        normal_disp: Array(f32) optional,
        base_radius: ScalarF32 optional,
        torus_radius: ScalarF32 optional,
        fold_angle: ScalarF32 optional,
        instance_scale: ScalarF32 optional,
    },
    outputs: {
        instances: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("base_radius"),
            label: "Tube Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("torus_radius"),
            label: "Major Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fold_angle"),
            label: "Fold Angle",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
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
    composition_notes: "Output capacity follows the `uv` input. Pair upstream with node.fractal_noise_per_copy scaled by an outer-card petal-amplitude (via node.array_math::ScaleOffset) to drive `normal_disp` for flower-petal-style fracturing. `instance_scale` is written into pos_scale.w — wire the SAME scale source into both this and node.cylinder_wrap_field when muxing across a morph so .w stays continuous. `fold_angle` is the X-axis rotation of the entire field about the origin — generic enough to be useful for any toroidal field that wants continuous rotation animation.",
    examples: [],
    picker: { label: "Torus Wrap Field", category: Atom },
    summary: "Wraps a flat grid of points around a torus, a donut shape, placing copies on its surface.",
    category: Geometry3D,
    role: Map,
    aliases: ["torus wrap", "donut", "wrap"],
    boundary_reason: FusedBundle,
}

impl Primitive for TorusWrapField {
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
        let base_radius = ctx.scalar_or_param("base_radius", 0.3);
        let torus_radius = ctx.scalar_or_param("torus_radius", 1.0);
        let fold_angle = ctx.scalar_or_param("fold_angle", 0.0);
        let instance_scale = ctx.scalar_or_param("instance_scale", 1.0);

        let Some(uv_buf) = ctx.inputs.array("uv") else {
            return;
        };
        let normal_disp_wired = ctx.inputs.array("normal_disp");
        let normal_disp_buf = normal_disp_wired.unwrap_or(uv_buf);
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
                include_str!("shaders/torus_wrap_field.wgsl"),
                "cs_main",
                "node.torus_wrap_field",
            )
        });

        let uniforms = Uniforms {
            count,
            base_radius,
            torus_radius,
            fold_angle,
            instance_scale,
            has_normal_disp: u32::from(normal_disp_wired.is_some()),
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
                    buffer: normal_disp_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.torus_wrap_field",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn torus_wrap_field_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();
        let f32_layout = ArrayType::of_known::<f32>();
        let inst_layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(TorusWrapField::TYPE_ID, "node.torus_wrap_field");

        let uv_in = TorusWrapField::INPUTS.iter().find(|p| p.name == "uv").unwrap();
        assert!(uv_in.required);
        assert_eq!(uv_in.ty, PortType::Array(vec2_layout));

        let disp_in = TorusWrapField::INPUTS
            .iter()
            .find(|p| p.name == "normal_disp")
            .unwrap();
        assert!(!disp_in.required);
        assert_eq!(disp_in.ty, PortType::Array(f32_layout));

        for name in ["base_radius", "torus_radius", "fold_angle", "instance_scale"] {
            let port = TorusWrapField::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(TorusWrapField::OUTPUTS.len(), 1);
        assert_eq!(TorusWrapField::OUTPUTS[0].name, "instances");
        assert_eq!(
            TorusWrapField::OUTPUTS[0].ty,
            PortType::Array(inst_layout),
        );
    }

    #[test]
    fn torus_wrap_field_output_follows_uv_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = TorusWrapField::new();
        let params = ParamValues::default();
        let inputs = [("uv", 160_000_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "instances", &params, &inputs),
            Some(160_000),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TorusWrapField::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.torus_wrap_field");
    }
}
