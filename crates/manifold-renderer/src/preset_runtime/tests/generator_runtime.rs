    //! Generator construction + per-frame regression tests (folded in from the
    //! deleted `JsonGraphGenerator` module). They drive the `from_*` generator
    //! constructors and the `render`/`apply_param_values`/`resize`/preview
    //! surface of the unified [`PresetRuntime`].
    use super::*;
    use crate::node_graph::PrimitiveRegistry;
    use manifold_core::Beats;
    use manifold_core::Seconds;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::params::Param;

    /// Build a single id-keyed manifest param for test [`ParamManifest`]
    /// literals — the id-keyed replacement for the old positional `&[f32]`
    /// slice `apply_param_values` used to take.
    fn slot(id: &str, value: f32) -> Param {
        let mut p = Param::bundled(ParamSpecDef {
            id: id.into(),
            name: id.into(),
            min: 0.0,
            max: 1.0,
            default_value: value,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: vec![],
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
        });
        p.value = value;
        p.base = value;
        p.exposed = true;
        p
    }

    /// Build a [`ParamManifest`] from `(id, value)` pairs, in the order
    /// given — mirrors the positional `&[f32]` slices these tests used to
    /// pass to `apply_param_values` before the id-keyed manifest replaced it.
    fn manifest(pairs: &[(&str, f32)]) -> ParamManifest {
        ParamManifest::from_params(pairs.iter().map(|(id, v)| slot(id, *v)).collect())
    }

    /// Regression for the "Lissajous repeats back-to-back in clip-trigger mode"
    /// bug: two bindings keyed by the same outer-card id (`clip_trigger`) must
    /// both pick up that slider's value (fan-out by source id, not position).
    #[test]
    fn fan_out_binding_writes_every_target_with_the_same_outer_value() {
        let json = include_str!("../../../assets/generator-presets/Lissajous.json");
        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Lissajous preset must load");

        // Address inner nodes by stable node_id (grouping prefixes handles,
        // node_id survives the flatten the loader applies).
        let mux_x_id = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("mux_x"))
            .expect("Lissajous declares a `mux_x` node");
        let mux_y_id = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("mux_y"))
            .expect("Lissajous declares a `mux_y` node");

        g.apply_param_values(&manifest(&[
            ("freq_x_rate", 0.13),
            ("freq_y_rate", 0.09),
            ("phase_rate", 0.07),
            ("line", 0.002),
            ("show_verts", 1.0),
            ("vert_size", 1.0),
            ("animate", 0.0),
            ("speed", 1.0),
            ("window", 0.1),
            ("scale", 1.0),
            ("clip_trigger", 1.0),
        ]));

        let mux_x = g.graph.get_node(mux_x_id).unwrap();
        assert!(
            matches!(
                mux_x.params.get("selector"),
                Some(ParamValue::Float(v)) if (*v - 1.0).abs() < 1e-5
            ),
            "mux_x.selector should be 1.0, got {:?}",
            mux_x.params.get("selector"),
        );
        let mux_y = g.graph.get_node(mux_y_id).unwrap();
        assert!(
            matches!(
                mux_y.params.get("selector"),
                Some(ParamValue::Float(v)) if (*v - 1.0).abs() < 1e-5
            ),
            "mux_y.selector should be 1.0 (fan-out from same `clip_trigger` outer \
             slider as mux_x), got {:?}",
            mux_y.params.get("selector"),
        );
    }

    /// BUG-104 — `clear_trigger_state` on a REAL shipped preset (Lissajous)
    /// walks the graph, finds exactly the nodes `is_trigger_latch` flags
    /// (`ratio` — `node.frequency_ratio`), and purges ONLY their
    /// `StateStore` buckets, leaving an ordinary node's (`render` —
    /// `node.draw_lines`) bucket untouched. No GPU needed —
    /// `clear_trigger_state` never touches the backend.
    #[test]
    fn clear_trigger_state_purges_only_flagged_nodes_state_store_buckets() {
        use crate::node_graph::NodeState;

        struct Probe;
        impl NodeState for Probe {}

        let json = include_str!("../../../assets/generator-presets/Lissajous.json");
        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Lissajous preset must load");

        let ratio_id = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("ratio"))
            .expect("Lissajous declares a `ratio` (frequency_ratio) node");
        let render_id = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("render"))
            .expect("Lissajous declares a `render` (draw_lines) node");

        assert!(
            g.graph.get_node(ratio_id).unwrap().node.is_trigger_latch(),
            "frequency_ratio must flag itself as a trigger latch"
        );
        assert!(
            !g.graph.get_node(render_id).unwrap().node.is_trigger_latch(),
            "draw_lines is not a trigger latch — clear_trigger_state must leave it alone"
        );

        // Seed a StateStore bucket under BOTH node ids (owner_key 0, the
        // generator convention) — clear_trigger_state must purge only the
        // one belonging to the flagged node.
        g.state_store.insert(ratio_id, 0, Probe);
        g.state_store.insert(render_id, 0, Probe);

        g.clear_trigger_state();

        assert!(
            g.state_store.get::<Probe>(ratio_id, 0).is_none(),
            "trigger-latch node's StateStore bucket must be purged"
        );
        assert!(
            g.state_store.get::<Probe>(render_id, 0).is_some(),
            "non-latch node's StateStore bucket must survive a trigger-only clear"
        );
    }

    /// BUG-104 Part 5(b) — the live build-time counterpart to
    /// `trigger_shadow_class_guard.rs`'s offline sweep: the REAL shipped
    /// Lissajous.json (post BUG-104 Part 3 fix) must build with ZERO
    /// `TriggerShadowsContinuousBinding` errors — proving the fix is
    /// structurally clean, not just visually plausible.
    #[test]
    fn lissajous_builds_with_no_trigger_shadow_errors() {
        let json = include_str!("../../../assets/generator-presets/Lissajous.json");
        let g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Lissajous preset must load");
        let shadow_errors: Vec<_> = g
            .errors()
            .iter()
            .filter(|e| matches!(e, ChainError::TriggerShadowsContinuousBinding { .. }))
            .collect();
        assert!(
            shadow_errors.is_empty(),
            "Lissajous must build with no BUG-104 trigger-shadow errors, got {shadow_errors:?}"
        );
    }

    /// BUG-104 Part 5(b) — same synthetic pre-fix shape as
    /// `trigger_shadow_class_guard.rs`'s regression test, but exercised
    /// through the REAL build path (`PresetRuntime::from_def` via
    /// `from_json_str`) to prove the warning reaches `PresetRuntime::
    /// errors()` — the channel editor UI / MCP-driven mutations / agent-
    /// authored graphs all read, not just the offline sweep test.
    #[test]
    fn from_json_str_surfaces_trigger_shadow_as_a_chain_error() {
        let json = r#"{
            "version": 2,
            "name": "SyntheticPreFixShape",
            "nodes": [
                { "id": 0, "nodeId": "input", "typeId": "system.generator_input" },
                { "id": 1, "nodeId": "lfo_x", "typeId": "node.lfo",
                  "params": { "angular_rate": { "type": "Float", "value": 0.13 } } },
                { "id": 2, "nodeId": "mux_x", "typeId": "node.switch_value" },
                { "id": 3, "nodeId": "uv", "typeId": "node.uv_field" },
                { "id": 4, "nodeId": "final_output", "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in_0" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ],
            "presetMetadata": {
                "id": "SyntheticPreFixShape",
                "displayName": "Synthetic",
                "category": "Geometry",
                "oscPrefix": "synthetic",
                "params": [
                    { "id": "freq_x_rate", "name": "Freq X Rate", "min": 0.0, "max": 1.0,
                      "defaultValue": 0.13, "wholeNumbers": false, "isToggle": false, "isTrigger": false },
                    { "id": "clip_trigger", "name": "Clip Trigger", "min": 0.0, "max": 1.0,
                      "defaultValue": 0.0, "wholeNumbers": false, "isToggle": true, "isTriggerGate": true, "isTrigger": false }
                ],
                "bindings": [
                    { "id": "freq_x_rate", "label": "Freq X Rate", "defaultValue": 0.13,
                      "target": { "kind": "node", "nodeId": "lfo_x", "param": "angular_rate" },
                      "convert": { "type": "Float" } },
                    { "id": "clip_trigger", "label": "Clip Trigger", "defaultValue": 0.0,
                      "target": { "kind": "node", "nodeId": "mux_x", "param": "selector" },
                      "convert": { "type": "Float" } }
                ]
            }
        }"#;
        let g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("synthetic pre-fix-shaped preset must still build (this is a warning, not a \
                     hard failure — the graph runs, it just has a shadowed fader)");
        let shadow_errors: Vec<_> = g
            .errors()
            .iter()
            .filter(|e| matches!(e, ChainError::TriggerShadowsContinuousBinding { .. }))
            .collect();
        assert_eq!(
            shadow_errors.len(),
            1,
            "from_json_str (-> from_def) must surface the trigger-shadow finding through \
             PresetRuntime::errors(), got {shadow_errors:?}"
        );
    }

    /// Regression for the "Plasma looks frozen" bug: outer-card slider values
    /// must reach the inner-node param via the preset's declared bindings.
    #[test]
    fn apply_param_values_routes_into_inner_node_params() {
        let json = include_str!("../../../assets/generator-presets/Plasma.json");
        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Plasma preset must load");
        let plasma_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "plasma")
            .map(|(_, id)| id)
            .expect("Plasma preset declares a node with handle `plasma`");

        g.apply_param_values(&manifest(&[
            ("pattern", 3.0),
            ("complexity", 0.75),
            ("contrast", 0.42),
            ("speed", 2.5),
            ("scale", 1.5),
            ("clip_trigger", 1.0),
        ]));

        let inst = g.graph.get_node(plasma_id).unwrap();
        assert!(matches!(
            inst.params.get("complexity"),
            Some(ParamValue::Float(v)) if (*v - 0.75).abs() < 1e-5
        ));
        assert!(matches!(
            inst.params.get("contrast"),
            Some(ParamValue::Float(v)) if (*v - 0.42).abs() < 1e-5
        ));
        assert!(matches!(
            inst.params.get("speed"),
            Some(ParamValue::Float(v)) if (*v - 2.5).abs() < 1e-5
        ));
        assert!(matches!(
            inst.params.get("scale"),
            Some(ParamValue::Float(v)) if (*v - 1.5).abs() < 1e-5
        ));
    }

    /// BUG-182 regression: a String param set directly on a node (the graph
    /// editor's param edit / file picker writes NODE params, not the card's
    /// `clip.string_params` map) must survive host string-param pushes whose
    /// map lacks the binding's key. The pre-fix behavior fell back to the
    /// binding's declared default for absent keys, so the card's empty
    /// `hdri_file` binding overwrote `node.hdri_source`'s `path` every frame.
    #[test]
    fn string_params_absent_key_does_not_clobber_node_level_value() {
        let json = include_str!("../../../assets/generator-presets/Text.json");
        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Text preset must load");
        let render_text = g
            .graph
            .handles()
            .find(|(h, _)| *h == "render_text")
            .map(|(_, id)| id)
            .expect("Text preset declares a node with handle `render_text`");

        // Construction seed: the def node carries no `text` param, so the
        // binding's declared default is planted.
        assert!(matches!(
            g.graph.get_node(render_text).unwrap().params.get("text"),
            Some(ParamValue::String(s)) if s.as_str() == "HELLO"
        ));

        // Direct node-level write — what SetGraphNodeParamCommand +
        // apply_inner_param_overrides produce for a graph-editor edit.
        g.graph
            .set_param(
                render_text,
                "text",
                ParamValue::String(std::sync::Arc::new("DIRECT".to_string())),
            )
            .expect("render_text declares `text`");

        // Neither a missing host map nor a map lacking the key may touch it.
        g.apply_string_params(None);
        let only_font: std::collections::BTreeMap<String, String> =
            [("fontFamily".to_string(), "Menlo".to_string())].into_iter().collect();
        g.apply_string_params(Some(&only_font));
        assert!(
            matches!(
                g.graph.get_node(render_text).unwrap().params.get("text"),
                Some(ParamValue::String(s)) if s.as_str() == "DIRECT"
            ),
            "absent host key must leave the node-level value alone"
        );
        // A present key in the same map DID write (only absent keys skip).
        assert!(matches!(
            g.graph.get_node(render_text).unwrap().params.get("fontFamily"),
            Some(ParamValue::String(s)) if s.as_str() == "Menlo"
        ));
    }

    /// The other half of BUG-182: an explicit host value must still win, land
    /// live, and not be reverted by later pushes that omit the key.
    #[test]
    fn string_params_explicit_host_value_wins_and_sticks() {
        let json = include_str!("../../../assets/generator-presets/Text.json");
        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("Text preset must load");
        let render_text = g
            .graph
            .handles()
            .find(|(h, _)| *h == "render_text")
            .map(|(_, id)| id)
            .expect("render_text handle");

        let host: std::collections::BTreeMap<String, String> =
            [("text".to_string(), "HOST".to_string())].into_iter().collect();
        g.apply_string_params(Some(&host));
        assert!(matches!(
            g.graph.get_node(render_text).unwrap().params.get("text"),
            Some(ParamValue::String(s)) if s.as_str() == "HOST"
        ));

        // A later push that omits the key leaves the host's value live
        // (sticky — defaults are a construction-time seed, not a per-frame
        // re-assertion).
        g.apply_string_params(None);
        assert!(matches!(
            g.graph.get_node(render_text).unwrap().params.get("text"),
            Some(ParamValue::String(s)) if s.as_str() == "HOST"
        ));
    }

    /// Construction seeding precedence (BUG-182): when the def node carries
    /// its OWN value for a string-bound param (a def-baked file path set
    /// directly on the node), that value must survive construction — the
    /// binding's declared default is only a fallback for params the def
    /// leaves unset.
    #[test]
    fn string_binding_construction_seed_respects_def_node_param_over_default() {
        use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
        let json = include_str!("../../../assets/generator-presets/Text.json");
        let mut def: EffectGraphDef =
            serde_json::from_str(json).expect("Text preset JSON must parse");
        let node_doc = def
            .nodes
            .iter_mut()
            .find(|n| n.node_id.as_str() == "render_text")
            .expect("render_text node doc");
        node_doc.params.insert(
            "text".to_string(),
            SerializedParamValue::String {
                value: "FROM_DEF".to_string(),
            },
        );

        let g = PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), None)
            .expect("Text preset with a def-baked `text` param must build");
        let render_text = g
            .graph
            .handles()
            .find(|(h, _)| *h == "render_text")
            .map(|(_, id)| id)
            .expect("render_text handle");

        assert!(
            matches!(
                g.graph.get_node(render_text).unwrap().params.get("text"),
                Some(ParamValue::String(s)) if s.as_str() == "FROM_DEF"
            ),
            "def node param must win over the binding's declared default (\"HELLO\")"
        );
        // A param the def does NOT set still gets the binding default.
        assert!(matches!(
            g.graph.get_node(render_text).unwrap().params.get("fontFamily"),
            Some(ParamValue::String(s)) if s.is_empty()
        ));
    }

    /// Regression for the OilyFluid "Speed slider snaps back" bug.
    /// `apply_inner_param_overrides` must clear the binding cache so the next
    /// `apply_param_values` re-asserts the bound card value over the def default.
    #[test]
    fn inner_param_overrides_re_assert_bound_card_values() {
        use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
        let json = include_str!("../../../assets/generator-presets/Plasma.json");
        let registry = PrimitiveRegistry::with_builtin();
        let mut g = PresetRuntime::from_json_str(json, &registry).expect("Plasma preset must load");
        let plasma_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "plasma")
            .map(|(_, id)| id)
            .expect("plasma handle");

        let card_values = manifest(&[
            ("pattern", 3.0),
            ("complexity", 0.75),
            ("contrast", 0.42),
            ("speed", 2.5),
            ("scale", 1.5),
            ("clip_trigger", 1.0),
        ]);
        g.apply_param_values(&card_values);
        assert!(matches!(
            g.graph.get_node(plasma_id).unwrap().params.get("speed"),
            Some(ParamValue::Float(v)) if (*v - 2.5).abs() < 1e-5
        ));

        let mut def: EffectGraphDef = serde_json::from_str(json).unwrap();
        for node in &mut def.nodes {
            if node.handle.as_deref() == Some("plasma") {
                node.params
                    .insert("speed".to_string(), SerializedParamValue::Float { value: 9.0 });
            }
        }
        g.apply_inner_param_overrides(&def);

        g.apply_param_values(&card_values);
        assert!(
            matches!(
                g.graph.get_node(plasma_id).unwrap().params.get("speed"),
                Some(ParamValue::Float(v)) if (*v - 2.5).abs() < 1e-5
            ),
            "bound Speed must re-assert its card value (2.5) over the def's baked 9.0; got {:?}",
            g.graph.get_node(plasma_id).unwrap().params.get("speed"),
        );
    }

    /// Generator mirror of the effect reshape proof: a `scale` on the card
    /// binding's `BindingDef` reshapes what the inner node sees.
    #[test]
    fn stock_generator_reshape_changes_inner_node() {
        let json = include_str!("../../../assets/generator-presets/Plasma.json");
        let registry = PrimitiveRegistry::with_builtin();

        let plasma_id = |g: &PresetRuntime| {
            g.graph
                .handles()
                .find(|(h, _)| *h == "plasma")
                .map(|(_, id)| id)
                .expect("plasma handle")
        };
        let values = manifest(&[
            ("pattern", 3.0),
            ("complexity", 0.75),
            ("contrast", 0.42),
            ("speed", 2.5),
            ("scale", 1.5),
            ("clip_trigger", 1.0),
        ]);

        let mut g0 = PresetRuntime::from_json_str(json, &registry).expect("load");
        g0.apply_param_values(&values);
        let id0 = plasma_id(&g0);
        assert!(matches!(
            g0.graph.get_node(id0).unwrap().params.get("complexity"),
            Some(ParamValue::Float(v)) if (*v - 0.75).abs() < 1e-5
        ));

        let mut def: manifold_core::effect_graph_def::EffectGraphDef =
            serde_json::from_str(json).expect("parse Plasma def");
        let meta = def
            .preset_metadata
            .as_mut()
            .expect("Plasma carries presetMetadata");
        meta.bindings
            .iter_mut()
            .find(|b| b.id == "complexity")
            .expect("complexity binding exists")
            .scale = 2.0;
        let reshaped_json = serde_json::to_string(&def).expect("serialize reshaped def");
        let mut g = PresetRuntime::from_json_str(&reshaped_json, &registry).expect("load");
        g.apply_param_values(&values);
        let id = plasma_id(&g);
        assert!(
            matches!(
                g.graph.get_node(id).unwrap().params.get("complexity"),
                Some(ParamValue::Float(v)) if (*v - 1.5).abs() < 1e-5
            ),
            "a ×2 reshape must scale plasma.complexity 0.75 -> 1.5, got {:?}",
            g.graph.get_node(id).unwrap().params.get("complexity"),
        );
        assert_eq!(
            values.get("complexity").unwrap().value,
            0.75,
            "the host manifest is never mutated"
        );
    }

    /// Regression for the on-stage FluidSim2D Curl bug: a binding's `scale`
    /// must fold into the inner-node param on the generator path.
    #[test]
    fn generator_binding_scale_folds_into_inner_param() {
        let json = r#"{
            "version": 1,
            "name": "ScaledBindingTest",
            "presetMetadata": {
                "id": "ScaledBindingTest",
                "displayName": "Scaled Binding Test",
                "category": "Generator",
                "oscPrefix": "scaledBindingTest",
                "params": [
                    { "id": "amt", "name": "Amount", "min": 0.0, "max": 10.0, "defaultValue": 0.0 }
                ],
                "bindings": [
                    { "id": "amt", "label": "Amount", "defaultValue": 0.0,
                      "target": { "kind": "handleNode", "handle": "so", "param": "offset" },
                      "scale": 0.5,
                      "convert": { "type": "Float" } }
                ]
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "handle": "uv" },
                { "id": 2, "typeId": "node.scale_offset_image", "handle": "so" },
                { "id": 3, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;

        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("scaled-binding test preset must load");
        let so_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "so")
            .map(|(_, id)| id)
            .expect("preset declares a `so` handle");

        g.apply_param_values(&manifest(&[("amt", 4.0)]));

        let inst = g.graph.get_node(so_id).unwrap();
        assert!(
            matches!(
                inst.params.get("offset"),
                Some(ParamValue::Float(v)) if (*v - 2.0).abs() < 1e-5
            ),
            "generator binding scale dropped: offset should be 4.0 * 0.5 = 2.0, got {:?}",
            inst.params.get("offset"),
        );
    }

    /// BUG-078 regression (fixed). Post-PARAM_STORAGE_BOUNDARIES-P2 (D4/D12),
    /// a calibration writes ONLY `PresetInstance.params[id].spec` — the graph's
    /// `preset_metadata.params` shadow is left stale until save (D12 derives
    /// it at serialize time, not before). A structural graph edit rebuilds
    /// the generator's `PresetRuntime` through EXACTLY this constructor
    /// (`registry.create_with_override` -> `PresetRuntime::from_def_with_device`;
    /// `from_def` here is the mock-backend equivalent).
    ///
    /// The fix threads the live per-instance `ParamManifest` into `from_def`,
    /// which overlays each param's reshape (range/curve/invert) from the
    /// manifest `spec` over the graph's shadow — so a post-calibration rebuild
    /// honors the fresh range. This test passes `Some(&values)` (the fresh
    /// manifest) and asserts the reshape follows it, not the stale shadow.
    ///
    /// The manifest built below stands in for what `EditParamMappingCommand`
    /// (`manifold-editing/src/commands/effects.rs`, `apply_to_manifest_spec`)
    /// actually writes into `PresetInstance.params["amt"].spec` on a real
    /// calibration: only `max` widens, 1.0 -> 2.0, curve stays Exponential so
    /// the note actually engages (`apply_card_reshape` only consults min/max
    /// when `invert || curve != Linear` — a min/max-only edit on an
    /// otherwise-identity binding can't be observed this way).
    #[test]
    fn generator_rebuild_reshape_honors_live_manifest_over_stale_shadow() {
        let json = r#"{
            "version": 1,
            "name": "StaleReshapeTest",
            "presetMetadata": {
                "id": "StaleReshapeTest",
                "displayName": "Stale Reshape Test",
                "category": "Generator",
                "oscPrefix": "staleReshapeTest",
                "params": [
                    { "id": "amt", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 0.0 }
                ],
                "bindings": [
                    { "id": "amt", "label": "Amount", "defaultValue": 0.0,
                      "target": { "kind": "handleNode", "handle": "so", "param": "offset" },
                      "convert": { "type": "Float" } }
                ]
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "handle": "uv" },
                { "id": 2, "typeId": "node.scale_offset_image", "handle": "so" },
                { "id": 3, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;

        // The def exactly as it sits in memory right after a calibration:
        // P2 writes ONLY the manifest, so this shadow still carries the
        // ORIGINAL (pre-calibration) range — with the curve engaged so
        // min/max actually enters the transform.
        let mut def: manifold_core::effect_graph_def::EffectGraphDef =
            serde_json::from_str(json).expect("parse StaleReshapeTest def");
        {
            let meta = def
                .preset_metadata
                .as_mut()
                .expect("StaleReshapeTest carries presetMetadata");
            let p = meta
                .params
                .iter_mut()
                .find(|p| p.id == "amt")
                .expect("amt param spec");
            p.curve = manifold_core::macro_bank::MacroCurve::Exponential;
            p.min = 0.0;
            p.max = 1.0; // STALE — pre-calibration range
        }

        // The freshly-calibrated manifest a rebuild SHOULD honor: same
        // curve, widened range 0..2 — exactly what `EditParamMappingCommand`
        // would have just written into `PresetInstance.params["amt"].spec`.
        let mut values = manifest(&[("amt", 1.0)]);
        {
            let p = values.get_mut("amt").expect("amt manifest entry");
            p.spec.curve = manifold_core::macro_bank::MacroCurve::Exponential;
            p.spec.min = 0.0;
            p.spec.max = 2.0; // FRESH — post-calibration
        }

        // This IS the production rebuild path (mock-backend form of
        // `PresetRuntime::from_def_with_device`). The fix threads the live
        // manifest as the reshape authority; the generator_renderer rebuild
        // path passes `layer.gen_params().params` here.
        let mut g = PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), Some(&values))
            .expect("StaleReshapeTest def loads");
        g.apply_param_values(&values);

        let so_id = g
            .graph
            .handles()
            .find(|(h, _)| *h == "so")
            .map(|(_, id)| id)
            .expect("preset declares a `so` handle");
        let offset = match g.graph.get_node(so_id).unwrap().params.get("offset") {
            Some(ParamValue::Float(v)) => *v,
            other => panic!("expected float, got {other:?}"),
        };

        // Post-fix behavior: amt=1.0 normalized against the FRESH 0..2 range
        // is 0.5 -> curved (Exponential, n^2) to 0.25 -> re-scaled to 0..2 ->
        // 0.5. The pre-fix (stale-shadow) output was 1.0 (normalized against
        // the STALE 0..1 range: 1.0 clamped to n=1.0, curved to 1.0, no
        // reshape at all). 0.5 proves the manifest's widened range won.
        assert!(
            (offset - 0.5).abs() < 1e-5,
            "a structural rebuild must resolve `amt`'s reshape from the live \
             manifest spec (min=0,max=2), not the graph's stale \
             `preset_metadata.params` shadow (min=0,max=1) — got {offset} \
             (1.0 would be the STALE 0..1 range's output)",
        );
    }

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn trivial_passthrough_generator_loads_and_executes() {
        let json = r#"{
            "version": 1,
            "name": "TestPassthrough",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "handle": "uv" },
                { "id": 2, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;

        let mut preset = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("trivial generator preset must load");
        assert_eq!(preset.type_id().as_str(), "TestPassthrough");
        preset.set_frame_context(FrameContextInputs {
            time: 1.5,
            beat: 0.5,
            aspect: 1.78,
            trigger_count: 4.0,
            anim_progress: 0.25,
            output_width: 1920.0,
            output_height: 1080.0,
        });
        preset.execute_frame(frame_time());
    }

    /// BUG per PARAM_TWO_WAY_BINDING_DESIGN.md P2 D5: a wired scalar input
    /// is resolved live, per-frame, via `EffectNodeContext::scalar_or_param`
    /// (wire first, param second) — it never writes back into
    /// `NodeInstance::params`. The old `live_node_params` read only the
    /// param map, so the editor's value inspector froze on a wire-driven
    /// scalar param while the render kept moving. `node.value` (a constant
    /// control source, `pure: true`) wired into
    /// `node.scale_offset_image`'s `scale` port — whose own `scale` param
    /// defaults to `1.0` and is never wired-through — is the minimal
    /// control-wire fixture that reproduces it.
    #[test]
    fn live_node_params_reports_wire_value_not_stale_param_default() {
        let json = r#"{
            "version": 1,
            "name": "TestWireTap",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "nodeId": "uv", "handle": "uv" },
                { "id": 2, "typeId": "node.value", "nodeId": "src", "handle": "src",
                  "params": { "value": { "type": "Float", "value": 0.75 } } },
                { "id": 3, "typeId": "node.scale_offset_image", "nodeId": "scaler", "handle": "scaler" },
                { "id": 4, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "scale" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;

        let mut g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("wire-tap fixture must load");
        let scaler_id = g
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("scaler"))
            .expect("fixture declares a `scaler` node");

        g.execute_frame(frame_time());

        // Sanity: the wire never writes NodeInstance::params — the param
        // map is still the primitive's declared default.
        assert!(
            matches!(
                g.graph.get_node(scaler_id).unwrap().params.get("scale"),
                Some(ParamValue::Float(v)) if (*v - 1.0).abs() < 1e-6
            ),
            "sanity: a wired scalar input must not write NodeInstance::params, got {:?}",
            g.graph.get_node(scaler_id).unwrap().params.get("scale"),
        );

        // The live tap must report the WIRE's value (0.75 from `src`), not
        // the stale param-map default (1.0).
        let scaler_node_id = g.graph.get_node(scaler_id).unwrap().node_id.clone();
        let live = g.live_node_params_watched();
        let scaler_values = live
            .iter()
            .find(|(id, _)| *id == scaler_node_id)
            .map(|(_, values)| values)
            .expect("scaler node reports live params");
        let scale_v = *scaler_values
            .iter()
            .find(|(name, _)| *name == "scale")
            .map(|(_, v)| v)
            .expect("scale is a declared param");
        assert!(
            (scale_v - 0.75).abs() < 1e-5,
            "live_node_params_watched should report the wire's live value \
             (0.75), not the stale param-map default (1.0); got {scale_v}"
        );
    }

    /// `PresetRuntime` holds a `Graph` which doesn't impl Debug, so we
    /// destructure the Result by hand rather than `expect_err`.
    fn unwrap_err(
        r: Result<PresetRuntime, JsonGeneratorLoadError>,
    ) -> JsonGeneratorLoadError {
        match r {
            Ok(_) => panic!("expected JsonGeneratorLoadError, got Ok(PresetRuntime)"),
            Err(e) => e,
        }
    }

    #[test]
    fn missing_generator_input_is_a_clean_error() {
        let json = r#"{
            "version": 1,
            "name": "Bad",
            "nodes": [ { "id": 0, "typeId": "system.final_output" } ],
            "wires": []
        }"#;
        let err = unwrap_err(PresetRuntime::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        ));
        assert!(
            matches!(err, JsonGeneratorLoadError::MissingGeneratorInput),
            "got {err:?}"
        );
    }

    #[test]
    fn infra_session_integration_smoke_test() {
        let json = r#"{
            "version": 2,
            "name": "InfraSmoke",
            "presetMetadata": {
                "id": "InfraSmoke",
                "displayName": "Infra Smoke",
                "category": "Diagnostic",
                "oscPrefix": "infra_smoke",
                "params": [],
                "bindings": []
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.wgsl_compute", "handle": "branch_a" },
                { "id": 2, "typeId": "node.wgsl_compute", "handle": "branch_b" },
                { "id": 3, "typeId": "node.switch_texture", "handle": "mux" },
                { "id": 4, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "trigger_count", "toNode": 3, "toPort": "selector" },
                { "fromNode": 1, "fromPort": "output_tex", "toNode": 3, "toPort": "in_0" },
                { "fromNode": 2, "fromPort": "output_tex", "toNode": 3, "toPort": "in_1" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;

        let preset = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("infra smoke preset must load");
        assert_eq!(preset.type_id().as_str(), "InfraSmoke");
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn bundled_strange_attractor_loads_and_compiles() {
        let device = crate::test_device();
        let json = include_str!("../../../assets/generator-presets/StrangeAttractor.json");
        let preset = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("bundled StrangeAttractor must load + compile");
        assert_eq!(preset.type_id().as_str(), "StrangeAttractor");
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn bundled_plasma_loads_and_compiles() {
        let device = crate::test_device();
        let json = include_str!("../../../assets/generator-presets/Plasma.json");
        let preset = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("bundled Plasma must load + compile");
        assert_eq!(preset.type_id().as_str(), "Plasma");
    }

    /// **I5** (`docs/CINEMATIC_POST_DESIGN.md`): the DoF chain (camera_lens ->
    /// render_scene[depth wired] -> coc_from_depth -> variable_blur H -> V)
    /// loads and compiles as ordinary preset JSON. CinematicScene was pulled
    /// from the bundled library 2026-07-16 (3D-infra test rig, not show
    /// content) and lives in `assets/reference-presets/`; the I5 gate keeps
    /// compiling it from there so the DoF-chain build check survives the
    /// unbundling (mirrors `bundled_plasma_loads_and_compiles` above).
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn bundled_cinematic_scene_loads_and_compiles() {
        let device = crate::test_device();
        let json = include_str!("../../../assets/reference-presets/CinematicScene.json");
        let preset = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("bundled CinematicScene must load + compile");
        assert_eq!(preset.type_id().as_str(), "CinematicScene");
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn resize_re_pre_allocates_array_buffers() {
        use crate::node_graph::{Backend, PortType};
        let device = crate::test_device();
        let json = include_str!("../../../assets/generator-presets/Lissajous.json");
        let mut g = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("Lissajous preset must load");

        let array_resources: Vec<ResourceId> = (0..g.plan.resource_count() as u32)
            .map(ResourceId)
            .filter(|id| matches!(g.plan.resource_type(*id), Some(PortType::Array(_))))
            .collect();
        assert!(
            !array_resources.is_empty(),
            "Lissajous preset must produce at least one Array<T> wire",
        );

        {
            let metal = g
                .executor
                .backend_mut()
                .as_any_mut()
                .and_then(|a| a.downcast_mut::<MetalBackend>())
                .expect("production path constructs a MetalBackend");
            for &res in &array_resources {
                let slot = metal
                    .slot_for(res)
                    .unwrap_or_else(|| panic!("Array resource {res:?} unbound after construction"));
                assert!(
                    Backend::array_buffer(metal, slot).is_some(),
                    "Array resource {res:?} has no backing buffer after construction",
                );
            }
        }

        g.resize(&device, 1280, 720);

        let metal = g
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("production path constructs a MetalBackend");
        for &res in &array_resources {
            let slot = metal
                .slot_for(res)
                .unwrap_or_else(|| panic!("Array resource {res:?} unbound after resize"));
            assert!(
                Backend::array_buffer(metal, slot).is_some(),
                "Array resource {res:?} has no backing buffer after resize",
            );
        }
    }

    /// Live project-resolution change must not kill a particle preset
    /// (Peter's report on Cymatics, 2026-07-16: "breaks when I change
    /// project resolution"). `resize()` wipes every pinned binding
    /// including Array<T> wires; a particle sim whose state rides those
    /// buffers (or whose re-seed never re-fires) comes back dead — black
    /// output, sand gone. This renders warm-up frames, resizes, renders
    /// again, and asserts the output still carries energy.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn cymatics_survives_live_resize() {
        use crate::preset_context::PresetContext;
        let device = crate::test_device();
        let json = include_str!("../../../assets/generator-presets/Cymatics.json");
        let registry = PrimitiveRegistry::with_builtin();
        let format = GpuTextureFormat::Rgba16Float;
        let (w0, h0) = (512u32, 512u32);
        let mut g = PresetRuntime::from_json_str_with_device(
            json, &registry, device.arc(), w0, h0, format, None,
        )
        .expect("Cymatics preset must load");

        let max_luma = |g: &mut PresetRuntime, w: u32, h: u32, frames: u32, base: u32| -> f32 {
            let target = RenderTarget::new(&device, w, h, format, "cymatics-resize-test");
            for f in 0..frames {
                let ctx = PresetContext {
                    time: (base + f) as f64 / 60.0,
                    beat: 0.0,
                    dt: 1.0 / 60.0,
                    width: w,
                    height: h,
                    output_width: w,
                    output_height: h,
                    aspect: w as f32 / h as f32,
                    owner_key: 0,
                    is_clip_level: false,
                    frame_count: i64::from(base + f),
                    anim_progress: 0.0,
                    trigger_count: 0,
                };
                let mut enc = device.create_encoder("cymatics-resize-frame");
                {
                    let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut enc, &device);
                    g.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }
            let bytes_per_row = w * 8;
            let buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut rb = device.create_encoder("cymatics-resize-readback");
            rb.copy_texture_to_buffer(&target.texture, &buf, w, h, bytes_per_row);
            rb.commit_and_wait_completed();
            let ptr = buf.mapped_ptr().expect("shared buffer mapped");
            let px: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            px.chunks(4)
                .map(|c| half::f16::from_bits(c[0]).to_f32())
                .fold(0.0f32, f32::max)
        };

        let before = max_luma(&mut g, w0, h0, 90, 0);
        assert!(
            before > 0.05,
            "Cymatics must render visible sand before resize (max luma {before})"
        );

        let (w1, h1) = (384u32, 640u32);
        g.resize(&device, w1, h1);

        let after = max_luma(&mut g, w1, h1, 90, 90);
        assert!(
            after > 0.05,
            "Cymatics must still render visible sand after a live resize \
             (max luma {after} — resize killed the particle state)"
        );
    }

    /// Same resize-survival contract for FluidSim2D — the tuned reference
    /// particle sim. Exists to prove (or refute) that the resize kill was
    /// a class bug across particle presets, not Cymatics-specific.
    ///
    /// Verdict 2026-07-16: it IS the class bug (max luma 0 after resize
    /// with the state-clear disabled) — but the b11e6511 state-clear that
    /// rescues Cymatics does NOT rescue FluidSim2D; its re-seed path never
    /// re-arms. Tracked as BUG-175 (docs/BUG_BACKLOG.md); un-ignore when
    /// fixing it — this test is the acceptance gate.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    #[ignore = "BUG-175: FluidSim2D stays black after live resize; reproducer kept as the fix's acceptance gate"]
    fn fluidsim2d_survives_live_resize() {
        use crate::preset_context::PresetContext;
        let device = crate::test_device();
        let json = include_str!("../../../assets/generator-presets/FluidSim2D.json");
        let registry = PrimitiveRegistry::with_builtin();
        let format = GpuTextureFormat::Rgba16Float;
        let (w0, h0) = (512u32, 512u32);
        let mut g = PresetRuntime::from_json_str_with_device(
            json, &registry, device.arc(), w0, h0, format, None,
        )
        .expect("FluidSim2D preset must load");

        let max_luma = |g: &mut PresetRuntime, w: u32, h: u32, frames: u32, base: u32| -> f32 {
            let target = RenderTarget::new(&device, w, h, format, "fluid-resize-test");
            for f in 0..frames {
                let ctx = PresetContext {
                    time: (base + f) as f64 / 60.0,
                    beat: 0.0,
                    dt: 1.0 / 60.0,
                    width: w,
                    height: h,
                    output_width: w,
                    output_height: h,
                    aspect: w as f32 / h as f32,
                    owner_key: 0,
                    is_clip_level: false,
                    frame_count: i64::from(base + f),
                    anim_progress: 0.0,
                    trigger_count: 0,
                };
                let mut enc = device.create_encoder("fluid-resize-frame");
                {
                    let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut enc, &device);
                    g.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }
            let bytes_per_row = w * 8;
            let buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut rb = device.create_encoder("fluid-resize-readback");
            rb.copy_texture_to_buffer(&target.texture, &buf, w, h, bytes_per_row);
            rb.commit_and_wait_completed();
            let ptr = buf.mapped_ptr().expect("shared buffer mapped");
            let px: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            px.chunks(4)
                .map(|c| half::f16::from_bits(c[0]).to_f32())
                .fold(0.0f32, f32::max)
        };

        let before = max_luma(&mut g, w0, h0, 90, 0);
        assert!(before > 0.05, "FluidSim2D must render before resize (max luma {before})");
        g.resize(&device, 384, 640);
        let after = max_luma(&mut g, 384, 640, 90, 90);
        assert!(
            after > 0.05,
            "FluidSim2D must still render after live resize (max luma {after})"
        );
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn aliased_array_io_routes_in_and_out_to_one_physical_slot() {
        use crate::node_graph::Backend;
        let device = crate::test_device();
        let json = include_str!("../../../assets/generator-presets/StrangeAttractor.json");
        let mut g = PresetRuntime::from_json_str_with_device(
            json,
            &PrimitiveRegistry::with_builtin(),
            device.arc(),
            1920,
            1080,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("StrangeAttractor preset must load");

        let find_node = |type_id: &str| -> NodeInstanceId {
            for step in g.plan.steps() {
                let inst = g.graph.get_node(step.node).expect("step's node");
                if inst.node.type_id().as_str() == type_id {
                    return step.node;
                }
            }
            panic!("node `{type_id}` not in compiled plan");
        };
        let integrate_node = find_node("node.wgsl_compute");
        let scatter_node = find_node("node.draw_particles");

        let resource_for = |node: NodeInstanceId, port: &str, is_input: bool| -> ResourceId {
            for step in g.plan.steps() {
                if step.node == node {
                    let ports = if is_input { &step.inputs } else { &step.outputs };
                    for &(name, id) in ports {
                        if name == port {
                            return id;
                        }
                    }
                }
            }
            panic!(
                "missing {} port `{port}` on node {node:?}",
                if is_input { "input" } else { "output" }
            );
        };

        let integrate_in_res = resource_for(integrate_node, "particles", true);
        let integrate_out_res = resource_for(integrate_node, "particles", false);
        let scatter_in_res = resource_for(scatter_node, "particles", true);

        let metal = g
            .executor
            .backend_mut()
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<MetalBackend>())
            .expect("production path constructs a MetalBackend");

        let in_slot = metal.slot_for(integrate_in_res).expect("integrate.in bound");
        let out_slot = metal.slot_for(integrate_out_res).expect("integrate.out bound");
        let scatter_slot = metal.slot_for(scatter_in_res).expect("scatter.particles bound");

        assert_eq!(in_slot, out_slot, "aliased_array_io in→out must share a slot");
        assert_eq!(
            out_slot, scatter_slot,
            "integrate.out and scatter.particles must resolve to the same slot",
        );
        assert!(
            Backend::array_buffer(metal, in_slot).is_some(),
            "the shared slot must back a real GpuBuffer",
        );
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn canvas_sized_array_outputs_scale_buffer_with_backend_canvas_dims() {
        use crate::node_graph::Backend;
        let device = crate::test_device();
        let json = include_str!("../../../assets/generator-presets/StrangeAttractor.json");

        let cases = [(1280u32, 720u32), (3840u32, 2160u32)];
        for (w, h) in cases {
            let mut g = PresetRuntime::from_json_str_with_device(
                json,
                &PrimitiveRegistry::with_builtin(),
                device.arc(),
                w,
                h,
                GpuTextureFormat::Rgba16Float,
                None,
            )
            .expect("preset must load");

            let scatter = (|| {
                for step in g.plan.steps() {
                    let inst = g.graph.get_node(step.node).expect("step's node");
                    if inst.node.type_id().as_str() == "node.draw_particles" {
                        return step.node;
                    }
                }
                panic!("scatter node missing");
            })();
            let accum_res = (|| {
                for step in g.plan.steps() {
                    if step.node == scatter {
                        for &(name, id) in &step.outputs {
                            if name == "accum" {
                                return id;
                            }
                        }
                    }
                }
                panic!("scatter.accum resource missing");
            })();

            let metal = g
                .executor
                .backend_mut()
                .as_any_mut()
                .and_then(|a| a.downcast_mut::<MetalBackend>())
                .expect("metal backend");
            let slot = metal.slot_for(accum_res).expect("scatter.accum unbound");
            let buf = Backend::array_buffer(metal, slot).expect("no backing buffer");
            let expected = (w as u64) * (h as u64) * 4;
            assert_eq!(
                buf.size, expected,
                "scatter.accum at canvas {w}x{h} should be {expected} bytes, got {}",
                buf.size,
            );
        }
    }

    #[test]
    fn bundled_trivial_passthrough_preset_loads_and_executes() {
        let json = include_str!("../../../assets/generator-presets/TrivialPassthrough.json");
        let mut preset = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("bundled TrivialPassthrough must load");
        assert_eq!(preset.type_id().as_str(), "TrivialPassthrough");
        preset.set_frame_context(FrameContextInputs {
            time: 0.0,
            beat: 0.0,
            aspect: 1.78,
            trigger_count: 0.0,
            anim_progress: 0.0,
            output_width: 1920.0,
            output_height: 1080.0,
        });
        preset.execute_frame(frame_time());
    }

    #[test]
    fn missing_final_output_is_a_clean_error() {
        let json = r#"{
            "version": 1,
            "name": "Bad",
            "nodes": [ { "id": 0, "typeId": "system.generator_input" } ],
            "wires": []
        }"#;
        let err = unwrap_err(PresetRuntime::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        ));
        assert!(
            matches!(err, JsonGeneratorLoadError::MissingFinalOutput),
            "got {err:?}"
        );
    }

    /// BUG-125: a generator JSON with TWO `system.final_output` nodes used to
    /// have its tracked output resolved via `.find()` over an unordered
    /// `AHashMap`, picking one nondeterministically per process and silently
    /// overwriting the loser's texture with the canvas format at render
    /// time. Rejected loudly at load instead.
    #[test]
    fn dual_final_output_is_rejected_at_load() {
        let json = r#"{
            "version": 1,
            "name": "Bad",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input" },
                { "id": 1, "typeId": "node.uv_field" },
                { "id": 2, "typeId": "system.final_output" },
                { "id": 3, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;
        let err = unwrap_err(PresetRuntime::from_json_str(
            json,
            &PrimitiveRegistry::with_builtin(),
        ));
        assert!(
            matches!(err, JsonGeneratorLoadError::MultipleFinalOutputs { count: 2 }),
            "got {err:?}"
        );
    }

    /// Node-output preview, grouped generator: selecting the collapsed
    /// `Flow Field` group resolves to the concrete producer of its `forceField`
    /// output. The group → producer map lives on the single segment now.
    #[test]
    fn grouped_generator_preview_resolves_group_to_producer() {
        let json = include_str!("../../../assets/generator-presets/FluidSim2D.json");
        let g = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("FluidSim2D preset must load");

        assert!(
            g.graph
                .instance_by_node_id(&manifold_core::NodeId::new("Flow Field"))
                .is_none(),
            "group container should have no runtime instance after flattening"
        );

        let seg = g.effect_nodes.first().expect("generator has one segment");
        let (producer, port) = seg
            .group_preview_map
            .iter()
            .find(|(group, _, _)| *group == manifold_core::NodeId::new("Flow Field"))
            .map(|(_, producer, port)| (producer.clone(), port.clone()))
            .expect("Flow Field group must be in the preview map");
        assert_eq!(
            producer,
            manifold_core::NodeId::new("field_blur_v"),
            "Flow Field's forceField output is produced by field_blur_v"
        );
        assert_eq!(port, "forceField", "the group's primary output port name");
        assert_eq!(
            crate::node_graph::PreviewEncoding::derive("node.gaussian_blur", &port),
            crate::node_graph::PreviewEncoding::VectorField,
        );
        assert!(
            g.graph.instance_by_node_id(&producer).is_some(),
            "the resolved producer must be a real runtime node"
        );
    }
