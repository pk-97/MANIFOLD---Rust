//! Persistent artistic foam fraction from water particle deformation.
//! The strain proxy approximates air entrainment; it is not a physical bubble model.
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;
use crate::node_graph::water::WaterParticle;
use manifold_gpu::{GpuBinding, GpuBuffer};
use std::borrow::Cow;

pub const WGSL: &str = include_str!("shaders/water_foam.wgsl");
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FoamUniforms {
    count: u32,
    dt: f32,
    gain: f32,
    decay: f32,
}

crate::primitive! {
 name: WaterFoam, type_id: "node.water_foam",
 purpose: "Persistent foam fraction from symmetric particle deformation and speed; an artistic air-entrainment approximation, not physical bubbles.",
 inputs: { particles: Array(WaterParticle) required, reset_trigger: ScalarF32 optional, time_scale: ScalarF32 optional, gain: ScalarF32 optional, half_life: ScalarF32 optional },
 outputs: { foam: Array(f32) },
 params: [
  ParamDef { name: Cow::Borrowed("gain"), label: "Foam gain", ty: ParamType::Float, default: ParamValue::Float(3.0), range: Some((0.0, 100.0)), enum_values: &[] },
  ParamDef { name: Cow::Borrowed("half_life"), label: "Foam half-life", ty: ParamType::Float, default: ParamValue::Float(2.0), range: Some((0.05, 60.0)), enum_values: &[] },
 ],
 depth_rule: Terminal, composition_notes: "Computes foam in one GPU update and captures the f32 state per owner; inactive slots clear.", examples: [], picker: { label: "Water Foam", category: Atom }, summary: "Persistent water foam approximation.", category: Particles3D, role: Filter, aliases: ["water foam"], boundary_reason: CrossFrameState,
}
struct FoamState {
    previous: GpuBuffer,
    capacity_bytes: u64,
    last_reset_trigger: Option<i32>,
    epoch: u64,
    last_frame_id: Option<u64>,
}
impl NodeState for FoamState {}

impl WaterFoam {
    pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
        let _ = device.create_compute_pipeline(WGSL, "cs_main", "node.water_foam");
    }
}

impl Primitive for WaterFoam {
    fn requires(&self) -> crate::node_graph::effect_node::NodeRequires {
        crate::node_graph::effect_node::NodeRequires {
            state_store: true,
            gpu_encoder: true,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _p: &crate::node_graph::effect_node::ParamValues,
        caps: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "foam")
            .then(|| {
                caps.iter()
                    .find(|(n, _)| *n == "particles")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(foam) = ctx.outputs.array("foam") else {
            return;
        };
        let count = (particles.size / 96).min(foam.size / 4) as u32;
        if count == 0 {
            return;
        }
        let Some(frame) = ctx.simulation_frame else {
            ctx.gpu_encoder().native_enc.clear_buffer(foam);
            ctx.error("node.water_foam: simulation clock required");
            return;
        };
        let gain = ctx.scalar_or_param("gain", 3.0);
        let half = ctx.scalar_or_param("half_life", 2.0);
        let time_scale = ctx.scalar_or_param("time_scale", 1.0);
        if !gain.is_finite()
            || gain < 0.0
            || !half.is_finite()
            || half < 0.05
            || !time_scale.is_finite()
            || time_scale < 0.0
            || !frame.delta.0.is_finite()
            || frame.delta.0 < 0.0
        {
            ctx.gpu_encoder().native_enc.clear_buffer(foam);
            ctx.error("node.water_foam: invalid gain, half-life, time scale or frame delta");
            return;
        }
        ctx.mark_gpu_accessed();
        let store = ctx
            .state
            .as_deref_mut()
            .expect("WaterFoam requires StateStore");
        let size = count as u64 * 4;
        let reset = ctx.inputs.scalar("reset_trigger").and_then(|v| match v {
            ParamValue::Float(x) => Some(x.round() as i32),
            _ => None,
        });
        let gpu = ctx.gpu.as_deref_mut().expect("WaterFoam requires GPU");
        let allocated = if store
            .get::<FoamState>(ctx.node_id, ctx.owner_key)
            .is_none_or(|s| s.capacity_bytes != size)
        {
            let previous = gpu.device.create_buffer(size);
            store.insert(
                ctx.node_id,
                ctx.owner_key,
                FoamState {
                    previous,
                    capacity_bytes: size,
                    last_reset_trigger: None,
                    epoch: frame.epoch,
                    last_frame_id: None,
                },
            );
            true
        } else {
            false
        };
        let state = store
            .get::<FoamState>(ctx.node_id, ctx.owner_key)
            .expect("foam state");
        let edge = reset.is_some_and(|v| state.last_reset_trigger.is_some_and(|old| old != v));
        if let Some(v) = reset {
            state.last_reset_trigger = Some(v);
        }
        let reset_fired = allocated || edge || state.epoch != frame.epoch;
        if reset_fired {
            gpu.native_enc.clear_buffer(&state.previous);
        }
        let dt = if reset_fired || !frame.advancing || state.last_frame_id == Some(frame.frame_id) {
            0.0
        } else {
            frame.delta.0 as f32 * time_scale
        };
        state.epoch = frame.epoch;
        state.last_frame_id = Some(frame.frame_id);
        let uniforms = FoamUniforms {
            count,
            dt,
            gain,
            decay: std::f32::consts::LN_2 / half.max(0.05),
        };
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(WGSL, "cs_main", "node.water_foam")
        });
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &state.previous,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: foam,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.water_foam",
        );
        // Our output is produced above, with no delayed graph input/back-edge.
        // Capture before the graph may recycle it; late_capture is not scheduled
        // for nodes without state-capture input ports.
        gpu.native_enc
            .copy_buffer_to_buffer(foam, &state.previous, size);
    }
}
