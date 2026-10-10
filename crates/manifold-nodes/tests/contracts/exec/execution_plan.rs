mod tests {
use manifold_node_engine::graph::Graph;
use manifold_node_engine::exec::execution_plan::compile;
    /// D6(a) follow-up (`docs/DEPTH_RELIGHT_DESIGN.md`): end-to-end proof
    /// using the REAL registered primitives, now that `node.edge_slope`
    /// (aliased "gradient" — a gradient-producing central-difference atom)
    /// and `node.surface_bumps` have both been converted to `GatherTexel` +
    /// `precision_critical`. A gradient-producer → surface_bumps chain
    /// (source → edge_slope → surface_bumps) must promote edge_slope's own
    /// output to Rgba32Float — closing the loop the synthetic TestNode
    /// tests above only approximated.
    #[test]
    fn real_gradient_producer_into_surface_bumps_gets_fp32_intermediate() {
        use manifold_node_engine::persistence::PrimitiveRegistry;

        let registry = PrimitiveRegistry::with_builtin();
        let mut g = Graph::new();
        let source = g.add_node(registry.construct("system.source").expect("source boundary registered"));
        let edge_slope = g.add_node(
            registry
                .construct("node.edge_slope")
                .expect("node.edge_slope registered"),
        );
        let surface_bumps = g.add_node(
            registry
                .construct("node.surface_bumps")
                .expect("node.surface_bumps registered"),
        );
        g.connect((source, "out"), (edge_slope, "in")).unwrap();
        g.connect((edge_slope, "out"), (surface_bumps, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let r_edge_slope_out = plan
            .steps()
            .iter()
            .find(|s| s.node == edge_slope)
            .and_then(|s| s.outputs.iter().find(|(n, _)| *n == "out"))
            .map(|&(_, r)| r)
            .expect("edge_slope's out resource is bound (surface_bumps wires it)");

        assert_eq!(
            plan.resource_format(r_edge_slope_out),
            Some(manifold_gpu::GpuTextureFormat::Rgba32Float),
            "node.edge_slope's output feeding node.surface_bumps's precision_critical \
             `in` (both now GatherTexel, no filtering consumer here) must be \
             allocated fp32",
        );
    }
}
