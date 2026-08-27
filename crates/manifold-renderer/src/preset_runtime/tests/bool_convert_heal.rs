    //! Peter 2026-08-27 critical: old projects rendered nothing for 3D
    //! scenes. The v1130 cinematic-tail migration stamped the motion_blur /
    //! bokeh `enabled` bindings without a `convert`, which deserializes as
    //! `Float`. A Float convert into a Bool-typed param (a) tripped the
    //! `into_graph` convert check, so the whole generator refused to load
    //! (black layer), and (b) when it did land, silently missed the Bool
    //! slot — readers match `ParamValue::Bool` exactly, so the toggle was
    //! dead and the effect stuck on. The cure is a load-time heal:
    //! Float/IntRound → BoolThreshold when the target param is declared
    //! Bool (`heal_bool_convert_bindings`, called at the top of `from_def`).
    use super::*;
    use crate::node_graph::ParamValue;
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::effects::ParamConvert;

    /// Lissajous with its stock `show_verts` binding (target
    /// `render.show_verts`, a Bool param) re-poisoned to the v1130 shape:
    /// convert Float. This is byte-for-byte what a migrated project carries.
    fn poisoned_lissajous() -> EffectGraphDef {
        let json = include_str!("../../../assets/generator-presets/Lissajous.json");
        let mut def: EffectGraphDef =
            serde_json::from_str(json).expect("Lissajous preset JSON must parse");
        let meta = def.preset_metadata.as_mut().expect("preset metadata");
        let binding = meta
            .bindings
            .iter_mut()
            .find(|b| b.id == "show_verts")
            .expect("show_verts binding");
        binding.convert = ParamConvert::Float;
        def
    }

    /// The disease, pinned: without the heal, the poisoned document is
    /// refused at load by the convert check — the black-layer failure.
    #[test]
    fn poisoned_binding_without_heal_is_refused_by_into_graph() {
        let def = poisoned_lissajous();
        match def.into_graph(&PrimitiveRegistry::with_builtin()) {
            Err(err @ LoadError::BindingConvertTypeMismatch { .. }) => {
                let _ = err;
            }
            Err(other) => panic!("expected BindingConvertTypeMismatch, got {other:?}"),
            Ok(_) => panic!("Float convert into a Bool target must be refused"),
        }
    }

    /// The cure, end to end: the same poisoned document builds through the
    /// real generator path, and the seeded value lands as a Bool — not a
    /// Float the node's readers would silently skip.
    #[test]
    fn from_def_heals_poisoned_binding_and_seeds_bool() {
        let def = poisoned_lissajous();
        let g = PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), None)
            .expect("poisoned binding must be healed at load, not refused");
        let render = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("render"))
            .expect("render node instance");
        let value = g
            .graph
            .get_node(render)
            .and_then(|n| n.params.get("show_verts").cloned())
            .expect("render exposes show_verts");
        assert!(
            matches!(value, ParamValue::Bool(true)),
            "healed binding must land a Bool (default 1.0 → true), got {value:?}"
        );
    }

    /// The heal itself: Float and IntRound into Bool targets upgrade;
    /// everything else — Float targets, unknown nodes, already-correct
    /// converts — is left byte-identical.
    #[test]
    fn heal_upgrades_only_float_converts_into_bool_targets() {
        let mut def = poisoned_lissajous();
        // Second binding: IntRound into the same Bool target — same class.
        let meta = def.preset_metadata.as_mut().unwrap();
        let mut int_round = meta.bindings.iter().find(|b| b.id == "show_verts").unwrap().clone();
        int_round.id = "show_verts_int".to_string();
        int_round.convert = ParamConvert::IntRound;
        meta.bindings.push(int_round);
        // Third binding: Float into an unknown node — unresolvable, skip.
        let mut orphan = meta.bindings.iter().find(|b| b.id == "show_verts").unwrap().clone();
        orphan.id = "orphan".to_string();
        orphan.target = manifold_core::effect_graph_def::BindingTarget::Node {
            node_id: manifold_core::NodeId::new("no_such_node"),
            param: "enabled".to_string(),
        };
        meta.bindings.push(orphan);

        let healed = crate::preset_runtime::build::heal_bool_convert_bindings(
            &mut def,
            &PrimitiveRegistry::with_builtin(),
        );
        assert_eq!(healed, 2, "the two Bool-target poisons heal, the orphan skips");

        let meta = def.preset_metadata.as_ref().unwrap();
        let get = |id: &str| meta.bindings.iter().find(|b| b.id == id).unwrap().convert;
        assert_eq!(get("show_verts"), ParamConvert::BoolThreshold);
        assert_eq!(get("show_verts_int"), ParamConvert::BoolThreshold);
        assert_eq!(get("orphan"), ParamConvert::Float, "unresolvable target: untouched");
        // A Float-typed neighbour (freq_x_rate → angular_rate, a Float
        // param) stays Float.
        assert_eq!(get("freq_x_rate"), ParamConvert::Float);
    }
