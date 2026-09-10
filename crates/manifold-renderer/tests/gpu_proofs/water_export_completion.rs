//! Native executor proof for the water completion readback boundary.
//!
//! The probe is deliberately a test-only EffectNode.  It is placed in the
//! repeat body of a real `node.water_state` graph and writes zero on the first
//! iteration and `8` on the second.  The assertion is made after the native
//! command buffer has completed, without rendering another frame.

use std::borrow::Cow;
use std::sync::Arc;

use manifold_core::Seconds;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::depth_rule::DepthRule;
use manifold_renderer::node_graph::ports::{
    ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
};
use manifold_renderer::node_graph::substeps::SimulationFrame;
use manifold_renderer::node_graph::{
    EffectNode, EffectNodeContext, EffectNodeType, ParamDef, ParamValue,
};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const STATUS_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read_write> status: array<u32>;
struct U { value: u32 };
@group(0) @binding(1) var<uniform> u: U;
@compute @workgroup_size(1) fn cs_main() { status[0] = u.value; }
"#;

struct CompletionProbe {
    ty: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    pipeline: std::sync::OnceLock<manifold_gpu::GpuComputePipeline>,
}

impl CompletionProbe {
    fn new() -> Self {
        Self {
            ty: EffectNodeType::new("test.water_completion_probe"),
            inputs: vec![
                NodePort {
                    name: Cow::Borrowed("particles"),
                    ty: PortType::Array(ArrayType::of_known::<
                        manifold_renderer::node_graph::water::WaterParticle,
                    >()),
                    kind: PortKind::Input,
                    required: true,
                },
                NodePort {
                    name: Cow::Borrowed("collider"),
                    ty: PortType::Transform,
                    kind: PortKind::Input,
                    required: true,
                },
                NodePort {
                    name: Cow::Borrowed("index"),
                    ty: PortType::Scalar(ScalarType::F32),
                    kind: PortKind::Input,
                    required: true,
                },
            ],
            outputs: vec![
                NodePort {
                    name: Cow::Borrowed("particles_out"),
                    ty: PortType::Array(ArrayType::of_known::<
                        manifold_renderer::node_graph::water::WaterParticle,
                    >()),
                    kind: PortKind::Output,
                    required: true,
                },
                NodePort {
                    name: Cow::Borrowed("collider_out"),
                    ty: PortType::Transform,
                    kind: PortKind::Output,
                    required: true,
                },
                NodePort {
                    name: Cow::Borrowed("status"),
                    ty: PortType::Array(ArrayType::of_known::<u32>()),
                    kind: PortKind::Output,
                    required: true,
                },
            ],
            pipeline: std::sync::OnceLock::new(),
        }
    }
}

impl EffectNode for CompletionProbe {
    fn type_id(&self) -> &EffectNodeType {
        &self.ty
    }
    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }
    fn parameters(&self) -> &[ParamDef] {
        static P: std::sync::OnceLock<Vec<ParamDef>> = std::sync::OnceLock::new();
        P.get_or_init(|| {
            vec![ParamDef {
                name: Cow::Borrowed("fault"),
                label: "Inject final fault",
                ty: manifold_renderer::node_graph::ParamType::Float,
                default: ParamValue::Float(1.0),
                range: Some((0.0, 1.0)),
                enum_values: &[],
            }]
        })
    }
    fn depth_rule(&self) -> DepthRule {
        DepthRule::Terminal
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let index = match ctx.inputs.scalar("index").unwrap_or(ParamValue::Float(0.0)) {
            ParamValue::Float(v) => v,
            _ => 0.0,
        };
        if let Some(collider) = ctx.inputs.transform("collider") {
            ctx.outputs.set_transform("collider_out", collider);
        }
        let status = ctx.outputs.array("status").expect("probe status output");
        let fault = ctx
            .params
            .get("fault")
            .and_then(|v| match v {
                ParamValue::Float(v) => Some(*v),
                _ => None,
            })
            .unwrap_or(1.0);
        let value = if index >= 0.5 && fault > 0.5 {
            8u32
        } else {
            0u32
        };
        let encoder = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_init(|| {
            encoder
                .device
                .create_compute_pipeline(STATUS_WGSL, "cs_main", "water-completion-probe")
        });
        encoder.native_enc.dispatch_compute(
            pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: status,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::bytes_of(&value),
                },
            ],
            [1, 1, 1],
            "water-completion-probe",
        );
    }
    fn aliased_array_io(&self) -> &[(&str, &str)] {
        &[("particles", "particles_out")]
    }
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &manifold_renderer::node_graph::ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        if port == "status" {
            Some(1)
        } else {
            inputs
                .iter()
                .find(|(name, _)| *name == "particles")
                .map(|(_, cap)| *cap)
        }
    }
}

struct CompletionSink {
    ty: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
}
impl CompletionSink {
    fn new() -> Self {
        Self {
            ty: EffectNodeType::new("test.water_completion_sink"),
            inputs: vec![NodePort {
                name: Cow::Borrowed("status"),
                ty: PortType::Array(ArrayType::of_known::<u32>()),
                kind: PortKind::Input,
                required: true,
            }],
            outputs: vec![NodePort {
                name: Cow::Borrowed("texture"),
                ty: PortType::Texture2D,
                kind: PortKind::Output,
                required: true,
            }],
        }
    }
}
impl EffectNode for CompletionSink {
    fn type_id(&self) -> &EffectNodeType {
        &self.ty
    }
    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn depth_rule(&self) -> DepthRule {
        DepthRule::Terminal
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if let Some(texture) = ctx.outputs.texture_2d("texture") {
            ctx.gpu_encoder()
                .native_enc
                .clear_texture(texture, 0.0, 0.0, 0.0, 1.0);
        }
    }
}

fn registry() -> manifold_renderer::node_graph::PrimitiveRegistry {
    let mut r = manifold_renderer::node_graph::PrimitiveRegistry::with_builtin();
    r.register("test.water_completion_probe", || {
        Box::new(CompletionProbe::new())
    });
    r.register("test.water_completion_sink", || {
        Box::new(CompletionSink::new())
    });
    r
}

// The graph is intentionally tiny: seed_water and the real water boundary
// own state; the probe is the only body node and forwards the state buffers.
fn graph_json(inject_fault: bool) -> String {
    r#"{"version":2,"name":"water-export-completion","nodes":[
      {"id":0,"typeId":"system.generator_input","nodeId":"input"},
      {"id":1,"typeId":"node.seed_water","nodeId":"seed","params":{"max_capacity":{"type":"Int","value":1}}},
      {"id":2,"typeId":"node.water_state","nodeId":"state","params":{"step_hz":{"type":"Float","value":120.0},"max_substeps":{"type":"Float","value":2.0}}},
      {"id":3,"typeId":"test.water_completion_probe","nodeId":"probe","params":{"fault":{"type":"Float","value":FAULT}}},
      {"id":4,"typeId":"test.water_completion_sink","nodeId":"sink","params":{}},
      {"id":5,"typeId":"node.transform_3d","nodeId":"collider_seed","params":{}},
      {"id":99,"typeId":"system.final_output","nodeId":"final"}],"wires":[
      {"fromNode":1,"fromPort":"out","toNode":2,"toPort":"seed"},
      {"fromNode":5,"fromPort":"transform","toNode":2,"toPort":"collider_seed"},
      {"fromNode":2,"fromPort":"out","toNode":3,"toPort":"particles"},
      {"fromNode":2,"fromPort":"collider_out","toNode":3,"toPort":"collider"},
      {"fromNode":2,"fromPort":"step_index","toNode":3,"toPort":"index"},
      {"fromNode":3,"fromPort":"particles_out","toNode":2,"toPort":"in"},
      {"fromNode":3,"fromPort":"collider_out","toNode":2,"toPort":"collider_in"},
      {"fromNode":3,"fromPort":"status","toNode":2,"toPort":"status_in"},
      {"fromNode":2,"fromPort":"status_out","toNode":4,"toPort":"status"},
      {"fromNode":4,"fromPort":"texture","toNode":99,"toPort":"in"}]}"#.replace("FAULT", if inject_fault { "1.0" } else { "0.0" })
}

#[test]
fn completed_final_substep_fault_is_reported_without_rerender() {
    let h = harness::shared();
    for inject_fault in [false, true] {
        let mut runtime = PresetRuntime::from_json_str_with_device(
            &graph_json(inject_fault),
            &registry(),
            Arc::clone(&h.device),
            h.width,
            h.height,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("water completion graph");
        let target = manifold_renderer::render_target::RenderTarget::new(
            &h.device,
            h.width,
            h.height,
            GpuTextureFormat::Rgba16Float,
            "water-export-completion",
        );
        assert!(
            runtime.runtime_fatal_error().is_none(),
            "fresh runtime must be clean"
        );
        runtime.set_simulation_frame(SimulationFrame {
            frame_id: 0,
            delta: Seconds(0.0),
            epoch: 1,
            advancing: true,
            exporting: true,
        });
        let mut ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut native = h.device.create_encoder("water-export-completion");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &Default::default());
        }
        native.commit_and_wait_completed();
        assert!(
            runtime.runtime_fatal_error().is_none(),
            "initial frame must be clean"
        );
        ctx.frame_count = 1;
        ctx.time = 1.0 / 60.0;
        runtime.set_simulation_frame(SimulationFrame {
            frame_id: 1,
            delta: Seconds(1.0 / 60.0),
            epoch: 1,
            advancing: true,
            exporting: true,
        });
        let mut native = h.device.create_encoder("water-export-completion");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &Default::default());
        }
        native.commit_and_wait_completed();
        if inject_fault {
            assert!(
                runtime
                    .runtime_fatal_error()
                    .as_deref()
                    .is_some_and(|e| e.contains("00000008"))
            );
        } else {
            assert!(runtime.runtime_fatal_error().is_none());
        }

        // A new epoch clears the completed generation; a paused zero-tick frame
        // must not report the previous frame's fault.
        ctx.frame_count = 2;
        ctx.dt = 0.0;
        runtime.set_simulation_frame(SimulationFrame {
            frame_id: 2,
            delta: Seconds(0.0),
            epoch: 2,
            advancing: false,
            exporting: true,
        });
        let mut native = h.device.create_encoder("water-export-completion-reset");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &Default::default());
        }
        native.commit_and_wait_completed();
        assert!(
            runtime.runtime_fatal_error().is_none(),
            "epoch reset/zero tick must clear stale fault"
        );
    }
}
