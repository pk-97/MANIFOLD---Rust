use manifold_node_engine::runtime::{PresetRuntime, ChainBuildInputs};
use manifold_node_engine::runtime::build::compute_topology_hash;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_core::{PresetTypeId, effects::PresetInstance};
#[cfg(feature = "gpu-proofs")]
    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }
#[cfg(feature = "gpu-proofs")]
    #[test]
    fn disabled_effects_are_excluded_from_active_set_and_change_hash() {
        // The user-facing invariant for the on/off toggle: setting
        // `enabled = false` MUST (a) flip the topology hash so the chain
        // rebuilds, and (b) exclude the effect from `active_effects` in
        // `try_build` so it stops rendering. Without these the toggle
        // appears to do nothing.
        let device = manifold_gpu::testkit::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let mut fx = make_default(PresetTypeId::MIRROR); // `amount` default = 1.0, so present in chain by default.
        assert!(fx.enabled, "PresetInstance::new defaults enabled = true");

        let hash_on = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        let cg_on = PresetRuntime::try_build(ChainBuildInputs { effects: &[fx.clone()], groups: &[], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: None }, None)
            .expect("Mirror chain builds at enabled = true");
        assert_eq!(
            cg_on.effect_slots_for_test().len(),
            1,
            "Mirror should contribute one effect slot when enabled",
        );

        fx.enabled = false;
        let hash_off = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        assert_ne!(
            hash_on, hash_off,
            "Toggling `enabled` MUST change the topology hash — otherwise the \
             chain caches the previous topology and the toggle appears dead.",
        );

        // With this as the only effect, the chain should refuse to build
        // (no active effects → None) — equivalent to "the chain becomes empty".
        let cg_off = PresetRuntime::try_build(ChainBuildInputs { effects: &[fx], groups: &[], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: None }, None);
        assert!(
            cg_off.is_none(),
            "Disabled effect must be filtered out of active_effects — got a chain with effects when it should be empty",
        );
    }

#[cfg(feature = "gpu-proofs")]
/// `docs/DEPTH_RELIGHT_DESIGN.md` P5, full loop: flip
    /// `PresetInstance::relight` and rebuild the SAME production path
    /// (`try_build` → `compute_topology_hash`) real `EditingService`
    /// commands drive — `manifold-editing`'s
    /// `toggle_relight_undo_roundtrip` (command_roundtrips.rs) proves the
    /// command correctly flips this same field through undo/redo;
    /// `manifold-renderer` can't depend on `manifold-editing` (crate-graph
    /// direction), so this half of the loop proves the OTHER end: the
    /// renderer reads that field, mints deterministic `rl_`-prefixed nodes
    /// when it's on, and the topology hash changes so a toggle actually
    /// rebuilds — then removes them cleanly when toggled back off.
    #[test]
    fn toggling_relight_adds_and_removes_rl_nodes_on_rebuild() {
        // Relight disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // `relight_active()` is false so no template is spliced. The augment
        // machinery itself stays covered by `node_graph::relight`'s ungated tests.
        if !manifold_foundation::RELIGHT_FEATURE_ENABLED {
            return;
        }
        let device = manifold_gpu::testkit::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let lambert_id = manifold_core::NodeId::new("rl_lambert");

        let mut fx = make_default(PresetTypeId::MIRROR);
        let hash_off = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        let cg_off = PresetRuntime::try_build(ChainBuildInputs { effects: &[fx.clone()], groups: &[], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: None }, None)
            .expect("Mirror chain builds with relight off");
        assert!(
            cg_off.graph.instance_by_node_id(&lambert_id).is_none(),
            "relight off must NOT contain the rl_lambert template node",
        );

        fx.relight = true;
        let hash_on = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        assert_ne!(
            hash_off, hash_on,
            "toggling relight MUST change the topology hash — otherwise the \
             chain never rebuilds and the toggle appears dead.",
        );
        // D8/P7: a relight-on card fuses, so `rl_lambert` lives inside the
        // fused kernel rather than as a standalone node. Force the unfused
        // (watched-editor) path to observe the spliced template node directly.
        let cg_on_unfused = PresetRuntime::try_build(ChainBuildInputs { effects: &[fx.clone()], groups: &[], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: Some(&fx.id) }, None)
        .expect("Mirror chain builds with relight on (watched / unfused)");
        assert!(
            cg_on_unfused.graph.instance_by_node_id(&lambert_id).is_some(),
            "relight on must splice the rl_lambert template node into the built chain",
        );

        // Toggle back off: the rebuilt chain must lose the template again —
        // proves this isn't a one-way sticky augmentation.
        fx.relight = false;
        let cg_off_again =
            PresetRuntime::try_build(ChainBuildInputs { effects: &[fx], groups: &[], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: None }, None)
                .expect("Mirror chain builds with relight off again");
        assert!(
            cg_off_again.graph.instance_by_node_id(&lambert_id).is_none(),
            "toggling relight back off must remove the rl_ template nodes on rebuild",
        );
    }
