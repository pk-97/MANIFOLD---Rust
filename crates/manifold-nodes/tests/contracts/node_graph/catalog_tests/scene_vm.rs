use manifold_nodes_scene::node_graph::scene_vm::{SceneVm, SceneObjectVm, MaterialVm};
use manifold_nodes_scene::node_graph::scene_items::SceneItemId;
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
        let vm = SceneVm::from_def(d.as_ref()).expect("Scene resolves");
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
        assert_eq!(water[0].control_nodes().first(), Some(&domain.node));
        // The domain shares its group-local doc id with the root camera.
        let camera = def.nodes.iter().find(|node| node.type_id == ORBIT_CAMERA_TYPE_ID).unwrap();
        assert!(!water[0].control_nodes().contains(&camera.node_id));
        let items = manifold_nodes_scene::node_graph::scene_items::scene_items(&def, &vm);
        let camera_nodes = &items.iter().find(|item| item.id == SceneItemId::Camera).expect("camera item").owned_nodes;
        assert_eq!(camera_nodes.first(), Some(&camera.node_id));
        assert!(!camera_nodes.contains(&domain.node));
    }
    #[test]
    fn water_family_row_ownership() {
        for preset in ["WaterDamBreakGpuFlip", "WaterDamBreakParticles"] {
            let preset_type = manifold_core::PresetTypeId::new(preset);
            let def = manifold_nodes::bundled_presets::bundled_preset_def(&preset_type)
                .expect("water family preset");
            let vm = SceneVm::from_def(def.as_ref()).expect("water family scene resolves");
            let family: Vec<_> = vm.objects.iter().filter_map(|object| match object {
                SceneObjectVm::Known(row)
                    if row.name == "Water" || row.parent.is_some() => Some(row),
                _ => None,
            }).collect();
            assert_eq!(family.iter().filter(|row| row.name == "Water").count(), 1, "{preset}");
            let water = family.iter().find(|row| row.name == "Water").expect("Water row");
            assert!(water.is_group, "{preset}: Water is the physical family parent");
            assert!(water.parent.is_none());
            assert!(water.group_node_id.is_some());
            assert!(water.look_node().is_none());
            assert_eq!(water.visible_addr.param_id, "parent_visible");
            assert!(water.liquid_domain.is_some());
            assert!(!water.control_nodes().is_empty());

            let names: Vec<_> = family.iter().map(|row| row.name.as_str()).collect();
            // Physics View's dots are the Dam Break's fifth family output, a
            // child row after the looks; the Particle View has none.
            let physics_view = preset == "WaterDamBreakGpuFlip";
            let expected: &[&str] = if physics_view {
                &["Water", "Foam", "Spray", "Bubbles", "Physics Dots"]
            } else {
                &["Water", "Foam", "Spray", "Bubbles"]
            };
            assert_eq!(names, expected, "{preset}: family order");
            let dots: Vec<_> = vm.objects.iter().filter(|object| matches!(object,
                SceneObjectVm::Known(row) if row.name == "Physics Dots")).collect();
            assert_eq!(dots.len(), usize::from(physics_view), "{preset}: no top-level dots row");
            for switch in ["water_vertices_switch", "water_indices_switch", "dots_visible",
                "dots_instances_switch", "dots_mesh_switch"] {
                assert_eq!(water.control_nodes().iter().any(|id| id.as_str() == switch), physics_view,
                    "{preset}: the Water row owns {switch}");
            }
            for row in family.iter().filter(|row| row.name != "Water") {
                assert_eq!(row.parent, Some(water.address.clone()), "{preset}: {} parent", row.name);
                assert!(row.look_node().is_some(), "{preset}: {} owns its platonic mesh", row.name);
                assert!(row.liquid_domain.is_none(), "{preset}: {} has no fluid domain", row.name);
                assert_eq!(row.control_nodes(), row.look_node().into_iter().collect::<Vec<_>>(), "{preset}: {} owns only its look", row.name);
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

/// The document id `reference` names, walking its group scope.
fn doc_id(def: &EffectGraphDef, reference: &SceneNodeRef) -> Option<u32> {
    node_of(def, reference).map(|node| node.id)
}

fn node_of<'a>(def: &'a EffectGraphDef, reference: &SceneNodeRef) -> Option<&'a manifold_core::effect_graph_def::EffectGraphNode> {
    let mut nodes = &def.nodes;
    for group in &reference.scope {
        nodes = &nodes.iter().find(|node| &node.node_id == group)?.group.as_deref()?.nodes;
    }
    nodes.iter().find(|node| node.node_id == reference.node)
}

/// What the graph itself says each model row is, read without the model:
/// a water's domain is a liquid domain inside the water's own group, the only
/// one there; a look sits beside its water and owns one look mesh; an
/// imported part sits in the group that is its parent; a placed source is a
/// role node under a water.
fn assert_model_matches_graph(label: &str, def: &EffectGraphDef, models: &[manifold_core::scene_object_model::SceneObjectModel]) {
    use manifold_core::scene_object_model::SceneObjectRowKind as Kind;
    for model in models {
        let object = node_of(def, &model.object).unwrap_or_else(|| panic!("{label}: {:?} resolves", model.object));
        let parent = model.parent.as_ref().map(|parent| {
            models.iter().find(|row| &row.object == parent).unwrap_or_else(|| panic!("{label}: {:?}'s parent is a row", model.object))
        });
        match model.kind {
            Kind::PhysicsParent => {
                let domain = model.liquid_domain.as_ref().unwrap_or_else(|| panic!("{label}: {:?} is a water with a domain", model.object));
                assert!(manifold_core::liquid_domain::is_liquid_domain(&node_of(def, domain).expect("the domain resolves").type_id),
                    "{label}: {:?} simulates a liquid domain", model.object);
                let owner = node_of(def, &model.owner).expect("the owner resolves");
                let body = owner.group.as_deref().unwrap_or_else(|| panic!("{label}: a water's owner is its group"));
                assert_eq!(manifold_core::liquid_domain::liquid_domains_in(&body.nodes).len(), 1, "{label}: one liquid in the water's group");
                assert!(model.object.scope.starts_with(&[model.owner.scope.clone(), vec![model.owner.node.clone()]].concat()),
                    "{label}: the water's object is inside its group");
            }
            Kind::Group => {
                assert_eq!(object.type_id, "group", "{label}: a group row is its graph group");
                if let Some(transform) = &model.transform {
                    assert_eq!(node_of(def, transform).expect("the group transform resolves").type_id, "node.transform_3d");
                }
            }
            Kind::LookChild => {
                let water = parent.unwrap_or_else(|| panic!("{label}: a look has its water"));
                assert!(water.kind == Kind::PhysicsParent && water.liquid_domain.is_some(), "{label}: a look's parent is a water");
                assert_eq!(model.object.scope, water.object.scope, "{label}: a look sits beside its water");
                assert_eq!(model.owner, water.owner, "{label}: a look follows its water's group");
                let [mesh] = model.controls.as_slice() else { panic!("{label}: a look owns one look mesh: {:?}", model.controls) };
                assert_eq!(node_of(def, mesh).expect("the look mesh resolves").type_id, "node.platonic_solid_mesh");
            }
            Kind::Object if object.type_id == "node.fluid_role_source" => {
                let water = parent.unwrap_or_else(|| panic!("{label}: a placed source has its water"));
                assert!(water.liquid_domain.is_some(), "{label}: a placed source feeds a water");
            }
            Kind::Object => {
                assert_eq!(object.type_id, "node.scene_object", "{label}: an object row is a scene object");
                if let Some(group) = parent {
                    let inside = [group.object.scope.clone(), vec![group.object.node.clone()]].concat();
                    assert!(model.object.scope.starts_with(&inside), "{label}: an object sits inside its group");
                    assert!(model.controls.is_empty(), "{label}: a part leaves its controls to its model");
                }
            }
        }
    }
}

/// Every Known row reads its parent, kind and controls from the scene
/// object model (PHYSICS_ENGINE_BOUNDARY_DESIGN.md section 5.2 (One
/// scene-object model)).
fn assert_rows_share_owner(label: &str, def: &EffectGraphDef) -> usize {
    let Some(vm) = SceneVm::from_def(def) else { return 0 };
    let models = SceneVm::object_models(def).expect("a scene has a model");
    assert_model_matches_graph(label, def, &models);
    let mut checked = 0;
    for row in vm.objects.iter().filter_map(|object| match object {
        SceneObjectVm::Known(row) => Some(row),
        SceneObjectVm::Custom { .. } => None,
    }) {
        let model = models
            .iter()
            .find(|model| model.object.node == row.object && doc_id(def, &model.object) == Some(row.object_node_id))
            .unwrap_or_else(|| panic!("{label}: {} has a model row", row.name));
        assert_eq!(row.kind, model.kind, "{label}: {} kind", row.name);
        assert_eq!(row.capabilities, model.capabilities, "{label}: {} capabilities", row.name);
        assert_eq!(row.controls, model.controls, "{label}: {} controls", row.name);
        assert_eq!(row.parent, model.parent, "{label}: {} parent", row.name);
        checked += 1;
    }
    checked
}

#[test]
fn physics_boundary_rows_share_owner() {
    use manifold_core::preset_def::PresetKind;
    let mut rows = 0;
    let mut families = 0;
    for preset in manifold_nodes::bundled_presets::bundled_preset_type_ids(PresetKind::Generator) {
        let def = manifold_nodes::bundled_presets::bundled_preset_def(&preset).expect("bundled def");
        rows += assert_rows_share_owner(preset.as_str(), &def);
        families += usize::from(SceneVm::from_def(&def).is_some_and(|vm| vm.objects.iter().any(|object| matches!(object,
            SceneObjectVm::Known(row) if row.kind == manifold_core::scene_object_model::SceneObjectRowKind::LookChild))));
    }
    assert!(families >= 2, "the water family presets are covered");
    // The shipped waters: one water, its looks in output order.
    for (preset, looks) in [("WaterDamBreakGpuFlip", 4), ("WaterDamBreakParticles", 3)] {
        let def = manifold_nodes::bundled_presets::bundled_preset_def(&manifold_core::PresetTypeId::new(preset)).expect("bundled def");
        let models = SceneVm::object_models(&def).expect("a scene");
        let waters: Vec<_> = models.iter().filter(|model| model.liquid_domain.is_some() && model.kind == manifold_core::scene_object_model::SceneObjectRowKind::PhysicsParent).collect();
        let [water] = waters.as_slice() else { panic!("{preset}: one water, got {}", waters.len()) };
        // Its placed sources, the Initial Fill among them, are no looks.
        let children = models.iter().filter(|model| {
            model.parent.as_ref() == Some(&water.object) && model.kind == manifold_core::scene_object_model::SceneObjectRowKind::LookChild
        }).count();
        assert_eq!(children, looks, "{preset}: the water's looks");
    }
    // An imported model: its group is the parent of its parts.
    let (import, _) = manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph(
        &manifold_nodes_scene::node_graph::gltf_import::testkit::azalea_fixture_path(),
    )
    .expect("the fixture imports");
    let imported = assert_rows_share_owner("azalea", &import);
    let vm = SceneVm::from_def(&import).expect("the import is a scene");
    assert!(vm.objects.iter().all(|object| matches!(object, SceneObjectVm::Known(_))), "every imported part is a row: {:?}", vm.objects.len());
    assert!(imported >= 3, "a parent and both its parts");
    assert!(rows > 20, "the bundled scenes have rows");
}

/// The stable id of the node `addr` names, walking its group scope by doc id.
fn node_at(def: &EffectGraphDef, scope: &[u32], doc: u32) -> Option<manifold_core::NodeId> {
    let mut nodes = &def.nodes;
    for group in scope {
        nodes = &nodes.iter().find(|node| node.id == *group)?.group.as_deref()?.nodes;
    }
    nodes.iter().find(|node| node.id == doc).map(|node| node.node_id.clone())
}

/// A gizmo handle and the card row it scrubs name the same param: every
/// node param a declared shape writes is a control of one scene item, and a
/// placed source's handles are its own item's.
#[test]
fn physics_boundary_shape_params_belong_to_their_item() {
    use manifold_core::preset_def::PresetKind;
    use manifold_nodes_scene::node_graph::scene_vm::{ShapeOwner, ShapeWrite};
    let mut checked = 0;
    for preset in manifold_nodes::bundled_presets::bundled_preset_type_ids(PresetKind::Generator) {
        let def = manifold_nodes::bundled_presets::bundled_preset_def(&preset).expect("bundled def");
        let Some(vm) = SceneVm::from_def(&def) else { continue };
        let items = manifold_nodes_scene::node_graph::scene_items::scene_items(&def, &vm);
        for shape in vm.shapes.iter().filter(|shape| shape.row.is_some()) {
            for param in &shape.params {
                let ShapeWrite::Node(addr) = &param.write else { continue };
                let node = node_at(&def, &addr.scope_path, addr.node_doc_id).expect("the written node resolves");
                let owner = items.iter().find(|item| item.owned_nodes.contains(&node)).map(|item| &item.id);
                assert!(owner.is_some(), "{preset}: {} of {:?} has no card row", param.id, shape.owner);
                if let ShapeOwner::RoleSource { node: role, .. } = &shape.owner
                    && items.iter().any(|item| matches!(&item.id, SceneItemId::Source { node, .. } if node == role))
                {
                    assert!(
                        matches!(owner, Some(SceneItemId::Source { node, .. }) if node == role),
                        "{preset}: a placed source's {} is its own row's, not {owner:?}",
                        param.id
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 20, "the bundled scenes declare shapes: {checked}");
}
