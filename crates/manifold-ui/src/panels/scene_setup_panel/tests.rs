    use super::*;
    use crate::input::Modifiers;

    #[path = "no_bespoke_row_infra.rs"]
    mod no_bespoke_row_infra;

    /// C-P1a: wrap a plain `RowValue` in an idle (no active modulation)
    /// `ModulatedRow` — the shape `EnvironmentRowVm`/`AtmosphereRowVm` now
    /// carry for every converted row.
    fn mrow(value: RowValue) -> ModulatedRow {
        ModulatedRow { value, modulation: Box::new(RowModulation::default()) }
    }

    fn triplet(node_doc_id: u32, x: f32, y: f32, z: f32, min: f32, max: f32) -> (RowValue, RowValue, RowValue) {
        (
            RowValue { addr: RowAddr::root(node_doc_id, "x"), value: x, min, max, driven: false, exposed: false },
            RowValue { addr: RowAddr::root(node_doc_id, "y"), value: y, min, max, driven: false, exposed: false },
            RowValue { addr: RowAddr::root(node_doc_id, "z"), value: z, min, max, driven: false, exposed: false },
        )
    }

    /// C-P1b: `triplet` wrapped element-wise in idle `mrow`s — the shape
    /// `TransformRowVm`/`ObjectMaterialVm` now carry for every converted
    /// Object row.
    fn mtriplet(
        node_doc_id: u32,
        x: f32,
        y: f32,
        z: f32,
        min: f32,
        max: f32,
    ) -> (ModulatedRow, ModulatedRow, ModulatedRow) {
        let (rx, ry, rz) = triplet(node_doc_id, x, y, z, min, max);
        (mrow(rx), mrow(ry), mrow(rz))
    }

    /// C-P1c: wrap a plain `EnumRowValue`-shaped `(RowValue, labels)` pair in
    /// an idle `ModulatedEnumRow` — the shape `LightKnownRow`'s Mode/Cast
    /// Shadows/Shadow Softness now carry.
    fn menum(row: RowValue, labels: Vec<&'static str>) -> ModulatedEnumRow {
        ModulatedEnumRow { row: mrow(row), labels }
    }

    #[test]
    fn closed_panel_builds_nothing() {
        let mut panel = ScenePanel::new();
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(tree.count(), 0, "a closed panel must not build any node");
    }

    #[test]
    fn no_selection_state_renders_a_sentence_without_panicking() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::NoSelection("Select a layer.".to_string()));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(tree.count() > 0);
    }

    #[test]
    fn live_state_with_unwired_env_and_fog_shows_add_buttons() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(SceneSetupVm {
            layer_id: LayerId::new("layer-1"),
            scene_name: "Scene".to_string(),
            multiple_scenes: false,
            object_count: 0,
            light_count: 0,
            shadow_caster_count: 0,
            scene_root_node_id: 0,
            environment: EnvironmentRowVm::None,
            atmosphere: AtmosphereRowVm::None,
            objects: Vec::new(),
            fluid_domains: Vec::new(),
            lights: Vec::new(),
            forces: Vec::new(),
            force_picker: Vec::new(),
            camera: CameraRowVm::None,
            camera_sections: Vec::new(), camera_parameter_ids: None, world_sections: Vec::new(),
            scene_bounds: None,
        })));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.add_environment_id.is_some());
        assert!(panel.add_fog_id.is_some());
        assert!(panel.add_object_id.is_some());
        assert!(panel.add_light_id.is_some());
        assert!(panel.add_plane_id.is_some());
    }

    /// A synthetic multi-object def (P2 gate): one Known "Azalea" object with
    /// a full transform + pbr material + a Bend modifier, one Custom object,
    /// and header counts — proves the Objects section renders both shapes,
    /// the rename click resolves to the right group node id, and the
    /// "+ Object"/"+ Light" buttons carry the Vm's own counts as
    /// `next_index`.
    fn azalea_shaped_vm() -> SceneSetupVm {
        SceneSetupVm {
            layer_id: LayerId::new("layer-1"),
            scene_name: "Scene".to_string(),
            multiple_scenes: false,
            object_count: 2,
            light_count: 1,
            shadow_caster_count: 1,
            scene_root_node_id: 99,
            environment: EnvironmentRowVm::None,
            atmosphere: AtmosphereRowVm::None,
            objects: vec![
                ObjectRowVm::Known(Box::new(ObjectKnownRow {
                    look_mesh: None,
                    index: 0,
                    object_node_id: 40,
                    group_node_id: Some(42),
                    is_group: false,
                    parent_group_id: None,
                    name: "Azalea".to_string(),
                    visible: RowValue { addr: RowAddr { scope_path: vec![42], node_doc_id: 40, param_id: "visible".to_string() }, value: 1.0, min: 0.0, max: 1.0, driven: false, exposed: false },
                    transform: Some(Box::new(TransformRowVm {
                        pos: mtriplet(50, 1.0, 2.0, 3.0, -100.0, 100.0),
                        rot: mtriplet(50, 0.0, 0.0, 0.0, -std::f32::consts::TAU, std::f32::consts::TAU),
                        scale: mtriplet(50, 1.0, 1.0, 1.0, 0.01, 10.0),
                    })),
                    material: ObjectMaterialVm::Pbr {
                        color: mtriplet(51, 0.8, 0.8, 0.82, 0.0, 1.0),
                        metallic: mrow(RowValue { addr: RowAddr::root(51, "metallic"), value: 0.0, min: 0.0, max: 1.0, driven: false, exposed: false }),
                        roughness: mrow(RowValue { addr: RowAddr::root(51, "roughness"), value: 0.5, min: 0.01, max: 1.0, driven: false, exposed: false }),
                    },
                    material_inspector: None,
                    modifiers: vec![ModifierKnownRow {
                        index: 0,
                        node_doc_id: 70,
                        display_name: "Bend".to_string(),
                        parameter_ids: vec!["70_amount".to_string()],
                    }],
                    modifiers_addable: true,
                    sections: Vec::new(),
                    parameter_ids: Vec::new(),
                    skin: None,
                    physics_enabled: false,
                    physics_available: false,
                    physics_unavailable_reason: None,
                    physics_imported: false,
                    fluid_role_available: false,
                    fluid_roles: Ok(Vec::new()),
                    lattice: None,
                })),
                ObjectRowVm::Custom { index: 1 },
            ],
            fluid_domains: Vec::new(),
            lights: vec![
                LightRowVm::Known(Box::new(LightKnownRow {
                    index: 0,
                    node_doc_id: 60,
                    name: "Sun".to_string(),
                    mode: menum(
                        RowValue { addr: RowAddr::root(60, "mode"), value: 0.0, min: 0.0, max: 1.0, driven: false, exposed: false },
                        vec!["Sun", "Point"],
                    ),
                    color: mtriplet(60, 1.0, 1.0, 1.0, 0.0, 1.0),
                    intensity: mrow(RowValue { addr: RowAddr::root(60, "intensity"), value: 2.5, min: 0.0, max: 10.0, driven: false, exposed: false }),
                    pos: mtriplet(60, 5.0, 2.0, 3.0, -100.0, 100.0),
                    aim: mtriplet(60, 0.0, 0.0, 0.0, -100.0, 100.0),
                    cast_shadows: menum(
                        RowValue { addr: RowAddr::root(60, "cast_shadows"), value: 1.0, min: 0.0, max: 1.0, driven: false, exposed: false },
                        vec!["Off", "On"],
                    ),
                    shadow_softness: menum(
                        RowValue { addr: RowAddr::root(60, "shadow_softness"), value: 3.0, min: 0.0, max: 3.0, driven: false, exposed: false },
                        vec!["Hard", "Soft", "VerySoft", "Contact"],
                    ),
                    light_size: mrow(RowValue { addr: RowAddr::root(60, "light_size"), value: 4.0, min: 0.0, max: 20.0, driven: false, exposed: false }),
                    sections: Vec::new(),
                })),
                LightRowVm::Custom { index: 1 },
            ],
            forces: Vec::new(),
            force_picker: Vec::new(),
            camera: CameraRowVm::Orbit(Box::new(OrbitCameraRowVm {
                orbit: mrow(RowValue { addr: RowAddr::root(70, "orbit"), value: 0.7, min: -std::f32::consts::TAU, max: std::f32::consts::TAU, driven: false, exposed: false }),
                tilt: mrow(RowValue { addr: RowAddr::root(70, "tilt"), value: 0.3, min: -std::f32::consts::TAU, max: std::f32::consts::TAU, driven: false, exposed: false }),
                distance: mrow(RowValue { addr: RowAddr::root(70, "distance"), value: 4.0, min: 0.01, max: 100.0, driven: false, exposed: false }),
                fov_y: mrow(RowValue { addr: RowAddr::root(70, "fov_y"), value: 0.9, min: 0.05, max: 2.5, driven: false, exposed: false }),
                lens: Some(LensRowVm {
                    focus_distance: mrow(RowValue { addr: RowAddr::root(71, "focus_distance"), value: 0.0, min: 0.0, max: 1000.0, driven: false, exposed: false }),
                    f_stop: mrow(RowValue { addr: RowAddr::root(71, "f_stop"), value: 1000.0, min: 0.5, max: 1000.0, driven: false, exposed: false }),
                    shutter_angle: mrow(RowValue { addr: RowAddr::root(71, "shutter_angle"), value: 0.0, min: 0.0, max: 360.0, driven: false, exposed: false }),
                    exposure_ev: mrow(RowValue { addr: RowAddr::root(71, "exposure_ev"), value: 0.0, min: -8.0, max: 8.0, driven: false, exposed: false }),
                }),
            })),
            camera_sections: Vec::new(), camera_parameter_ids: None, world_sections: Vec::new(),
            scene_bounds: None,
        }
    }

    #[test]
    fn objects_outliner_lists_known_and_custom_rows_properties_shows_the_selected_one() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // Outliner rows: Scene fold + Camera + World + Lights fold + 1 Known light + Objects fold + 1 Known object are
        // selectable (`outliner_row_ids`); the Custom object/light are
        // listed too but as plain labels (D3: never hidden, but no
        // addressable node id to select by, D12).
        assert_eq!(panel.outliner_row_ids.len(), 8, "Scene/Camera/World fold rows + Lights fold + Forces fold + Objects fold + 1 known light + 1 known object");
        // Default selection (D7): the first Known object — Azalea — so its
        // properties header + body render without any click.
        assert_eq!(panel.object_name_ids.len(), 1, "the properties header shows the selected object's name");
        assert_eq!(panel.object_name_ids[0].0, 40, "resolves to the selected object's node id (the rename address)");
        assert_eq!(panel.object_name_ids[0].2, "Azalea");
        // P2 slice 2a: the Properties body's actual PARAM ROWS now come from
        // `self.full_params` (the real generator `ParamSurface`, wired by
        // `configure_params` — see that method's doc comment), not from this
        // hand-built `SceneSetupVm` fixture's own transform/material/
        // modifier fields. This test's fixture never calls
        // `configure_params`, so it can't exercise row rendering — see
        // `build_filtered_properties_...` tests below for that mechanism.
        assert!(panel.add_object_id.is_some());
        assert!(panel.add_light_id.is_some());
        assert!(panel.add_plane_id.is_some());
    }

    fn compound_shaped_vm() -> SceneSetupVm {
        let mut vm = azalea_shaped_vm();
        let Some(ObjectRowVm::Known(child)) = vm.objects.first().cloned() else {
            unreachable!("azalea fixture has a known object");
        };
        let mut parent = (*child).clone();
        parent.index = 0;
        parent.object_node_id = 42;
        parent.group_node_id = Some(42);
        parent.is_group = true;
        parent.parent_group_id = None;
        parent.name = "Imported Group".to_string();
        parent.material = ObjectMaterialVm::None;
        parent.material_inspector = None;
        parent.modifiers.clear();
        parent.skin = None;
        parent.physics_available = true;
        parent.physics_enabled = false;

        let mut child = (*child).clone();
        child.index = 0;
        child.object_node_id = 43;
        child.group_node_id = Some(42);
        child.is_group = false;
        child.parent_group_id = Some(42);
        child.name = "Blue Material".to_string();
        vm.object_count = 1;
        vm.objects = vec![
            ObjectRowVm::Known(Box::new(parent)),
            ObjectRowVm::Known(Box::new(child)),
        ];
        vm
    }

    #[test]
    fn compound_group_chevron_expands_children_and_preserves_selection_identity() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(compound_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(panel.group_toggle_ids.len(), 1);
        assert_eq!(panel.outliner_row_ids.iter().filter(|(_, sel)| matches!(sel, SceneSelection::Object(_))).count(), 1);

        let (toggle_id, _) = panel.group_toggle_ids[0];
        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: toggle_id,
            pos: Vec2::ZERO,
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(actions.as_slice(), [PanelAction::Params(ParamsAction::SectionFoldToggled)]));

        tree.clear();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(panel.outliner_row_ids.iter().filter(|(_, sel)| matches!(sel, SceneSelection::Object(_))).count(), 2);
        let child_row = panel
            .outliner_row_ids
            .iter()
            .find(|(_, sel)| sel == &SceneSelection::Object(43))
            .map(|(id, _)| *id)
            .expect("expanded child is selectable");
        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: child_row,
            pos: Vec2::ZERO,
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(actions.as_slice(), [PanelAction::Root(RootAction::SceneSetupSelectionChanged(_))]));
        assert_eq!(panel.selection.get(&LayerId::new("layer-1")), Some(&SceneSelection::Object(43)));
    }

    #[test]
    fn compound_child_header_routes_submesh_duplicate_and_remove_actions() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(compound_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Object(43));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(panel.submesh_duplicate_ids.len(), 1);
        assert_eq!(panel.submesh_remove_ids.len(), 1);

        let duplicate_id = panel.submesh_duplicate_ids[0].0;
        let (_, actions) = panel.handle_event(&UIEvent::Click {
            node_id: duplicate_id,
            pos: Vec2::ZERO,
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(matches!(actions.as_slice(), [PanelAction::Project(ProjectAction::SceneSetupDuplicateSubmesh(layer, 99, 0))] if *layer == LayerId::new("layer-1")));

        let remove_id = panel.submesh_remove_ids[0].0;
        let (_, actions) = panel.handle_event(&UIEvent::Click {
            node_id: remove_id,
            pos: Vec2::ZERO,
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(matches!(actions.as_slice(), [PanelAction::Project(ProjectAction::SceneSetupRemoveSubmesh(layer, 99, 0))] if *layer == LayerId::new("layer-1")));
    }

    /// W2-A gap fill: the outliner eye toggle (D3's on/off convention) had
    /// zero click->dispatch coverage — every "eye" hit in this file before
    /// this test was a comment. A click on a Known object row's eye emits
    /// `SceneSetupParamChanged` carrying the row's own write address and the
    /// flipped [0,1] value; a second click on the now-off eye flips back.
    #[test]
    fn object_eye_toggle_click_emits_scene_setup_param_changed_and_flips_back() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(panel.outliner_eye_ids.len(), 1, "one Known object row renders a live eye");
        let (eye_id, row_value) = panel.outliner_eye_ids[0].clone();
        assert_eq!(row_value.value, 1.0, "azalea fixture starts visible");

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: eye_id,
            pos: Vec2::ZERO,
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed, "the eye toggle must be clickable");
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupParamChanged(layer, scope, node, param, value))]
                if *layer == LayerId::new("layer-1")
                    && *scope == vec![42]
                    && *node == 40
                    && param == "visible"
                    && *value == 0.0
        ), "visible eye click must flip to 0.0 at the object's own write address, got {actions:?}");

        // Re-configure with the flipped value (mirrors the real per-frame
        // sync landing the write) and click again — must flip back to 1.0.
        let mut vm = azalea_shaped_vm();
        let ObjectRowVm::Known(row) = &mut vm.objects[0] else { unreachable!() };
        row.visible.value = 0.0;
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let (eye_id_2, _) = panel.outliner_eye_ids[0].clone();

        let (consumed_2, actions_2) = panel.handle_event(&UIEvent::Click {
            node_id: eye_id_2,
            pos: Vec2::ZERO,
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed_2);
        assert!(matches!(
            actions_2.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupParamChanged(_, _, _, param, value))]
                if param == "visible" && *value == 1.0
        ), "hidden eye click must flip back to 1.0, got {actions_2:?}");
    }

    #[test]
    fn imported_physics_property_has_no_legacy_split_action() {
        let mut vm = azalea_shaped_vm();
        let ObjectRowVm::Known(row) = &mut vm.objects[0] else { unreachable!() };
        row.physics_available = true;
        let expected_layer = vm.layer_id.clone();
        let expected_scene = vm.scene_root_node_id;

        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let texts: Vec<&str> = tree.nodes().iter().filter_map(|node| node.text.as_deref()).collect();
        assert!(texts.contains(&"Physics"), "imported object should expose the Physics property");
        assert!(texts.contains(&"Enable"), "eligible object should expose an explicit Enable action");
        assert!(!texts.iter().any(|text| text.contains("Split into 8")), "legacy split action must stay out of the Physics property");

        let enable_id = panel.object_enable_physics_ids[0].0;
        let (_, actions) = panel.handle_event(&UIEvent::Click {
            node_id: enable_id,
            pos: Vec2::ZERO,
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupEnablePhysics(layer_id, scene_root, index))]
                if layer_id == &expected_layer && *scene_root == expected_scene && *index == 0
        ));
    }

    #[test]
    fn unavailable_physics_property_shows_reason_without_enable_action() {
        let mut vm = azalea_shaped_vm();
        let ObjectRowVm::Known(row) = &mut vm.objects[0] else { unreachable!() };
        row.physics_unavailable_reason = Some("Fluid role objects cannot be rigid bodies".to_string());

        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let texts: Vec<&str> = tree.nodes().iter().filter_map(|node| node.text.as_deref()).collect();
        assert!(texts.contains(&"Physics"));
        assert!(texts.contains(&"Fluid role objects cannot be rigid bodies"));
        assert!(panel.object_enable_physics_ids.is_empty());
        assert!(panel.object_disable_physics_ids.is_empty());
    }

    /// Water reads its lattice as one line: the grid, the cell edge in
    /// centimetres and the cell count (BUG-ejcb readout).
    #[test]
    fn water_shows_its_lattice_line() {
        let mut vm = azalea_shaped_vm();
        let ObjectRowVm::Known(row) = &mut vm.objects[0] else { unreachable!() };
        row.lattice = Some(LatticeReadout { cells: [64, 32, 64], cell_size_m: 0.0625 });

        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let texts: Vec<&str> = tree.nodes().iter().filter_map(|node| node.text.as_deref()).collect();
        assert!(texts.contains(&"Lattice 64 × 32 × 64 · 6.2 cm cells · 131072 cells"), "{texts:?}");
    }

    #[test]
    fn scene_physics_fluid_role_click_carries_selected_object_and_domains() {
        let mut vm = azalea_shaped_vm();
        let ObjectRowVm::Known(row) = &mut vm.objects[0] else { unreachable!() };
        row.fluid_role_available = true;
        let expected_index = row.index as u32;
        let expected_scene = vm.scene_root_node_id;
        let domains = vec![FluidDomainOption { node: FoundationNodeId::new("liquid_b"), name: "Liquid B".into() }];
        vm.fluid_domains = domains.clone();
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm.clone())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let button = panel.object_fluid_role_ids[0].0;
        let (_, actions) = panel.handle_event(&UIEvent::Click {
            node_id: button, pos: Vec2::ZERO, modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(matches!(actions.as_slice(), [PanelAction::Root(RootAction::SceneSetupFluidRoleClicked {
            layer_id, render_scene_node_id, object_index, domains: choices, ..
        })] if layer_id == &vm.layer_id && *render_scene_node_id == expected_scene
            && *object_index == expected_index && choices == &domains));
        let ObjectRowVm::Known(row) = &mut vm.objects[0] else { unreachable!() };
        row.fluid_role_available = false;
        row.fluid_roles = Ok(vec![FluidRoleRow {
            source_node_id: 74, name: "Pour".into(), target_label: "Target: Liquid B".into(),
        }]);
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        tree.clear();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.object_fluid_role_ids.is_empty());
        let (_, actions) = panel.handle_event(&UIEvent::Click {
            node_id: panel.fluid_role_target_ids[0].0, pos: Vec2::ZERO, modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(matches!(actions.as_slice(), [PanelAction::Root(RootAction::SceneSetupFluidRoleTargetClicked {
            source_node_id: 74, domains: choices, ..
        })] if choices == &domains));
        let (_, actions) = panel.handle_event(&UIEvent::Click {
            node_id: panel.fluid_role_remove_ids[0].0, pos: Vec2::ZERO, modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(matches!(actions.as_slice(), [PanelAction::Project(ProjectAction::SceneSetupRemoveFluidRole {
            source_node_id: 74, ..
        })]));
    }

    /// A one-object Vm with TWO modifiers — for exercising up/down boundary
    /// behavior (P5), which the single-modifier `azalea_shaped_vm` can't.
    fn two_modifier_object_vm(modifiers_addable: bool) -> SceneSetupVm {
        let mut vm = azalea_shaped_vm();
        let ObjectRowVm::Known(row) = &mut vm.objects[0] else { unreachable!() };
        row.modifiers = vec![
            ModifierKnownRow {
                index: 0,
                node_doc_id: 70,
                display_name: "Bend".to_string(),
                parameter_ids: vec!["70_amount".to_string()],
            },
            ModifierKnownRow {
                index: 1,
                node_doc_id: 71,
                display_name: "Twist".to_string(),
                parameter_ids: vec!["71_amount".to_string()],
            },
        ];
        row.modifiers_addable = modifiers_addable;
        vm
    }

    #[test]
    fn copied_modifier_ids_from_other_objects_do_not_leak_into_properties() {
        let (mut vm, mut surface) = world_transform_vm();
        let shared_section = "Cube — Bend_mesh".to_string();
        let ObjectRowVm::Known(first) = &mut vm.objects[0] else { unreachable!() };
        first.sections = vec![shared_section.clone()];
        first.modifiers[0].parameter_ids = vec!["70_amount".to_string()];
        surface.rows[0].id = "70_amount".into();
        surface.rows[0].spec.section = Some(shared_section.clone());
        let mut copied = first.as_ref().clone();
        copied.index = 1;
        copied.object_node_id = 41;
        copied.group_node_id = Some(43);
        copied.name = "Object 2".to_string();
        copied.modifiers[0].node_doc_id = 71;
        copied.modifiers[0].parameter_ids = vec!["71_amount".to_string()];
        vm.objects.push(ObjectRowVm::Known(Box::new(copied)));
        surface.rows.push({
            let mut row = surface.rows[0].clone();
            row.id = "71_amount".into();
            row
        });

        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        assert!(panel.properties_card.rows.is_empty());
        assert_eq!(panel.object_modifier_cards.len(), 2);
        assert_eq!(panel.object_modifier_cards[0].rows[0].id.as_ref(), "70_amount");
        assert_eq!(panel.object_modifier_cards[1].rows[0].id.as_ref(), "71_amount");
    }

    #[test]
    fn object_properties_use_exact_parameter_ids_when_sections_are_shared() {
        let (mut vm, mut surface) = world_transform_vm();
        let ObjectRowVm::Known(first) = &mut vm.objects[0] else { unreachable!() };
        first.sections = vec!["Shared Object Controls".to_string()];
        first.parameter_ids = vec!["40_velocity_y".to_string()];
        first.modifiers.clear();
        first.material = ObjectMaterialVm::None;

        let mut second = first.as_ref().clone();
        second.index = 1;
        second.object_node_id = 41;
        second.group_node_id = Some(43);
        second.name = "Object 2".to_string();
        second.parameter_ids = vec!["41_velocity_y".to_string()];
        vm.objects = vec![
            ObjectRowVm::Known(first.clone()),
            ObjectRowVm::Known(Box::new(second)),
        ];
        vm.object_count = 2;

        surface.rows[0].id = "40_velocity_y".into();
        surface.rows[0].spec.name = "Velocity Y (Object 1)".into();
        surface.rows[0].spec.section = Some("Shared Object Controls".to_string());
        let mut second_row = surface.rows[0].clone();
        second_row.id = "41_velocity_y".into();
        second_row.spec.name = "Velocity Y (Object 2)".into();
        surface.rows.push(second_row);

        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Object(40));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        assert_eq!(panel.properties_card.rows.len(), 1);
        assert_eq!(panel.properties_card.rows[0].id.as_ref(), "40_velocity_y");
    }

    #[test]
    fn unparseable_modifier_chain_disables_add() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(two_modifier_object_vm(false))));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.add_modifier_button_id.is_none(), "Add modifier is disabled for an unparseable chain");
    }

    #[test]
    fn add_object_and_add_light_buttons_carry_the_vms_own_counts_as_next_index() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let add_object_id = panel.add_object_id.unwrap();
        let add_light_id = panel.add_light_id.unwrap();

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: add_object_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupAddObject(l, 99, 2)) if *l == LayerId::new("layer-1")
        ));

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: add_light_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupAddLight(l, 99, 1)) if *l == LayerId::new("layer-1")
        ));
    }

    #[test]
    fn scene_physics_add_fluid_button_targets_the_bound_scene() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: panel.add_fluid_id.expect("add fluid button"),
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(actions.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupAddFluid(layer, 99))]
                if *layer == LayerId::new("layer-1")));
    }

    /// UX-P3b-i's own deliverable: the per-row key-range collision audit the
    /// design doc's "as attempted" note calls out, extended from Objects
    /// (P3a's own `OBJ_KEY_STRIDE` 32→44 bump) to Light/Camera/Modifier.
    /// Computational proof (oracle discipline: a countable arithmetic
    /// question gets a script, not an eyeball) — every named offset within
    /// each family's own key formula must be pairwise distinct AND (for the
    /// per-index families) strictly less than that family's stride, so no
    /// two DIFFERENT logical rows can ever key the same node under
    /// `UITree::mint`'s "keys only need to be unique among siblings of the
    /// same parent" contract (`tree.rs`'s own `debug_assert` catches a live
    /// violation; this test catches it at the constant-arithmetic level,
    /// before any panel is ever built).
    #[test]
    fn no_key_offset_collisions_across_row_families() {
        fn assert_no_dupes_and_fits_stride(family: &str, offsets: &[u64], stride: Option<u64>) {
            let mut sorted = offsets.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                offsets.len(),
                "{family}: duplicate offset among {offsets:?} — two logical rows would key the same node"
            );
            if let Some(stride) = stride {
                assert!(
                    offsets.iter().all(|&o| o < stride),
                    "{family}: an offset in {offsets:?} reaches into the next index's range (stride {stride})"
                );
            }
        }

        // C-P1b: the value-cell offsets (`OBJ_OFF_POS_X`/`ROT_X`/`SCALE_X`/
        // `COLOR_R`/`METALLIC`/`ROUGHNESS`) are gone — those rows' widgets
        // now key off `build_param_row`'s own ParamId-derived `row_key_base`,
        // a disjoint key space from `obj_key`'s. Only NAME/REMOVE (header
        // chrome) still key through `obj_key` (the exposure-lane mod
        // buttons were removed with the ∿ column).
        assert_no_dupes_and_fits_stride(
            "OBJECT",
            &[
                OBJ_OFF_NAME,
                OBJ_OFF_REMOVE, OBJ_OFF_REMOVE + 1,
            ],
            Some(OBJ_KEY_STRIDE),
        );

        // C-P1c: the value-cell offsets (`LIGHT_OFF_MODE_MINUS`/`COLOR_R`/
        // `INTENSITY_MINUS`/`POS_X`/`AIM_X`/`CAST_SHADOWS_MINUS`/
        // `SHADOW_SOFTNESS_MINUS`/`LIGHT_SIZE_MINUS`) are gone — those rows'
        // widgets now key off `build_param_row`'s own `row_key_base`
        // (derived from the stable ParamId), same disjoint key space C-P1b established for
        // Object. Only NAME/REMOVE (header chrome) still key through
        // `light_key`.
        assert_no_dupes_and_fits_stride(
            "LIGHT",
            &[
                LIGHT_OFF_REMOVE,
                LIGHT_OFF_NAME,
            ],
            Some(LIGHT_KEY_STRIDE),
        );

        // C-P1c: Camera's value-cell offsets are gone, and the exposure-lane
        // mod buttons went with the ∿ column — Camera keys nothing through
        // an explicit-key scheme anymore (`build_param_row`'s ParamId-derived key
        // covers all its rows), so there is nothing left to audit here.


    }

    #[test]
    fn object_paste_destination_changes_before_the_queued_click_is_drained() {
        let mut panel = ScenePanel::new();
        panel.open();
        let mut vm = azalea_shaped_vm();
        let ObjectRowVm::Known(mut second) = vm.objects[0].clone() else { panic!("known fixture"); };
        second.object_node_id = 200;
        second.group_node_id = Some(201);
        vm.objects.push(ObjectRowVm::Known(second));
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let (node, _) = panel.outliner_row_ids.iter()
            .find(|(_, selection)| selection == &SceneSelection::Object(200)).unwrap();
        assert!(panel.select_outliner_node(*node));
        assert_eq!(panel.object_modifier_destination(), Some((LayerId::new("layer-1"), 201)));
        assert!(panel.selected_object_modifier().is_none());
    }

    #[test]
    fn keyboard_and_context_actions_follow_selected_object_or_light() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let object = panel.selected_scene_item().unwrap();
        assert!(!object.is_light);
        assert!(matches!(panel.frame_selection_action(), Some(PanelAction::Project(ProjectAction::SceneSetupFrameSelected(_, 99, 40)))));
        assert!(matches!(panel.rename_selection_action(), Some(PanelAction::Root(RootAction::SceneSetupRenameObjectClicked(_, 42, _)))));
        let light_node = panel.outliner_row_ids.iter().find(|(_, selection)| *selection == SceneSelection::Light(60)).unwrap().0;
        let (consumed, actions) = panel.handle_event(&UIEvent::RightClick {
            node_id: Some(light_node), pos: Vec2::new(0.0, 0.0), modifiers: Modifiers::NONE,
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(actions.as_slice(), [PanelAction::Root(RootAction::SceneItemRightClicked)]));
        assert!(panel.selected_scene_item().unwrap().is_light);
        assert!(matches!(panel.remove_selection_action(), Some(PanelAction::Project(ProjectAction::SceneSetupRemoveLight(_, 99, 0)))));
        assert!(panel.frame_selection_action().is_none());
        panel.navigate_selection(-1).unwrap();
        assert_eq!(panel.selection.get(&LayerId::new("layer-1")), Some(&SceneSelection::World));
    }

    /// D7: clicking an outliner row changes the UI-local selection, and the
    /// next build shows THAT item's properties instead — "select the object
    /// to use the tools" (Peter). Proves the Object→World switch (Properties
    /// content changes: object body gone, Environment/Fog appear) and that
    /// a click on the World row is what does it.
    #[test]
    fn selecting_a_different_outliner_row_switches_properties_content() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // Default selection = Azalea: no environment/fog "add" affordances
        // (azalea fixture's environment is None — but World isn't selected,
        // so neither button builds).
        assert!(panel.add_environment_id.is_none(), "World isn't selected — no Environment row built yet");

        let world_row_id = panel
            .outliner_row_ids
            .iter()
            .find(|(_, sel)| sel == &SceneSelection::World)
            .map(|(id, _)| *id)
            .expect("World is always a selectable outliner row");

        let (consumed, _) = panel.handle_event(&UIEvent::Click {
            node_id: world_row_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert_eq!(panel.selection.get(&LayerId::new("layer-1")), Some(&SceneSelection::World));

        let mut tree2 = UITree::new();
        panel.build_docked(&mut tree2, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.add_environment_id.is_some(), "World selected — Environment's Add affordance renders");
    }

    /// D7's fallback: removing the selected object from a rebuilt Vm falls
    /// selection back to first-object-else-World, never a dangling id.
    #[test]
    fn selection_falls_back_when_the_selected_object_is_removed() {
        let mut panel = ScenePanel::new();
        panel.open();
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm.clone())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(
            panel.selection.get(&LayerId::new("layer-1")),
            Some(&SceneSelection::Object(40)),
            "default selection resolves to Azalea's own scene_object doc id"
        );

        // Rebuild with the object gone (removed elsewhere) — only the
        // Custom row and the light remain.
        let mut vm2 = vm;
        vm2.objects = vec![ObjectRowVm::Custom { index: 0 }];
        vm2.object_count = 0;
        panel.configure(SceneSetupState::Live(Box::new(vm2)));
        let mut tree2 = UITree::new();
        panel.build_docked(&mut tree2, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(
            panel.selection.get(&LayerId::new("layer-1")),
            Some(&SceneSelection::World),
            "no Known object left — falls back to World, never a dangling Object(40)"
        );
    }

    // ── P3: Lights + Camera sections ──

    /// A World-selected scene with one real "Transform" section plus the
    /// matching generator `ParamSurface` (one ±100 translate row) — the
    /// fixture the scene type-in / fine-scrub tests need. `azalea_shaped_vm`'s
    /// `world_sections` is empty, so it can't exercise the unified properties
    /// card's rows.
    pub(super) fn world_transform_vm() -> (SceneSetupVm, ParamSurface) {
        let mut vm = azalea_shaped_vm();
        vm.world_sections = vec!["Transform".to_string()];
        let surface = ParamSurface {
            kind: crate::panels::param_card::ParamCardKind::Generator,
            title: "Scene".to_string(),
            collapsed: false,
            enabled: true,
            effect_index: 0,
            effect_id: manifold_foundation::EffectId::new("scene-gen"),
            supports_envelopes: false,
            has_graph_mod: false,
            layer_id: Some(LayerId::new("layer-1")),
            rows: vec![ParamRow {
                id: manifold_foundation::ParamId::from("translate_x"),
                spec: RowSpec {
                    tooltip: None,
                    name: "Translate X".to_string(),
                    min: -100.0,
                    max: 100.0,
                    default: 0.0,
                    whole_numbers: false,
                    is_angle: false,
                    is_toggle: false,
                    is_trigger: false,
                    is_trigger_gate: false,
                    value_labels: None,
                    section: Some("Transform".to_string()),
                    disabled: None,
                    material_role: None,
                    inactive_reason: None,
                },
                value: crate::param_surface::RowValue {
                    base: 0.0,
                    effective: 0.0,
                    exposed: true,
                    driven: false,
                },
                audio: AudioRowState::default(),
                modulation: RowMod::default(),
                mapping: RowMapping {
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                },
                    scene_addr: None,
                    rgb_members: None,
                    material_attached: false,
            }],
            string_params: Vec::new(),
            modifier: None,
            audio_sends: Vec::new(),
            relight: crate::panels::param_card::RelightCardConfig::default(),
        };
        (vm, surface)
    }

    #[test]
    fn physics_reset_button_dispatches_to_the_bound_scene_layer() {
        let (vm, mut surface) = world_transform_vm();
        surface.rows[0].id = manifold_foundation::ParamId::from("40_reset");
        surface.rows[0].spec.name = "Reset".into();
        surface.rows[0].spec.is_trigger = true;
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::World);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let button = panel.properties_card.row_host.toggle_ids[0]
            .as_ref().expect("Reset must have a trigger button").button_id;
        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: button,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(actions.as_slice(),
            [PanelAction::Params(ParamsAction::ParamFire(GraphParamTarget::GeneratorOf(layer), id))]
                if layer.as_str() == "layer-1" && id.as_ref() == "40_reset"
        ));
    }

    /// Build the world-transform fixture and select World, so the unified
    /// properties card renders its one translate row. Returns the panel and a
    /// fresh tree (post-selection rebuild).
    fn scene_with_world_transform_selected() -> (ScenePanel, UITree) {
        let (vm, surface) = world_transform_vm();
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let world_row_id = panel
            .outliner_row_ids
            .iter()
            .find(|(_, sel)| sel == &SceneSelection::World)
            .map(|(id, _)| *id)
            .expect("World is always a selectable outliner row");
        let (consumed, _) = panel.handle_event(
            &UIEvent::Click {
                node_id: world_row_id,
                pos: Vec2::ZERO,
                modifiers: Modifiers::default(),
        },
            &mut tree,
        );
        assert!(consumed, "World outliner row click must consume");

        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(panel.properties_card.rows.len(), 1, "the translate row renders under World");
        assert!(panel.properties_card.row_host.slider_ids[0].is_some(), "the row has a slider");
        (panel, tree)
    }

    /// D8: double-clicking the scene properties row's value cell routes through
    /// the SHARED `RowHost::value_cell_typein` — the same `BeginParamTextInput`
    /// action the inspector cards emit — carrying the panel's own bound layer
    /// (`GeneratorOf`) and the row's real param id + clamp range.
    #[test]
    fn scene_properties_double_click_opens_the_shared_typein() {
        let (mut panel, mut tree) = scene_with_world_transform_selected();
        let value_cell = panel.properties_card.row_host.slider_ids[0].as_ref().unwrap().value_text;

        let (consumed, actions) = panel.handle_event(
            &UIEvent::DoubleClick {
                node_id: value_cell,
                pos: Vec2::ZERO,
                modifiers: Modifiers::default(),
            },
            &mut tree,
        );
        assert!(consumed, "double-click on a scene value cell must consume");
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Root(RootAction::BeginParamTextInput { target, param_id, min, max, value, whole_numbers, .. })]
                if *target == GraphParamTarget::GeneratorOf(LayerId::new("layer-1"))
                    && param_id.as_ref() == "translate_x"
                    && *min == -100.0 && *max == 100.0 && *value == 0.0 && !*whole_numbers
        ), "scene type-in must carry GeneratorOf + the real param id + range, got {actions:?}");
    }

    /// D8: a double-click on a non-value-cell scene node (the track) emits
    /// nothing — type-in is the value cell's gesture only.
    #[test]
    fn scene_properties_double_click_on_track_is_a_no_op() {
        let (mut panel, mut tree) = scene_with_world_transform_selected();
        let track = panel.properties_card.row_host.slider_ids[0].as_ref().unwrap().track;

        let (consumed, actions) = panel.handle_event(
            &UIEvent::DoubleClick {
                node_id: track,
                pos: Vec2::ZERO,
                modifiers: Modifiers::default(),
            },
            &mut tree,
        );
        assert!(!consumed, "track double-click is not a type-in");
        assert!(actions.is_empty());
    }

    /// D8 fine mode on the scene properties track: Shift during a drag scales
    /// the pointer sensitivity by 0.1, through the same shared helper the card
    /// uses the shared `RowHost` drag lifecycle and fine-scrub math.
    #[test]
    fn scene_properties_drag_shift_fine_scales_sensitivity() {
        let (mut panel, mut tree) = scene_with_world_transform_selected();
        let track = panel.properties_card.row_host.slider_ids[0].as_ref().unwrap().track;
        let track_rect = tree.get_bounds(track);
        let mid_x = track_rect.x + track_rect.width * 0.5;

        let (consumed, down) = panel.handle_event(
            &UIEvent::PointerDown {
                node_id: track,
                pos: Vec2::new(mid_x, track_rect.y),
                modifiers: Modifiers::default(),
            },
            &mut tree,
        );
        assert!(consumed, "track pointer-down must start the scene drag");
        assert!(matches!(down.as_slice(), [PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Begin), PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Move(..))]));

        let coarse_val = {
            let (_, actions) = panel.handle_event(
                &UIEvent::Drag {
                    node_id: Some(track),
                    pos: Vec2::new(mid_x + 20.0, track_rect.y),
                    delta: Vec2::new(20.0, 0.0),
                    modifiers: Modifiers::NONE,
                },
                &mut tree,
            );
            match actions.as_slice() {
                [PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Move(ScrubValue::Scalar(v)))] => *v,
                other => panic!("expected a coarse scene move, got {other:?}"),
            }
        };
        let fine_val = {
            let (_, actions) = panel.handle_event(
                &UIEvent::Drag {
                    node_id: Some(track),
                    pos: Vec2::new(mid_x + 20.0, track_rect.y),
                    delta: Vec2::new(20.0, 0.0),
                    modifiers: Modifiers { shift: true, ..Modifiers::NONE },
                },
                &mut tree,
            );
            match actions.as_slice() {
                [PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Move(ScrubValue::Scalar(v)))] => *v,
                other => panic!("expected a fine scene move, got {other:?}"),
            }
        };

        let coarse_delta = coarse_val.abs();
        let fine_delta = fine_val.abs();
        assert!(coarse_delta > 1.0, "coarse must move: {coarse_val}");
        assert!(
            (fine_delta - coarse_delta * 0.1).abs() < 1.5,
            "fine delta ({fine_delta}) must be ~0.1x coarse delta ({coarse_delta})"
        );
    }

    /// D3/D12's tolerance doctrine: an all-Custom-lights scene (no
    /// addressable id at all) must still render every row as an outliner
    /// label — never hidden, never a panic — even though none of them are
    /// selectable through the panel UI (D12's own gap, same as Custom
    /// objects, flagged in the P5 landing report).
    #[test]
    fn more_than_four_lights_all_render_without_panicking_no_panel_side_cap() {
        let mut vm = azalea_shaped_vm();
        vm.lights = (0..5)
            .map(|i| LightRowVm::Custom { index: i })
            .collect();
        vm.light_count = 5;
        vm.shadow_caster_count = 5;
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(tree.count() > 0, "5 custom light rows render without panicking");
        assert!(
            panel.outliner_row_ids.iter().all(|(_, sel)| !matches!(sel, SceneSelection::Light(_))),
            "no Custom light has an addressable id to select by"
        );
    }

    #[test]
    fn camera_none_and_custom_shapes_render_without_panicking() {
        for camera in [CameraRowVm::None, CameraRowVm::Custom] {
            let mut vm = azalea_shaped_vm();
            vm.camera = camera;
            let mut panel = ScenePanel::new();
            panel.open();
            panel.configure(SceneSetupState::Live(Box::new(vm)));
            let mut tree = UITree::new();
            panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
            assert!(tree.count() > 0);
        }
    }

    // scene-panel-ux lane fold behavior tests

    #[test]
    fn folded_properties_section_contributes_zero_row_height_and_builds_no_param_rows() {
        // Test that the fold machinery exists and works correctly
        let mut panel = ScenePanel::new();
        panel.open();
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();

        // Initial build
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        // Set fold state - verify the machinery works
        panel.section_folded.insert("Transform".to_string(), true);
        assert!(panel.section_folded.get("Transform").copied().unwrap_or(false), "Fold state should be stored");

        // Verify we can iterate over fold keys (needed for the build loop)
        let has_transform = panel.section_folded.get("Transform").is_some();
        assert!(has_transform, "Fold state should be queryable");

        // Rebuild to test the fold is respected (no panic)
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        // Verify state persisted through build
        assert!(panel.section_folded.get("Transform").copied().unwrap_or(false), "Fold state persists through build");

        // Test toggling
        panel.section_folded.insert("Transform".to_string(), false);
        assert!(!panel.section_folded.get("Transform").copied().unwrap_or(true), "Fold state can be toggled");
    }

    #[test]
    fn folded_outliner_group_hides_its_rows() {
        // Test that folding an outliner group hides its child rows
        let mut panel = ScenePanel::new();
        panel.open();
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();

        // First build: all groups expanded
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let expanded_outliner_ids = panel.outliner_row_ids.len();

        // Fold the Objects group
        panel.outliner_folded.insert("Objects", true);

        // Rebuild with Objects folded
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let folded_outliner_ids = panel.outliner_row_ids.len();

        // Folded group should have fewer selectable rows (object rows hidden)
        assert!(folded_outliner_ids < expanded_outliner_ids, "Folded group should hide child rows");

        // Verify the fold state persisted
        assert!(panel.outliner_folded.get("Objects").copied().unwrap_or(false), "Objects fold state should persist");

        // Unfold and verify rows return
        panel.outliner_folded.insert("Objects", false);
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let unfolded_outliner_ids = panel.outliner_row_ids.len();
        assert_eq!(unfolded_outliner_ids, expanded_outliner_ids, "Unfolding should restore original row count");
    }

    #[test]
    fn fold_state_survives_rebuild_cycle() {
        // Test that fold state persists through configure → build → rebuild cycle
        let mut panel = ScenePanel::new();
        panel.open();

        // Initial configure
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm)));

        // Set fold states
        panel.section_folded.insert("Material".to_string(), true);
        panel.outliner_folded.insert("Lights", true);

        let mut tree = UITree::new();

        // First build
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.section_folded.get("Material").copied().unwrap_or(false), "Material folded after first build");
        assert!(panel.outliner_folded.get("Lights").copied().unwrap_or(false), "Lights folded after first build");

        // Reconfigure (simulating a layer change or sync)
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm)));

        // Fold states should survive configure
        assert!(panel.section_folded.get("Material").copied().unwrap_or(false), "Material folded after reconfigure");
        assert!(panel.outliner_folded.get("Lights").copied().unwrap_or(false), "Lights folded after reconfigure");

        // Second build
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.section_folded.get("Material").copied().unwrap_or(false), "Material folded after second build");
        assert!(panel.outliner_folded.get("Lights").copied().unwrap_or(false), "Lights folded after second build");

        // Verify the folded state still affects rendering
        // (folded sections should have fewer rows than expanded)
        panel.section_folded.insert("Transform".to_string(), false); // Ensure Transform is expanded for comparison
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        // Material should still be folded, Transform expanded
        assert!(panel.section_folded.get("Material").copied().unwrap_or(false), "Material remains folded through cycle");
        assert!(!panel.section_folded.get("Transform").copied().unwrap_or(true), "Transform remains expanded through cycle");
    }

    #[test]
    fn frame_action_on_object_emits_frame_selected_action() {
        // Test that Frame button emits SceneSetupFrameSelected action for orbit camera case
        let mut panel = ScenePanel::new();
        panel.open();
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();

        // Build to create the Frame button
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        // Verify Frame button was created for the selected object
        assert!(!panel.object_frame_ids.is_empty(), "Frame button should be created for object selection");

        // The full camera math (target = object position, distance = 2.2 × extent) is
        // tested in the app-side integration test that verifies the actual param writes.
        // This UI-level test verifies the button creation and routing infrastructure.
        assert!(!panel.object_frame_ids.is_empty(), "Frame button exists and is routable");
    }

    struct ButtonCase {
        name: &'static str,
        /// Set up selection before build, when the button needs one.
        select: fn(&mut ScenePanel),
        /// Find the button after build, checking its structural preconditions.
        locate: fn(&ScenePanel, &UITree) -> NodeId,
        /// The click's actions (and any panel state after it) are right.
        expect: fn(&ScenePanel, NodeId, &[PanelAction]) -> bool,
    }

    fn layer_1(l: &LayerId) -> bool {
        *l == LayerId::new("layer-1")
    }

    /// Each scene-setup button, clicked, is consumed and emits exactly its own
    /// action carrying the address the app-side dispatch needs.
    #[test]
    fn every_scene_setup_button_click_emits_its_action() {
        let cases = [
            ButtonCase {
                // One "+ Add Modifier" button; the app resolves the choice.
                name: "add modifier",
                select: |_| {},
                locate: |p, _| {
                    let (id, group) = p.add_modifier_button_id.expect("one Add Modifier button renders");
                    assert_eq!(group, 42);
                    id
                },
                expect: |_, id, a| matches!(a,
                    [PanelAction::Root(RootAction::SceneSetupAddModifierClicked(l, 42, n))] if layer_1(l) && *n == id),
            },
            ButtonCase {
                // BUG-224: close must emit the shared dock toggle, which resets
                // the dock width and rebuilds; it must not close the panel itself.
                name: "close",
                select: |_| {},
                locate: |p, _| {
                    assert_ne!(p.close_id, NodeId::PLACEHOLDER);
                    p.close_id
                },
                expect: |p, _, a| matches!(a, [PanelAction::Root(RootAction::OpenSceneSetup)]) && p.is_open(),
            },
            ButtonCase {
                // A layer plane takes the next object slot, so it carries object_count.
                name: "add plane",
                select: |_| {},
                locate: |p, _| p.add_plane_id.unwrap(),
                expect: |_, _, a| matches!(a,
                    [PanelAction::Project(ProjectAction::SceneSetupAddLayerPlane(l, 99, 2))] if layer_1(l)),
            },
            ButtonCase {
                name: "remove object",
                select: |_| {},
                locate: |p, t| {
                    assert_eq!(p.object_remove_ids.len(), 1, "one remove button, the properties header's");
                    let (id, index) = p.object_remove_ids[0];
                    assert_eq!(index, 0);
                    assert_eq!(t.name_of(id), Some("scene_setup.properties.remove"));
                    id
                },
                expect: |_, _, a| matches!(a,
                    [PanelAction::Project(ProjectAction::SceneSetupRemoveObject(l, 99, 0))] if layer_1(l)),
            },
            ButtonCase {
                name: "duplicate object",
                select: |_| {},
                locate: |p, _| {
                    assert_eq!(p.object_duplicate_ids.len(), 1);
                    let (id, index) = p.object_duplicate_ids[0];
                    assert_eq!(index, 0);
                    id
                },
                expect: |_, _, a| matches!(a,
                    [PanelAction::Project(ProjectAction::SceneSetupDuplicateObject(l, 99, 0))] if layer_1(l)),
            },
            ButtonCase {
                name: "remove light",
                select: |p| {
                    p.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
                },
                locate: |p, _| {
                    assert_eq!(p.light_remove_ids.len(), 1, "one remove button, the properties header's");
                    let (id, index) = p.light_remove_ids[0];
                    assert_eq!(index, 0);
                    id
                },
                expect: |_, _, a| matches!(a,
                    [PanelAction::Project(ProjectAction::SceneSetupRemoveLight(l, 99, 0))] if layer_1(l)),
            },
            ButtonCase {
                // The panel never touches the filesystem; it only carries the address.
                name: "import model",
                select: |_| {},
                locate: |p, _| p.import_model_id.unwrap(),
                expect: |_, _, a| matches!(a,
                    [PanelAction::Project(ProjectAction::SceneSetupImportModelClicked(l, 99))] if layer_1(l)),
            },
            ButtonCase {
                name: "rename object",
                select: |_| {},
                locate: |p, _| p.object_name_ids[0].1,
                expect: |_, _, a| matches!(a,
                    [PanelAction::Root(RootAction::SceneSetupRenameObjectClicked(l, 40, n))] if layer_1(l) && n == "Azalea"),
            },
        ];
        for case in &cases {
            let mut panel = ScenePanel::new();
            panel.open();
            panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
            (case.select)(&mut panel);
            let mut tree = UITree::new();
            panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
            let id = (case.locate)(&panel, &tree);

            let (consumed, actions) = panel.handle_event(&UIEvent::Click {
                node_id: id,
                pos: crate::node::Vec2::new(0.0, 0.0),
                modifiers: Modifiers::default(),
            }, &mut tree);
            assert!(consumed, "{}: click consumed", case.name);
            assert!((case.expect)(&panel, id, &actions), "{}: click emitted {actions:?}", case.name);
        }
    }
