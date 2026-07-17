//! `node.trigger_ease_to` — beat-clocked snap-and-glide on a scalar.
//!
//! On each new trigger edge, captures the value the output had at that
//! instant as `prev_target` and the incoming `target` as `curr_target`,
//! then eases from `prev_target` to `curr_target` along a cubic
//! ease-out curve over the next `window_beats` beats. After the window
//! completes the output rests at `curr_target` until the next trigger.
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

use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, NodeRequires,
};
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

const TRIGGER_EASE_TO_INPUTS: [NodeInput; 3] = [
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
        let beat = ctx.time.beats.0 as f32;

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;
        let store = ctx
            .state
            .as_deref_mut()
            .expect("TriggerEaseTo::evaluate requires a StateStore");

        // Read prior state, or seed with the incoming target so the
        // first frame doesn't animate from 0 to the real value.
        let prior = store.get::<EaseState>(node_id, owner_key);
        let mut next = match prior {
            Some(s) => EaseState {
                last_trigger: s.last_trigger,
                trigger_at_beat: s.trigger_at_beat,
                prev_target: s.prev_target,
                curr_target: s.curr_target,
            },
            None => EaseState {
                last_trigger: None,
                trigger_at_beat: beat,
                prev_target: target,
                curr_target: target,
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

    #[test]
    fn trigger_ease_to_declares_target_trigger_optional_window_and_one_scalar_out() {
        let node = TriggerEaseTo::new();
        let ins = node.inputs();
        assert_eq!(ins.len(), 3);
        assert_eq!(ins[0].name, "target");
        assert!(ins[0].required);
        assert_eq!(ins[1].name, "trigger");
        assert!(ins[1].required);
        assert_eq!(ins[2].name, "window_beats");
        assert!(!ins[2].required);
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
        assert!((v0 - pi_4).abs() < 1e-5, "first observation must snap, got {v0}");
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
