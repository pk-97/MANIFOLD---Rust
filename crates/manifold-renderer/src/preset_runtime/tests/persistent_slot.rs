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
        AffineTransform, AudioSpectrum, Feedback, Gain, Mix, Vignette,
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

    #[test]
    fn transient_slots_reuse_only_matching_resolved_dimensions() {
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Source::new()));
        let spectrum = graph.add_node(Box::new(AudioSpectrum::new()));
        let mix = graph.add_node(Box::new(Mix::new()));
        let gain_1 = graph.add_node(Box::new(Gain::new()));
        let gain_2 = graph.add_node(Box::new(Gain::new()));
        let gain_3 = graph.add_node(Box::new(Gain::new()));
        let out = graph.add_node(Box::new(FinalOutput::new()));

        graph.connect((src, "out"), (mix, "a")).unwrap();
        graph.connect((spectrum, "out"), (mix, "b")).unwrap();
        graph.connect((mix, "out"), (gain_1, "in")).unwrap();
        graph.connect((gain_1, "out"), (gain_2, "in")).unwrap();
        graph.connect((gain_2, "out"), (gain_3, "in")).unwrap();
        graph.connect((gain_3, "out"), (out, "in")).unwrap();

        let plan = compile(&graph).expect("mixed fixed/canvas chain compiles");
        let source_res = plan
            .steps()
            .iter()
            .find(|step| step.node == src)
            .and_then(|step| step.outputs.iter().find(|(port, _)| *port == "out"))
            .map(|(_, res)| *res)
            .expect("source produces an out resource");
        let resource_for = |node| {
            plan.steps()
                .iter()
                .find(|step| step.node == node)
                .and_then(|step| step.outputs.iter().find(|(port, _)| *port == "out"))
                .map(|(_, res)| *res)
                .expect("node produces an out resource")
        };

        let assignment = assign_texture2d_slots(&plan, source_res, (1080, 1920));
        let spectrum_res = resource_for(spectrum);
        let mix_res = resource_for(mix);
        let gain_1_res = resource_for(gain_1);
        let gain_2_res = resource_for(gain_2);
        let gain_3_res = resource_for(gain_3);

        let slot = |res| assignment.resource_to_slot[&res];
        assert_eq!(assignment.slot_dims[slot(spectrum_res).0 as usize], (512, 256));
        assert_eq!(assignment.slot_dims[slot(mix_res).0 as usize], (1080, 1920));
        assert_eq!(assignment.slot_dims[slot(gain_1_res).0 as usize], (1080, 1920));
        assert_eq!(assignment.slot_dims[slot(gain_2_res).0 as usize], (1080, 1920));
        assert_eq!(assignment.slot_dims[slot(gain_3_res).0 as usize], (1080, 1920));
        assert_ne!(slot(spectrum_res), slot(gain_1_res));

        // The canvas chain still recycles its transient slots once their
        // lifetimes end; the fixed-size spectrum slot cannot be substituted.
        assert_eq!(slot(gain_2_res), slot(mix_res));
        assert_eq!(slot(gain_3_res), slot(gain_1_res));
        assert_eq!(assignment.slot_count, 4);
    }
