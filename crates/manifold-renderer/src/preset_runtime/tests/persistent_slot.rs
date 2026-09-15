    //! Regression: a feedback chain like
    //! `source → feedback → affine → gain → vignette → mix`
    //! where `mix.out` wires back to `feedback.in` (closing the per-
    //! frame loop) must NOT have `feedback.in`'s resource (which is
    //! `mix.out`) and `feedback.out`'s resource share a physical
    //! Texture2D slot.
    //!
    //! Without dedicated slots for persistent resources, the simulator's
    //! free-pool ping-pong was assigning the same slot to both:
    //! `feedback.out` got Slot(N) at step 0, was freed at step 2 when
    //! affine read it, and Slot(N) was eventually pulled out of the
    //! pool again at mix's step for `mix.out` — making them aliases.
    //! At runtime that turns Feedback's copy(prev→out) followed by
    //! copy(in→prev) into a no-op: `in` and `out` point at the same
    //! MTLTexture, so the "capture" step reads the value the "emit"
    //! step just wrote, never picking up the producer's actual write.
    //! Symptom: feedback effects look like a pass-through with no
    //! accumulation.
    //!
    //! The fix lives in `assign_texture2d_slots`: every persistent
    //! resource pre-allocates its own slot that never enters the free
    //! pool. This test pins the contract by constructing the exact
    //! topology and asserting the two slots differ.
    use super::*;
    use crate::node_graph::primitives::{
        AffineTransform, Feedback, Gain, Mix, Vignette,
    };
    use crate::node_graph::{FinalOutput, Graph, Source, compile};

    #[test]
    fn feedback_in_and_out_get_distinct_slots_in_the_closed_loop() {
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Source::new()));
        let fb = graph.add_node(Box::new(Feedback::new()));
        let aff = graph.add_node(Box::new(AffineTransform::new()));
        let gain = graph.add_node(Box::new(Gain::new()));
        let vig = graph.add_node(Box::new(Vignette::new()));
        let mix = graph.add_node(Box::new(Mix::new()));
        let out = graph.add_node(Box::new(FinalOutput::new()));

        graph.connect((src, "out"), (mix, "a")).unwrap();
        graph.connect((fb, "out"), (aff, "in")).unwrap();
        graph.connect((aff, "out"), (gain, "in")).unwrap();
        graph.connect((gain, "out"), (vig, "in")).unwrap();
        graph.connect((vig, "out"), (mix, "b")).unwrap();
        // The state-capture edge — allowed because Feedback declares
        // `breaks_dependency_cycle`. This is the wire that would have
        // collapsed feedback.out and mix.out onto the same physical
        // slot under the pre-fix simulator.
        graph.connect((mix, "out"), (fb, "in")).unwrap();
        graph.connect((mix, "out"), (out, "in")).unwrap();

        let plan = compile(&graph).expect("feedback chain compiles");

        let src_res = plan
            .steps()
            .iter()
            .find(|s| s.node == src)
            .and_then(|s| s.outputs.iter().find(|(p, _)| *p == "out").map(|(_, r)| *r))
            .expect("source produces an out resource");
        let assignment = assign_texture2d_slots(&plan, src_res, (64, 64));

        let mix_out_res = plan
            .steps()
            .iter()
            .find(|s| s.node == mix)
            .and_then(|s| s.outputs.iter().find(|(p, _)| *p == "out").map(|(_, r)| *r))
            .expect("mix produces an out resource");
        let fb_out_res = plan
            .steps()
            .iter()
            .find(|s| s.node == fb)
            .and_then(|s| s.outputs.iter().find(|(p, _)| *p == "out").map(|(_, r)| *r))
            .expect("feedback produces an out resource");

        let mix_slot = assignment
            .resource_to_slot
            .get(&mix_out_res)
            .copied()
            .expect("mix.out has a slot");
        let fb_slot = assignment
            .resource_to_slot
            .get(&fb_out_res)
            .copied()
            .expect("feedback.out has a slot");

        assert_ne!(
            mix_slot, fb_slot,
            "mix.out and feedback.out MUST live on distinct physical slots. \
             Sharing a slot means feedback.in (which points at mix.out) and \
             feedback.out alias the same MTLTexture at runtime, and the \
             primitive's capture step reads back what its emit step just \
             wrote — feedback never accumulates state across frames. \
             Pre-fix, the simulator's free-pool ping-pong would assign \
             Slot(1) to both. The persistent-resource pre-allocation in \
             `assign_texture2d_slots` is what keeps them apart.",
        );

        // Sanity: the persistent resource's slot must be in the slot
        // assignment (mix.out is what feedback.in reads).
        let plan_persistent: std::collections::HashSet<_> =
            plan.persistent_resources().iter().copied().collect();
        assert!(
            plan_persistent.contains(&mix_out_res),
            "compile() must mark mix.out as a persistent resource — \
             without that, the slot simulator can't dedicate a slot for it"
        );
    }
