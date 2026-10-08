use manifold_node_engine::validation::validate;
use manifold_node_engine::atomic::FluidSim2D;
use manifold_node_engine::{graph::Graph, scene::boundary_nodes::Source, scene::boundary_nodes::FinalOutput, exec::effect_node::FrameTime, exec::execution::Executor, exec::execution_plan::compile};
use manifold_core::{Beats, Seconds};
    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }


    /// Hero test: FluidSim's auxiliary output (density) wired through
    /// downstream nodes (Threshold → Mix with the source) and out to
    /// FinalOutput. Validates the whole atomic-with-rich-ports flow:
    /// the host can use FluidSim's internals for compositing, not just
    /// its main composited output.
    #[test]
    fn fluid_sim_density_can_be_wired_downstream() {
        use {manifold_node_engine::primitives::mix::Mix, manifold_nodes_image::node_graph::primitives::filter::Threshold};

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let fluid = g.add_node(Box::new(FluidSim2D::new()));
        let thresh = g.add_node(Box::new(Threshold::new()));
        let mix = g.add_node(Box::new(Mix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));

        // Source feeds FluidSim's source AND Mix's `a` input (fan-out).
        g.connect((src, "out"), (fluid, "source")).unwrap();
        g.connect((src, "out"), (mix, "a")).unwrap();

        // FluidSim's auxiliary `density` output drives a Threshold whose
        // result becomes the Mix `b` input.
        g.connect((fluid, "density"), (thresh, "source")).unwrap();
        g.connect((thresh, "out"), (mix, "b")).unwrap();

        g.connect((mix, "out"), (out, "in")).unwrap();

        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }
