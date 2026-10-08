use manifold_node_engine::load::loaded_preset_view::outer_routings_from_view;
use manifold_core::PresetTypeId;

    /// BUG-103 regression: a glTF-imported scene's per-object card knobs can
    /// target the `mat_k` material node that lives INSIDE that object's
    /// group box.
    /// `outer_routings_from_view` used to build its `node_id → handle` map
    /// from top-level nodes only, so those in-group bindings were silently
    /// dropped — the routing never reached the editor and the group face
    /// showed no D6 mirror row for exactly the imported-scene case the
    /// feature exists for. Drives the REAL importer + the REAL resolution
    /// path, exactly what the pristine `graph_snapshot` arm runs
    /// (`snapshot_for_view` → `outer_routings_from_view`).
    #[test]
    fn gltf_import_group_material_bindings_resolve_through_groups() {
        // BUG-w5wv: this test's whole premise is the shared Ambient knob's
        // in-group fan-out, which only exists on a `node.pbr_material`
        // (`node.unlit_material` has no `ambient` param, `fs_unlit` has no
        // lighting to fill). The azalea CC0 fixture this test used to load
        // turned out to itself declare `KHR_materials_unlit` on both its
        // materials — a real asset, not a bug — so it no longer exercises
        // this path at all. `two_material_pbr.glb` is a tiny hand-built
        // synthetic fixture (two flat-shaded triangles, two ordinary
        // non-unlit PBR materials, no Blender needed) that keeps the
        // through-groups resolution premise intact with real importer
        // machinery.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/hostile/two_material_pbr.glb");
        assert!(path.exists(), "two_material_pbr.glb fixture missing at {}", path.display());
        let (def, _report) = manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph(&path)
            .expect("assemble two_material_pbr import graph");

        // Build the LoadedPresetView the pristine path builds — same
        // canonical_def + owned bindings that `build_view` produces, just from
        // the imported def instead of a bundled catalog entry.
        let meta = def.preset_metadata.clone().expect("import def carries metadata");
        let view = manifold_node_engine::load::loaded_preset_view::testkit::imported_view(
            PresetTypeId::from_string("test.gltf_import".to_string()), def,
        );

        let routings = outer_routings_from_view(&view);

        // Every canonical binding that targets a node resolves now.
        assert_eq!(
            routings.len(),
            meta.bindings.len(),
            "every node-targeting card binding must resolve, in-group ones included"
        );

        // The payoff: the shared Ambient knob's fan-out into each object's
        // material node is present, keyed by the material node's own
        // (unprefixed) handle so the D6 group-face join (`find_node_by_handle`)
        // finds it inside the group body — the in-group resolution path
        // this test exists to cover.
        let has = |handle: &str, param: &str| {
            routings
                .iter()
                .any(|r| r.node_handle == handle && r.inner_param == param)
        };
        assert!(has("mat_0", "ambient"), "object 0's Ambient fan-out resolves inside its group");
        assert!(has("mat_1", "ambient"), "object 1's Ambient fan-out resolves inside its group");

        // And the top-level spine still resolves (no regression on the 9 that
        // always worked).
        assert!(has("camera", "orbit"), "top-level camera binding still resolves");
        assert!(has("sun", "intensity"), "top-level sun binding still resolves");
    }
