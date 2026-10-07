#[cfg(test)]
mod migration_tests {
    use manifold_core::effect_graph_def::EffectGraphDef;
    use crate::node_graph::primitives::blob_bounds::wire_blob_bounds;
    use manifold_node_engine::graph::Graph;
    use manifold_node_engine::load::graph_loader::{instantiate_def, HandleScope, BoundaryHandling};
    use manifold_node_engine::persistence::PrimitiveRegistry;

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }
    /// Each liquid field consumer's bounds wiring as (consumer type, blob
    /// source type, blob source port), plus how many bounds nodes feed them.
    /// Panics unless every consumer reads `bounds` from a node.blob_bounds fed
    /// the consumer's own blobs.
    fn blob_bounds_wiring(def: &EffectGraphDef) -> (Vec<(String, String, String)>, usize) {
        let type_of = |id: u32| def.nodes.iter().find(|n| n.id == id).map(|n| n.type_id.clone()).expect("wired node exists");
        let source = |to: u32, port: &str| {
            let w = def.wires.iter().find(|w| w.to_node == to && w.to_port == port).unwrap_or_else(|| panic!("node {to} has no {port} wire"));
            (w.from_node, w.from_port.clone())
        };
        let mut wiring = Vec::new();
        let mut bounds_nodes = std::collections::BTreeSet::new();
        for consumer in def.nodes.iter().filter(|n| matches!(n.type_id.as_str(), "node.particle_volume" | "node.lattice_bricks")) {
            let (bounds, port) = source(consumer.id, "bounds");
            assert_eq!((type_of(bounds).as_str(), port.as_str()), ("node.blob_bounds", "bounds"));
            let blobs = source(consumer.id, "blobs");
            assert_eq!(source(bounds, "blobs"), blobs, "{}: bounds measure other blobs", consumer.type_id);
            bounds_nodes.insert(bounds);
            wiring.push((consumer.type_id.clone(), type_of(blobs.0), blobs.1));
        }
        wiring.sort();
        (wiring, bounds_nodes.len())
    }

    #[test]
    fn liquid_fields_saved_before_blob_bounds_load_with_the_shipped_wiring() {
        const SHIPPED: &str = include_str!("../../../assets/generator-presets/WaterDamBreakGpuFlip.json");
        let flat = |doc: serde_json::Value| {
            let def: EffectGraphDef = serde_json::from_value(doc).expect("parse");
            manifold_core::flatten::flatten_groups(&def).expect("flattens")
        };
        let shipped_doc: serde_json::Value = serde_json::from_str(SHIPPED).expect("shipped preset");
        let mut shipped = flat(shipped_doc.clone());
        let expected = blob_bounds_wiring(&shipped);
        assert_eq!(expected.0.len(), 2, "the shipped surface feeds both field consumers");
        assert_eq!(expected.1, 1, "one reduction serves both");
        assert!(!wire_blob_bounds(&mut shipped), "a wired graph is untouched");

        // Reconstruct the old compiled topology at any authored group depth.
        let mut old_def = flat(shipped_doc);
        let removed: Vec<_> = old_def.nodes.iter().filter(|node| node.type_id == "node.blob_bounds")
            .map(|node| node.id).collect();
        assert_eq!(removed.len(), 1, "the fixture removes the shared bounds reduction");
        old_def.nodes.retain(|node| !removed.contains(&node.id));
        old_def.wires.retain(|wire| !removed.contains(&wire.from_node) && !removed.contains(&wire.to_node));
        let mut old = old_def.clone();
        assert!(old.wires.iter().all(|w| w.to_port != "bounds"), "the fixture is the old shape");
        assert!(wire_blob_bounds(&mut old));
        assert_eq!(blob_bounds_wiring(&old), expected);
        assert!(!wire_blob_bounds(&mut old), "the migration is idempotent");

        // The old shape through the real loader builds and validates.
        let mut graph = Graph::new();
        instantiate_def(
            &mut graph,
            &old_def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
            &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default(),
        )
        .expect("the old surface builds");
        let type_of = |id| graph.get_node(id).map(|n| n.node.type_id().as_str().to_owned());
        let source = |id, port: &str| {
            graph.wires_into(id).find(|w| w.to.1 == port).map(|w| w.from).unwrap_or_else(|| panic!("{port} unwired"))
        };
        let consumers: Vec<_> = graph
            .nodes()
            .filter(|n| matches!(n.node.type_id().as_str(), "node.particle_volume" | "node.lattice_bricks"))
            .map(|n| n.id)
            .collect();
        assert_eq!(consumers.len(), 2);
        let mut bounds_nodes = Vec::new();
        for consumer in consumers {
            let (bounds, port) = source(consumer, "bounds");
            assert_eq!((type_of(bounds).as_deref(), port), (Some("node.blob_bounds"), "bounds"));
            assert_eq!(source(bounds, "blobs"), source(consumer, "blobs"), "bounds measure the consumer's blobs");
            if !bounds_nodes.contains(&bounds) {
                bounds_nodes.push(bounds);
            }
        }
        assert_eq!(bounds_nodes.len(), 1, "one reduction serves both");
        manifold_node_engine::validation::validate(&graph).expect("the migrated surface validates");

        // Peter's saved water layer. Its other pre-1180 params need the project
        // loader's migrations before the renderer builds it, so its surface is
        // checked as the loader's flattened document.
        let mut layer: EffectGraphDef = serde_json::from_str(include_str!(
            "../../../../manifold-io/tests/fixtures/water_layer_graph_v1160.json"
        ))
        .expect("saved layer");
        layer.scene_modifiers.clear();
        let mut layer = manifold_core::flatten::flatten_groups(&layer).expect("flattens");
        assert!(layer.wires.iter().all(|w| w.to_port != "bounds"), "the saved layer is the old shape");
        assert!(wire_blob_bounds(&mut layer));
        let (wiring, bounds_nodes) = blob_bounds_wiring(&layer);
        assert_eq!(bounds_nodes, 1, "{wiring:?}");
        assert!(wiring.iter().all(|(_, source, port)| source == "node.shape_particle_blobs" && port == "blobs"), "{wiring:?}");
    }

}
