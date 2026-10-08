//! `node.envelope_beats` — a trigger-window pulse measured in beats.
//!
//! Each integer change on `trigger` starts an attack/hold/release event. The
//! beat clock is the source of truth, so the same authored window has the same
//! shape at every tempo. `attack_beats`, `hold_beats`, `window_beats`, and
//! `tail_beats` are read live while an event is active; changing them changes
//! that event's phase, rather than taking a snapshot on the trigger edge. The
//! first observed
//! trigger arms silently unless `initial_count` is wired and differs from it;
//! that explicit baseline is useful when a graph is created after the host has
//! already advanced its real clip/audio counter. A completed event remains
//! complete across backward seeks; seeking before an active event's trigger
//! beat cancels that event so it cannot acquire a negative phase.

use std::borrow::Cow;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

use manifold_node_engine::runtime::beat_envelope::{BeatEnvelopeState, BeatEnvelopeDurations};

const DEFAULT_WINDOW_BEATS: f32 = 0.25;

manifold_node_engine::primitive! {
    name: EnvelopeBeats,
    type_id: "node.envelope_beats",
    purpose: "Emit a beat-based attack/hold/release event on each integer trigger change. The first observed trigger arms silently unless `initial_count` explicitly differs; completed events stay complete across backward seeks, and `elapsed_beats` remains available during the optional tail.",
    inputs: {
        trigger: ScalarF32 required,
        window_beats: ScalarF32 optional,
        initial_count: ScalarF32 optional,
        attack_beats: ScalarF32 optional,
        hold_beats: ScalarF32 optional,
        tail_beats: ScalarF32 optional,
    },
    outputs: {
        out: ScalarF32,
        elapsed_beats: ScalarF32,
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
        ParamDef {
            name: Cow::Borrowed("attack_beats"),
            label: "Attack (beats)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("hold_beats"),
            label: "Hold (beats)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("tail_beats"),
            label: "Tail (beats)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire a trigger_count stream to `trigger`; use live `attack_beats`, `hold_beats`, `window_beats`, and `tail_beats` controls to shape the event in beats. Keep `initial_count` unwired for ordinary first-observation suppression, or wire the host's pre-event baseline to make a pre-existing count advance emit once. `elapsed_beats` stays available through the tail for later spatial bands while `out` is already zero.",
    examples: [],
    picker: { label: "Envelope Beats", category: Driver },
    summary: "Turns each trigger advance into a linear pulse whose duration is measured in musical beats.",
    category: Control,
    role: Control,
    aliases: ["beat envelope", "beat pulse", "trigger pulse"],
    boundary_reason: NonGpu,
    extra_fields: {
        state: BeatEnvelopeState = BeatEnvelopeState::default(),
    },
}

impl Primitive for EnvelopeBeats {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(trigger_value) = ctx
            .inputs
            .scalar("trigger")
            .and_then(|value| value.as_scalar())
        else {
            return;
        };
        let initial_count = ctx
            .inputs
            .scalar("initial_count")
            .and_then(|value| value.as_scalar());
        let output = self.state.step(
            trigger_value,
            initial_count,
            ctx.time.beats,
            BeatEnvelopeDurations {
                window: ctx.scalar_or_param("window_beats", DEFAULT_WINDOW_BEATS),
                attack: ctx.scalar_or_param("attack_beats", 0.0),
                hold: ctx.scalar_or_param("hold_beats", 0.0),
                tail: ctx.scalar_or_param("tail_beats", 0.0),
            },
        );
        let Some((output, elapsed_output)) = output else {
            return;
        };
        ctx.outputs.set_scalar("out", ParamValue::Float(output));
        ctx.outputs
            .set_scalar("elapsed_beats", ParamValue::Float(elapsed_output));
    }

    fn clear_state(&mut self) {
        self.state.clear();
    }

    fn is_trigger_latch(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};

    use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeType, FrameTime};
    use manifold_node_engine::exec::execution_plan::compile;
    use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
    use manifold_node_engine::{exec::execution::Executor, graph::Graph};

    struct ScalarSink {
        type_id: EffectNodeType,
        seen: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl EffectNode for ScalarSink {
        fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
            manifold_node_engine::scene::depth_rule::DepthRule::Terminal
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
        plan: manifold_node_engine::exec::execution_plan::ExecutionPlan,
        executor: Executor,
        trigger: manifold_node_engine::exec::effect_node::NodeInstanceId,
        envelope: manifold_node_engine::exec::effect_node::NodeInstanceId,
        seen: std::sync::Arc<std::sync::atomic::AtomicU32>,
        elapsed_seen: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl Harness {
        fn new(window: f32, initial_count: Option<f32>) -> Self {
            Self::new_with_stages(0.0, 0.0, window, 0.0, initial_count)
        }

        fn new_with_stages(
            attack: f32,
            hold: f32,
            window: f32,
            tail: f32,
            initial_count: Option<f32>,
        ) -> Self {
            let mut graph = Graph::new();
            let trigger =
                graph.add_node(Box::new(manifold_node_engine::primitives::value::Value::new()));
            let envelope = graph.add_node(Box::new(EnvelopeBeats::new()));
            let seen = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(f32::NAN.to_bits()));
            let elapsed_seen =
                std::sync::Arc::new(std::sync::atomic::AtomicU32::new(f32::NAN.to_bits()));
            let sink = graph.add_node(Box::new(ScalarSink {
                type_id: EffectNodeType::new("test.envelope_beats_sink"),
                seen: seen.clone(),
            }));
            let elapsed_sink = graph.add_node(Box::new(ScalarSink {
                type_id: EffectNodeType::new("test.envelope_beats_elapsed_sink"),
                seen: elapsed_seen.clone(),
            }));
            graph
                .set_param(trigger, "value", ParamValue::Float(0.0))
                .unwrap();
            graph
                .set_param(envelope, "window_beats", ParamValue::Float(window))
                .unwrap();
            graph
                .set_param(envelope, "attack_beats", ParamValue::Float(attack))
                .unwrap();
            graph
                .set_param(envelope, "hold_beats", ParamValue::Float(hold))
                .unwrap();
            graph
                .set_param(envelope, "tail_beats", ParamValue::Float(tail))
                .unwrap();
            graph
                .connect((trigger, "out"), (envelope, "trigger"))
                .unwrap();
            if let Some(value) = initial_count {
                let source =
                    graph.add_node(Box::new(manifold_node_engine::primitives::value::Value::new()));
                graph
                    .set_param(source, "value", ParamValue::Float(value))
                    .unwrap();
                graph
                    .connect((source, "out"), (envelope, "initial_count"))
                    .unwrap();
            }
            graph.connect((envelope, "out"), (sink, "in")).unwrap();
            graph
                .connect((envelope, "elapsed_beats"), (elapsed_sink, "in"))
                .unwrap();
            let plan = compile(&graph).unwrap();
            Self {
                graph,
                plan,
                executor: Executor::with_mock(),
                trigger,
                envelope,
                seen,
                elapsed_seen,
            }
        }

        fn tick(&mut self, trigger: f32, beat: f64) -> f32 {
            self.tick_at(trigger, beat, beat * 0.5)
        }

        fn tick_values(&mut self, trigger: f32, beat: f64) -> (f32, f32) {
            self.tick_at(trigger, beat, beat * 0.5);
            (
                f32::from_bits(self.seen.load(std::sync::atomic::Ordering::Relaxed)),
                f32::from_bits(self.elapsed_seen.load(std::sync::atomic::Ordering::Relaxed)),
            )
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
    fn extracted_state_preserves_nonfinite_trigger_and_clear_semantics() {
        let mut state = BeatEnvelopeState::default();
        let durations = BeatEnvelopeDurations {
            window: 1.0,
            ..Default::default()
        };
        assert_eq!(state.step(f32::NAN, None, Beats(0.0), durations), None);
        assert_eq!(
            state.step(0.0, None, Beats(0.0), durations),
            Some((0.0, -1.0))
        );
        assert_eq!(
            state.step(1.0, None, Beats(1.0), durations),
            Some((1.0, 0.0))
        );
        state.clear();
        assert_eq!(
            state.step(1.0, None, Beats(1.0), durations),
            Some((0.0, -1.0))
        );
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

    #[test]
    fn overnight_modifier_attack_hold_release_tail_and_elapsed_are_beat_based() {
        let mut harness = Harness::new_with_stages(1.0, 2.0, 3.0, 1.0, Some(0.0));
        assert_eq!(harness.tick_values(1.0, 0.0), (0.0, 0.0));
        assert_eq!(harness.tick_values(1.0, 0.5), (0.5, 0.5));
        assert_eq!(harness.tick_values(1.0, 1.0), (1.0, 1.0));
        assert_eq!(harness.tick_values(1.0, 2.5), (1.0, 2.5));
        assert_eq!(harness.tick_values(1.0, 3.0), (1.0, 3.0));
        assert_eq!(harness.tick_values(1.0, 4.5), (0.5, 4.5));
        assert_eq!(harness.tick_values(1.0, 6.0), (0.0, 6.0));
        assert_eq!(harness.tick_values(1.0, 6.5), (0.0, 6.5));
        assert_eq!(harness.tick_values(1.0, 7.0), (0.0, -1.0));
    }

    #[test]
    fn overnight_modifier_backward_seek_cancels_live_event_without_resurrection() {
        let mut harness = Harness::new_with_stages(0.0, 0.0, 4.0, 2.0, Some(0.0));
        assert_eq!(harness.tick(1.0, 10.0), 1.0);
        assert_eq!(harness.tick_values(1.0, 9.0), (0.0, -1.0));
        assert_eq!(harness.tick_values(1.0, 10.5), (0.0, -1.0));
        assert_eq!(harness.tick_values(2.0, 10.5), (1.0, 0.0));
    }
}
