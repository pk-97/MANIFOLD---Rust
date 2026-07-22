    //! The chain runner accumulates structured errors during build
    //! and per-frame run. Each entry carries the effect's identity
    //! so a future editor surface can attach it to the right card.
    //!
    //! Today the immediate user-visible benefit is the consistent
    //! `[chain-error]` terminal log; tomorrow these are the data
    //! the editor reads via [`PresetRuntime::errors`]. The tests below
    //! pin one variant from the per-build path so the surface
    //! doesn't silently regress.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::{PresetInstance, ParamConvert, UserParamBinding};

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    /// A user-exposed binding pointing at a handle the splice didn't
    /// register surfaces as a structured `UserBindingResolveFailed`
    /// entry on the chain's error log. Pre-change: this was a bare
    /// `eprintln!` with no programmatic surface — the editor couldn't
    /// highlight the broken slider.
    #[test]
    fn unresolved_user_binding_surfaces_as_structured_chain_error() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let mut fx = make_default(PresetTypeId::INVERT_COLORS);
        // Reference a handle that the canonical Invert splice does
        // NOT register. Resolution fails at build time → records a
        // UserBindingResolveFailed error and the slider stays inert.
        fx.append_user_binding(UserParamBinding {
            id: "user.broken.1".to_string(),
            label: "Broken".to_string(),
            node_id: NodeId::new("does_not_exist"),
            legacy_node_handle: None,
            inner_param: "amount".to_string(),
            min: 0.0,
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

        let cg = PresetRuntime::try_build(&[fx.clone()], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("Invert chain still builds; the binding just fails to resolve");

        let errors = cg.errors();
        let matching = errors.iter().find(|e| {
            matches!(
                e,
                ChainError::UserBindingResolveFailed {
                    binding_id,
                    node_id,
                    rehydrate: false,
                    ..
                } if binding_id == "user.broken.1" && node_id == "does_not_exist"
            )
        });
        assert!(
            matching.is_some(),
            "expected a UserBindingResolveFailed entry naming the broken binding; \
             got {errors:?}",
        );
    }

    /// Sanity: a chain whose effects all resolve cleanly has an
    /// empty error log. Paired with the negative test so a
    /// regression that always-records or always-reads-empty
    /// surfaces visibly.
    #[test]
    fn clean_chain_has_no_errors() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = make_default(PresetTypeId::INVERT_COLORS);

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("clean Invert chain builds");

        assert!(
            cg.errors().is_empty(),
            "clean chain must have no structured errors; got {:?}",
            cg.errors()
        );
    }
