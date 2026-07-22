    //! PARAM_MANIFEST_GATE_DESIGN.md P1, INV-1: a provisional manifest
    //! (built against an incomplete registry, `pending_wire` still `Some`)
    //! must never reach `PresetRuntime::try_build` silently.
    use manifold_core::effects::PresetInstance;

    /// A bare `PresetInstance` deserialize referencing an effect type that
    /// isn't registered anywhere, with a params map — the keep-don't-drop
    /// path (BUG-036) seeds a placeholder-spec param and leaves
    /// `pending_wire` `Some` because the template never resolved. No
    /// `Project`/loader machinery needed: this is the direct, minimal
    /// repro for "manifest built provisionally, reconcile never ran".
    fn provisional_instance() -> PresetInstance {
        let json = r#"{
            "id": "bug080_test_instance",
            "effectType": "Bug080UnregisteredType",
            "params": { "foo": { "value": 0.5 } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).expect("deserialize test fixture");
        assert!(
            fx.manifest_provisional(),
            "fixture must be provisional (unregistered effect type, wire stash present)"
        );
        fx
    }

    #[test]
    fn bug080_provisional_manifest_asserts_at_chain_build() {
        let fx = provisional_instance();
        let result = std::panic::catch_unwind(|| super::assert_manifest_gate(&fx));
        assert!(
            result.is_err(),
            "assert_manifest_gate must panic (via debug_assert!) when handed a \
             provisional manifest — a load/ingest path skipped reconcile_param_manifests()"
        );
    }

    #[test]
    fn bug080_loader_path_never_provisional() {
        // A freshly-constructed, template-resolved instance (the shape every
        // instance is in once `PresetInstance::reconcile_manifest` — and thus
        // the loader — has actually run against a known template) must never
        // trip the gate.
        let fx = manifold_core::preset_definition_registry::create_default(
            &manifold_core::PresetTypeId::COLOR_GRADE,
        );
        assert!(
            !fx.manifest_provisional(),
            "a template-resolved instance must never be provisional"
        );
        // Must not panic.
        super::assert_manifest_gate(&fx);
    }
