use manifold_nodes_scene::node_graph::scene_exposure::{look_metadata, testkit::SCENE_VOCABULARY_TYPE_IDS};

    #[test]
    fn water_look_metadata_exposes_only_size() {
        let metadata = look_metadata();
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].name, "radius");
        assert_eq!(metadata[0].label, "Size");
        assert!(!SCENE_VOCABULARY_TYPE_IDS.contains(&"node.platonic_solid_mesh"));
        for preset in ["PhysicsSolids", "PhysicsBoxes", "WaterFloatingBoxMatter"] {
            let def = crate::bundled_presets::bundled_preset_def(&manifold_core::PresetTypeId::new(preset))
                .expect("shipped preset");
            assert!(def.preset_metadata.as_ref().unwrap().params.iter().all(|spec| spec.name != "Size"),
                "{preset} must not acquire Water look controls on load");
        }
    }

    use manifold_nodes_scene::node_graph::scene_exposure::{metadata_for_node_type, migrate_scene_exposures};
    use manifold_nodes_scene::node_graph::scene_exposure::testkit::{migrate_bokeh_source_coc, section_name_for_node};
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::effect_graph_def::{
        EffectGraphNode, EffectGraphWire, GroupDef, GroupInterface,
    };
    use manifold_core::NodeId;
    use std::collections::BTreeMap;

    fn graph_node(id: u32, node_id: &str, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(node_id),
            type_id: type_id.to_string(),
            handle: Some(node_id.to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn graph_wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire {
            from_node,
            from_port: from_port.to_string(),
            to_node,
            to_port: to_port.to_string(),
        }
    }

    fn graph_def(nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphDef {
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes,
            wires,
        }
    }

    fn grouped_node(id: u32, nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphNode {
        let mut group = graph_node(id, "dof", "group");
        group.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: Vec::new(),
                outputs: Vec::new(),
                params: Vec::new(),
            },
            nodes,
            wires,
            tint: None,
        }));
        group
    }

    fn canonical_coc_scope() -> (Vec<EffectGraphNode>, Vec<EffectGraphWire>) {
        (
            vec![
                graph_node(10, "coc", "node.coc_from_depth"),
                graph_node(11, "dilate", "node.coc_dilate"),
                graph_node(12, "bokeh", "node.bokeh_gather"),
            ],
            vec![
                graph_wire(10, "out", 11, "in"),
                graph_wire(11, "out", 12, "width"),
            ],
        )
    }

    #[test]
    fn migrate_nested_canonical_coc_wire_removes_dilate_and_is_idempotent() {
        let (nodes, wires) = canonical_coc_scope();
        let mut def = graph_def(vec![grouped_node(1, nodes, wires)], Vec::new());

        assert!(migrate_scene_exposures(&mut def));
        let group = def.nodes[0].group.as_ref().expect("nested dof group");
        assert!(group.nodes.iter().all(|node| node.type_id != "node.coc_dilate"));
        assert!(group.wires.iter().any(|wire| {
            wire.from_node == 10
                && wire.from_port == "out"
                && wire.to_node == 12
                && wire.to_port == "width"
        }));
        assert_eq!(group.nodes.iter().find(|node| node.id == 12).unwrap().params["blur_alpha"],
            manifold_core::effect_graph_def::SerializedParamValue::Bool { value: true });

        let after = def.clone();
        assert!(!migrate_scene_exposures(&mut def));
        assert_eq!(def, after, "repeating load migration must be a no-op");
    }

    #[test]
    fn migrate_keeps_shared_dilate_for_other_consumers() {
        let mut def = graph_def(
            vec![
                graph_node(10, "coc", "node.coc_from_depth"),
                graph_node(11, "dilate", "node.coc_dilate"),
                graph_node(12, "bokeh", "node.bokeh_gather"),
                graph_node(13, "other", "node.variable_blur"),
            ],
            vec![
                graph_wire(10, "out", 11, "in"),
                graph_wire(11, "out", 12, "width"),
                graph_wire(11, "out", 13, "width"),
            ],
        );

        assert!(migrate_bokeh_source_coc(&mut def));
        assert!(def.nodes.iter().any(|node| node.id == 11));
        assert!(def.wires.iter().any(|wire| {
            wire.from_node == 10 && wire.to_node == 12 && wire.to_port == "width"
        }));
        assert!(def.wires.iter().any(|wire| {
            wire.from_node == 11 && wire.to_node == 13 && wire.to_port == "width"
        }));
        assert!(def.wires.iter().any(|wire| wire.to_node == 11 && wire.to_port == "in"));
    }

    #[test]
    fn camera_dof_migration_keeps_explicit_alpha_choice() {
        let mut bokeh = graph_node(12, "bokeh", "node.bokeh_gather");
        bokeh.params.insert("blur_alpha".to_string(),
            manifold_core::effect_graph_def::SerializedParamValue::Bool { value: false });
        let mut def = graph_def(
            vec![graph_node(10, "coc", "node.coc_from_depth"), bokeh],
            vec![graph_wire(10, "out", 12, "width")],
        );
        assert!(!migrate_bokeh_source_coc(&mut def));
        assert_eq!(def.nodes[1].params["blur_alpha"],
            manifold_core::effect_graph_def::SerializedParamValue::Bool { value: false });
    }

    #[test]
    fn migrate_leaves_noncanonical_coc_producer_untouched() {
        let mut def = graph_def(
            vec![
                graph_node(10, "custom", "node.custom_coc"),
                graph_node(11, "dilate", "node.coc_dilate"),
                graph_node(12, "bokeh", "node.bokeh_gather"),
            ],
            vec![
                graph_wire(10, "out", 11, "in"),
                graph_wire(11, "out", 12, "width"),
            ],
        );
        let before = def.clone();
        assert!(!migrate_bokeh_source_coc(&mut def));
        assert_eq!(def, before);
    }

    #[test]
    fn bokeh_controls_migrate_without_changing_saved_lens_or_toggle() {
        let mut def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version":2,
            "nodes":[{"id":5,"nodeId":"bokeh","typeId":"node.bokeh_gather",
                "params":{"enabled":{"type":"Bool","value":false},
                          "max_radius":{"type":"Float","value":16.0}}}],
            "wires":[]
        }))
        .unwrap();
        assert!(migrate_scene_exposures(&mut def));
        let metadata = def.preset_metadata.as_ref().unwrap();
        for name in ["enabled", "aperture", "quality"] {
            assert!(metadata.bindings.iter().any(|b| matches!(&b.target,
                manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                if node_id.as_str() == "bokeh" && param == name)));
        }
        assert!(!metadata.bindings.iter().any(|b| matches!(&b.target,
            manifold_core::effect_graph_def::BindingTarget::Node { param, .. } if param == "max_radius")));
        assert_eq!(
            def.nodes[0].params["enabled"],
            manifold_core::effect_graph_def::SerializedParamValue::Bool { value: false }
        );
        assert!(!migrate_scene_exposures(&mut def));
    }

    #[test]
    fn metadata_for_light_includes_enum_and_float_params() {
        let meta = metadata_for_node_type("node.light");
        assert!(!meta.is_empty());
        let mode = meta.iter().find(|m| m.name == "mode").expect("mode present");
        assert!(matches!(mode.convert, manifold_core::effects::ParamConvert::EnumRound));
        assert!(!mode.value_labels.is_empty());
        let intensity = meta
            .iter()
            .find(|m| m.name == "intensity")
            .expect("intensity present");
        assert!(matches!(
            intensity.convert,
            manifold_core::effects::ParamConvert::Float
        ));
    }

    /// R2 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `cast_shadows` stays
    /// `Float`/0..1 (modulatable by an LFO/trigger — `ParamType::Bool` would
    /// lose that) but declares `enum_values: ["Off", "On"]` as display-only
    /// labels; `metadata_for_node_type` must carry them into `value_labels`
    /// even though the param's real type isn't `Enum`. Regression coverage
    /// for the R2 bug: the scene panel's Cast Shadows row showed the raw
    /// float ("1.00") instead of "On"/"Off" because `value_labels` was
    /// previously populated ONLY for `ParamType::Enum`.
    #[test]
    fn metadata_for_light_carries_cast_shadows_display_labels_despite_float_type() {
        let meta = metadata_for_node_type("node.light");
        let cast_shadows = meta
            .iter()
            .find(|m| m.name == "cast_shadows")
            .expect("cast_shadows present");
        assert!(
            matches!(cast_shadows.convert, manifold_core::effects::ParamConvert::Float),
            "stays a modulatable Float param, not converted to Bool"
        );
        assert_eq!(
            cast_shadows.value_labels,
            vec!["Off".to_string(), "On".to_string()],
            "display labels carried through despite the Float type"
        );
    }

    /// Reference emitter controls share the existing manifest-backed
    /// Whitewater card, with FLIP defaults and no separate widget path.
    #[test]
    fn whitewater_step_exposes_reference_emitter_controls() {
        let metadata = metadata_for_node_type("node.whitewater_step");
        let names: Vec<&str> = metadata.iter().map(|param| param.name.as_str()).collect();
        for name in ["enabled", "amount", "wavecrest_emission", "turbulence_emission", "min_turbulence", "max_turbulence", "inside_emission", "dust_emission"] {
            assert!(names.contains(&name), "missing {name}: {names:?}");
        }
        for (name, expected) in [("wavecrest_emission", 175.0), ("turbulence_emission", 175.0), ("min_turbulence", 100.0), ("max_turbulence", 200.0)] {
            let value = metadata.iter().find(|p| p.name == name).unwrap();
            assert_eq!(value.default_value, manifold_core::effect_graph_def::SerializedParamValue::Float { value: expected });
        }
        let enabled = &metadata[0];
        assert!(!enabled.whole_numbers);
        assert!(matches!(enabled.convert, manifold_core::effects::ParamConvert::Float));
        assert_eq!(enabled.value_labels, vec!["Off".to_string(), "On".to_string()]);
        let node = graph_node(7, "whitewater", "node.whitewater_step");
        assert_eq!(section_name_for_node(&node), "Whitewater");
    }

    #[test]
    fn metadata_for_unknown_type_is_empty() {
        assert!(metadata_for_node_type("node.definitely_not_real").is_empty());
    }


    #[test]
    fn retired_cpu_flip_graph_is_not_migrated_or_exposed() {
        let fluid = graph_node(9, "fluid", manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID);
        let mut def = graph_def(vec![fluid], Vec::new());
        let before = serde_json::to_vec(&def).unwrap();
        assert!(!migrate_scene_exposures(&mut def));
        assert_eq!(serde_json::to_vec(&def).unwrap(), before);
        assert!(metadata_for_node_type(manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID).is_empty());
    }

    #[test]
    fn metadata_for_mesh_modifiers_preserves_unbounded_angles_with_degree_presentation() {
        for type_id in ["node.bend_mesh", "node.twist_mesh"] {
            let angle = metadata_for_node_type(type_id)
                .into_iter()
                .find(|param| param.name == "angle")
                .unwrap_or_else(|| panic!("{type_id} angle metadata missing"));
            assert!(
                angle.is_angle,
                "{type_id} angle must be presented in degrees"
            );
            assert_eq!(
                (angle.min, angle.max),
                (-std::f32::consts::TAU, std::f32::consts::TAU),
                "{type_id} gets a useful exposure band while its primitive stays unbounded"
            );
        }

        // The already-bounded rotate descriptor is the control case: the
        // projection must preserve its authored ±TAU band rather than apply
        // a second fallback.
        let rotate = metadata_for_node_type("node.rotate_3d");
        for axis in ["angle_x", "angle_y", "angle_z"] {
            let angle = rotate.iter().find(|param| param.name == axis).unwrap();
            assert!(angle.is_angle);
            assert_eq!(
                (angle.min, angle.max),
                (-std::f32::consts::TAU, std::f32::consts::TAU)
            );
        }
    }

    #[test]
    fn material_inspector_metadata_classifies_every_descriptor() {
        let metadata = metadata_for_node_type("node.pbr_material");
        assert_eq!(metadata.len(), 297);
        assert!(metadata.iter().all(|param| param.material_role.is_some()));
        assert_eq!(
            metadata
                .iter()
                .filter(|param| matches!(
                    param.material_role,
                    Some(manifold_core::material_inspector::MaterialParamRole::FeatureMode(_))
                ))
                .count(),
            8
        );
    }

    /// R2 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md), manifest level: the
    /// REAL production metadata (`metadata_for_node_type`, not a synthetic
    /// fixture) stamped through `stamp_scene_node_exposures_into` (the exact
    /// call `AddSceneLightCommand::execute` makes) must produce a
    /// `ParamSpecDef` for `cast_shadows` carrying `value_labels`, so the
    /// scene panel's `format_param_value` (which reads straight off the
    /// manifest's `ParamSpecDef.value_labels`) substitutes "On"/"Off" text
    /// instead of the raw float.
    #[test]
    fn stamped_light_manifest_carries_cast_shadows_value_labels() {
        use manifold_core::scene_exposure::stamp_scene_node_exposures_into;

        let node_id = NodeId::new("light_0");
        let light_metadata = metadata_for_node_type("node.light");
        let mut params = Vec::new();
        let mut bindings = Vec::new();
        stamp_scene_node_exposures_into(
            &mut params,
            &mut bindings,
            1,
            &node_id,
            "node.light",
            "Light 1",
            &light_metadata,
            &std::collections::BTreeMap::new(),
        );

        let cast_shadows_spec = params
            .iter()
            .find(|p| p.name == "Cast Shadows")
            .expect("cast_shadows exposed onto the manifest");
        assert_eq!(
            cast_shadows_spec.value_labels,
            vec!["Off".to_string(), "On".to_string()],
            "the stamped manifest ParamSpecDef carries the display labels"
        );
    }

    /// RAYTRACING_DESIGN.md D14/section 5.2/section 9 RD9: the scene root's RT toggles surface on
    /// the scene panel via the same vocabulary migration as every other
    /// scene control — curated to EXACTLY the three toggles, so the root
    /// node's dozens of other params never flood the panel.
    #[test]
    fn migrate_stamps_render_scene_rt_toggles_only_under_rendering_section() {
        use std::collections::BTreeMap;

        let def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![manifold_core::effect_graph_def::EffectGraphNode {
                id: 7,
                node_id: NodeId::new("scene_root"),
                type_id: "node.render_scene".to_string(),
                handle: None,
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };

        let mut migrated = def.clone();
        assert!(migrate_scene_exposures(&mut migrated));
        let meta = migrated.preset_metadata.expect("metadata stamped");
        let stamped: Vec<&str> = meta
            .bindings
            .iter()
            .filter_map(|b| match &b.target {
                manifold_core::effect_graph_def::BindingTarget::Node { param, .. } => {
                    Some(param.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            stamped,
            vec![
                "rt_enabled",
                "temporal_upscale",
                "rt_reflections",
                "rt_shadows",
                "rt_ao",
                "rt_gi",
                "rt_denoise_feed"
            ],
            "exactly the seven RT toggles, nothing else from the root node"
        );
        for spec in &meta.params {
            assert_eq!(spec.section.as_deref(), Some("Rendering"));
            assert!(spec.is_toggle, "{} must surface as a toggle row", spec.name);
            assert!(
                spec.card_visible,
                "{} must be card-visible (the curated inspector card shows ALL stamped RT toggles)",
                spec.name,
            );
        }
    }

    /// 2026-08-27 (Peter's unusable-ranges report): a pre-fix project carries
    /// the lens's legacy neutral `f_stop = 1000` plus a stamp the widen rule
    /// stretched to 0.5–1000. Load must rewrite the stored value to 32
    /// (top of the band) and un-stretch the stamp — AND force bokeh
    /// `enabled` false (the 1000 proves DoF was never dialed; off is the
    /// toggle now, and migrated projects must not GAIN visible DoF on load).
    /// Any other stored f-stop is a performer's choice and must survive.
    #[test]
    fn migrate_repairs_legacy_lens_f_stop_1000_and_unstretches_stamp() {
        use manifold_core::effect_graph_def::SerializedParamValue;
        use std::collections::BTreeMap;

        let lens = |f_stop: f32, id: u32, node_id: &str| {
            let mut params = BTreeMap::new();
            params.insert("f_stop".to_string(), SerializedParamValue::Float { value: f_stop });
            manifold_core::effect_graph_def::EffectGraphNode {
                id,
                node_id: NodeId::new(node_id),
                type_id: "node.camera_lens".to_string(),
                handle: Some(node_id.to_string()),
                params,
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }
        };

        let bokeh = manifold_core::effect_graph_def::EffectGraphNode {
            id: 5,
            node_id: NodeId::new("bokeh"),
            type_id: "node.bokeh_gather".to_string(),
            handle: Some("bokeh".to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };

        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![lens(1000.0, 3, "lens_legacy"), lens(2.8, 4, "lens_dialed"), bokeh],
            wires: vec![],
        };

        // First pass stamps and repairs; doctor the repaired lens back to
        // the exact pre-fix state (legacy seed + stretched stamp + DoF
        // default-on) to prove the load path repairs a REAL old project,
        // not just a fresh one.
        assert!(migrate_scene_exposures(&mut def));
        {
            let legacy = def.nodes.iter_mut().find(|n| n.id == 3).unwrap();
            legacy.params.insert(
                "f_stop".to_string(),
                SerializedParamValue::Float { value: 1000.0 },
            );
            let bokeh = def.nodes.iter_mut().find(|n| n.id == 5).unwrap();
            bokeh.params.insert(
                "enabled".to_string(),
                SerializedParamValue::Bool { value: true },
            );
            let meta = def.preset_metadata.as_mut().unwrap();
            let binding = meta
                .bindings
                .iter_mut()
                .find(|b| b.id == "3_f_stop")
                .expect("stamped f_stop binding");
            binding.default_value = 1000.0;
            let spec = meta.params.iter_mut().find(|p| p.id == "3_f_stop").unwrap();
            spec.default_value = 1000.0;
            spec.max = 1000.0;
            // The bokeh stamp a real tail project carries — bokeh_gather is
            // not in the scene vocabulary, so the generic stamper never
            // makes one; the import assembly / v1130 migration does.
            meta.bindings.push(manifold_core::effect_graph_def::BindingDef {
                id: "5_enabled".to_string(),
                label: "Enabled".to_string(),
                default_value: 1.0,
                target: manifold_core::effect_graph_def::BindingTarget::Node {
                    node_id: NodeId::new("bokeh"),
                    param: "enabled".to_string(),
                },
                convert: manifold_core::effects::ParamConvert::BoolThreshold,
                user_added: false,
                scale: 1.0,
                offset: 0.0,
                default_mirrors_node_param: true,
            });
            meta.params.push(manifold_core::effect_graph_def::ParamSpecDef {
                id: "5_enabled".to_string(),
                name: "Enabled".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 1.0,
                section: Some("Camera".to_string()),
                ..Default::default()
            });
        }

        assert!(
            migrate_scene_exposures(&mut def),
            "legacy state must be repaired on load"
        );
        let legacy = def.nodes.iter().find(|n| n.id == 3).unwrap();
        assert_eq!(
            legacy.params.get("f_stop"),
            Some(&SerializedParamValue::Float { value: 32.0 }),
            "legacy 1000 seed rewritten to the in-band neutral"
        );
        let bokeh = def.nodes.iter().find(|n| n.id == 5).unwrap();
        assert_eq!(
            bokeh.params.get("enabled"),
            Some(&SerializedParamValue::Bool { value: false }),
            "DoF forced off — the legacy 1000 proves it was never dialed"
        );
        let meta = def.preset_metadata.as_ref().unwrap();
        let spec = meta.params.iter().find(|p| p.id == "3_f_stop").unwrap();
        assert_eq!(spec.default_value, 32.0);
        assert_eq!((spec.min, spec.max), (0.5, 32.0), "stretched stamp un-stretched to the band");
        assert_eq!(
            meta.bindings.iter().find(|b| b.id == "3_f_stop").unwrap().default_value,
            32.0
        );
        assert_eq!(
            meta.params.iter().find(|p| p.id == "5_enabled").unwrap().default_value,
            0.0,
            "bokeh stamp default follows the forced-off node param"
        );
        assert_eq!(
            meta.bindings.iter().find(|b| b.id == "5_enabled").unwrap().default_value,
            0.0
        );
        let dialed = def.nodes.iter().find(|n| n.id == 4).unwrap();
        assert_eq!(
            dialed.params.get("f_stop"),
            Some(&SerializedParamValue::Float { value: 2.8 }),
            "a performer's chosen aperture is never rewritten"
        );

        let after_repair = def.clone();
        assert!(
            !migrate_scene_exposures(&mut def),
            "second migration run is a no-op once repaired"
        );
        assert_eq!(def, after_repair);
    }

    #[test]
    fn migrate_is_idempotent() {
        use std::collections::BTreeMap;

        let def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![manifold_core::effect_graph_def::EffectGraphNode {
                id: 1,
                node_id: NodeId::new("sun"),
                type_id: "node.light".to_string(),
                handle: Some("Sun".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };

        let mut first = def.clone();
        assert!(migrate_scene_exposures(&mut first));
        let mut second = first.clone();
        assert!(!migrate_scene_exposures(&mut second));
        assert_eq!(first, second);
    }
