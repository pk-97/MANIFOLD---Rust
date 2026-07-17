//! `node.smoothing` — exponential one-pole filter on a scalar wire.
//!
//! Takes an incoming scalar (Luminance, audio amplitude, MIDI CC,
//! whatever) and emits the smoothed signal on its `out` port. The
//! filter coefficient comes from a `time_constant` param (in seconds)
//! and the frame's `delta` — the *response time* of the filter is
//! roughly the time constant, independent of frame rate.
//!
//! First stateful scalar primitive. Holds a single `f32` (the
//! previous smoothed value) in the runtime's [`StateStore`], keyed by
//! `node_id + owner_key` — same pattern Feedback uses for its texture
//! state. Cleared on seek / pause via `clear_state` so playback
//! resumes from the input value rather than a frozen pre-pause value.
//!
//! [`StateStore`]: crate::node_graph::StateStore

use std::borrow::Cow;
use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, NodeRequires,
};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::state_store::NodeState;

pub const SMOOTHING_TYPE_ID: &str = "node.smoothing";

const SMOOTHING_INPUTS: [NodeInput; 3] = [
    NodePort {
        name: Cow::Borrowed("in"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: true,
    },
    // Port-shadows-param for `time_constant`: when wired, the
    // upstream scalar overrides the param every frame. Lets one
    // shared Value node feed several Smoothings (e.g. two compass
    // axes that have to share a "reactivity" handle on the card).
    NodePort {
        name: Cow::Borrowed("time_constant"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
    // Reset on integer-edge changes. Zeroes `previous` so the next
    // frame's emit is `0 + (input - 0) * alpha = input * alpha`.
    // First observation arms without firing so chain rebuilds don't
    // cause spurious resets. Matches `array_feedback` /
    // `node.feedback` edge-detect shape.
    NodePort {
        name: Cow::Borrowed("reset_trigger"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
];

const SMOOTHING_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Scalar(ScalarType::F32),
    kind: PortKind::Output,
    required: false,
}];

const SMOOTHING_PARAMS: [ParamDef; 1] = [ParamDef {
    name: Cow::Borrowed("time_constant"),
    label: "Time Constant (s)",
    ty: ParamType::Float,
    // 100ms — fast enough to track musical gestures, slow enough to
    // tame frame-to-frame jitter on noisy sources like Luminance.
    default: ParamValue::Float(0.1),
    range: Some((0.001, 2.0)),
    enum_values: &[],
}];

#[derive(Debug)]
pub struct Smoothing {
    type_id: EffectNodeType,
    /// Last observed `reset_trigger` integer. `None` until the first
    /// observation. Lives on the primitive struct (not StateStore)
    /// to match the `array_feedback` convention — chain rebuilds
    /// reset this to `None` and the first frame post-rebuild arms
    /// without firing, so a held-high trigger doesn't cause an
    /// unintended state clear after editing.
    last_reset_trigger: Option<i32>,
}

impl Smoothing {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(SMOOTHING_TYPE_ID),
            last_reset_trigger: None,
        }
    }
}

impl Default for Smoothing {
    fn default() -> Self {
        Self::new()
    }
}

/// Persistent state. One f32 — the previous emitted value.
struct SmoothingState {
    previous: f32,
}

impl NodeState for SmoothingState {}

impl EffectNode for Smoothing {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::NonGpu)
    }
    fn inputs(&self) -> &[NodeInput] {
        &SMOOTHING_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &SMOOTHING_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &SMOOTHING_PARAMS
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let input = match ctx.inputs.scalar("in") {
            Some(ParamValue::Float(f)) => f,
            _ => 0.0,
        };
        // Wire-driven `time_constant` shadows the param when present.
        // Same port-shadows-param convention Gain / AffineTransform
        // use. Floor at 0.001 to keep `1 - exp(-dt/tau)` finite.
        let time_constant = match ctx.inputs.scalar("time_constant") {
            Some(ParamValue::Float(f)) => f.max(0.001),
            _ => match ctx.params.get("time_constant") {
                Some(ParamValue::Float(f)) => f.max(0.001),
                _ => 0.1,
            },
        };
        let dt = ctx.time.delta.0 as f32;

        // EMA coefficient: alpha = 1 - exp(-dt / tau). At dt = tau,
        // alpha ≈ 0.63; the smoothed value moves ~63% of the way
        // toward the input over one time-constant. Frame-rate
        // independent: same response shape at 30fps and 120fps.
        let alpha = 1.0 - (-dt / time_constant).exp();

        // Identity for state lookup. Same pattern as Feedback.
        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let store = ctx
            .state
            .as_deref_mut()
            .expect("Smoothing::evaluate requires a StateStore");

        // Reset-on-trigger: on integer-edge changes of
        // `reset_trigger`, drop `previous` to 0 so the next emit is
        // `0 + (input - 0) * alpha = input * alpha`. First
        // observation arms without firing. Read BEFORE the previous
        // lookup so the same frame's emit reflects the cleared
        // state (matches the texture-feedback contract — reset
        // affects this frame's output, not next frame's).
        let mut reset_now = false;
        if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("reset_trigger") {
            let current = v.round() as i32;
            let edge = match self.last_reset_trigger {
                Some(prev_count) => current != prev_count,
                None => false,
            };
            self.last_reset_trigger = Some(current);
            reset_now = edge;
        }

        // First frame for this owner: initialise to the input so we
        // don't bleed from 0 toward the first real measurement.
        // Reset edge: drop to 0 regardless of stored state.
        let previous = if reset_now {
            0.0
        } else {
            store
                .get::<SmoothingState>(node_id, owner_key)
                .map(|s| s.previous)
                .unwrap_or(input)
        };
        let smoothed = previous + (input - previous) * alpha;
        store.insert(
            node_id,
            owner_key,
            SmoothingState { previous: smoothed },
        );

        ctx.outputs
            .set_scalar("out", ParamValue::Float(smoothed));
    }

    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: true,
            gpu_encoder: false,
        }
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: SMOOTHING_TYPE_ID,
        create: || Box::new(Smoothing::new()),
        picker: Some(crate::node_graph::palette::PickerInfo {
            label: "Smoothing",
            category: crate::node_graph::palette::PaletteCategory::Driver,
        }),
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};
    use std::sync::{Arc, Mutex};

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::effect_node::FrameTime;
    use crate::node_graph::execution_plan::compile;
    use crate::node_graph::graph::Graph;
    use crate::node_graph::primitives::Value;
    use crate::node_graph::state_store::StateStore;
    use crate::node_graph::Executor;

    fn frame_time(dt_secs: f32) -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(dt_secs as f64),
            frame_count: 0,
        }
    }

    struct Capture {
        type_id: EffectNodeType,
        seen: Arc<Mutex<Option<f32>>>,
    }
    impl EffectNode for Capture {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: Cow::Borrowed("in"),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            }];
            &INPUTS
        }
        fn outputs(&self) -> &[NodeOutput] {
            &[]
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("in") {
                *self.seen.lock().unwrap() = Some(v);
            }
        }
    }

    fn drive(
        input_value: f32,
        time_constant: f32,
        frames: usize,
        dt_per_frame: f32,
    ) -> Vec<f32> {
        // CPU-only primitive but execute_frame_with_state still wants a
        // real GpuEncoder. Construct one against the shared test device;
        // Smoothing won't dispatch anything through it.
        let device = crate::test_device();
        let seen = Arc::new(Mutex::new(None));
        let mut g = Graph::new();
        let val = g.add_node(Box::new(Value::new()));
        let smooth = g.add_node(Box::new(Smoothing::new()));
        let sink = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        g.set_param(val, "value", ParamValue::Float(input_value)).unwrap();
        g.set_param(
            smooth,
            "time_constant",
            ParamValue::Float(time_constant),
        )
        .unwrap();
        g.connect((val, "out"), (smooth, "in")).unwrap();
        g.connect((smooth, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let mut store = StateStore::new();
        let mut exec = Executor::with_mock();
        let mut samples = Vec::with_capacity(frames);
        for _ in 0..frames {
            let mut enc = device.create_encoder("smoothing-test");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                exec.execute_frame_with_state(
                    &mut g,
                    &plan,
                    frame_time(dt_per_frame),
                    &mut gpu,
                    &mut store,
                    0,
                );
            }
            // No need to commit — nothing was encoded.
            samples.push(seen.lock().unwrap().expect("Capture saw a value"));
        }
        samples
    }

    /// First frame's emit equals the input — no bleed from zero.
    #[test]
    fn first_frame_initialises_to_input() {
        let samples = drive(0.7, 0.1, 1, 1.0 / 60.0);
        assert!(
            (samples[0] - 0.7).abs() < 1e-5,
            "first emit should equal input, got {}",
            samples[0],
        );
    }

    /// Held input → stays at the input forever (no drift).
    #[test]
    fn constant_input_holds_value() {
        let samples = drive(0.5, 0.1, 10, 1.0 / 60.0);
        for (i, s) in samples.iter().enumerate() {
            assert!(
                (s - 0.5).abs() < 1e-5,
                "frame {i}: expected 0.5, got {s}",
            );
        }
    }

    /// At dt ≈ time_constant the smoothed value moves ~63% toward
    /// the target on a single step. Exact value: 1 - exp(-1) ≈ 0.6321.
    /// Seed previous=0 (run a frame with input=0), then step to 1 with
    /// dt = tau; expect ~0.6321.
    #[test]
    fn single_step_at_tau_reaches_about_63_percent() {
        let device = crate::test_device();
        let seen = Arc::new(Mutex::new(None));
        let mut g = Graph::new();
        let val = g.add_node(Box::new(Value::new()));
        let smooth = g.add_node(Box::new(Smoothing::new()));
        let sink = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        g.set_param(val, "value", ParamValue::Float(0.0)).unwrap();
        g.set_param(smooth, "time_constant", ParamValue::Float(0.1)).unwrap();
        g.connect((val, "out"), (smooth, "in")).unwrap();
        g.connect((smooth, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let mut store = StateStore::new();
        let mut exec = Executor::with_mock();
        // Frame 0: seed previous = 0.0 (matches input).
        {
            let mut enc = device.create_encoder("smoothing-tau-0");
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            exec.execute_frame_with_state(&mut g, &plan, frame_time(0.1), &mut gpu, &mut store, 0);
        }
        // Jump the input to 1.0 and step one frame at dt = tau.
        g.set_param(val, "value", ParamValue::Float(1.0)).unwrap();
        {
            let mut enc = device.create_encoder("smoothing-tau-1");
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            exec.execute_frame_with_state(&mut g, &plan, frame_time(0.1), &mut gpu, &mut store, 0);
        }
        let v = seen.lock().unwrap().expect("captured");
        // alpha = 1 - exp(-1) ≈ 0.6321; smoothed = 0 + (1 - 0) * 0.6321.
        assert!(
            (v - 0.6321).abs() < 0.005,
            "expected ~0.6321 at dt=tau, got {v}",
        );
    }

    /// Frame-rate independence: smoothing at 30fps and 120fps over
    /// the same wall-clock time should produce approximately the
    /// same trajectory shape.
    #[test]
    fn frame_rate_independence_across_30_and_120fps() {
        // 100ms total simulated time on each.
        let samples_30 = drive(1.0, 0.05, 3, 1.0 / 30.0); // dt ~33ms × 3 ≈ 100ms
        let samples_120 = drive(1.0, 0.05, 12, 1.0 / 120.0); // dt ~8ms × 12 ≈ 100ms
        let final_30 = samples_30[samples_30.len() - 1];
        let final_120 = samples_120[samples_120.len() - 1];
        // First-frame initialisation == input == 1.0 in both cases.
        // After that, subsequent input is also 1.0, so both stay at 1.0.
        assert!(
            (final_30 - final_120).abs() < 0.02,
            "30fps end={final_30}, 120fps end={final_120}",
        );
    }
}
