//! `node.envelope_beats` — a trigger-window pulse measured in beats.
//!
//! Each integer change on `trigger` starts a linear return from `1.0` to
//! `0.0` over `window_beats`. The beat clock is the source of truth, so the
//! same authored window has the same shape at every tempo. The first observed
//! trigger arms silently unless `initial_count` is wired and differs from it;
//! that explicit baseline is useful when a graph is created after the host has
//! already advanced its real clip/audio counter.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const DEFAULT_WINDOW_BEATS: f32 = 0.25;

crate::primitive! {
    name: EnvelopeBeats,
    type_id: "node.envelope_beats",
    purpose: "Emit a linear beat-window pulse on each integer trigger change. The first observed trigger arms silently unless `initial_count` explicitly differs, and a completed pulse stays complete across backward seeks until the next trigger.",
    inputs: {
        trigger: ScalarF32 required,
        window_beats: ScalarF32 optional,
        initial_count: ScalarF32 optional,
    },
    outputs: {
        out: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("window_beats"),
            label: "Window (beats)",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_WINDOW_BEATS),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire a trigger_count stream to `trigger`; wire a beat-window control to `window_beats` when the default quarter-beat pulse is not enough. Keep `initial_count` unwired for ordinary first-observation suppression, or wire the host's pre-event baseline to make a pre-existing count advance emit once.",
    examples: [],
    picker: { label: "Envelope Beats", category: Driver },
    summary: "Turns each trigger advance into a linear pulse whose duration is measured in musical beats.",
    category: Control,
    role: Control,
    aliases: ["beat envelope", "beat pulse", "trigger pulse"],
    boundary_reason: NonGpu,
    extra_fields: {
        last_count: Option<i32> = None,
        hit_beat: manifold_core::Beats = manifold_core::Beats::ZERO,
        active: bool = false,
    },
}

impl Primitive for EnvelopeBeats {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(trigger_value) = ctx
            .inputs
            .scalar("trigger")
            .and_then(|value| value.as_scalar())
            .filter(|value| value.is_finite())
        else {
            return;
        };
        let trigger = trigger_value.round() as i32;
        let initial_count = ctx
            .inputs
            .scalar("initial_count")
            .and_then(|value| value.as_scalar())
            .filter(|value| value.is_finite())
            .map(|value| value.round() as i32);
        let window_beats = ctx
            .scalar_or_param("window_beats", DEFAULT_WINDOW_BEATS)
            .max(0.0) as f64;
        let beat = ctx.time.beats;

        let event = match self.last_count {
            Some(last) => {
                let changed = trigger != last;
                self.last_count = Some(trigger);
                changed
            }
            None => {
                self.last_count = Some(trigger);
                initial_count.is_some_and(|baseline| baseline != trigger)
            }
        };
        if event {
            self.hit_beat = beat;
            self.active = true;
        }

        let output = if !self.active || !window_beats.is_finite() || window_beats <= 0.0 {
            self.active = false;
            0.0
        } else {
            let elapsed = (beat - self.hit_beat).0;
            if elapsed >= window_beats {
                self.active = false;
                0.0
            } else {
                (1.0 - (elapsed / window_beats)).clamp(0.0, 1.0) as f32
            }
        };
        ctx.outputs.set_scalar("out", ParamValue::Float(output));
    }

    fn clear_state(&mut self) {
        self.last_count = None;
        self.hit_beat = manifold_core::Beats::ZERO;
        self.active = false;
    }

    fn is_trigger_latch(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};

    use crate::node_graph::effect_node::{EffectNode, EffectNodeType, FrameTime};
    use crate::node_graph::execution_plan::compile;
    use crate::node_graph::ports::{
        NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
    };
    use crate::node_graph::{Executor, Graph};

    struct ScalarSink {
        type_id: EffectNodeType,
        seen: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl EffectNode for ScalarSink {
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
            crate::node_graph::depth_rule::DepthRule::Terminal
        }

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
            let value = ctx
                .inputs
                .scalar("in")
                .and_then(|value| value.as_scalar())
                .expect("scalar output");
            self.seen
                .store(value.to_bits(), std::sync::atomic::Ordering::Relaxed);
        }
    }

    struct Harness {
        graph: Graph,
        plan: crate::node_graph::execution_plan::ExecutionPlan,
        executor: Executor,
        trigger: crate::node_graph::NodeInstanceId,
        envelope: crate::node_graph::NodeInstanceId,
        seen: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl Harness {
        fn new(window: f32, initial_count: Option<f32>) -> Self {
            let mut graph = Graph::new();
            let trigger =
                graph.add_node(Box::new(crate::node_graph::primitives::value::Value::new()));
            let envelope = graph.add_node(Box::new(EnvelopeBeats::new()));
            let seen = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(f32::NAN.to_bits()));
            let sink = graph.add_node(Box::new(ScalarSink {
                type_id: EffectNodeType::new("test.envelope_beats_sink"),
                seen: seen.clone(),
            }));
            graph
                .set_param(trigger, "value", ParamValue::Float(0.0))
                .unwrap();
            graph
                .set_param(envelope, "window_beats", ParamValue::Float(window))
                .unwrap();
            graph
                .connect((trigger, "out"), (envelope, "trigger"))
                .unwrap();
            if let Some(value) = initial_count {
                let source =
                    graph.add_node(Box::new(crate::node_graph::primitives::value::Value::new()));
                graph
                    .set_param(source, "value", ParamValue::Float(value))
                    .unwrap();
                graph
                    .connect((source, "out"), (envelope, "initial_count"))
                    .unwrap();
            }
            graph.connect((envelope, "out"), (sink, "in")).unwrap();
            let plan = compile(&graph).unwrap();
            Self {
                graph,
                plan,
                executor: Executor::with_mock(),
                trigger,
                envelope,
                seen,
            }
        }

        fn tick(&mut self, trigger: f32, beat: f64) -> f32 {
            self.tick_at(trigger, beat, beat * 0.5)
        }

        fn tick_at(&mut self, trigger: f32, beat: f64, seconds: f64) -> f32 {
            self.graph
                .set_param(self.trigger, "value", ParamValue::Float(trigger))
                .unwrap();
            self.executor.execute_frame(
                &mut self.graph,
                &self.plan,
                FrameTime {
                    beats: Beats(beat),
                    seconds: Seconds(seconds),
                    delta: Seconds(1.0 / 60.0),
                    frame_count: 0,
                },
            );
            let value = f32::from_bits(self.seen.load(std::sync::atomic::Ordering::Relaxed));
            assert!(value.is_finite(), "EnvelopeBeats must emit a finite scalar");
            value
        }
    }

    #[test]
    fn declares_trigger_window_initial_inputs_and_window_param() {
        use crate::node_graph::primitive::PrimitiveSpec;
        let inputs = EnvelopeBeats::INPUTS;
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].name, "trigger");
        assert!(inputs[0].required);
        assert_eq!(inputs[1].name, "window_beats");
        assert!(!inputs[1].required);
        assert_eq!(inputs[2].name, "initial_count");
        assert!(!inputs[2].required);
        assert_eq!(EnvelopeBeats::PARAMS.len(), 1);
        assert_eq!(EnvelopeBeats::PARAMS[0].name, "window_beats");
        assert_eq!(EnvelopeBeats::PARAMS[0].range, Some((0.0, 16.0)));
    }

    #[test]
    fn same_beat_window_is_tempo_independent() {
        let mut fast = Harness::new(1.0, None);
        assert_eq!(fast.tick_at(0.0, 0.0, 0.0), 0.0);
        assert_eq!(fast.tick_at(1.0, 2.0, 0.5), 1.0);
        assert_eq!(fast.tick_at(1.0, 2.5, 0.625), 0.5);

        let mut slow = Harness::new(1.0, None);
        assert_eq!(slow.tick_at(0.0, 4.0, 4.0), 0.0);
        assert_eq!(slow.tick_at(1.0, 8.0, 8.0), 1.0);
        assert_eq!(slow.tick_at(1.0, 8.5, 8.5), 0.5);
    }

    #[test]
    fn rapid_retrigger_restarts_from_one() {
        let mut harness = Harness::new(1.0, None);
        assert_eq!(harness.tick(0.0, 0.0), 0.0);
        assert_eq!(harness.tick(1.0, 2.0), 1.0);
        assert_eq!(harness.tick(1.0, 2.5), 0.5);
        assert_eq!(harness.tick(2.0, 2.75), 1.0);
    }

    #[test]
    fn first_count_is_silent_without_baseline_and_fires_when_baseline_differs() {
        let mut silent = Harness::new(1.0, None);
        assert_eq!(silent.tick(3.0, 0.0), 0.0);

        let mut initialized = Harness::new(1.0, Some(0.0));
        assert_eq!(initialized.tick(3.0, 0.0), 1.0);
    }

    #[test]
    fn zero_window_settles_immediately_and_completed_pulse_stays_complete() {
        let mut zero = Harness::new(0.0, None);
        assert_eq!(zero.tick(0.0, 0.0), 0.0);
        assert_eq!(zero.tick(1.0, 1.0), 0.0);

        let mut completed = Harness::new(1.0, None);
        assert_eq!(completed.tick(0.0, 0.0), 0.0);
        assert_eq!(completed.tick(1.0, 2.0), 1.0);
        assert_eq!(completed.tick(1.0, 3.0), 0.0);
        assert_eq!(completed.tick(1.0, 2.5), 0.0);
    }

    #[test]
    fn clear_state_rearms_silently() {
        let mut harness = Harness::new(1.0, None);
        assert_eq!(harness.tick(0.0, 0.0), 0.0);
        assert_eq!(harness.tick(1.0, 1.0), 1.0);
        let node = harness.graph.get_node_mut(harness.envelope).unwrap();
        assert!(node.node.is_trigger_latch());
        node.node.clear_state();
        assert_eq!(harness.tick(1.0, 1.1), 0.0);
        assert_eq!(harness.tick(2.0, 1.2), 1.0);
    }
}
