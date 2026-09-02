//! `node.scene_array` — emit a linear `Array<InstanceTransform>` along one
//! axis, for scene-loop instancing (SCENE_LOOP_DESIGN.md D2).
//!
//! One instance per copy, translated `i * cell_size` along the chosen axis.
//! The same node feeds ALL object groups — copy count changes are one param
//! write, not N. Source atom on the freeze codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const AXIS_LABELS: &[&str] = &["+X", "-X", "+Y", "-Y", "+Z", "-Z"];

/// Generated-codegen uniform layout. Params in PARAMS order:
/// count (Int→i32), axis (Enum→u32), cell_size (f32), then dispatch_count (u32).
/// 4 words = 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneArrayUniforms {
    count: i32,
    axis: u32,
    cell_size: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: SceneArray,
    type_id: "node.scene_array",
    purpose: "Linear Array<InstanceTransform> along one axis for scene-loop instancing. count copies, each translated i * cell_size along axis (+X/-X/+Y/-Y/+Z/-Z). The same node feeds ALL object groups — copy count changes are one param write, not N. Identity TRS (no rotation, unit scale) for each instance. Source atom on the freeze codegen path.",
    inputs: {},
    outputs: {
        out: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("count"),
            label: "Count",
            ty: ParamType::Int,
            default: ParamValue::Float(3.0),
            range: Some((1.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(4), // +Z
            range: None,
            enum_values: AXIS_LABELS,
        },
        ParamDef {
            name: Cow::Borrowed("cell_size"),
            label: "Cell Size",
            ty: ParamType::Float,
            default: ParamValue::Float(10.0),
            range: Some((0.01, 1000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Source atom — no inputs. Output capacity = count (1..8). The same cell_size value feeds both this node and node.loop_camera — the plan builder computes it once from scene_bounds so camera travel per loop equals instance spacing by construction (SCENE_LOOP_DESIGN D4).",
    examples: [],
    picker: { label: "Scene Array", category: Atom },
    summary: "Lays out copies in a line along one axis, spacing them evenly for a looping flythrough.",
    category: Geometry3D,
    role: Source,
    aliases: ["scene array", "instance line", "loop copies"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/scene_array_body.wgsl"),
}

impl Primitive for SceneArray {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let count = params
            .get("count")
            .and_then(|v| v.as_u32_clamped(1))
            .unwrap_or(3);
        Some(count)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let count = match ctx.params.get("count") {
            Some(ParamValue::Float(n)) => (*n).round().clamp(1.0, 8.0) as u32,
            _ => 3,
        };
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 4, // +Z
        };
        let cell_size = match ctx.params.get("cell_size") {
            Some(ParamValue::Float(f)) => *f,
            _ => 10.0,
        };

        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let item_size = std::mem::size_of::<InstanceTransform>() as u64;
        let capacity = (out_buf.size / item_size) as u32;
        let count = count.min(capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.scene_array standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.scene_array",
            )
        });

        let uniforms = SceneArrayUniforms {
            count: count as i32,
            axis,
            cell_size,
            dispatch_count: capacity,
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
            "node.scene_array",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scene_array_declares_zero_inputs_and_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(SceneArray::TYPE_ID, "node.scene_array");
        assert!(SceneArray::INPUTS.is_empty());
        assert_eq!(SceneArray::OUTPUTS.len(), 1);
        assert_eq!(SceneArray::OUTPUTS[0].name, "out");
        assert_eq!(SceneArray::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn scene_array_has_three_params() {
        let names: Vec<&str> = SceneArray::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["count", "axis", "cell_size"]);
    }

    #[test]
    fn axis_enum_has_six_options() {
        let axis_param = SceneArray::PARAMS
            .iter()
            .find(|p| p.name == "axis")
            .expect("axis param");
        assert_eq!(axis_param.ty, ParamType::Enum);
        assert_eq!(axis_param.enum_values.len(), 6);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SceneArray::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scene_array");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    /// CPU oracle: compute the expected InstanceTransform array for given params.
    fn cpu_scene_array(count: u32, axis: u32, cell_size: f32) -> Vec<InstanceTransform> {
        (0..count)
            .map(|i| {
                let t = i as f32 * cell_size;
                let mut pos_scale = [0.0f32; 4];
                let rot_pad = [0.0f32; 4];
                pos_scale[3] = 1.0; // unit scale
                match axis {
                    0 => pos_scale[0] = t, // +X
                    1 => pos_scale[0] = -t, // -X
                    2 => pos_scale[1] = t, // +Y
                    3 => pos_scale[1] = -t, // -Y
                    4 => pos_scale[2] = t, // +Z
                    5 => pos_scale[2] = -t, // -Z
                    _ => pos_scale[2] = t,
                }
                InstanceTransform { pos_scale, rot_pad }
            })
            .collect()
    }

    #[test]
    fn scene_array_matches_cpu_all_axes() {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<SceneArray>()
            .expect("scene_array codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, crate::node_graph::freeze::codegen::ENTRY, "scene_array_test");

        for axis in 0u32..6u32 {
            let count = 5u32;
            let cell_size = 7.5f32;
            let capacity = count;

            let out_buf = device.create_buffer_shared(capacity as u64 * 32);
            let mut enc = device.create_encoder("scene_array_test");
            let uniforms = SceneArrayUniforms {
                count: count as i32,
                axis,
                cell_size,
                dispatch_count: capacity,
            };
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                    GpuBinding::Buffer { binding: 1, buffer: &out_buf, offset: 0 },
                ],
                [capacity.div_ceil(256), 1, 1],
                "scene_array_test",
            );
            enc.commit_and_wait_completed();

            let expected = cpu_scene_array(count, axis, cell_size);
            let ptr = out_buf.mapped_ptr().expect("shared out buffer");
            let gpu_data =
                unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, capacity as usize) };

            for i in 0..capacity as usize {
                for c in 0..4 {
                    assert!(
                        (gpu_data[i].pos_scale[c] - expected[i].pos_scale[c]).abs() < 1e-6,
                        "axis {axis} slot {i} pos_scale[{c}]: gpu={} expected={}",
                        gpu_data[i].pos_scale[c],
                        expected[i].pos_scale[c]
                    );
                    assert!(
                        (gpu_data[i].rot_pad[c] - expected[i].rot_pad[c]).abs() < 1e-6,
                        "axis {axis} slot {i} rot_pad[{c}]"
                    );
                }
            }
        }
    }
}
