
    /// Every bundled host preset, scene modifiers expanded and groups
    /// flattened. Scene-modifier recipes prepare only on a host scene, so the
    /// hosts are what the wiring guards walk.
    fn flat_bundled_hosts() -> Vec<(String, manifold_core::effect_graph_def::EffectGraphDef)> {
        use manifold_nodes::bundled_presets::{bundled_preset_def, bundled_preset_type_ids};
        use manifold_core::preset_def::PresetKind;

        let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
        let mut hosts = Vec::new();
        for kind in [PresetKind::Effect, PresetKind::Generator] {
            for type_id in bundled_preset_type_ids(kind) {
                let def = bundled_preset_def(&type_id).expect("bundled preset");
                let expanded = manifold_node_engine::load::expand::expand_scene_modifiers(def.as_ref(), &registry)
                    .unwrap_or_else(|error| panic!("{type_id}: {error}"));
                let flat = manifold_core::flatten::flatten_groups(&expanded)
                    .unwrap_or_else(|error| panic!("{type_id}: {error}"));
                hosts.push((type_id.to_string(), flat));
            }
        }
        hosts
    }

    /// The one wire into `id.port`, as (from node, from port).
    fn source<'a>(
        type_id: &str,
        flat: &'a manifold_core::effect_graph_def::EffectGraphDef,
        id: u32,
        port: &str,
    ) -> (u32, &'a str) {
        let mut wires = flat.wires.iter().filter(|w| w.to_node == id && w.to_port == port);
        let wire = wires.next().unwrap_or_else(|| panic!("{type_id}: {port} is not wired"));
        assert!(wires.next().is_none(), "{type_id}: {port} has two sources");
        (wire.from_node, wire.from_port.as_str())
    }

    fn type_of(flat: &manifold_core::effect_graph_def::EffectGraphDef, id: u32) -> Option<&str> {
        flat.nodes.iter().find(|n| n.id == id).map(|n| n.type_id.as_str())
    }

    /// Every bundled Liquid Surface meshes on the lattice a solver's frame
    /// node published: its solid, node counts and box are wired straight from
    /// one node.matter_frame or node.liquid_frame, never
    /// a hand-made transform or value that could drop the padding. A
    /// display-time solid is a node.mix_arrays of that frame's two solids.
    #[test]
    fn liquid_surface_lattice_comes_from_the_frame() {
        const FRAMES: [&str; 2] = ["node.matter_frame", "node.liquid_frame"];
        let mut checked = Vec::new();
        for (type_id, flat) in flat_bundled_hosts() {
            let source = |id: u32, port: &str| source(&type_id, &flat, id, port);
            for volume in flat.nodes.iter().filter(|n| n.type_id == "node.particle_volume") {
                let (mut frame, mut port) = source(volume.id, "solid");
                if type_of(&flat, frame) == Some("node.mix_arrays") {
                    let (a, b) = (source(frame, "a"), source(frame, "b"));
                    assert_eq!((a.1, b), ("solid_a", (a.0, "solid_b")), "{type_id}: display solid");
                    (frame, port) = a;
                }
                assert!(FRAMES.contains(&type_of(&flat, frame).unwrap_or("")), "{type_id}: solid from {port}");
                assert!(matches!(port, "solid_a" | "solid_b"), "{type_id}: solid from {port}");
                let (box_node, _) = source(volume.id, "center_x");
                assert_eq!(type_of(&flat, box_node), Some("node.transform_components"), "{type_id}: lattice box");
                assert_eq!(source(box_node, "transform"), (frame, "grid_bounds"), "{type_id}: lattice box");
                for axis in ["x", "y", "z"] {
                    let grid_nodes = format!("grid_nodes_{axis}");
                    assert_eq!(source(volume.id, &format!("nodes_{axis}")), (frame, grid_nodes.as_str()), "{type_id}");
                    for (to, from) in [("center", "pos"), ("size", "scale")] {
                        let port = format!("{from}_{axis}");
                        assert_eq!(source(volume.id, &format!("{to}_{axis}")), (box_node, port.as_str()), "{type_id}");
                    }
                }
                checked.push(type_id.clone());
            }
        }
        assert!(!checked.is_empty(), "no bundled preset meshes a liquid surface");
    }

    /// Every bundled liquid mesher reads the clamped level set: the solid and
    /// border clamp is the last step before meshing (BUG-koy0 (solid clamp
    /// before smoothing)), after any smoothing, on the same solid, box, bin
    /// size and lattice as the volume it clamps.
    #[test]
    fn liquid_surface_meshes_the_clamped_level_set() {
        const MESHERS: [&str; 2] = ["node.count_surface_triangles", "node.volume_surface_mesh"];
        let mut checked = 0;
        for (type_id, flat) in flat_bundled_hosts() {
            let source = |id: u32, port: &str| source(&type_id, &flat, id, port);
            for mesher in flat.nodes.iter().filter(|n| MESHERS.contains(&n.type_id.as_str())) {
                let (clamp, port) = source(mesher.id, "levelset");
                assert_eq!(type_of(&flat, clamp), Some("node.clamp_liquid_to_solids"), "{type_id}: mesher reads {port}");
                assert_eq!(port, "clamped", "{type_id}");
                // Back through the smoothing chain to the volume.
                let mut upstream = source(clamp, "levelset");
                while type_of(&flat, upstream.0) == Some("node.smooth_lattice") {
                    upstream = source(upstream.0, "levelset");
                }
                // Optional closing is three distinct operations: grow,
                // rebuild distance, shrink. Solids still clip last.
                if type_of(&flat, upstream.0) == Some("node.offset_lattice") {
                    assert_eq!(upstream.1, "out");
                    upstream = source(upstream.0, "levelset");
                    assert_eq!(type_of(&flat, upstream.0), Some("node.redistance_lattice"));
                    assert_eq!(upstream.1, "out");
                    upstream = source(upstream.0, "levelset");
                    assert_eq!(type_of(&flat, upstream.0), Some("node.offset_lattice"));
                    assert_eq!(upstream.1, "out");
                    upstream = source(upstream.0, "levelset");
                }
                let volume = upstream.0;
                assert_eq!(upstream, (volume, "levelset"), "{type_id}: the clamp's level set");
                assert_eq!(type_of(&flat, volume), Some("node.particle_volume"), "{type_id}: the clamp's level set");
                assert_eq!(source(clamp, "solid"), source(volume, "solid"), "{type_id}: solid");
                assert_eq!(source(clamp, "cell_size"), source(volume, "cell_size"), "{type_id}: bin size");
                for axis in ["x", "y", "z"] {
                    let volume_nodes = format!("volume_nodes_{axis}");
                    assert_eq!(source(clamp, &format!("nodes_{axis}")), (volume, volume_nodes.as_str()), "{type_id}");
                    let solid_nodes = source(clamp, &format!("solid_nodes_{axis}"));
                    assert_eq!(solid_nodes, source(volume, &format!("nodes_{axis}")), "{type_id}");
                    for side in ["center", "size"] {
                        let port = format!("{side}_{axis}");
                        assert_eq!(source(clamp, &port), source(volume, &port), "{type_id}: {port}");
                    }
                }
                checked += 1;
            }
        }
        assert!(checked > 0, "no bundled preset meshes a liquid surface");
    }
