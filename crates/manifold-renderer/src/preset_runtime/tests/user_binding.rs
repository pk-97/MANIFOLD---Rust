    //! Regression: a user-exposed inner-graph parameter must actually
    //! propagate its outer slot value to the inner node every frame.
    //!
    //! Pre-unification: the chain's per-frame apply called
    //! `apply_param_bindings(static, &[], …)`, so exposing a param via
    //! the graph editor produced a visible effect-card slider that
    //! silently wrote into a discarded list. The user-visible symptom:
    //! setting `Transform.rotation = 0.48` directly in the graph
    //! editor rotated the image, but exposing the same param on the
    //! Mirror card and dragging its slider to 0.48 did nothing.
    //!
    //! After the bindings unification (Phase 1) the runtime walks a
    //! single `slot.bindings: Vec<ResolvedBinding>` — the `&[]` bug
    //! class is structurally unrepresentable.
    use super::*;
    use crate::node_graph::ParamValue;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::{
        PresetInstance, UserParamBinding, ParamConvert,
    };


    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    /// Set an existing manifest param's live + base value by id, marking it
    /// exposed — the id-keyed replacement for the old positional
    /// `fx.param_values[i] = ParamSlot::exposed(v)` write.
    fn set_slot(fx: &mut PresetInstance, id: &str, value: f32) {
        let p = fx
            .params
            .get_mut(id)
            .unwrap_or_else(|| panic!("param `{id}` exists in the manifest"));
        p.value = value;
        p.base = value;
        p.exposed = true;
    }

    /// Clone the canonical preset def for `ty` and set a non-identity
    /// `scale` on the named card binding's [`BindingDef`] — the post-note
    /// home for a per-instance reshape (the deleted `ParamMapping` note's
    /// scale folded onto the binding spec). Returns the divergent def for
    /// the caller to hang on `fx.graph`.
    fn def_with_binding_scale(
        ty: PresetTypeId,
        binding_id: &str,
        scale: f32,
    ) -> manifold_core::effect_graph_def::EffectGraphDef {
        let mut def = (*loaded_preset_view_by_id(&ty)
            .expect("preset view exists for type")
            .canonical_def)
            .clone();
        let meta = def
            .preset_metadata
            .as_mut()
            .expect("preset carries presetMetadata");
        let binding = meta
            .bindings
            .iter_mut()
            .find(|b| b.id == binding_id)
            .expect("named card binding exists");
        binding.scale = scale;
        def
    }

    fn affine_scale(cg: &PresetRuntime, slot: &EffectSlot) -> ParamValue {
        let (_, affine_id) = slot
            .handles
            .iter()
            .find(|(h, _)| h.as_ref() == "affine")
            .expect("StylizedFeedback graph registers `affine` handle");
        cg.graph
            .get_node(*affine_id)
            .and_then(|n| n.params.get("scale").cloned())
            .expect("affine_transform exposes a `scale` param")
    }

    /// Core model proof: a per-instance reshape (now a `scale` on the
    /// card binding's [`BindingDef`] in the instance's own graph, after
    /// the `ParamMapping` note was deleted) reshapes what the inner node
    /// sees (`zoom` → `affine.scale`), while the param's VALUE SLOT stays
    /// byte-identical — the load-bearing invariant for the live rig
    /// (Ableton / drivers / OSC / envelopes write that slot, untouched).
    #[test]
    fn stock_param_reshape_changes_inner_node_without_touching_the_slot() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        // Mirror the per-frame apply `run()` performs: push the live
        // `params` manifest through the slot's bindings into the graph.
        fn apply(cg: &mut PresetRuntime, values: &ParamManifest) {
            let slot = &mut cg.effect_nodes[0];
            slot.bound.apply(&mut cg.graph, values);
        }

        // Control: same effect, zoom = 0.3, identity binding → inner sees 0.3.
        let mut control = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        set_slot(&mut control, "zoom", 0.3);
        // Build unfused (pass the effect as the watched preview) so the inner
        // affine node survives for inspection — region fusion would otherwise
        // fold it into a single kernel and the handle would vanish.
        let mut cg0 =
            PresetRuntime::try_build(std::slice::from_ref(&control), &[], &primitives, &device, None, 256, 256, Some(&control.id), None)
                .expect("control chain builds");
        apply(&mut cg0, &control.params);
        let slot0 = &cg0.effect_nodes[0];
        assert_eq!(
            affine_scale(&cg0, slot0),
            ParamValue::Float(0.3),
            "with an identity binding, the stock zoom slot value passes straight through",
        );

        // With a ×2 reshape on the `zoom` binding: inner sees 0.6, slot
        // still reads 0.3.
        let mut fx = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        set_slot(&mut fx, "zoom", 0.3);
        fx.graph = Some(def_with_binding_scale(
            PresetTypeId::STYLIZED_FEEDBACK,
            "zoom",
            2.0,
        ));
        fx.graph_version = fx.graph_version.wrapping_add(1);
        fx.graph_structure_version = fx.graph_structure_version.wrapping_add(1);
        let mut cg =
            PresetRuntime::try_build(std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256, Some(&fx.id), None)
                .expect("reshaped chain builds");
        apply(&mut cg, &fx.params);
        let slot = &cg.effect_nodes[0];
        assert_eq!(
            affine_scale(&cg, slot),
            ParamValue::Float(0.6),
            "a ×2 reshape must scale what the inner node sees (0.3 → 0.6)",
        );
        // The invariant: the value slot the modulation surface writes is
        // byte-identical with and without the reshape.
        assert_eq!(
            fx.params.get("zoom").unwrap().value,
            0.3,
            "the reshape must NEVER rewrite the value slot — that slot \
             is what Ableton / drivers / OSC / envelopes address every frame",
        );
    }

    fn stylized_with_translate_exposed(translate_value: f32) -> PresetInstance {
        let mut fx = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        // StylizedFeedback's graph registers an affine_transform under
        // the handle `"affine"`. Its static card exposes gain / scale /
        // rotation, but NOT `translate_x` — so a user-tail binding to
        // `affine.translate_x` is the sole writer of that inner param.
        fx.append_user_binding(UserParamBinding {
            id: "user.affine.translate_x.1".to_string(),
            label: "Translate X".to_string(),
            node_id: NodeId::new("affine"),
            legacy_node_handle: None,
            inner_param: "translate_x".to_string(),
            min: -1.0,
            max: 1.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        });
        // Drag the user-tail slider to `translate_value`. With static
        // count = 3 (amount, zoom, rotate) the user binding is the 4th
        // manifest entry, keyed by its binding id.
        assert_eq!(
            fx.params.len(),
            4,
            "StylizedFeedback with 3 static + 1 user-tail = 4 param slots",
        );
        set_slot(&mut fx, "user.affine.translate_x.1", translate_value);
        fx
    }

    /// Build-time hydrate: the chain's unified
    /// `EffectSlot.bindings` must include one entry per
    /// `fx.user_param_bindings` after the static prefix, each resolved
    /// to the correct inner node + param.
    #[test]
    fn build_time_hydrate_resolves_user_binding_to_inner_node() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = stylized_with_translate_exposed(0.48);

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("StylizedFeedback chain with one user binding builds");

        let slot = cg
            .effect_nodes
            .first()
            .expect("StylizedFeedback contributes one effect slot");
        // `EffectSlot` no longer stores a static count; the static prefix is
        // the run of `BindingSource::Static` entries at the head of the
        // unified bindings list.
        let n_static = slot
            .bound
            .bindings
            .iter()
            .filter(|b| matches!(b.source, crate::node_graph::BindingSource::Static))
            .count();
        assert_eq!(
            slot.bound.bindings.len(),
            n_static + 1,
            "user-tail binding for affine.translate_x must hydrate at build time",
        );
        let user_rb = &slot.bound.bindings[n_static];
        assert_eq!(user_rb.source, crate::node_graph::BindingSource::User);
        match &user_rb.target {
            crate::node_graph::ResolvedTarget::Node { param, .. } => {
                assert_eq!(*param, "translate_x");
            }
            _ => panic!("user binding must resolve to a Node target"),
        }
    }

    /// Per-frame apply: after build, calling `apply_bindings` with
    /// the chain's stored unified binding list must write the
    /// user-tail param value to the inner Transform node.
    #[test]
    fn exposed_slider_value_reaches_inner_node() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = stylized_with_translate_exposed(0.48);

        let mut cg = PresetRuntime::try_build(
            std::slice::from_ref(&fx),
            &[],
            &primitives,
            &device,
            None,
            256,
            256,
            None,
            None,
        )
        .expect("StylizedFeedback chain with one user binding builds");

        // Mirror the per-frame apply that `run()` would execute:
        // walk the slot's unified bindings against fx.params.
        let slot = &mut cg.effect_nodes[0];
        slot.bound.apply(&mut cg.graph, &fx.params);

        // Inspect the inner affine node's `translate_x` param — it
        // must reflect the user-tail slot's value, not its primitive
        // default of 0.0.
        let (_, xform_id) = slot
            .handles
            .iter()
            .find(|(h, _)| h.as_ref() == "affine")
            .expect("StylizedFeedback graph registers `affine` handle");
        let translate_x = cg
            .graph
            .get_node(*xform_id)
            .and_then(|n| n.params.get("translate_x").cloned())
            .expect("affine_transform exposes a `translate_x` param");

        assert_eq!(
            translate_x,
            ParamValue::Float(0.48),
            "exposed user-binding slider must propagate to the inner \
             affine.translate_x param. If this is `Float(0.0)`, the \
             per-frame apply walked the wrong slice — the regression \
             that motivated this fix.",
        );
    }

    /// Symmetric default-seed regression for user bindings — mirror
    /// of `binding_seed_tests::soft_focus_inner_blur_starts_at_binding_default_not_primitive_default`
    /// for the user tier.
    ///
    /// Builds a StylizedFeedback chain whose user-exposed
    /// `affine.translate_x` binding declares `default_value = 0.42`,
    /// and asserts that the inner affine node's `translate_x` param
    /// starts at `0.42` (the binding default) rather than `0.0` (the
    /// affine_transform primitive's `ParamDef::default`). Catches the
    /// latent "user binding default not seeded" bug: without the
    /// unified `apply_binding_defaults` walk covering the user tail,
    /// exposed sliders would have to be "touched" to push their
    /// declared default through.
    #[test]
    fn user_binding_with_nonzero_default_seeds_inner_at_build_time() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let mut fx = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        fx.append_user_binding(UserParamBinding {
            id: "user.affine.translate_x.1".to_string(),
            label: "Translate X".to_string(),
            node_id: NodeId::new("affine"),
            legacy_node_handle: None,
            inner_param: "translate_x".to_string(),
            min: -1.0,
            max: 1.0,
            default_value: 0.42,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        });
        // Leave the outer slot at its declared default so the test
        // depends on the seed pass, not on the apply-with-divergent-
        // value path.
        assert_eq!(fx.params.len(), 4);
        set_slot(&mut fx, "user.affine.translate_x.1", 0.42);

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("StylizedFeedback chain with one user binding builds");
        let slot = cg
            .effect_nodes
            .first()
            .expect("StylizedFeedback contributes one effect slot");
        let (_, xform_id) = slot
            .handles
            .iter()
            .find(|(h, _)| h.as_ref() == "affine")
            .expect("StylizedFeedback graph registers `affine` handle");
        let translate_x = cg
            .graph
            .get_node(*xform_id)
            .and_then(|n| n.params.get("translate_x").cloned())
            .expect("affine_transform exposes a `translate_x` param");
        assert_eq!(
            translate_x,
            ParamValue::Float(0.42),
            "user-binding default seed must plant 0.42 into affine.translate_x \
             at build time. If this is Float(0.0), the unified \
             apply_binding_defaults walk regressed and exposed sliders \
             will need to be 'touched' before they take their declared default.",
        );
    }
