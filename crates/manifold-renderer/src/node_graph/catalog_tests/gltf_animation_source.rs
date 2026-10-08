use crate::node_graph::primitives::gltf_animation_source::{GltfAnimationSource, testkit::{translation_channel, anim_set_one_clip}};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, FrameTime};
use manifold_node_engine::parameters::{ParamDef, ParamValue};
use std::{borrow::Cow, sync::Arc};

    /// GLTF_ANIMATION_DESIGN.md A1's performer-gesture gate, at the
    /// GRAPH level: `node.lfo` (Saw, one cycle per beat) wired directly
    /// into `node.gltf_animation_source.progress`, sampled just before
    /// and just after the LFO's own wrap point (beats 0.999 and 1.001 —
    /// the LFO itself wraps `fract()` internally, already proven in
    /// `lfo.rs`'s tests). Uses a bounce-shaped translation track (rises
    /// then returns to 0 — the SAME shape `BoxAnimated.glb`'s real
    /// translation channel has, verified against the actual fixture
    /// bytes this session) so the two samples are expected to be close.
    ///
    /// NOTE for whoever reads this next to `BoxAnimated.glb` itself:
    /// that asset's ROTATION channel does NOT loop seamlessly (identity
    /// at t=0, held at 180°-about-X from t=2.5 to the clip's end at
    /// t=3.708 — verified against the raw keyframe bytes) — a real
    /// authored-content property of that specific fixture, not a
    /// sampler defect. A whole-frame pixel "near-progress-0 vs
    /// near-progress-1" render assertion would be FALSE for that asset;
    /// this test instead exercises the wrap-continuity claim on data
    /// that genuinely loops, at the value level (the reliable oracle
    /// for a computable claim), and `progress_wraps_not_clamps_at_the_loop_boundary`
    /// above independently proves the sampler's wrap-vs-clamp mechanics
    /// hold regardless of asset content.
    #[test]
    fn lfo_driven_progress_loops_a_seamless_track_continuously_at_the_wrap_point() {
        use manifold_node_engine::exec::effect_node::EffectNode;
        use manifold_node_engine::exec::effect_node::EffectNodeType;
        use manifold_node_engine::exec::execution_plan::compile;
        use manifold_node_engine::graph::Graph;
        use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
        use crate::node_graph::primitives::lfo::Lfo;
        use manifold_node_engine::exec::execution::Executor;
        use manifold_core::{Beats, Seconds};
        use std::sync::Mutex;

        struct Capture {
            type_id: EffectNodeType,
            seen: std::sync::Arc<Mutex<Option<ParamValue>>>,
        }
        impl EffectNode for Capture {
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
                *self.seen.lock().unwrap() = ctx.inputs.scalar("in");
            }
        }

        fn sample_pos_y_at(beats: f32) -> f32 {
            let seen = std::sync::Arc::new(Mutex::new(None));
            let mut g = Graph::new();
            let lfo = g.add_node(Box::new(Lfo::new()));
            g.set_param(lfo, "rate_mode", ParamValue::Enum(0)).unwrap(); // Musical
            g.set_param(lfo, "rate", ParamValue::Enum(0)).unwrap(); // "1/1" — one cycle per beat
            g.set_param(lfo, "shape", ParamValue::Enum(2)).unwrap(); // Saw

            let mut anim_prim = GltfAnimationSource::new();
            // Bounce shape matching BoxAnimated.glb's real translation
            // track: 0 -> peak -> peak (hold) -> back to 0. Pre-seeded
            // directly onto the primitive (bypassing the background
            // loader — same technique `run_with` uses) since this test
            // drives a real compiled Graph/Executor, not `run_with`'s
            // single-node harness.
            let channel = translation_channel(
                0,
                &[(0.0, [0.0, 2.52, 0.0]), (0.4, [0.0, 2.52, 0.0]), (1.0, [0.0, 0.0, 0.0])],
            );
            anim_prim.anim_set = Some(Arc::new(anim_set_one_clip(vec![channel], 1.0, 1).into()));
            let anim = g.add_node(Box::new(anim_prim));
            g.set_param(anim, "duration_s", ParamValue::Float(1.0)).unwrap();
            g.set_param(anim, "translation_node", ParamValue::Float(0.0)).unwrap();
            g.connect((lfo, "out"), (anim, "progress")).unwrap();

            let sink = g.add_node(Box::new(Capture {
                type_id: EffectNodeType::new("test.capture"),
                seen: seen.clone(),
            }));
            g.connect((anim, "pos_y"), (sink, "in")).unwrap();

            let plan = compile(&g).unwrap();
            let mut exec = Executor::with_mock();
            exec.execute_frame(
                &mut g,
                &plan,
                FrameTime {
                    beats: Beats(beats as f64),
                    seconds: Seconds(beats as f64 * 0.5),
                    delta: Seconds(1.0 / 60.0),
                    frame_count: 0,
                },
            );
            match seen.lock().unwrap().clone() {
                Some(ParamValue::Float(f)) => f,
                v => panic!("gltf_animation_source did not emit a Float on pos_y: {v:?}"),
            }
        }

        // rate=1/1 -> the LFO completes one Saw cycle per beat, so
        // beats=0.999 sits just before its own wrap and beats=1.001
        // just after (into the next cycle) — the LFO's `fract()` makes
        // these progress ~0.999 and ~0.001 respectively.
        let near_end = sample_pos_y_at(0.999);
        let near_start = sample_pos_y_at(1.001);
        assert!(
            (near_end - near_start).abs() < 0.05,
            "a seamless (0->peak->0) track must read continuously across the LFO's wrap: \
             near-end={near_end}, near-start={near_start}"
        );
    }
