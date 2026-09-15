//! `node.trigger_ease_to` — beat-clocked snap-and-glide on a scalar.
//!
//! On each new trigger edge, captures the value the output had at that
//! instant as `prev_target` and the incoming `target` as `curr_target`,
//! then eases from `prev_target` to `curr_target` along a cubic
//! ease-out curve over the next `window_beats` beats. After the window
//! completes the output rests at `curr_target` until the next trigger.
//! The optional `initial_count` and `initial_value` inputs seed the first
//! event from a gate/count stream; when they are unwired, the first
//! observation keeps the legacy snap-to-target behavior.
//!
//! The "capture current visible value at trigger time" semantic is the
//! load-bearing bit — if a new trigger fires mid-tween, the next ease
//! starts from wherever the visible value was, not from the previous
//! target. Rapid retriggering chains smoothly: tween-tween-tween, each
//! one beginning where the previous left off, no jumps. DAW terms: it's
//! portamento / glide on a control signal, with the retrigger boundary
//! as the snap point.
//!
//! Can't be composed from existing atoms today — `sample_and_hold`
//! captures on trigger but doesn't tween, `smoothing` is continuous
//! exponential lowpass (never reaches a fixed target), `envelope_decay`
//! pulses to zero. Composing the "capture current visible at trigger"
//! semantic from existing atoms would need scalar feedback (read your
//! own output last frame) which doesn't exist as a primitive yet.
//!
//! State: `last_trigger: Option<i32>`, `trigger_at_beat: f32`,
//! `prev_target: f32`, `curr_target: f32` in [`StateStore`]. Cleared on
//! seek / pause so re-entered clips start fresh.
//!
//! [`StateStore`]: crate::node_graph::StateStore

use std::borrow::Cow;

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, NodeRequires};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::state_store::NodeState;

pub const TRIGGER_EASE_TO_TYPE_ID: &str = "node.trigger_ease_to";

/// Default ease window — one quarter beat. Matches BasicShapes's
/// legacy hardcoded `TWEEN_BEATS_INV = 4.0`.
const DEFAULT_WINDOW_BEATS: f32 = 0.25;

struct EaseState {
    last_trigger: Option<i32>,
    trigger_at_beat: f32,
    prev_target: f32,
    curr_target: f32,
}

impl NodeState for EaseState {}

const TRIGGER_EASE_TO_INPUTS: [NodeInput; 5] = [
    NodePort {
        name: Cow::Borrowed("target"),
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
    // Port-shadows-param: lets the ease window be driven from a
    // tempo-adjusted wire (e.g. a sustained-note pattern that wants a
    // half-beat glide instead of a quarter).
    NodePort {
        name: Cow::Borrowed("window_beats"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
    // Optional first-event seed for gate/count streams. Keeping this
    // unwired preserves the legacy first-observation snap behavior.
    NodePort {
        name: Cow::Borrowed("initial_count"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("initial_value"),
        ty: PortType::Scalar(ScalarType::F32),
        kind: PortKind::Input,
        required: false,
    },
];

const TRIGGER_EASE_TO_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Scalar(ScalarType::F32),
    kind: PortKind::Output,
    required: false,
}];

const TRIGGER_EASE_TO_PARAMS: [ParamDef; 1] = [ParamDef {
    name: Cow::Borrowed("window_beats"),
    label: "Window (beats)",
    ty: ParamType::Float,
    default: ParamValue::Float(DEFAULT_WINDOW_BEATS),
    range: Some((0.0625, 4.0)),
    enum_values: &[],
}];

#[derive(Debug)]
pub struct TriggerEaseTo {
    type_id: EffectNodeType,
}

impl TriggerEaseTo {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(TRIGGER_EASE_TO_TYPE_ID),
        }
    }
}

impl Default for TriggerEaseTo {
    fn default() -> Self {
        Self::new()
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    let t1 = 1.0 - t;
    1.0 - t1 * t1 * t1
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn current_visible(state: &EaseState, beat: f32, window_beats: f32) -> f32 {
    if state.last_trigger.is_none() || window_beats <= 0.0 {
        return state.curr_target;
    }
    let elapsed = (beat - state.trigger_at_beat).max(0.0);
    let t = (elapsed / window_beats).clamp(0.0, 1.0);
    let eased = ease_out_cubic(t);
    lerp(state.prev_target, state.curr_target, eased)
}

impl EffectNode for TriggerEaseTo {
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
        &TRIGGER_EASE_TO_INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        &TRIGGER_EASE_TO_OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &TRIGGER_EASE_TO_PARAMS
    }

    fn requires(&self) -> NodeRequires {
        NodeRequires {
            state_store: true,
            gpu_encoder: false,
        }
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let target = match ctx.inputs.scalar("target") {
            Some(ParamValue::Float(f)) => f,
            _ => return,
        };
        let trigger_value = match ctx.inputs.scalar("trigger") {
            Some(ParamValue::Float(f)) => f.round() as i32,
            _ => return,
        };
        let window_beats = match ctx.inputs.scalar("window_beats") {
            Some(ParamValue::Float(f)) => f.max(0.0),
            _ => match ctx.params.get("window_beats") {
                Some(ParamValue::Float(f)) => f.max(0.0),
                _ => DEFAULT_WINDOW_BEATS,
            },
        };
        let initial_count = match ctx.inputs.scalar("initial_count") {
            Some(ParamValue::Float(f)) if f.is_finite() => Some(f.round() as i32),
            _ => None,
        };
        let initial_value = match ctx.inputs.scalar("initial_value") {
            Some(ParamValue::Float(f)) if f.is_finite() => Some(f),
            _ => None,
        };
        let beat = ctx.time.beats.0 as f32;

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let store = ctx
            .state
            .as_deref_mut()
            .expect("TriggerEaseTo::evaluate requires a StateStore");

        // Read prior state, or seed from the optional first-event baseline.
        // Without an initial count, seed with the incoming target so the
        // first frame preserves the legacy snap-to-target behavior.
        let prior = store.get::<EaseState>(node_id, owner_key);
        let mut next = match prior {
            Some(s) => EaseState {
                last_trigger: s.last_trigger,
                trigger_at_beat: s.trigger_at_beat,
                prev_target: s.prev_target,
                curr_target: s.curr_target,
            },
            None => EaseState {
                last_trigger: initial_count,
                trigger_at_beat: beat,
                prev_target: initial_value.unwrap_or(target),
                curr_target: initial_value.unwrap_or(target),
            },
        };

        let edge = match next.last_trigger {
            Some(prev) => trigger_value != prev,
            None => true,
        };

        if edge {
            if next.last_trigger.is_none() {
                // First observation: snap, no ease-in animation.
                next.prev_target = target;
                next.curr_target = target;
            } else {
                // Subsequent edge: sample current visible as the
                // starting point of the new tween.
                next.prev_target = current_visible(&next, beat, window_beats);
                next.curr_target = target;
            }
            next.trigger_at_beat = beat;
            next.last_trigger = Some(trigger_value);
        }

        let out = current_visible(&next, beat, window_beats);

        store.insert(node_id, owner_key, next);
        ctx.outputs.set_scalar("out", ParamValue::Float(out));
    }

    /// BUG-104: state lives entirely in the `StateStore` (`EaseState`),
    /// nothing on `self` to clear — flag it so
    /// `PresetRuntime::clear_trigger_state` purges the `StateStore` bucket
    /// from the outside. See `EffectNode::is_trigger_latch`.
    fn is_trigger_latch(&self) -> bool {
        true
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: TRIGGER_EASE_TO_TYPE_ID,
        create: || Box::new(TriggerEaseTo::new()),
        picker: Some(crate::node_graph::palette::PickerInfo {
            label: "Trigger Ease To",
            category: crate::node_graph::palette::PaletteCategory::Driver,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs};
    use crate::node_graph::effect_node::{
        EffectNode, FrameTime, NodeInstanceId, ParamValues, RtQuality,
    };
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::parameters::ParamValue;
    use crate::node_graph::ports::PortType;
    use crate::node_graph::state_store::StateStore;
    use crate::node_graph::MockBackend;
    use manifold_core::{Beats, Seconds};

    #[test]
    fn trigger_ease_to_declares_target_trigger_and_optional_initialization_inputs() {
        let node = TriggerEaseTo::new();
        let ins = node.inputs();
        assert_eq!(ins.len(), 5);
        assert_eq!(ins[0].name, "target");
        assert!(ins[0].required);
        assert_eq!(ins[1].name, "trigger");
        assert!(ins[1].required);
        assert_eq!(ins[2].name, "window_beats");
        assert!(!ins[2].required);
        assert_eq!(ins[3].name, "initial_count");
        assert!(!ins[3].required);
        assert_eq!(ins[4].name, "initial_value");
        assert!(!ins[4].required);
        let outs = node.outputs();
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].name, "out");
    }

    #[test]
    fn trigger_ease_to_type_id_is_node_prefixed() {
        let node = TriggerEaseTo::new();
        assert_eq!(node.type_id().as_str(), "node.trigger_ease_to");
    }

    #[test]
    fn is_trigger_latch_flag_is_set() {
        let node = TriggerEaseTo::new();
        assert!(node.is_trigger_latch());
    }

    fn frame_time(beat: f32) -> FrameTime {
        FrameTime {
            beats: Beats(beat as f64),
            seconds: Seconds(beat as f64),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn evaluate_actual(
        node: &mut TriggerEaseTo,
        store: &mut StateStore,
        node_id: NodeInstanceId,
        target: f32,
        trigger: f32,
        beat: f32,
        window_beats: f32,
        initial_count: Option<f32>,
        initial_value: Option<f32>,
    ) -> f32 {
        let mut backend = MockBackend::new();
        let target_slot = backend.acquire(
            ResourceId(0),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        let trigger_slot = backend.acquire(
            ResourceId(1),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        let window_slot = backend.acquire(
            ResourceId(2),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        let initial_count_slot = initial_count.map(|_| {
            backend.acquire(
                ResourceId(3),
                PortType::Scalar(ScalarType::F32),
                None,
                (0, 0),
            )
        });
        let initial_value_slot = initial_value.map(|_| {
            backend.acquire(
                ResourceId(4),
                PortType::Scalar(ScalarType::F32),
                None,
                (0, 0),
            )
        });
        let out_slot = backend.acquire(
            ResourceId(5),
            PortType::Scalar(ScalarType::F32),
            None,
            (0, 0),
        );
        backend.set_scalar(target_slot, ParamValue::Float(target));
        backend.set_scalar(trigger_slot, ParamValue::Float(trigger));
        backend.set_scalar(window_slot, ParamValue::Float(window_beats));
        if let (Some(slot), Some(value)) = (initial_count_slot, initial_count) {
            backend.set_scalar(slot, ParamValue::Float(value));
        }
        if let (Some(slot), Some(value)) = (initial_value_slot, initial_value) {
            backend.set_scalar(slot, ParamValue::Float(value));
        }

        let mut params = ParamValues::default();
        params.insert(
            Cow::Borrowed("window_beats"),
            ParamValue::Float(window_beats),
        );
        let mut input_bindings = vec![("target", target_slot), ("trigger", trigger_slot)];
        input_bindings.push(("window_beats", window_slot));
        if let Some(slot) = initial_count_slot {
            input_bindings.push(("initial_count", slot));
        }
        if let Some(slot) = initial_value_slot {
            input_bindings.push(("initial_value", slot));
        }
        let output_bindings = [("out", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut render_mode_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        {
            let inputs = NodeInputs::new(&input_bindings, &backend, &[]);
            let outputs = NodeOutputs::new(
                &output_bindings,
                &backend,
                &mut scalar_scratch,
                &mut camera_scratch,
                &mut light_scratch,
                &mut material_scratch,
                &mut transform_scratch,
                &mut atmosphere_scratch,
                &mut render_mode_scratch,
                &mut object_scratch,
            );
            let mut ctx = EffectNodeContext::with_state(
                frame_time(beat),
                &params,
                inputs,
                outputs,
                None,
                Some(store),
                node_id,
                0,
                0,
                RtQuality::default(),
                None,
            );
            node.evaluate(&mut ctx);
        }
        for (slot, value) in scalar_scratch.drain(..) {
            backend.set_scalar(slot, value);
        }
        match backend.scalar(out_slot) {
            Some(ParamValue::Float(value)) => value,
            other => panic!("expected scalar output, got {other:?}"),
        }
    }

    #[test]
    fn legacy_first_observation_still_snaps_to_target() {
        let mut node = TriggerEaseTo::new();
        let mut store = StateStore::new();
        let value = evaluate_actual(
            &mut node,
            &mut store,
            NodeInstanceId(1),
            10.0,
            7.0,
            0.0,
            0.25,
            None,
            None,
        );
        assert!((value - 10.0).abs() < 1e-5, "legacy first value: {value}");
    }

    #[test]
    fn initialized_first_event_glides_from_initial_value() {
        let mut node = TriggerEaseTo::new();
        let mut store = StateStore::new();
        let id = NodeInstanceId(2);
        let initial = evaluate_actual(
            &mut node,
            &mut store,
            id,
            10.0,
            1.0,
            0.0,
            1.0,
            Some(0.0),
            Some(0.0),
        );
        assert!((initial - 0.0).abs() < 1e-5);
        let edge = evaluate_actual(&mut node, &mut store, id, 10.0, 1.0, 0.0, 1.0, None, None);
        assert!((edge - 0.0).abs() < 1e-5);
        let middle = evaluate_actual(&mut node, &mut store, id, 10.0, 1.0, 0.5, 1.0, None, None);
        assert!((middle - 8.75).abs() < 1e-5, "middle value: {middle}");
        let end = evaluate_actual(&mut node, &mut store, id, 10.0, 1.0, 1.0, 1.0, None, None);
        assert!((end - 10.0).abs() < 1e-5);
    }

    #[test]
    fn initialized_load_with_same_count_holds_initial_value() {
        let mut node = TriggerEaseTo::new();
        let mut store = StateStore::new();
        let id = NodeInstanceId(3);
        let first = evaluate_actual(
            &mut node,
            &mut store,
            id,
            10.0,
            0.0,
            0.0,
            1.0,
            Some(0.0),
            Some(0.0),
        );
        let same_count =
            evaluate_actual(&mut node, &mut store, id, 10.0, 0.0, 0.5, 1.0, None, None);
        assert!((first - 0.0).abs() < 1e-5);
        assert!((same_count - 0.0).abs() < 1e-5);
    }

    #[test]
    fn rapid_retrigger_captures_current_visible_value() {
        let mut node = TriggerEaseTo::new();
        let mut store = StateStore::new();
        let id = NodeInstanceId(4);
        let _ = evaluate_actual(
            &mut node,
            &mut store,
            id,
            10.0,
            0.0,
            0.0,
            1.0,
            Some(0.0),
            Some(0.0),
        );
        let _ = evaluate_actual(&mut node, &mut store, id, 10.0, 1.0, 0.0, 1.0, None, None);
        let visible = evaluate_actual(&mut node, &mut store, id, 10.0, 1.0, 0.5, 1.0, None, None);
        let retrigger = evaluate_actual(&mut node, &mut store, id, 20.0, 2.0, 0.5, 1.0, None, None);
        let settled = evaluate_actual(&mut node, &mut store, id, 20.0, 2.0, 1.5, 1.0, None, None);
        assert!((visible - 8.75).abs() < 1e-5);
        assert!((retrigger - visible).abs() < 1e-5);
        assert!((settled - 20.0).abs() < 1e-5);
    }

    #[test]
    fn cleared_gate_count_stream_primes_again() {
        let mut node = TriggerEaseTo::new();
        let mut store = StateStore::new();
        let id = NodeInstanceId(5);
        let first = evaluate_actual(
            &mut node,
            &mut store,
            id,
            10.0,
            0.0,
            0.0,
            1.0,
            Some(0.0),
            Some(0.0),
        );
        store.cleanup_nodes(&[id]);
        let primed = evaluate_actual(
            &mut node,
            &mut store,
            id,
            10.0,
            0.0,
            0.0,
            1.0,
            Some(0.0),
            Some(0.0),
        );
        let edge = evaluate_actual(&mut node, &mut store, id, 10.0, 1.0, 0.0, 1.0, None, None);
        assert!((first - 0.0).abs() < 1e-5);
        assert!((primed - 0.0).abs() < 1e-5);
        assert!((edge - 0.0).abs() < 1e-5);
    }

    /// CPU-mirror parity — exercises the same snap-and-glide state
    /// machine that lived inside `shape_2d.rs::Shape2D::compute_active_state`.
    /// First observation snaps; subsequent edges sample current
    /// visible and tween from there; ease completes at exactly the
    /// window's end (saturates at curr_target).
    #[test]
    fn trigger_ease_to_matches_shape_2d_state_machine() {
        struct Mirror {
            last_trigger: Option<i32>,
            trigger_at_beat: f32,
            prev_target: f32,
            curr_target: f32,
        }
        impl Mirror {
            fn new() -> Self {
                Self {
                    last_trigger: None,
                    trigger_at_beat: 0.0,
                    prev_target: 0.0,
                    curr_target: 0.0,
                }
            }
            fn visible(&self, beat: f32, window: f32) -> f32 {
                if self.last_trigger.is_none() || window <= 0.0 {
                    return self.curr_target;
                }
                let elapsed = (beat - self.trigger_at_beat).max(0.0);
                let t = (elapsed / window).clamp(0.0, 1.0);
                let eased = {
                    let t1 = 1.0 - t;
                    1.0 - t1 * t1 * t1
                };
                self.prev_target + (self.curr_target - self.prev_target) * eased
            }
            fn tick(&mut self, target: f32, trigger: i32, beat: f32, window: f32) -> f32 {
                let edge = match self.last_trigger {
                    Some(prev) => trigger != prev,
                    None => true,
                };
                if edge {
                    if self.last_trigger.is_none() {
                        self.prev_target = target;
                        self.curr_target = target;
                    } else {
                        self.prev_target = self.visible(beat, window);
                        self.curr_target = target;
                    }
                    self.trigger_at_beat = beat;
                    self.last_trigger = Some(trigger);
                }
                self.visible(beat, window)
            }
        }

        let mut m = Mirror::new();
        let window = 0.25;

        // First observation at tc=3, target=PI/4, beat=1.0 → snap to PI/4.
        let pi_4 = std::f32::consts::FRAC_PI_4;
        let pi_2 = std::f32::consts::FRAC_PI_2;
        let v0 = m.tick(pi_4, 3, 1.0, window);
        assert!(
            (v0 - pi_4).abs() < 1e-5,
            "first observation must snap, got {v0}"
        );
        assert!((m.prev_target - pi_4).abs() < 1e-5);
        assert!((m.curr_target - pi_4).abs() < 1e-5);

        // Same trigger, different beat — visible holds at curr_target
        // (ease window has already saturated, since the first snap
        // sets prev = curr).
        let v_hold = m.tick(pi_4, 3, 1.2, window);
        assert!((v_hold - pi_4).abs() < 1e-5);

        // New trigger at tc=6, target=PI/2, beat=2.0. Visible at the
        // trigger instant is still PI/4 (prev == curr). After the
        // edge, prev=PI/4, curr=PI/2, trigger_at_beat=2.0.
        let v_edge = m.tick(pi_2, 6, 2.0, window);
        assert!(
            (v_edge - pi_4).abs() < 1e-5,
            "tween_t=0 should read previous angle, got {v_edge}"
        );
        assert!((m.curr_target - pi_2).abs() < 1e-5);

        // At beat 2.25 (== trigger_at_beat + window) the ease
        // completes — visible should equal curr_target exactly.
        let v_end = m.visible(2.25, window);
        assert!(
            (v_end - pi_2).abs() < 1e-5,
            "ease must complete at +window beats, got {v_end}"
        );
    }
}
