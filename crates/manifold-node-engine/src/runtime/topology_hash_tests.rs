    //! Regression: the topology hash must include only structure-
    //! affecting fields (`enabled`, `graph_structure_version`, relight
    //! toggle, etc.). Per-frame param values, including `amount`, must
    //! NOT affect the hash — otherwise live modulation rebuilds the
    //! chain and wipes primitive state.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;



    // Hash mechanics need a mutable parameter manifest, not a catalog preset.
    fn hash_fixture(ty: PresetTypeId) -> PresetInstance {
        let mut instance = PresetInstance::new(ty);
        let spec = serde_json::from_value(serde_json::json!({
            "id": "amount", "name": "Amount", "min": 0.0, "max": 1.0,
            "defaultValue": 1.0
        })).expect("hash fixture parameter spec");
        instance.params = manifold_core::params::ParamManifest::from_params(vec![
            manifold_core::params::Param::bundled(spec)
        ]);
        instance
    }

    #[test]
    fn hash_changes_when_effect_becomes_the_watched_preview_target() {
        // Opening the graph editor on an effect must rebuild the chain holding
        // it so it flips fused → unfused (per-node preview + live edits). The
        // gate at `should_render_fused` only re-runs on rebuild, so the watched
        // flag has to move the topology hash. Membership-local: a `preview_effect`
        // that isn't in the chain leaves the hash unchanged (no churn elsewhere).
        let fx = hash_fixture(PresetTypeId::COLOR_GRADE);
        let other = hash_fixture(PresetTypeId::VORONOI_PRISM);

        let unwatched = compute_topology_hash(std::slice::from_ref(&fx), &[], 256, 256, None);
        let watched =
            compute_topology_hash(std::slice::from_ref(&fx), &[], 256, 256, Some(&fx.id));
        assert_ne!(
            unwatched, watched,
            "topology hash must change when an effect becomes the watched target \
             — otherwise opening its editor never rebuilds it unfused.",
        );

        // A watch on an effect NOT in this chain must not perturb the hash.
        let watch_elsewhere =
            compute_topology_hash(std::slice::from_ref(&fx), &[], 256, 256, Some(&other.id));
        assert_eq!(
            unwatched, watch_elsewhere,
            "watching an effect absent from this chain must leave its hash \
             unchanged — unrelated chains must not churn when the editor opens.",
        );
    }

    #[test]
    fn amount_sweep_does_not_change_topology_hash() {
        // `amount` is a performance control, not structure. Dragging it
        // through zero must NOT rebuild the chain, so stateful effects
        // (Feedback, Watercolor, Bloom, ...) keep their accumulated
        // state across the bypass moment. The only structural skip is
        // `PresetInstance.enabled`.
        let mut fx = hash_fixture(PresetTypeId::VORONOI_PRISM);
        assert!(fx.set_base_param("amount", 0.0));

        let hash_at_zero = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);

        assert!(fx.set_base_param("amount", 0.5));
        let hash_at_half = compute_topology_hash(&[fx], &[], 256, 256, None);

        assert_eq!(
            hash_at_zero, hash_at_half,
            "amount is a value, not structure: sweeping through zero must not change the topology hash"
        );
    }



    /// D8/P7: float relight knobs are live uniforms, so dragging them must NOT
    /// change the topology hash (no chain rebuild). `height_from` changes
    /// template topology and legitimately rebuilds.
    #[test]
    fn relight_float_knobs_do_not_change_topology_hash() {
        let mut fx = hash_fixture(PresetTypeId::MIRROR);
        fx.relight = true;
        let base = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);

        fx.relight_params.light_x += 0.1;
        fx.relight_params.light_y += 0.1;
        fx.relight_params.relief += 0.1;
        fx.relight_params.ao_intensity += 0.1;
        fx.relight_params.shadow_softness += 0.1;
        fx.relight_params.gain += 0.1;
        let knobs_moved = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        assert_eq!(
            base, knobs_moved,
            "float relight knob drags must not change the topology hash",
        );

        fx.relight_params.height_from = manifold_core::effects::RelightHeightFrom::Luminance;
        let height_from_changed = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        if manifold_foundation::RELIGHT_FEATURE_ENABLED {
            assert_ne!(
                base, height_from_changed,
                "height_from changes template topology and must rebuild",
            );
        } else {
            // Feature disabled app-wide: `relight_active()` is false, so the
            // relight template is never spliced and no relight field — knob or
            // height_from — touches the topology hash.
            assert_eq!(
                base, height_from_changed,
                "with the relight feature disabled, height_from must not affect the hash",
            );
        }
    }

    #[test]
    fn value_edit_keeps_hash_but_structure_edit_changes_it() {
        // The core of the "don't reset state on every edit" fix: a value- or
        // position-only graph edit bumps `graph_version` (for the UI snapshot)
        // but NOT `graph_structure_version`, so the topology hash is unchanged
        // and the chain is NOT rebuilt (state preserved). Only a structural
        // edit moves the hash.
        let mut fx = hash_fixture(PresetTypeId::MIRROR);
        let base = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);

        // Value / position edit: snapshot version moves, structure doesn't.
        fx.graph_version = fx.graph_version.wrapping_add(1);
        assert_eq!(
            base,
            compute_topology_hash(&[fx.clone()], &[], 256, 256, None),
            "a value/position edit must NOT change the topology hash (no rebuild, \
             state preserved)",
        );

        // Structural edit: structure version moves → hash changes → rebuild.
        fx.graph_structure_version = fx.graph_structure_version.wrapping_add(1);
        assert_ne!(
            base,
            compute_topology_hash(&[fx], &[], 256, 256, None),
            "a structural edit MUST change the topology hash so the chain rebuilds",
        );
    }
