use manifold_node_engine::{graph::Graph, snapshot::GraphSnapshot};

    /// `GraphSnapshot::from_def` builds a snapshot directly from an
    /// `EffectGraphDef`, matching the editor's per-card path.
    #[test]
    fn from_def_builds_snapshot_with_named_handles() {
        use manifold_node_engine::persistence::EffectGraphDefExt;
        use manifold_core::effect_graph_def::EffectGraphDef;

        // Build a small named-handle graph via an existing composite
        // builder and serialize it to a def — the round-trip should
        // produce the same snapshot structure end-to-end.
        let mut g = Graph::new();
        let src = g.add_node_named(
            "source",
            Box::new(manifold_node_engine::scene::boundary_nodes::Source::new()),
        );
        let handle = manifold_nodes_image::node_graph::composites::build_soft_focus(&mut g, (src, "out"))
            .expect("build_soft_focus");
        let _out = g.add_node_named(
            "final_output",
            Box::new(manifold_node_engine::scene::boundary_nodes::FinalOutput::new()),
        );
        let final_out_id = g.node_id_by_handle("final_output").unwrap();
        g.connect(handle.output(), (final_out_id, "in")).unwrap();

        let def = EffectGraphDef::from_graph(&g);
        let snap = GraphSnapshot::from_def(&def).expect("from_def succeeds");

        // Same number of nodes + wires as the live graph.
        // 4 nodes: Source, blur, mix, FinalOutput.
        // 4 wires: src→blur.source, src→mix.a, blur.out→mix.b, mix.out→final.in.
        assert_eq!(snap.nodes.len(), 4);
        assert_eq!(snap.wires.len(), 4);
        // Named handles survive.
        assert!(snap
            .nodes
            .iter()
            .any(|n| n.node_handle.as_deref() == Some("source")));
        assert!(snap
            .nodes
            .iter()
            .any(|n| n.node_handle.as_deref() == Some("blur")));
        assert!(snap
            .nodes
            .iter()
            .any(|n| n.node_handle.as_deref() == Some("mix")));
        assert!(snap
            .nodes
            .iter()
            .any(|n| n.node_handle.as_deref() == Some("final_output")));
    }

    /// Regression: `editor_pos` saved in the def must survive
    /// `from_def`. Without the overlay step the round-trip through a
    /// live `Graph` strips positions and the editor canvas
    /// auto-lays-out on every reopen.
    #[test]
    fn from_def_preserves_editor_pos_through_overlay() {
        use manifold_node_engine::persistence::EffectGraphDefExt;
        use manifold_core::effect_graph_def::EffectGraphDef;

        // Build a minimal soft-focus graph with a moved Source node.
        let mut g = Graph::new();
        let src = g.add_node_named(
            "source",
            Box::new(manifold_node_engine::scene::boundary_nodes::Source::new()),
        );
        let handle = manifold_nodes_image::node_graph::composites::build_soft_focus(&mut g, (src, "out"))
            .expect("build_soft_focus");
        let final_out = g.add_node_named(
            "final_output",
            Box::new(manifold_node_engine::scene::boundary_nodes::FinalOutput::new()),
        );
        g.connect(handle.output(), (final_out, "in")).unwrap();
        let mut def = EffectGraphDef::from_graph(&g);
        // Simulate a MoveGraphNodeCommand on the Source node (doc id 0).
        def.nodes[0].editor_pos = Some((123.0, 456.0));

        let snap = GraphSnapshot::from_def(&def).expect("from_def succeeds");
        let snap_source = snap
            .nodes
            .iter()
            .find(|n| n.node_handle.as_deref() == Some("source"))
            .unwrap();
        assert_eq!(snap_source.editor_pos, Some((123.0, 456.0)));
    }
