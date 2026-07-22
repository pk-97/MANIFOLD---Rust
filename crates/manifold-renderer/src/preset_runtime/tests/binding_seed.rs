    //! Regression: a freshly-built chain must plant each binding's
    //! declared `default_value` into its inner-node target. Otherwise
    //! the per-frame skip cache lies about what's been written and the
    //! card has to be "touched" to push the correct value through —
    //! see [`apply_binding_defaults`].
    use super::*;
    use crate::node_graph::ParamValue;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;
    

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    /// SoftFocus is the canonical reproducer: its outer `radius`
    /// binding default is `6.0`, but the underlying `Blur` primitive's
    /// `ParamDef::default` is `4.0`. Without the seed pass, the inner
    /// node starts at `4.0` and the user has to touch the slider for
    /// the cache compare to diverge and the binding to actually write.
    #[test]
    fn soft_focus_inner_blur_starts_at_binding_default_not_primitive_default() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = make_default(PresetTypeId::SOFT_FOCUS_GRAPH);

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("SoftFocus chain should build");

        let slot = cg
            .effect_nodes
            .first()
            .expect("SoftFocus contributes one effect slot");
        let (_, blur_id) = slot
            .handles
            .iter()
            .find(|(h, _)| h.as_ref() == "blur")
            .expect("SoftFocus splice registers a `blur` handle");
        let blur = cg
            .graph
            .get_node(*blur_id)
            .expect("blur node id resolves on the freshly-built graph");
        let radius = blur
            .params
            .get("radius")
            .cloned()
            .expect("Blur primitive exposes `radius` param");

        assert_eq!(
            radius,
            ParamValue::Float(6.0),
            "Blur.radius must start at the SoftFocus binding default (6.0), \
             not the Blur primitive default (4.0). If it's 4.0 the binding-default \
             seed pass regressed and effect cards will need to be 'touched' \
             before they take their settings."
        );
    }
