use manifold_node_engine::load::binding_migration::*;
use manifold_nodes::bundled_presets::bundled_preset_def;
use manifold_core::project::Project;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
    };
    use manifold_core::effects::{PresetInstance, ParamConvert};

    /// Build a metadata-only stub graph carrying one user-added binding —
    /// the exact shape the v1.3→v1.4 JSON fold-in emits when an effect had
    /// `userParamBindings` but no per-instance graph.
    fn stub_with_user_binding(node_handle: &str, inner: &str, id: &str) -> EffectGraphDef {
        let meta = PresetMetadata {
            id: PresetTypeId::new(""),
            display_name: String::new(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
                layer_types: None,
            params: vec![ParamSpecDef {
                tooltip: None,
                id: id.to_string(),
                name: inner.to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 0.0,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: Vec::new(),
                format_string: None,
                osc_suffix: String::new(),
                curve: Default::default(),
                invert: false,
                is_angle: false,
                is_trigger_gate: false,
                wraps: false,
                section: None,
                card_visible: true,
                material_role: None,
            }],
            bindings: vec![BindingDef {
                id: id.to_string(),
                label: inner.to_string(),
                default_value: 0.0,
                // `node_id == handle` — the convention the canonical
                // preset stamp uses, so this resolves after the lift.
                target: BindingTarget::Node {
                    node_id: manifold_core::NodeId::new(node_handle),
                    param: inner.to_string(),
                },
                convert: ParamConvert::Float,
                user_added: true,
                scale: 1.0,
                offset: 0.0,
                default_mirrors_node_param: false,
            }],
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
            scene_modifier: None,
            scene_bounds: None,
        };
        EffectGraphDef {
            version: 0,
            name: None,
            description: None,
            preset_metadata: Some(meta),
            scene_modifiers: Vec::new(),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    #[test]
    fn stub_graph_is_completed_with_canonical_topology() {
        // Bloom is a `graph: None` effect. A migrated user binding for it
        // arrives as a metadata-only stub; the lift must restore Bloom's
        // canonical nodes while keeping the user binding.
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.graph = Some(stub_with_user_binding("blur", "radius", "user.blur.radius.1"));
        project.settings.master_effects.push(fx);

        migrate_user_param_bindings_to_node_id(&mut project);

        let g = project.settings.master_effects[0]
            .graph
            .as_ref()
            .expect("graph present");
        assert!(!g.nodes.is_empty(), "canonical topology lifted in");
        let meta = g.preset_metadata.as_ref().expect("metadata present");
        assert!(
            meta.bindings.iter().any(|b| b.id == "user.blur.radius.1" && b.user_added),
            "user binding preserved on the lifted graph"
        );
        // The user binding's value slot still resolves by id.
        assert!(
            project.settings.master_effects[0]
                .user_param_bindings()
                .iter()
                .any(|b| b.id == "user.blur.radius.1"),
            "user binding enumerates from the lifted graph"
        );
    }

    #[test]
    fn already_completed_graph_is_left_alone() {
        // A graph that already has nodes (real override, or a re-loaded
        // completed stub) must not be re-lifted — idempotency.
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        let canonical = bundled_preset_def(&PresetTypeId::BLOOM)
            .expect("Bloom preset present")
            .clone();
        let node_count = canonical.nodes.len();
        fx.graph = Some(canonical);
        project.settings.master_effects.push(fx);

        migrate_user_param_bindings_to_node_id(&mut project);

        assert_eq!(
            project.settings.master_effects[0].graph.as_ref().unwrap().nodes.len(),
            node_count,
            "completed graph untouched"
        );
    }

    #[test]
    fn graph_none_effect_is_left_alone() {
        // No graph, no migration — a plain effect stays `graph: None`.
        let mut project = Project::default();
        let fx = PresetInstance::new(PresetTypeId::BLOOM);
        project.settings.master_effects.push(fx);

        migrate_user_param_bindings_to_node_id(&mut project);

        assert!(project.settings.master_effects[0].graph.is_none());
    }
