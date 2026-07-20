//! `node.sample_and_hold` — capture an input scalar on trigger-edge
//! changes and hold it until the next edge.
//!
//! Companion to `node.envelope_decay` and `node.inject_burst` for the
//! "trigger-time mode hold" pattern: when a clip-trigger fires, the
//! mode active at trigger time should drive the modulation until the
//! envelope decays (or until the next trigger), even if the
//! performer wiggles the mode slider mid-decay.
//!
//! Behavior:
//! - First observation of `trigger` initialises `held` from `value`.
//! - Each subsequent integer-edge change of `trigger` re-captures
//!   `value` into `held`.
//! - Between edges, `out` emits the held value regardless of how
//!   `value` changes.
//!
//! State: `last_trigger: Option<i32>`, `held: f32` in [`StateStore`].
//! Cleared on seek / pause so re-entered clips start fresh.

use std::borrow::Cow;

use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, NodeRequires,
};
use crate::node_graph::parameters::{ParamDef, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::state_store::NodeState;

pub const SAMPLE_AND_HOLD_TYPE_ID: &str = "node.sample_and_hold";

struct HoldState {
    last_trigger: Option<i32>,
    held: f32,
}

impl NodeState for HoldState {}

const SAMPLE_AND_HOLD_INPUTS: [NodeInput; 2] = [
    NodePort {
        name: Cow::Borrowed("value"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: Cow::Borrowed("trigger"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: true,
    },
];

const SAMPLE_AND_HOLD_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Scalar(ScalarType::F32),
    kind: PortKind::Output,
    required: false,
}];

const SAMPLE_AND_HOLD_PARAMS: [ParamDef; 0] = [];

#[derive(Debug)]
pub struct SampleAndHold {
    type_id: EffectNodeType,
}

impl SampleAndHold {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(SAMPLE_AND_HOLD_TYPE_ID),
        }
    }
}

impl Default for SampleAndHold {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for SampleAndHold {
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
        &SAMPLE_AND_HOLD_INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        &SAMPLE_AND_HOLD_OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &SAMPLE_AND_HOLD_PARAMS
    }

    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: true,
            gpu_encoder: false,
        }
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let value = match ctx.inputs.scalar("value") {
            Some(ParamValue::Float(f)) => f,
            _ => return,
        };
        let trigger = match ctx.inputs.scalar("trigger") {
            Some(ParamValue::Float(f)) => f.round() as i32,
            _ => return,
        };

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let store = ctx
            .state
            .as_deref_mut()
            .expect("SampleAndHold::evaluate requires a StateStore");

        let (last_trigger, prev_held) = store
            .get::<HoldState>(node_id, owner_key)
            .map(|s| (s.last_trigger, s.held))
            .unwrap_or((None, value));

        let edge = match last_trigger {
            Some(prev) => trigger != prev,
            None => true, // first observation: capture
        };
        let held = if edge { value } else { prev_held };

        store.insert(
            node_id,
            owner_key,
            HoldState {
                last_trigger: Some(trigger),
                held,
            },
        );

        ctx.outputs.set_scalar("out", ParamValue::Float(held));
    }

    /// BUG-104: `held`/`last_trigger` live entirely in the `StateStore`
    /// (this primitive has no `extra_fields`), so there is nothing to
    /// reset on `self` — `clear_state()` stays the default no-op. Flagging
    /// `is_trigger_latch` is what lets `PresetRuntime::clear_trigger_state`
    /// purge this node's `StateStore` bucket from the outside.
    fn is_trigger_latch(&self) -> bool {
        true
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: SAMPLE_AND_HOLD_TYPE_ID,
        create: || Box::new(SampleAndHold::new()),
        picker: Some(crate::node_graph::palette::PickerInfo {
            label: "Sample & Hold",
            category: crate::node_graph::palette::PaletteCategory::Driver,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declares_value_and_trigger_inputs_and_one_scalar_out() {
        let node = SampleAndHold::new();
        let inputs = node.inputs();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].name, "value");
        assert!(inputs[0].required);
        assert_eq!(inputs[1].name, "trigger");
        assert!(inputs[1].required);
        let outputs = node.outputs();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "out");
    }

    #[test]
    fn type_id_is_node_prefixed() {
        let node = SampleAndHold::new();
        assert_eq!(node.type_id().as_str(), "node.sample_and_hold");
    }

    /// CPU mirror — confirms the trigger-edge capture semantics.
    /// Matches the legacy FluidSimCore behaviour where
    /// `active_clip_trigger_mode` is set ONCE at trigger time and
    /// stays until the next trigger.
    #[test]
    fn captures_on_edge_holds_between() {
        struct Mirror {
            last_trigger: Option<i32>,
            held: f32,
        }
        impl Mirror {
            fn new() -> Self {
                Self {
                    last_trigger: None,
                    held: 0.0,
                }
            }
            fn tick(&mut self, value: f32, trigger: i32) -> f32 {
                let edge = match self.last_trigger {
                    Some(prev) => trigger != prev,
                    None => true,
                };
                if edge {
                    self.held = value;
                }
                self.last_trigger = Some(trigger);
                self.held
            }
        }

        let mut m = Mirror::new();
        // First observation captures.
        assert_eq!(m.tick(2.0, 0), 2.0);
        // Same trigger, value changes — held value stays.
        assert_eq!(m.tick(3.0, 0), 2.0);
        assert_eq!(m.tick(4.0, 0), 2.0);
        // Edge → recapture.
        assert_eq!(m.tick(5.0, 1), 5.0);
        // Same trigger, value changes again — stays.
        assert_eq!(m.tick(0.0, 1), 5.0);
        // Next edge → recapture.
        assert_eq!(m.tick(1.0, 2), 1.0);
    }

    #[test]
    fn is_trigger_latch_flag_is_set() {
        let node = SampleAndHold::new();
        let en: &dyn crate::node_graph::EffectNode = &node;
        assert!(en.is_trigger_latch());
    }
}

/// BUG-104 — proves the exact mechanism `PresetRuntime::clear_trigger_state`
/// relies on for StateStore-backed trigger latches: since `held` lives only
/// in the `StateStore` (never on `self`), a plain `clear_state()` call does
/// nothing — release requires `StateStore::cleanup_nodes` from the outside,
/// keyed by the node id `is_trigger_latch` flagged. Needs a real GpuEncoder
/// (`execute_frame_with_state`'s signature, even for this CPU-only
/// primitive) — gated behind `gpu-proofs` like `smoothing.rs`'s equivalent
/// StateStore harness; run via `cargo test -p manifold-renderer --features
/// gpu-proofs sample_and_hold::`, never nextest.
#[cfg(all(test, feature = "gpu-proofs"))]
mod trigger_latch_release_tests {
    use super::*;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::effect_node::FrameTime;
    use crate::node_graph::execution_plan::compile;
    use crate::node_graph::graph::Graph;
    use crate::node_graph::primitives::Value;
    use crate::node_graph::state_store::StateStore;
    use crate::node_graph::Executor;
    use manifold_core::{Beats, Seconds};
    use std::sync::{Arc, Mutex};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    struct Capture {
        type_id: EffectNodeType,
        seen: Arc<Mutex<Option<f32>>>,
    }
    impl EffectNode for Capture {
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule { crate::node_graph::depth_rule::DepthRule::Terminal } // test fixture
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

    #[test]
    fn cleanup_nodes_releases_the_held_latch_so_the_next_frame_recaptures() {
        let device = crate::test_device();
        let seen = Arc::new(Mutex::new(None));
        let mut g = Graph::new();
        let value = g.add_node(Box::new(Value::new()));
        let trigger = g.add_node(Box::new(Value::new()));
        let sh = g.add_node(Box::new(SampleAndHold::new()));
        let sink = g.add_node(Box::new(Capture {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        g.set_param(value, "value", ParamValue::Float(1.0)).unwrap();
        g.set_param(trigger, "value", ParamValue::Float(0.0)).unwrap();
        g.connect((value, "out"), (sh, "value")).unwrap();
        g.connect((trigger, "out"), (sh, "trigger")).unwrap();
        g.connect((sh, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let mut store = StateStore::new();
        let mut exec = Executor::with_mock();
        let run_frame = |g: &mut Graph, exec: &mut Executor, store: &mut StateStore| {
            let mut enc = device.create_encoder("sample-and-hold-test");
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            exec.execute_frame_with_state(g, &plan, frame_time(), &mut gpu, store, 0);
        };

        // Frame 0: first observation captures value=1.0.
        run_frame(&mut g, &mut exec, &mut store);
        assert_eq!(seen.lock().unwrap().take(), Some(1.0));

        // Frame 1: value changes to 2.0 but trigger hasn't moved — held
        // stays at the captured 1.0.
        g.set_param(value, "value", ParamValue::Float(2.0)).unwrap();
        run_frame(&mut g, &mut exec, &mut store);
        assert_eq!(
            seen.lock().unwrap().take(),
            Some(1.0),
            "held should still be the frame-0 capture; the fader is latched out"
        );

        // Release exactly this node's StateStore bucket — what
        // `PresetRuntime::clear_trigger_state` does for every
        // `is_trigger_latch` node on transport stop / project load.
        store.cleanup_nodes(&[sh]);

        // Frame 2: trigger STILL hasn't moved (mirrors "Trigger disabled,
        // no new edge fires") — with the latch released, first-observation
        // semantics re-arm and the now-current value=2.0 is recaptured
        // immediately, instead of staying stuck on 1.0 forever.
        run_frame(&mut g, &mut exec, &mut store);
        assert_eq!(
            seen.lock().unwrap().take(),
            Some(2.0),
            "cleanup_nodes should release the latch so the live value reaches the output again"
        );
    }
}
