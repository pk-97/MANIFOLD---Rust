use manifold_nodes_scene::node_graph::scene_vm::{SceneVm, SceneObjectVm, MaterialVm};
use manifold_core::{SceneNodeRef, effect_graph_def::EffectGraphDef};
use manifold_nodes_scene::node_graph::scene_vm::testkit::ORBIT_CAMERA_TYPE_ID;

    /// Regression gate for the migrated-project shape `migrate_scene_object_wires`
    /// actually produces (D5's "same-scope re-point"): a minted `node.scene_object`
    /// stays a ROOT-level sibling of the mesh producer's group, `vertices`/
    /// `material`/`transform` wired straight from the GROUP's own boundary port —
    /// not nested inside it (the shape a fresh glTF import produces instead,
    /// already covered by this file's `grouped_scene_object_def`-style tests).
    /// without `resolve_producer_through_group`,
    /// the shipped, already-migrated bundled scene preset (Scene and
    /// the ~9 others P2 regenerated) silently showed no transform/material
    /// controls and a wrong vertex count in the panel, despite rendering
    /// correctly (the render path reads through `SceneObject`'s resolved Slots,
    /// never through this trace).
    #[test]
    fn bundled_scene_starter_preset_resolves_transform_material_and_vertex_count() {
        let preset_type = manifold_core::PresetTypeId::from_string("Scene".to_string());
        let d = manifold_nodes::bundled_presets::bundled_preset_def(&preset_type)
            .expect("Scene is a bundled preset");
        let vm = SceneVm::from_def(d).expect("Scene resolves");
        assert_eq!(vm.objects.len(), 1, "Cube");
        for obj in &vm.objects {
            let SceneObjectVm::Known(row) = obj else {
                panic!("Scene's objects must resolve Known, not Custom — migration shape unparsed");
            };
            assert!(row.transform.is_some(), "{}: transform must resolve through the group boundary", row.name);
            assert!(
                !matches!(row.material, MaterialVm::None),
                "{}: material must resolve through the group boundary",
                row.name
            );
        }
        assert!(vm.header.vertex_count > 0, "vertex count must resolve through the group boundary, not silently 0");
        assert!(vm.header.vertex_count_exact, "Scene's mesh sources have known vertex counts");
    }
    /// The GPU liquid's water is found by the same walk forces use, through
    /// its Liquid Surface group, so its object carries the domain's panel.
    #[test]
    fn scene_vm_traces_matter_domain() {
        let def: EffectGraphDef = serde_json::from_str(include_str!(
            "../../../../assets/generator-presets/WaterDamBreakMatter.json"
        ))
        .expect("preset parses");
        let vm = SceneVm::from_def(&def).expect("scene resolves");
        let domain = def
            .nodes
            .iter()
            .filter_map(|group| Some((group, group.group.as_deref()?)))
            .find_map(|(group, body)| {
                let node = body.nodes.iter().find(|node| {
                    node.type_id == manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID
                })?;
                Some(SceneNodeRef { scope: vec![group.node_id.clone()], node: node.node_id.clone() })
            })
            .expect("the preset has a matter domain");
        let water: Vec<_> = vm
            .objects
            .iter()
            .filter_map(|row| match row {
                SceneObjectVm::Known(row) if row.liquid_domain.is_some() => Some(row),
                _ => None,
            })
            .collect();
        assert_eq!(water.len(), 1, "one water object");
        assert_eq!(water[0].liquid_domain.as_ref(), Some(&domain));
        assert_eq!(water[0].fluid_controls.first(), Some(&domain.node));
        // The domain shares its group-local doc id with the root camera.
        let camera = def.nodes.iter().find(|node| node.type_id == ORBIT_CAMERA_TYPE_ID).unwrap();
        assert!(!water[0].fluid_controls.contains(&camera.node_id));
        assert_eq!(vm.camera_controls.first(), Some(&camera.node_id));
        assert!(!vm.camera_controls.contains(&domain.node));
    }
    #[test]
    fn water_family_row_ownership() {
        for preset in ["WaterDamBreakGpuFlip", "WaterDamBreakParticles"] {
            let preset_type = manifold_core::PresetTypeId::new(preset);
            let def = manifold_nodes::bundled_presets::bundled_preset_def(&preset_type)
                .expect("water family preset");
            let vm = SceneVm::from_def(def).expect("water family scene resolves");
            let family: Vec<_> = vm.objects.iter().filter_map(|object| match object {
                SceneObjectVm::Known(row)
                    if row.name == "Water" || row.parent_group_id.is_some() => Some(row),
                _ => None,
            }).collect();
            assert_eq!(family.iter().filter(|row| row.name == "Water").count(), 1, "{preset}");
            let water = family.iter().find(|row| row.name == "Water").expect("Water row");
            assert!(water.is_group, "{preset}: Water is the physical family parent");
            assert!(water.parent_group_id.is_none());
            assert!(water.group_node_id.is_some());
            assert!(water.look_mesh.is_none());
            assert_eq!(water.visible_addr.param_id, "parent_visible");
            assert!(water.liquid_domain.is_some());
            assert!(!water.fluid_controls.is_empty());

            let names: Vec<_> = family.iter().map(|row| row.name.as_str()).collect();
            assert_eq!(names, ["Water", "Foam", "Spray", "Bubbles"], "{preset}: family order");
            for row in family.iter().filter(|row| row.name != "Water") {
                assert_eq!(row.parent_group_id, Some(water.object_node_id), "{preset}: {} parent", row.name);
                assert!(row.look_mesh.is_some(), "{preset}: {} owns its platonic mesh", row.name);
                assert!(row.liquid_domain.is_none(), "{preset}: {} has no fluid domain", row.name);
                assert!(row.fluid_controls.is_empty(), "{preset}: {} has no fluid controls", row.name);
                assert!(row.fluid_domain.is_none());
                assert!(row.fluid_domain_transform.is_none());
                assert!(row.transform.is_none());
                assert!(row.transform_chain.is_empty());
                assert!(row.modifier_chain.is_empty());
                assert!(row.physics.is_none());
                assert!(row.skin.is_none());
            }
        }
    }
