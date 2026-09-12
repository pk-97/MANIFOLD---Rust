//! `node.analytic_echo_instances` — expand each source instance into a
//! bounded, analytic arc/helix of shared mesh copies.
//!
//! Every source slot owns eight output slots.  Slot zero is the present source
//! transform; slots one through seven evaluate the same scene-space formula at
//! a stable echo index.  The atom changes position and applies coherent taper
//! to scale; Euler orientation, reflection marker, and zero-scale holes remain
//! intact.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const ECHO_CAPACITY: u32 = 8;

/// Generated-codegen uniform layout: params in declaration order followed by
/// the injected output dispatch count.  Twelve words occupy 48 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EchoUniforms {
    count: i32,
    radius: f32,
    rise: f32,
    phase: f32,
    arc: f32,
    taper: f32,
    enabled: f32,
    scene_radius: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: AnalyticEchoInstances,
    type_id: "node.analytic_echo_instances",
    purpose: "Expand each source InstanceTransform into up to eight shared-mesh echo slots along a scene-space analytic arc/helix. Echo 0 is the exact source; later slots translate coherently around sourceOffsetXYZ while preserving orientation, reflection marker, and inactive holes while taper scales the complete object transform. Count, radius, rise, phase, arc, taper, and enabled are port-shadowed live controls; radius and rise are relative to sceneRadius.",
    inputs: {
        instances: Array(InstanceTransform) required,
        count: ScalarF32 optional,
        radius: ScalarF32 optional,
        rise: ScalarF32 optional,
        phase: ScalarF32 optional,
        arc: ScalarF32 optional,
        taper: ScalarF32 optional,
        enabled: ScalarF32 optional,
        scene_radius: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
    },
    outputs: {
        instances: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("count"),
            label: "Count",
            ty: ParamType::Int,
            default: ParamValue::Float(3.0),
            range: Some((1.0, ECHO_CAPACITY as f32)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Spread / Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.75),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rise"),
            label: "Rise",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("phase"),
            label: "Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("arc"),
            label: "Arc",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-4.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("taper"),
            label: "Taper",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("enabled"),
            label: "Enabled",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scene_radius"),
            label: "Scene Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 100000.0)),
            enum_values: &[],
        },
        // Per-object preparation context.  These remain ordinary params so the
        // scene modifier compiler can port-shadow them from sourceOffsetXYZ.
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100000.0, 100000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100000.0, 100000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100000.0, 100000.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity is checked as source capacity × 8 at preparation time and never grows with Count. Source slot s maps to output slots s*8..s*8+7, so changing Count reveals stable slots. Echo 0 is byte-for-byte source; Count is clamped to 1..8, and Enabled <= 0 leaves only echo 0 active. Later echoes translate in scene space by sceneRadius * (radius*u*cos((phase+arc*u)*TAU), rise*u, radius*u*sin((phase+arc*u)*TAU)), where u = echoIndex/7. Taper scales the complete object transform around the scene origin: position = taperFactor*instancePosition + (taperFactor-1)*sourceOffsetXYZ + arc and scale = taperFactor*sourceScale, preserving relative placement of multi-part GLB objects. No Euler composition, geometry history, or opacity is involved.",
    examples: ["SpatialEchoes"],
    picker: { label: "Analytic Echoes", category: Atom },
    summary: "Fans each source object into a small, formula-driven trail of shared mesh copies.",
    category: Geometry3D,
    role: Filter,
    aliases: ["analytic echoes", "spatial echoes", "echo instances", "instance trail", "phase fan"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/analytic_echo_instances_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for AnalyticEchoInstances {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "instances" {
            return None;
        }
        let source_capacity = input_capacities
            .iter()
            .find(|(name, _)| *name == "instances")
            .map(|(_, n)| *n)?;
        source_capacity.checked_mul(ECHO_CAPACITY)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let count = ctx
            .scalar_or_param("count", 3.0)
            .round()
            .clamp(1.0, ECHO_CAPACITY as f32) as i32;
        let radius = ctx.scalar_or_param("radius", 0.75);
        let rise = ctx.scalar_or_param("rise", 0.25);
        let phase = ctx.scalar_or_param("phase", 0.0);
        let arc = ctx.scalar_or_param("arc", 1.0);
        let taper = ctx.scalar_or_param("taper", 0.0).clamp(0.0, 1.0);
        let enabled = ctx.scalar_or_param("enabled", 1.0);
        let scene_radius = ctx.scalar_or_param("scene_radius", 1.0);
        let source_offset_x = ctx.scalar_or_param("source_offset_x", 0.0);
        let source_offset_y = ctx.scalar_or_param("source_offset_y", 0.0);
        let source_offset_z = ctx.scalar_or_param("source_offset_z", 0.0);

        let Some(in_buf) = ctx.inputs.array("instances") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("instances") else {
            return;
        };
        let item_size = std::mem::size_of::<InstanceTransform>() as u64;
        let source_capacity = (in_buf.size / item_size) as u32;
        let output_capacity = (out_buf.size / item_size) as u32;
        let Some(expected_output_capacity) = source_capacity.checked_mul(ECHO_CAPACITY) else {
            log::warn!("node.analytic_echo_instances: source capacity overflow");
            return;
        };
        if source_capacity == 0 || output_capacity != expected_output_capacity {
            log::warn!(
                "node.analytic_echo_instances: source/output capacities {} / {} do not satisfy ×{} contract",
                source_capacity,
                output_capacity,
                ECHO_CAPACITY,
            );
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = EchoUniforms {
            count,
            radius,
            rise,
            phase,
            arc,
            taper,
            enabled,
            scene_radius,
            source_offset_x,
            source_offset_y,
            source_offset_z,
            dispatch_count: output_capacity,
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
            [output_capacity.div_ceil(256), 1, 1],
            "node.analytic_echo_instances",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn structured_modifier_echo_ports_and_capacity() {
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let prim = AnalyticEchoInstances::new();
        let instances = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(
            AnalyticEchoInstances::TYPE_ID,
            "node.analytic_echo_instances"
        );
        assert_eq!(
            AnalyticEchoInstances::INPUTS[0].ty,
            PortType::Array(instances)
        );
        assert!(AnalyticEchoInstances::INPUTS[0].required);
        assert_eq!(
            AnalyticEchoInstances::OUTPUTS[0].ty,
            PortType::Array(instances)
        );
        for name in [
            "count",
            "radius",
            "rise",
            "phase",
            "arc",
            "taper",
            "enabled",
            "scene_radius",
            "source_offset_x",
            "source_offset_y",
            "source_offset_z",
        ] {
            let port = AnalyticEchoInstances::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required);
        }
        assert_eq!(
            AnalyticEchoInstances::INPUT_ACCESS,
            &[crate::node_graph::freeze::classify::InputAccess::BufferGather]
        );
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "instances",
                &ParamValues::default(),
                &[("instances", 17)]
            ),
            Some(136)
        );
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "instances",
                &ParamValues::default(),
                &[("instances", u32::MAX)]
            ),
            None
        );
    }

    #[test]
    fn structured_modifier_echo_uniform_is_48_bytes() {
        assert_eq!(std::mem::size_of::<EchoUniforms>(), 48);
    }

    #[test]
    fn structured_modifier_echo_is_registered_as_filter() {
        let prim = AnalyticEchoInstances::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.analytic_echo_instances");
    }
}
