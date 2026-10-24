use manifold_core::{PresetTypeId, effects::PresetInstance};
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::build::compute_topology_hash;
use manifold_node_engine::runtime::{ChainBuildInputs, PresetRuntime};
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
    let cg_on = PresetRuntime::try_build(
        ChainBuildInputs {
            effects: &[fx.clone()],
            groups: &[],
            primitives: &primitives,
            device: &device,
            pool: None,
            width: 256,
            height: 256,
            preview_effect: None,
        },
        None,
    )
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
    let cg_off = PresetRuntime::try_build(
        ChainBuildInputs {
            effects: &[fx],
            groups: &[],
            primitives: &primitives,
            device: &device,
            pool: None,
            width: 256,
            height: 256,
            preview_effect: None,
        },
        None,
    );
    assert!(
        cg_off.is_none(),
        "Disabled effect must be filtered out of active_effects — got a chain with effects when it should be empty",
    );
}
