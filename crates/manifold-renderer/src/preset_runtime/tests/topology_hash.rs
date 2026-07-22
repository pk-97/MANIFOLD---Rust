    //! Regression: the topology hash must include each effect's
    //! current `is_skipped` state. Without it, dragging an
    //! `amount` slider away from 0 doesn't trigger a chain rebuild,
    //! so a freshly-added effect (which starts at `amount = 0` for
    //! most types) never enters the graph until the user toggles
    //! `enabled` — visible as the "add effect → must toggle to
    //! work" symptom.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    #[test]
    fn hash_changes_when_effect_becomes_the_watched_preview_target() {
        // Opening the graph editor on an effect must rebuild the chain holding
        // it so it flips fused → unfused (per-node preview + live edits). The
        // gate at `should_render_fused` only re-runs on rebuild, so the watched
        // flag has to move the topology hash. Membership-local: a `preview_effect`
        // that isn't in the chain leaves the hash unchanged (no churn elsewhere).
        let fx = make_default(PresetTypeId::COLOR_GRADE);
        let other = make_default(PresetTypeId::VORONOI_PRISM);

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
    fn hash_changes_when_skip_predicate_flips() {
        // Dragging an effect's `amount` slider across 0 must change
        // the topology hash so the chain rebuilds — without that, the
        // effect can't transition between "in graph" and "skipped"
        // states without a separate enabled toggle.
        //
        // Set up the test scenario explicitly: amount=0 first, then
        // amount=0.5. The §9.1.5 audit moved most effects' default
        // amount off zero, so we can't rely on the default for this
        // fixture.
        let mut fx = make_default(PresetTypeId::VORONOI_PRISM);
        fx.set_base_param("amount", 0.0);

        let hash_at_zero = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);

        fx.set_base_param("amount", 0.5);
        let hash_at_half = compute_topology_hash(&[fx], &[], 256, 256, None);

        assert_ne!(
            hash_at_zero, hash_at_half,
            "topology hash must change when an effect's SkipMode::OnZero \
             predicate flips — otherwise the chain doesn't rebuild and \
             the user has to toggle enabled to bring the effect into the \
             graph. See the doc-comment on `compute_topology_hash`."
        );
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn disabled_effects_are_excluded_from_active_set_and_change_hash() {
        // The user-facing invariant for the on/off toggle: setting
        // `enabled = false` MUST (a) flip the topology hash so the chain
        // rebuilds, and (b) exclude the effect from `active_effects` in
        // `try_build` so it stops rendering. Without these the toggle
        // appears to do nothing.
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let mut fx = make_default(PresetTypeId::MIRROR); // `amount` default = 1.0, so present in chain by default.
        assert!(fx.enabled, "PresetInstance::new defaults enabled = true");

        let hash_on = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        let cg_on = PresetRuntime::try_build(&[fx.clone()], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("Mirror chain builds at enabled = true");
        assert_eq!(
            cg_on.effect_nodes.len(),
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
        let cg_off = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None);
        assert!(
            cg_off.is_none(),
            "Disabled effect must be filtered out of active_effects — got a chain with effects when it should be empty",
        );
    }

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
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn toggling_relight_adds_and_removes_rl_nodes_on_rebuild() {
        // Relight disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // `relight_active()` is false so no template is spliced. The augment
        // machinery itself stays covered by `node_graph::relight`'s ungated tests.
        if !manifold_foundation::RELIGHT_FEATURE_ENABLED {
            return;
        }
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let lambert_id = manifold_core::NodeId::new("rl_lambert");

        let mut fx = make_default(PresetTypeId::MIRROR);
        let hash_off = compute_topology_hash(&[fx.clone()], &[], 256, 256, None);
        let cg_off = PresetRuntime::try_build(&[fx.clone()], &[], &primitives, &device, None, 256, 256, None, None)
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
        let cg_on_unfused = PresetRuntime::try_build(
            &[fx.clone()], &[], &primitives, &device, None, 256, 256, Some(&fx.id), None
        )
        .expect("Mirror chain builds with relight on (watched / unfused)");
        assert!(
            cg_on_unfused.graph.instance_by_node_id(&lambert_id).is_some(),
            "relight on must splice the rl_lambert template node into the built chain",
        );

        // Toggle back off: the rebuilt chain must lose the template again —
        // proves this isn't a one-way sticky augmentation.
        fx.relight = false;
        let cg_off_again =
            PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
                .expect("Mirror chain builds with relight off again");
        assert!(
            cg_off_again.graph.instance_by_node_id(&lambert_id).is_none(),
            "toggling relight back off must remove the rl_ template nodes on rebuild",
        );
    }

    /// D8/P7: float relight knobs are live uniforms, so dragging them must NOT
    /// change the topology hash (no chain rebuild). `height_from` changes
    /// template topology and legitimately rebuilds.
    #[test]
    fn relight_float_knobs_do_not_change_topology_hash() {
        let mut fx = make_default(PresetTypeId::MIRROR);
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
        let mut fx = make_default(PresetTypeId::MIRROR);
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

    #[test]
    fn stateful_effects_never_skip() {
        // Stateful effects must keep their workers alive across an
        // `amount → 0 → up` drag so their accumulated state (Feedback
        // prev-frame texture, Bloom mip pyramid, Watercolor ping-pong,
        // DNN worker spool, etc.) survives the bypass moment.
        // Tagging them `SkipMode::Never` is how we guarantee that.
        // Bloom is intentionally absent: its decomposed graph
        // (threshold → downsample → blur → mix) is stateless, so it has
        // no per-instance state to preserve and can stay SkipMode::OnZero.
        for ty in [
            PresetTypeId::STYLIZED_FEEDBACK,
            PresetTypeId::WATERCOLOR,
            PresetTypeId::DEPTH_OF_FIELD,
            PresetTypeId::WIREFRAME_DEPTH,
            PresetTypeId::BLOB_TRACKING,
            PresetTypeId::AUTO_GAIN,
        ] {
            let view = loaded_preset_view_by_id(&ty).unwrap_or_else(|| {
                panic!("{:?}: missing LoadedPresetView", ty);
            });
            assert!(
                matches!(view.skip_mode, crate::node_graph::SkipMode::Never),
                "{:?}: stateful effects must be SkipMode::Never so their \
                 per-instance state survives an amount → 0 → up slider drag",
                ty,
            );
        }
    }
