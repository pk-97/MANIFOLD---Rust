//! [`PresetRuntime`] — one cached [`Graph`] per `EffectChain`.
//!
//! Each chain compiles its full effect sequence (every active
//! [`PresetInstance`], plus `Mix` sub-graphs for wet/dry groups)
//! into a single graph runtime instance: one [`Graph`], one
//! [`ExecutionPlan`], one [`MetalBackend`], one [`Executor`]. That's
//! one ping/pong recycle pool for the chain, one executor step loop
//! per frame, one input-texture pre-bind per frame — no per-effect
//! dispatch overhead.
//!
//! Primitive state (mip pyramids, feedback buffers, depth workers)
//! lives inside the boxed [`EffectNode`] owned by the cached
//! [`Graph`]. Per-frame param changes refresh in place via
//! [`apply_bindings`]; topology changes (effect added /
//! removed / reordered / type-swapped, group enabled / disabled
//! toggle, group crossing the 1.0 wet/dry boundary, render-resolution
//! change) rebuild from scratch.
//!
//! ## Build-time wiring
//!
//! Linear sequence:
//!
//! ```text
//! Source ──▶ eff_1 ──▶ eff_2 ──▶ … ──▶ eff_n ──▶ FinalOutput
//! ```
//!
//! Wet/dry group with `wet_dry < 1.0` (spans effects `e_i..e_j`):
//!
//! ```text
//! pre_group ─┬─▶ e_i ──▶ … ──▶ e_j ──▶ Mix.b
//!            └────────────────────────▶ Mix.a
//! Mix.out (= lerp(dry, wet, wet_dry)) ─▶ next_node
//! ```
//!
//! ## Per-frame cost
//!
//! - 1 `copy_texture_to_texture` (upstream input → source slot)
//! - 1 `apply_bindings` call per effect (unified static + user tail)
//! - K `set_param` calls (one per Mix node, refreshing `amount`)
//! - 1 `execute_frame_with_gpu` covering N + K + 2 step iterations
//!   (Source + N effects + K Mix nodes + FinalOutput)
//! - 1 `texture_2d` lookup for the chain output
//!
//! The single `copy_texture_to_texture` is the only residual overhead
//! relative to the legacy chain's direct-from-input first-effect
//! dispatch: the backend's slot API takes owned `RenderTarget`s, not
//! borrowed `&GpuTexture`s, so the upstream input is materialised
//! into the source slot once per chain invocation.

use ahash::AHashMap;
use manifold_core::PresetTypeId;
use manifold_core::NodeId;
use manifold_core::effects::{EffectGroup, PresetInstance, RelightField, RelightParams};
use manifold_core::id::{EffectGroupId, EffectId};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, TexturePool};

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Mix;
use crate::node_graph::{
    BindingSource, BoundGraph, EffectGraphDefExt, ExecutionPlan, Executor, FINAL_OUTPUT_TYPE_ID,
    FinalOutput, FrameTime, GENERATOR_INPUT_TYPE_ID, Graph, GraphError, LoadError, LoadedPresetView,
    MetalBackend, NodeInstanceId, ParamBinding, ParamValue, PrimitiveRegistry, ResolvedBinding,
    ResolvedTarget,
    ResourceId, Slot, Source, SpliceResult, StateStore, apply_binding_defaults, compile,
    splice_def_into_chain,
};
use crate::node_graph::{is_skipped_for, loaded_preset_view_by_id};
use crate::preset_context::PresetContext;
use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef};
use manifold_core::params::ParamManifest;
use manifold_core::{Beats, Seconds};
use crate::render_target::RenderTarget;

mod errors;
pub use errors::{ChainError, JsonGeneratorLoadError};
use errors::record_chain_error;

mod bindings;
use bindings::{StringBindingResolution, def_string_param_value, RelightParamWrite, build_relight_writes};

mod segments;
pub use segments::{prewarm_chain_segments, prewarm_project_chain_segments};
use segments::{SegmentMember, classify_segment_member, segment_run, build_segment_cards};

mod build;
use build::{compute_topology_hash, close_mix_group, assign_texture2d_slots, OpenGroup, SlotAssignment};

mod core;
pub use core::PresetRuntime;
use core::{EffectSlot, PresetIo, chain_active_effects, assert_manifest_gate, GRAPH_FORMAT};

mod instrumentation;

#[cfg(all(test, feature = "gpu-proofs"))]
mod multi_segment_tests {
    //! Regression tests for the multi-segment wet/dry group support in
    //! `PresetRuntime::try_build`. A "multi-segment" group is one whose
    //! enabled effects sit in non-contiguous positions in the chain —
    //! e.g. group `g` contains effects at indices 0 and 2, with a
    //! non-group effect at index 1 between them.
    //!
    //! Pre-fix: `try_build` rejected this layout via the
    //! `enabled_groups_are_contiguous` preflight; the chain fell back
    //! to the legacy per-effect dispatcher.
    //!
    //! Post-fix: the build loop's open/close-on-every-transition
    //! pattern emits one Mix sub-graph per segment, each fed from the
    //! pre-segment output and feeding the post-segment input. All Mix
    //! nodes register under the same `EffectGroupId` in
    //! `group_mix_nodes`, so the per-frame `wet_dry` refresh sets the
    //! `amount` param on every segment uniformly.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::{EffectGroup, PresetInstance};
    use manifold_core::id::EffectGroupId;
    

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    #[test]
    fn non_contiguous_group_builds_multi_segment_mix() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");

        // Chain: Invert(g1) → ChromaticAberration → Invert(g1)
        // Effects on either side belong to g1; the middle effect doesn't.
        let mut e1 = make_default(PresetTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let e2 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        let mut e3 = make_default(PresetTypeId::INVERT_COLORS);
        e3.group_id = Some(g1_id.clone());

        let g1 = EffectGroup {
            id: g1_id.clone(),
            name: "g1".to_string(),
            enabled: true,
            collapsed: false,
            wet_dry: 0.5,
            parent_group_id: None,
        };

        let result =
            PresetRuntime::try_build(&[e1, e2, e3], &[g1], &primitives, &device, None, 256, 256, None, None);

        let cg = result.expect(
            "PresetRuntime should build for a non-contiguous wet/dry group \
             (multi-segment Mix support)",
        );

        // Two segments → two Mix sub-graphs, both keyed to g1.
        assert_eq!(
            cg.group_mix_nodes.len(),
            2,
            "non-contiguous group with 2 segments must emit 2 Mix sub-graphs",
        );
        for (gid, _) in &cg.group_mix_nodes {
            assert_eq!(gid.as_str(), "g1");
        }
    }

    #[test]
    fn contiguous_group_still_builds_single_mix() {
        // Regression guard: the contiguous case still produces exactly
        // one Mix sub-graph.
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");

        let mut e1 = make_default(PresetTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let mut e2 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        e2.group_id = Some(g1_id.clone());
        let e3 = make_default(PresetTypeId::INVERT_COLORS);

        let g1 = EffectGroup {
            id: g1_id.clone(),
            name: "g1".to_string(),
            enabled: true,
            collapsed: false,
            wet_dry: 0.5,
            parent_group_id: None,
        };

        let result =
            PresetRuntime::try_build(&[e1, e2, e3], &[g1], &primitives, &device, None, 256, 256, None, None);

        let cg = result.expect("PresetRuntime should build for contiguous group");
        assert_eq!(cg.group_mix_nodes.len(), 1);
    }

    #[test]
    fn three_segment_group_builds_three_mix_sub_graphs() {
        // Chain: Invert(g1) → Chroma → Invert(g1) → Chroma → Invert(g1)
        // Group g1 has three non-contiguous segments.
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");
        let mut e1 = make_default(PresetTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let e2 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        let mut e3 = make_default(PresetTypeId::INVERT_COLORS);
        e3.group_id = Some(g1_id.clone());
        let e4 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        let mut e5 = make_default(PresetTypeId::INVERT_COLORS);
        e5.group_id = Some(g1_id.clone());

        let g1 = EffectGroup {
            id: g1_id.clone(),
            name: "g1".to_string(),
            enabled: true,
            collapsed: false,
            wet_dry: 0.3,
            parent_group_id: None,
        };

        let result = PresetRuntime::try_build(
            &[e1, e2, e3, e4, e5],
            &[g1],
            &primitives,
            &device,
            None,
            256,
            256,
            None,
            None,
        );

        let cg = result.expect("PresetRuntime should build for three-segment group");
        assert_eq!(cg.group_mix_nodes.len(), 3);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod binding_seed_tests {
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
}

#[cfg(test)]
mod topology_hash_tests {
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
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod user_binding_tests {
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
}

#[cfg(test)]
mod bug080_manifest_gate_tests {
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
}

#[cfg(test)]
mod persistent_slot_tests {
    //! Regression: a feedback chain like
    //! `source → feedback → affine → gain → vignette → mix`
    //! where `mix.out` wires back to `feedback.in` (closing the per-
    //! frame loop) must NOT have `feedback.in`'s resource (which is
    //! `mix.out`) and `feedback.out`'s resource share a physical
    //! Texture2D slot.
    //!
    //! Without dedicated slots for persistent resources, the simulator's
    //! free-pool ping-pong was assigning the same slot to both:
    //! `feedback.out` got Slot(N) at step 0, was freed at step 2 when
    //! affine read it, and Slot(N) was eventually pulled out of the
    //! pool again at mix's step for `mix.out` — making them aliases.
    //! At runtime that turns Feedback's copy(prev→out) followed by
    //! copy(in→prev) into a no-op: `in` and `out` point at the same
    //! MTLTexture, so the "capture" step reads the value the "emit"
    //! step just wrote, never picking up the producer's actual write.
    //! Symptom: feedback effects look like a pass-through with no
    //! accumulation.
    //!
    //! The fix lives in `assign_texture2d_slots`: every persistent
    //! resource pre-allocates its own slot that never enters the free
    //! pool. This test pins the contract by constructing the exact
    //! topology and asserting the two slots differ.
    use super::*;
    use crate::node_graph::primitives::{
        AffineTransform, Feedback, Gain, Mix, Vignette,
    };
    use crate::node_graph::{FinalOutput, Graph, Source, compile};

    #[test]
    fn feedback_in_and_out_get_distinct_slots_in_the_closed_loop() {
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(Source::new()));
        let fb = graph.add_node(Box::new(Feedback::new()));
        let aff = graph.add_node(Box::new(AffineTransform::new()));
        let gain = graph.add_node(Box::new(Gain::new()));
        let vig = graph.add_node(Box::new(Vignette::new()));
        let mix = graph.add_node(Box::new(Mix::new()));
        let out = graph.add_node(Box::new(FinalOutput::new()));

        graph.connect((src, "out"), (mix, "a")).unwrap();
        graph.connect((fb, "out"), (aff, "in")).unwrap();
        graph.connect((aff, "out"), (gain, "in")).unwrap();
        graph.connect((gain, "out"), (vig, "in")).unwrap();
        graph.connect((vig, "out"), (mix, "b")).unwrap();
        // The state-capture edge — allowed because Feedback declares
        // `breaks_dependency_cycle`. This is the wire that would have
        // collapsed feedback.out and mix.out onto the same physical
        // slot under the pre-fix simulator.
        graph.connect((mix, "out"), (fb, "in")).unwrap();
        graph.connect((mix, "out"), (out, "in")).unwrap();

        let plan = compile(&graph).expect("feedback chain compiles");

        let src_res = plan
            .steps()
            .iter()
            .find(|s| s.node == src)
            .and_then(|s| s.outputs.iter().find(|(p, _)| *p == "out").map(|(_, r)| *r))
            .expect("source produces an out resource");
        let assignment = assign_texture2d_slots(&plan, src_res, (64, 64));

        let mix_out_res = plan
            .steps()
            .iter()
            .find(|s| s.node == mix)
            .and_then(|s| s.outputs.iter().find(|(p, _)| *p == "out").map(|(_, r)| *r))
            .expect("mix produces an out resource");
        let fb_out_res = plan
            .steps()
            .iter()
            .find(|s| s.node == fb)
            .and_then(|s| s.outputs.iter().find(|(p, _)| *p == "out").map(|(_, r)| *r))
            .expect("feedback produces an out resource");

        let mix_slot = assignment
            .resource_to_slot
            .get(&mix_out_res)
            .copied()
            .expect("mix.out has a slot");
        let fb_slot = assignment
            .resource_to_slot
            .get(&fb_out_res)
            .copied()
            .expect("feedback.out has a slot");

        assert_ne!(
            mix_slot, fb_slot,
            "mix.out and feedback.out MUST live on distinct physical slots. \
             Sharing a slot means feedback.in (which points at mix.out) and \
             feedback.out alias the same MTLTexture at runtime, and the \
             primitive's capture step reads back what its emit step just \
             wrote — feedback never accumulates state across frames. \
             Pre-fix, the simulator's free-pool ping-pong would assign \
             Slot(1) to both. The persistent-resource pre-allocation in \
             `assign_texture2d_slots` is what keeps them apart.",
        );

        // Sanity: the persistent resource's slot must be in the slot
        // assignment (mix.out is what feedback.in reads).
        let plan_persistent: std::collections::HashSet<_> =
            plan.persistent_resources().iter().copied().collect();
        assert!(
            plan_persistent.contains(&mix_out_res),
            "compile() must mark mix.out as a persistent resource — \
             without that, the slot simulator can't dedicate a slot for it"
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod generator_input_tests {
    //! Regression for the effect-side `system.generator_input` surface.
    //! Effects that include a `system.generator_input` node in their
    //! preset get per-frame scalars (time / beat / aspect / output
    //! dims) pushed to it by the chain runner, the same way generators
    //! do. The standard port-shadows-param machinery then propagates
    //! those scalars to inner primitives via wires — no per-effect
    //! Rust code, no hardcoded `apply_ctx_params_at` match list.
    //!
    //! These tests pin two contracts:
    //! 1. **Splice surface**: a preset that includes
    //!    `system.generator_input` causes [`SpliceResult::generator_input_id`]
    //!    to be `Some`, threaded onto [`EffectSlot::generator_input_node`].
    //! 2. **Per-frame push**: [`PresetRuntime::run`] writes the
    //!    [`PresetContext`]'s `time` / `beat` / `aspect` / output dims
    //!    into the generator_input node's params via `set_param`.
    use super::*;
    use crate::node_graph::ParamValue;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::EffectGraphDef;

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    /// A divergent PresetInstance whose graph contains a
    /// `system.generator_input` node. Uses Invert as the host effect
    /// type so we get a known canonical to override; the divergent def
    /// is what actually drives splicing.
    fn invert_with_generator_input() -> PresetInstance {
        let custom_def: EffectGraphDef = serde_json::from_str(
            r#"{
                "version": 1,
                "name": "test",
                "nodes": [
                    { "id": 0, "typeId": "system.source" },
                    { "id": 1, "typeId": "system.generator_input", "handle": "input" },
                    { "id": 2, "typeId": "node.invert", "handle": "invert" },
                    { "id": 3, "typeId": "system.final_output" }
                ],
                "wires": [
                    { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "in" },
                    { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
                ]
            }"#,
        )
        .expect("test fixture parses");

        let mut fx = make_default(PresetTypeId::INVERT_COLORS);
        // Mark the divergent path live so try_build picks it up. A divergent
        // def is a structural change, so bump the structure version too.
        fx.graph = Some(custom_def);
        fx.graph_version = fx.graph_version.wrapping_add(1);
        fx.graph_structure_version = fx.graph_structure_version.wrapping_add(1);
        fx
    }

    /// Build-time contract: a divergent def with a
    /// `system.generator_input` node populates the EffectSlot's
    /// `generator_input_node` field.
    #[test]
    fn splice_threads_generator_input_id_onto_effect_slot() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = invert_with_generator_input();

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("chain builds with a divergent def including system.generator_input");

        let slot = cg
            .effect_nodes
            .first()
            .expect("Invert contributes one effect slot");
        assert!(
            slot.generator_input_node.is_some(),
            "EffectSlot.generator_input_node must populate when the def \
             includes a system.generator_input node — without this the \
             chain runner has nowhere to push frame-context scalars and \
             effects can't react to project time/beat."
        );
    }

    /// Build-time symmetry: presets without `system.generator_input`
    /// leave `EffectSlot.generator_input_node` as `None`. Most
    /// shipping effects today fall in this bucket — the field is
    /// opt-in.
    #[test]
    fn splice_leaves_generator_input_node_none_when_absent() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        // Canonical Invert preset has no system.generator_input.
        let fx = make_default(PresetTypeId::INVERT_COLORS);

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("Invert chain builds without divergent def");

        let slot = cg
            .effect_nodes
            .first()
            .expect("Invert contributes one effect slot");
        assert!(
            slot.generator_input_node.is_none(),
            "EffectSlot.generator_input_node should stay None when the \
             preset doesn't include a system.generator_input — opt-in surface."
        );
    }

    /// Per-frame contract: after `PresetRuntime::run`, the generator_input
    /// node's `time` / `beat` / `aspect` / `output_width` /
    /// `output_height` params reflect the [`PresetContext`].
    /// Exercises the param-write half of the system; the
    /// scalar-wire-propagation half is covered by the
    /// `generator_input_params_drive_scalar_outputs` test in
    /// `boundary_nodes.rs`.
    #[test]
    fn run_pushes_frame_context_into_generator_input_params() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = invert_with_generator_input();

        let mut cg =
            PresetRuntime::try_build(std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256, None, None)
                .expect("chain builds");

        let gi_id = cg
            .effect_nodes
            .first()
            .and_then(|s| s.generator_input_node)
            .expect("splice populated generator_input_node");

        // A dummy input texture for `run` to install into the source slot.
        let input = crate::render_target::RenderTarget::new(
            &device,
            256,
            256,
            GpuTextureFormat::Rgba16Float,
            "test-source-input",
        );

        let mut native_enc = device.create_encoder("generator-input-test");
        let mut gpu = GpuEncoder::new(&mut native_enc, &device);

        let ctx = PresetContext {
            time: 1.5,
            beat: 2.25,
            dt: 1.0 / 60.0,
            width: 1920,
            height: 1080,
            output_width: 3840,
            output_height: 2160,
            aspect: 1920.0 / 1080.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        cg.run(&mut gpu, &input.texture, &[fx], &[], &ctx);

        let node = cg
            .graph
            .get_node(gi_id)
            .expect("generator_input node id still valid");
        let read = |name: &str| -> Option<f32> {
            node.params.get(name).and_then(|v| match v {
                ParamValue::Float(f) => Some(*f),
                _ => None,
            })
        };
        assert_eq!(read("time"), Some(1.5));
        assert_eq!(read("beat"), Some(2.25));
        // aspect derives from ctx.width / ctx.height (the render-resolution
        // dims, not the upscale-target output_* fields).
        assert!((read("aspect").unwrap() - (1920.0 / 1080.0)).abs() < 1e-5);
        assert_eq!(read("output_width"), Some(3840.0));
        assert_eq!(read("output_height"), Some(2160.0));
    }

    /// `trigger_count` used to stay pinned at 0.0 for
    /// effect-chain generator_input nodes ("clip-side concepts that don't
    /// reach the effect chain"). This is the effect-chain half of the P2
    /// gate — the generator half lives in
    /// `generator_renderer::tests` (`effective_trigger_count_sums_clip_and_audio_and_respects_clip_edge_mode`).
    /// Together they prove the SAME effective count (clip edge + audio
    /// fires) reaches both a generator's own graph and an effect chain on
    /// the same layer.
    #[test]
    fn run_feeds_nonzero_trigger_count_into_generator_input_effect_slot() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = invert_with_generator_input();

        let mut cg =
            PresetRuntime::try_build(std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256, None, None)
                .expect("chain builds");

        let gi_id = cg
            .effect_nodes
            .first()
            .and_then(|s| s.generator_input_node)
            .expect("splice populated generator_input_node");

        let input = crate::render_target::RenderTarget::new(
            &device,
            256,
            256,
            GpuTextureFormat::Rgba16Float,
            "test-source-input",
        );
        let mut native_enc = device.create_encoder("generator-input-trigger-count-test");
        let mut gpu = GpuEncoder::new(&mut native_enc, &device);

        // A layer whose generator has been triggered 7 times (clip launches
        // + audio fires, already summed by the caller per §8 D1) — the
        // effect chain on that same layer must see the SAME 7, not the old
        // pinned 0.0.
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: 256,
            height: 256,
            output_width: 256,
            output_height: 256,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 7,
        };

        cg.run(&mut gpu, &input.texture, &[fx], &[], &ctx);

        let node = cg
            .graph
            .get_node(gi_id)
            .expect("generator_input node id still valid");
        let trigger_count = node.params.get("trigger_count").and_then(|v| match v {
            ParamValue::Float(f) => Some(*f),
            _ => None,
        });
        assert_eq!(
            trigger_count,
            Some(7.0),
            "effect chain's generator_input.trigger_count must reflect the \
             owning layer's effective count (D5), not stay pinned at 0.0"
        );
    }

    /// §8 D6 — Strobe reachability proof: the bundled Strobe preset's
    /// `clip_trigger` card (Trigger Gate → Envelope Decay → Max-combine with
    /// the beat gate) actually flashes when the layer's effective
    /// `trigger_count` jumps, and does NOT when the card is off. This is the
    /// concrete "kick fires Strobe" acceptance demo at the L1 (graph-value)
    /// level — the live app/stem look is still L4-owed (logged in the design
    /// doc), but this proves the wiring is live, not just present in the JSON.
    #[test]
    fn strobe_clip_trigger_card_flashes_on_trigger_count_jump_when_enabled() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let run_and_read_flash_amount = |clip_trigger_on: bool| -> f32 {
            let mut fx = manifold_core::preset_definition_registry::create_default(
                &PresetTypeId::new("Strobe"),
            );
            if let Some(p) = fx.params.get_mut("clip_trigger") {
                p.value = if clip_trigger_on { 1.0 } else { 0.0 };
                p.base = p.value;
            } else {
                panic!("Strobe must ship a clip_trigger card (§8 D6)");
            }

            let mut cg = PresetRuntime::try_build(
                std::slice::from_ref(&fx),
                &[],
                &primitives,
                &device,
                None,
                64,
                64,
                None,
                None,
            )
            .expect("Strobe chain builds");

            let input = crate::render_target::RenderTarget::new(
                &device,
                64,
                64,
                GpuTextureFormat::Rgba16Float,
                "strobe-test-input",
            );
            let mut native_enc = device.create_encoder("strobe-trigger-test");
            let mut gpu = GpuEncoder::new(&mut native_enc, &device);

            let ctx_at = |trigger_count: u32| PresetContext {
                time: 0.0,
                // beat = 0.0 parks node.beat_gate's square wave at 0 (phase
                // 0.0 < duty 0.5) so the Max-combine isolates the trigger
                // path — a bare beat-gate contribution would confound the
                // assertion below.
                beat: 0.0,
                dt: 1.0 / 60.0,
                width: 64,
                height: 64,
                output_width: 64,
                output_height: 64,
                aspect: 1.0,
                owner_key: 0,
                is_clip_level: false,
                frame_count: 0,
                anim_progress: 0.0,
                trigger_count,
            };

            // Watch combine_gate's scalar I/O — `preview_scalar_io` only
            // captures for a NON-texture-outputting node (`node.math`'s `out`
            // is a bare scalar, unlike `flash`'s image output, which the
            // executor deliberately skips scalar capture for — see
            // `execution.rs`'s preview-capture step: image nodes show their
            // texture, not numbers). `.params` was tried first and rejected:
            // it only reflects bound/set values, never what a port-shadowed
            // wire evaluates to (confirmed by inspection — combine_gate's and
            // flash's `.params` stayed at their authoring defaults across both
            // frames below, even though the wires clearly carried real data).
            cg.set_preview_target(&fx.id, Some(&manifold_core::NodeId::new("combine_gate")));

            // Frame 1: baseline at trigger_count 0, settles initial state.
            cg.run(&mut gpu, &input.texture, &[fx.clone()], &[], &ctx_at(0));
            // Frame 2: the layer's effective count jumps (a kick fired).
            cg.run(&mut gpu, &input.texture, &[fx], &[], &ctx_at(5));

            let (_inputs, outputs) = cg.preview_scalar_io();
            outputs
                .iter()
                .find(|(name, _)| name == "out")
                .map(|(_, v)| *v)
                .expect("combine_gate's watched scalar outputs must include `out`")
        };

        let on = run_and_read_flash_amount(true);
        let off = run_and_read_flash_amount(false);

        // node.envelope_decay snaps to 1.0 THEN decays once by this frame's dt
        // in the same evaluate() call, so the observable post-frame value
        // after a fire is exp(-decay_rate * dt) = exp(-12/60) ≈ 0.819, never
        // a full 1.0 — 0.7 comfortably separates "just fired" from "at rest".
        assert!(
            on > 0.7,
            "clip_trigger ON: a trigger_count jump must snap the envelope \
             (and therefore flash.amount, via the Max-combine) toward 1.0 \
             (observably ~0.82 one frame later), got {on}"
        );
        assert!(
            off < 0.1,
            "clip_trigger OFF: the Trigger Gate must absorb the count jump \
             so flash.amount stays at the beat gate's (parked-at-0) value, got {off}"
        );
    }

    /// **The production main-path proof (design §12.3 step 5).** With the freeze
    /// toggle on (default), [`PresetRuntime::try_build`] renders a canonical
    /// ColorGrade card through the FUSED node, not the 7 atoms: the built chain
    /// graph contains one `node.wgsl_compute` and none of the original
    /// `node.exposure` / `node.mix` workers, and it runs one frame producing an
    /// output texture. This is what puts the optimised fused kernel on screen.
    #[test]
    fn colorgrade_chain_renders_via_fused_node() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        // Honor the kill-switch: when MANIFOLD_FREEZE is off this path is
        // intentionally the unfused one, so the assertion wouldn't hold.
        if !crate::node_graph::freeze::install::freeze_enabled() {
            return;
        }

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = make_default(PresetTypeId::new("ColorGrade"));

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
        .expect("ColorGrade chain builds");

        // Main-path proof: the fused kernel replaced the atom chain.
        let type_ids: Vec<&str> =
            cg.graph.nodes().map(|n| n.node.type_id().as_str()).collect();
        assert!(
            type_ids.contains(&"node.wgsl_compute"),
            "fused chain must contain the fused WGSL node; got {type_ids:?}"
        );
        assert!(
            !type_ids.contains(&"node.exposure") && !type_ids.contains(&"node.mix"),
            "fused chain must NOT still contain unfused ColorGrade atoms; got {type_ids:?}"
        );

        // And it renders one frame, producing an output texture (the fused
        // kernel actually dispatched through the production chain).
        let input = crate::render_target::RenderTarget::new(
            &device,
            256,
            256,
            GRAPH_FORMAT,
            "cg-fused-input",
        );
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: 256,
            height: 256,
            output_width: 256,
            output_height: 256,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut native_enc = device.create_encoder("cg-fused-run");
        {
            let mut gpu = GpuEncoder::new(&mut native_enc, &device);
            let out =
                cg.run(&mut gpu, &input.texture, std::slice::from_ref(&fx), &[], &ctx);
            assert!(out.is_some(), "fused ColorGrade chain produced an output texture");
        }
        native_enc.commit_and_wait_completed();
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod chain_error_tests {
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
}

#[cfg(test)]
mod generator_runtime_tests {
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
        let json = include_str!("../assets/generator-presets/Lissajous.json");
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

        let json = include_str!("../assets/generator-presets/Lissajous.json");
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
        let json = include_str!("../assets/generator-presets/Lissajous.json");
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
        let json = include_str!("../assets/generator-presets/Plasma.json");
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
        let json = include_str!("../assets/generator-presets/Text.json");
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
        let json = include_str!("../assets/generator-presets/Text.json");
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
        let json = include_str!("../assets/generator-presets/Text.json");
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
        let json = include_str!("../assets/generator-presets/Plasma.json");
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
        let json = include_str!("../assets/generator-presets/Plasma.json");
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
        preset.set_frame_context(1.5, 0.5, 1.78, 4.0, 0.25, 1920.0, 1080.0);
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
        let json = include_str!("../assets/generator-presets/StrangeAttractor.json");
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
        let json = include_str!("../assets/generator-presets/Plasma.json");
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
        let json = include_str!("../assets/reference-presets/CinematicScene.json");
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
        let json = include_str!("../assets/generator-presets/Lissajous.json");
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
        let json = include_str!("../assets/generator-presets/Cymatics.json");
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
        let json = include_str!("../assets/generator-presets/FluidSim2D.json");
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
        let json = include_str!("../assets/generator-presets/StrangeAttractor.json");
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
        let json = include_str!("../assets/generator-presets/StrangeAttractor.json");

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
        let json = include_str!("../assets/generator-presets/TrivialPassthrough.json");
        let mut preset = PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
            .expect("bundled TrivialPassthrough must load");
        assert_eq!(preset.type_id().as_str(), "TrivialPassthrough");
        preset.set_frame_context(0.0, 0.0, 1.78, 0.0, 0.0, 1920.0, 1080.0);
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
        let json = include_str!("../assets/generator-presets/FluidSim2D.json");
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
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod chain_fusion_tests {
    //! Cross-card chain fusion integration (docs/CHAIN_FUSION_DESIGN.md):
    //! the per-card build and the fused-segment build of the SAME two-card
    //! chain must render within the pointwise fusion budget of each other,
    //! and the cards' `param_values` must keep driving the fused chain
    //! through the retargeted bindings.

    use super::*;
    use crate::gpu_encoder::GpuEncoder;
    use crate::node_graph::freeze::TextureDiff;
    use crate::node_graph::freeze::install as freeze_install;
    use crate::preset_context::PresetContext;
    use half::f16;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;
    use manifold_gpu::{
        GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    fn set_param(fx: &mut PresetInstance, id: &str, v: f32) {
        let ty = fx.effect_type().clone();
        let p = fx
            .params
            .get_mut(id)
            .unwrap_or_else(|| panic!("param id `{id}` on {ty:?}"));
        p.value = v;
        p.base = v;
    }

    fn ctx(w: u32, h: u32) -> PresetContext {
        PresetContext {
            time: 0.5,
            beat: 1.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: w as f32 / h as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        }
    }

    fn gradient_input(device: &manifold_gpu::GpuDevice, w: u32, h: u32) -> manifold_gpu::GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32(x as f32 / w as f32);
                px[i + 1] = f16::from_f32(y as f32 / h as f32);
                px[i + 2] = f16::from_f32(0.5);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "chain-fusion-test-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn run_once(
        cg: &mut PresetRuntime,
        device: &manifold_gpu::GpuDevice,
        input: &manifold_gpu::GpuTexture,
        effects: &[PresetInstance],
        pc: &PresetContext,
    ) {
        let mut enc = device.create_encoder("chain-fusion-test");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            cg.run(&mut gpu, input, effects, &[], pc);
        }
        enc.commit_and_wait_completed();
    }

    /// Copy a runtime's current output into a standalone target so a later
    /// run can't overwrite it.
    fn snapshot_output(
        cg: &PresetRuntime,
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
    ) -> crate::render_target::RenderTarget {
        let out = cg.output_texture().expect("chain produced output");
        let rt = crate::render_target::RenderTarget::new(
            device,
            w,
            h,
            GpuTextureFormat::Rgba16Float,
            "chain-fusion-test-snap",
        );
        let mut enc = device.create_encoder("chain-fusion-snap");
        enc.copy_texture_to_texture(out, &rt.texture, w, h, 1);
        enc.commit_and_wait_completed();
        rt
    }

    #[test]
    fn fused_segment_build_matches_per_card_build_and_stays_param_driven() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        // Two adjacent ColorGrades with distinct, non-trivial params — the
        // same type twice exercises the segment namespacing on real presets.
        let mut e1 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e1, "amount", 1.0);
        set_param(&mut e1, "gain", 1.2);
        let mut e2 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e2, "amount", 1.0);
        set_param(&mut e2, "gain", 0.85);
        set_param(&mut e2, "saturation", 0.6);
        let effects = vec![e1, e2];

        // ── Per-card build first: the segment cache is cold, the lookup goes
        // Pending (tests never enqueue the worker), and the chain splices
        // per-card — today's production path, our oracle. ──
        let mut per_card = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("per-card chain builds");
        assert_eq!(per_card.effect_nodes.len(), 2);
        assert!(
            per_card.pending_segments,
            "cold cache must leave the chain waiting on the segment compile"
        );
        assert!(
            !per_card.awaiting_segment_swap(),
            "no swap signal until a worker result lands"
        );

        // ── Compile the segment synchronously (the worker's job, minus the
        // gate) and seed the cache, then rebuild: the Ready path. ──
        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        freeze_install::seed_segment_cache_for_test(&cards, &primitives)
            .expect("two pointwise ColorGrades fuse across the seam");

        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused-segment chain builds");
        assert_eq!(fused.effect_nodes.len(), 2, "one EffectSlot per card survives");
        assert!(!fused.pending_segments);
        let fused_kernels = fused
            .graph
            .nodes()
            .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
            .count();
        assert_eq!(
            fused_kernels, 1,
            "both cards must collapse into ONE cross-card kernel"
        );

        // ── Parity at build params. ──
        run_once(&mut per_card, &device, &input, &effects, &pc);
        run_once(&mut fused, &device, &input, &effects, &pc);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused segment must match per-card chain: max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );

        // ── Live param drive: move card 2's gain on the host slice only. The
        // binding apply path must push it into the fused kernel's uniform. ──
        let before = snapshot_output(&fused, &device, w, h);
        let mut effects2 = effects.clone();
        set_param(&mut effects2[1], "gain", 1.6);
        run_once(&mut fused, &device, &input, &effects2, &pc);
        let moved = differ.compare(
            &device,
            &before.texture,
            fused.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "a card slider move must visibly drive the fused segment"
        );
        run_once(&mut per_card, &device, &input, &effects2, &pc);
        let r2 = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r2.passes(0.005) && r2.over_count < 64,
            "after the slider move the two builds must still agree: max_abs={}, over={}/{}",
            r2.max_abs,
            r2.over_count,
            r2.total
        );
    }

    /// BUG-111: an in-place inner-param edit (value/position edit — bumps
    /// `graph_version` only, no rebuild) on a card that is a member of a
    /// fused multi-card SEGMENT must still reach the live kernel. The
    /// segment's `node_map`/`fused_retarget` are keyed with the `c{i}.`
    /// per-card prefix (`freeze::segment::card_prefix`), built from the
    /// concatenated segment def, while the per-frame override path reads
    /// each card's own UNPREFIXED `fx.graph`. Without translating through
    /// that prefix (`EffectSlot::card_prefix` →
    /// `BoundGraph::apply_inner_overrides_prefixed`) the override misses
    /// every node in the map and silently no-ops — the old value keeps
    /// rendering until an unrelated rebuild. Segment sibling of
    /// `bound_graph::inner_override_routes_fused_away_node_through_retarget`.
    #[test]
    fn fused_segment_inner_override_reaches_live_kernel() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        // Two adjacent ColorGrades — same fusable two-card segment shape as
        // `fused_segment_build_matches_per_card_build_and_stays_param_driven`.
        let mut e1 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e1, "amount", 1.0);
        let mut e2 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e2, "amount", 1.0);
        let effects = vec![e1, e2];

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [(view1.canonical_def.as_ref(), view1), (view2.canonical_def.as_ref(), view2)];
        freeze_install::seed_segment_cache_for_test(&cards, &primitives)
            .expect("two pointwise ColorGrades fuse across the seam");

        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused-segment chain builds");
        assert!(!fused.pending_segments);
        let fused_kernels = fused
            .graph
            .nodes()
            .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
            .count();
        assert_eq!(
            fused_kernels, 1,
            "both cards must collapse into ONE cross-card kernel — every one \
             of card 2's inner nodes, including `clamp`, is fused away and \
             only reachable through the segment's retarget map"
        );

        run_once(&mut fused, &device, &input, &effects, &pc);
        let before = snapshot_output(&fused, &device, w, h);

        // Card 2's own (unprefixed) per-instance graph, with `clamp.max`
        // edited to clip the output hard. `clamp` carries no card-slider
        // binding (unlike gain/saturation/contrast/…, which ColorGrade DOES
        // bind — an edit there would just be re-asserted-over by the live
        // binding on the very next apply, proving nothing about the override
        // path itself). Bump `graph_version` only, NOT
        // `graph_structure_version`, so the runtime takes the in-place
        // override path instead of rebuilding.
        let mut effects2 = effects.clone();
        let mut edited = (*view2.canonical_def).clone();
        {
            use manifold_core::effect_graph_def::SerializedParamValue;
            let clamp = edited
                .nodes
                .iter_mut()
                .find(|n| n.node_id.as_str() == "clamp")
                .expect("ColorGrade has a `clamp` node");
            clamp
                .params
                .insert("max".to_string(), SerializedParamValue::Float { value: 0.05 });
        }
        effects2[1].graph = Some(edited);
        effects2[1].bump_graph_version();
        assert_eq!(
            effects2[1].graph_structure_version, effects[1].graph_structure_version,
            "sanity: this must be a value-only edit, not a rebuild"
        );

        run_once(&mut fused, &device, &input, &effects2, &pc);
        let differ = TextureDiff::new(&device);
        let moved = differ.compare(
            &device,
            &before.texture,
            fused.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "an inner-param edit on a fused SEGMENT member must reach the \
             live kernel (BUG-111) — clamping card 2's output to 0.05 must \
             visibly darken the frame: max_abs={}, over={}/{}",
            moved.max_abs,
            moved.over_count,
            moved.total
        );
    }

    /// D8/P7: a relight-on card must render identically whether the freeze
    /// compiler collapses it to one fused kernel or it runs per-atom. The
    /// fused path augments with DEFAULT knob values and writes live values
    /// per-frame; the unfused path splices the template with live values at
    /// build time. Both must land on the same pixels.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn relight_on_fused_matches_unfused_on_probe_graphs() {
        // Relight disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // both paths render inert, so this would pass vacuously — skip instead.
        if !manifold_foundation::RELIGHT_FEATURE_ENABLED {
            return;
        }
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        for probe in [PresetTypeId::MIRROR, PresetTypeId::COLOR_GRADE] {
            let probe = probe.clone();
            let mut fx = make_default(probe.clone());
            set_param(&mut fx, "amount", 1.0);
            fx.relight = true;

            let mut fused = PresetRuntime::try_build(
                std::slice::from_ref(&fx), &[], &primitives, &device, None, w, h,
                None, None,
            )
            .expect("fused relight-on chain builds");
            assert!(!fused.pending_segments);
            let fused_kernel_count = fused
                .graph
                .nodes()
                .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
                .count();
            assert!(
                fused_kernel_count >= 1,
                "{probe:?}: relight-on card must use at least one fused kernel"
            );

            // Force the unfused path by watching the card: `should_render_fused`
            // returns false, so the relight template splices per-atom.
            let mut unfused = PresetRuntime::try_build(
                std::slice::from_ref(&fx), &[], &primitives, &device, None, w, h,
                Some(&fx.id), None,
            )
            .expect("unfused relight-on chain builds");
            let unfused_kernel_count = unfused
                .graph
                .nodes()
                .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
                .count();
            assert_eq!(
                unfused_kernel_count, 0,
                "{probe:?}: watched card must not be fused"
            );

            run_once(&mut fused, &device, &input, std::slice::from_ref(&fx), &pc);
            run_once(&mut unfused, &device, &input, std::slice::from_ref(&fx), &pc);
            run_once(&mut fused, &device, &input, std::slice::from_ref(&fx), &pc);
            run_once(&mut unfused, &device, &input, std::slice::from_ref(&fx), &pc);

            let differ = TextureDiff::new(&device);
            let r = differ.compare(
                &device,
                fused.output_texture().unwrap(),
                unfused.output_texture().unwrap(),
                1.0e-2,
                3.0e-2,
            );
            assert!(
                r.passes(0.005) && r.over_count < 64,
                "{probe:?}: fused relight must match unfused relight: max_abs={}, over={}/{}",
                r.max_abs, r.over_count, r.total
            );
        }
    }

    /// D8/P7: float-knob edits are live uniforms, so dragging a knob on a
    /// fused relight-on card must visibly change the output without rebuilding
    /// the chain. This proves the per-frame `EffectSlot::relight_writes` path
    /// reaches the fused kernel.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn relight_knob_drag_visibly_changes_fused_output() {
        // Relight disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // no relight template is fused, so knob drags have no output to change.
        if !manifold_foundation::RELIGHT_FEATURE_ENABLED {
            return;
        }
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let mut fx = make_default(PresetTypeId::MIRROR);
        set_param(&mut fx, "amount", 1.0);
        fx.relight = true;
        fx.relight_params.light_x = 0.7;
        fx.relight_params.light_y = -0.4;
        fx.relight_params.relief = 0.6;
        fx.relight_params.gain = 1.8;

        let mut cg = PresetRuntime::try_build(
            std::slice::from_ref(&fx), &[], &primitives, &device, None, w, h,
            None, None,
        )
        .expect("fused relight-on chain builds");
        run_once(&mut cg, &device, &input, std::slice::from_ref(&fx), &pc);
        let before = snapshot_output(&cg, &device, w, h);

        fx.relight_params.light_x = -0.7;
        fx.relight_params.gain = 0.5;
        run_once(&mut cg, &device, &input, std::slice::from_ref(&fx), &pc);

        let differ = TextureDiff::new(&device);
        let moved = differ.compare(
            &device,
            &before.texture,
            cg.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "dragging relight knobs on a fused card must change the output: max_abs={}, over={}/{}",
            moved.max_abs, moved.over_count, moved.total
        );
    }

    /// D8/P7: the fused-view cache key must be knob-invariant for float D3
    /// knobs. Building a relight-on card with two different float-knob sets
    /// must hit the same cache entry; only `height_from` (topology) may mint
    /// a new entry.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn relight_float_knob_drag_hits_fused_view_cache() {
        // Relight disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // no relight template is fused, so there is no knob-invariant cache path.
        if !manifold_foundation::RELIGHT_FEATURE_ENABLED {
            return;
        }
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let mut fx = make_default(PresetTypeId::COLOR_GRADE);
        fx.relight = true;

        // Prime the cache with default knobs.
        let _ = PresetRuntime::try_build(
            std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256,
            None, None,
        )
        .expect("prime build");
        let cache_len_after_default =
            crate::node_graph::freeze::install::fused_effect_cache_len_for_test();

        // Move every float knob; the cache should NOT grow.
        fx.relight_params.light_x += 0.5;
        fx.relight_params.light_y -= 0.3;
        fx.relight_params.relief += 0.4;
        fx.relight_params.ao_intensity += 0.5;
        fx.relight_params.shadow_softness += 0.2;
        fx.relight_params.gain += 0.5;
        let _ = PresetRuntime::try_build(
            std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256,
            None, None,
        )
        .expect("knob-drag build");
        let cache_len_after_knobs =
            crate::node_graph::freeze::install::fused_effect_cache_len_for_test();
        assert_eq!(
            cache_len_after_default, cache_len_after_knobs,
            "float-knob drag must be a fused-view cache HIT, not a new compile"
        );

        // `height_from` changes template topology: this MAY mint a new entry.
        fx.relight_params.height_from =
            manifold_core::effects::RelightHeightFrom::InvertedLuminance;
        let _ = PresetRuntime::try_build(
            std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256,
            None, None,
        )
        .expect("height-from build");
        let cache_len_after_height_from =
            crate::node_graph::freeze::install::fused_effect_cache_len_for_test();
        assert!(
            cache_len_after_height_from >= cache_len_after_knobs,
            "height_from is allowed to add a fused-view variant"
        );
    }

    /// D8/P7: a fused segment may now mix relight-on and relight-off members.
    /// The relight-on member is augmented with default params before the
    /// segment is concatenated, so the whole run fuses into one segment view.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn mixed_relight_segment_fuses_to_one_region() {
        // Relight disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // no relight template is spliced, so there is no mixed-region case to fuse.
        if !manifold_foundation::RELIGHT_FEATURE_ENABLED {
            return;
        }
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);

        let mut e1 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e1, "amount", 1.0);
        e1.relight = true;

        let mut e2 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e2, "amount", 1.0);
        e2.relight = false;

        let effects = vec![e1, e2];

        // Seed the segment cache so the chain builds fused (tests don't enqueue
        // the background worker).
        let cards = build_segment_cards(&[0, 1], &[(0, &effects[0]), (1, &effects[1])], &primitives);
        let card_refs: Vec<(&EffectGraphDef, &'static LoadedPresetView)> =
            cards.iter().map(|(d, v)| (d, *v)).collect();
        freeze_install::seed_segment_cache_for_test(&card_refs, &primitives)
            .expect("mixed ColorGrade segment fuses");

        let cg = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("mixed relight segment chain builds");
        assert!(!cg.pending_segments, "mixed segment must be ready after seeding");
        assert_eq!(
            cg.effect_nodes.len(),
            2,
            "one EffectSlot per member survives"
        );
        let fused_kernels = cg
            .graph
            .nodes()
            .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
            .count();
        // The relight template cannot collapse to a single kernel — its blur
        // pair and GTAO are gather/camera cut points — so the strict claim is:
        // the segment path ran (one segment view, per-card path not taken),
        // the template's nodes are present, and BOTH template stretches fused
        // (the base+height region and the shading region).
        assert!(
            fused_kernels >= 2,
            "mixed relight-on/off segment must fuse both template regions, got {fused_kernels}"
        );
        assert!(
            cg.graph.nodes().any(|n| {
                crate::node_graph::relight::is_relight_node_id(n.node_id.as_str())
            }),
            "relight template nodes must be present in the fused segment graph"
        );
    }

    /// State harvest (docs/CHAIN_FUSION_DESIGN.md §5): rebuilding a chain
    /// with the prior runtime as donor must carry a feedback trail across the
    /// rebuild — the rebuilt chain continues exactly like a chain that never
    /// rebuilt. A rebuild WITHOUT the donor must visibly reset (sensitivity
    /// check: the trail actually accumulated something worth preserving).
    #[test]
    fn rebuild_with_prior_carries_feedback_trail_across() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        // StylizedFeedback (node.feedback trail in the StateStore) followed
        // by a ColorGrade — a realistic dial-in chain. Drive `rotate` so the
        // feedback trail genuinely evolves frame-to-frame: at the default
        // (zoom 0.95, rotate 0) a static self-similar gradient hits a
        // fixed point in one frame, so the output would be frame-invariant
        // and neither the harvest nor the sensitivity check would prove
        // anything. Rotation makes the prev spiral, so frame 1 ≠ frame 9.
        let mut fb = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        set_param(&mut fb, "amount", 1.0);
        set_param(&mut fb, "rotate", 10.0);
        set_param(&mut fb, "zoom", 0.9);
        let mut cg = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut cg, "amount", 1.0);
        set_param(&mut cg, "gain", 1.1);
        let effects = vec![fb, cg];

        let build = |prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(
                &effects, &[], &primitives, &device, None, w, h, None, prior,
            )
            .expect("chain builds")
        };

        const WARM: usize = 6;
        // Three post-rebuild frames, not one: a frozen ping-pong (the
        // shadowed-slot swap-failure class) still shows the carried trail on
        // frame 1 and only diverges once the state should have ADVANCED.
        const TAIL: usize = 3;
        // Reference: never rebuilt, runs WARM+TAIL frames.
        let mut reference = build(None);
        for _ in 0..WARM + TAIL {
            run_once(&mut reference, &device, &input, &effects, &pc);
        }

        // Harvested: WARM frames, rebuild WITH the prior, TAIL more frames.
        let mut donor = build(None);
        for _ in 0..WARM {
            run_once(&mut donor, &device, &input, &effects, &pc);
        }
        let mut harvested = build(Some(&mut donor));
        for _ in 0..TAIL {
            run_once(&mut harvested, &device, &input, &effects, &pc);
        }

        // Reset: WARM frames, rebuild WITHOUT the prior, one more frame.
        let mut fresh_donor = build(None);
        for _ in 0..WARM {
            run_once(&mut fresh_donor, &device, &input, &effects, &pc);
        }
        let mut reset = build(None);
        run_once(&mut reset, &device, &input, &effects, &pc);

        let differ = TextureDiff::new(&device);
        let carried = differ.compare(
            &device,
            reference.output_texture().unwrap(),
            harvested.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            carried.over_count, 0,
            "harvested rebuild must continue the trail exactly like an \
             un-rebuilt chain: max_abs={}, over={}/{}",
            carried.max_abs, carried.over_count, carried.total
        );
        let wiped = differ.compare(
            &device,
            reference.output_texture().unwrap(),
            reset.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            wiped.over_count > 0,
            "sensitivity: a donor-less rebuild must visibly reset the trail \
             (otherwise this test proves nothing)"
        );
    }

    /// Repro harness for the 2026-06-11 on-stage report: Infrared →
    /// QuadMirror fused as a segment washed the frame to the palette's dark
    /// end. Fused segment vs per-card build of the same chain, real GPU.
    #[test]
    fn infrared_quadmirror_segment_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let mut per_card = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        let seeded = freeze_install::seed_segment_cache_for_test(&cards, &primitives);
        if seeded.is_none() {
            // No seam-spanning region — nothing fused, nothing to prove.
            return;
        }
        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused chain builds");

        run_once(&mut per_card, &device, &input, &effects, &pc);
        run_once(&mut fused, &device, &input, &effects, &pc);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused Infrared→QuadMirror segment must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// Repro for the 2026-06-11 follow-up report: same Infrared → QuadMirror
    /// chain, but with a NON-DEFAULT palette (Arctic, selector 6 — the setting
    /// in the on-stage screenshots). The shipped guard only proves palette 0;
    /// this drives the build-time value and a live palette switch.
    #[test]
    fn infrared_quadmirror_segment_nondefault_palette() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let mut per_card = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        let seeded = freeze_install::seed_segment_cache_for_test(&cards, &primitives);
        if seeded.is_none() {
            return;
        }
        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused chain builds");

        run_once(&mut per_card, &device, &input, &effects, &pc);
        run_once(&mut fused, &device, &input, &effects, &pc);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused Infrared(Arctic)→QuadMirror must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );

        // Live palette switch on the fused chain: 6 → 2 (Green NV) must both
        // visibly change the output and still match per-card.
        let mut effects2 = effects.clone();
        set_param(&mut effects2[0], "palette", 2.0);
        let before = snapshot_output(&fused, &device, w, h);
        run_once(&mut fused, &device, &input, &effects2, &pc);
        let moved = differ.compare(
            &device,
            &before.texture,
            fused.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "a live palette switch must visibly drive the fused chain"
        );
        run_once(&mut per_card, &device, &input, &effects2, &pc);
        let r2 = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r2.passes(0.005) && r2.over_count < 64,
            "after a live palette switch the fused chain must match per-card: \
             max_abs={}, over={}/{}",
            r2.max_abs,
            r2.over_count,
            r2.total
        );
    }

    /// Wireframe-like input: transparent black background (alpha 0), thin
    /// opaque white lines — the content class from the 2026-06-11 screenshots
    /// (generator wireframes), where Infrared→QuadMirror killed the frame but
    /// QuadMirror→Infrared rendered. The gradient repro (alpha 1 everywhere)
    /// passes, so alpha across the fused seam is the variable under test.
    fn wireframe_input(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
    ) -> manifold_gpu::GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let on_line = x % 32 < 2 || y % 32 < 2;
                let v = if on_line { 1.0 } else { 0.0 };
                px[i] = f16::from_f32(v);
                px[i + 1] = f16::from_f32(v);
                px[i + 2] = f16::from_f32(v);
                px[i + 3] = f16::from_f32(v); // alpha 0 off-line, like a generator
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "chain-fusion-wireframe-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Same fused-vs-per-card proof on the wireframe-like (alpha-0 background)
    /// input, both chain orders.
    #[test]
    fn infrared_quadmirror_segment_alpha_zero_background() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        // Deliberately NOT 256x256: the gradient_ramp LUT strip is 256 wide,
        // and a 256 canvas can mask cross-resolution sampling bugs by making
        // strip texels and canvas texels coincide.
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        for order in ["ir_qm", "qm_ir"] {
            let mut ir = make_default(PresetTypeId::INFRARED);
            set_param(&mut ir, "amount", 1.0);
            set_param(&mut ir, "palette", 6.0); // Arctic
            let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
            set_param(&mut qm, "amount", 1.0);
            let effects = if order == "ir_qm" {
                vec![ir, qm]
            } else {
                vec![qm, ir]
            };

            let mut per_card = PresetRuntime::try_build(
                &effects, &[], &primitives, &device, None, w, h, None, None,
            )
            .expect("per-card chain builds");

            let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
            let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
            let cards = [
                (view1.canonical_def.as_ref(), view1),
                (view2.canonical_def.as_ref(), view2),
            ];
            if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
                continue;
            }
            let mut fused = PresetRuntime::try_build(
                &effects, &[], &primitives, &device, None, w, h, None, None,
            )
            .expect("fused chain builds");

            // Several STABLE frames: static-param specialization compiles a
            // baked variant once the value-key holds a frame and dispatches it
            // from then on — the steady-state path a live show actually runs.
            // One frame would only ever prove the generic kernel.
            for _ in 0..4 {
                run_once(&mut per_card, &device, &input, &effects, &pc);
                run_once(&mut fused, &device, &input, &effects, &pc);
            }
            let differ = TextureDiff::new(&device);
            let r = differ.compare(
                &device,
                per_card.output_texture().unwrap(),
                fused.output_texture().unwrap(),
                1.0e-2,
                3.0e-2,
            );
            assert!(
                r.passes(0.005) && r.over_count < 64,
                "[{order}] fused must match per-card on alpha-0 background: \
                 max_abs={}, over={}/{}",
                r.max_abs,
                r.over_count,
                r.total
            );
        }
    }

    /// The PRODUCTION swap sequence, end-to-end: build per-card (cold segment
    /// cache), render frames, the background compile lands, rebuild WITH the
    /// running chain as harvest donor, fused segment swaps in, keep rendering.
    /// The shipped guards seed the cache BEFORE the first build, so the
    /// mid-show swap-in (the path the app actually takes) was never proven.
    #[test]
    fn infrared_quadmirror_mid_show_swap_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let build = |prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(
                &effects, &[], &primitives, &device, None, w, h, None, prior,
            )
            .expect("chain builds")
        };

        // Per-card reference, never swapped.
        let mut reference = build(None);
        for _ in 0..6 {
            run_once(&mut reference, &device, &input, &effects, &pc);
        }

        // Production path: per-card frames, then the segment compile lands
        // and the chain rebuilds with the outgoing runtime as donor.
        let mut donor = build(None);
        for _ in 0..3 {
            run_once(&mut donor, &device, &input, &effects, &pc);
        }
        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
            return;
        }
        let mut swapped = build(Some(&mut donor));
        for _ in 0..3 {
            run_once(&mut swapped, &device, &input, &effects, &pc);
        }

        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            reference.output_texture().unwrap(),
            swapped.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "mid-show fused swap must match the per-card chain: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// The GraphTestsV4 layer-1 shape: a DISABLED card sits between the two
    /// enabled cards that fuse (Infrared ON → EdgeStretch OFF → QuadMirror ON).
    /// Segment fusion concatenates enabled cards across the gap; anything that
    /// indexes params by raw chain position would hand the fused kernel the
    /// disabled card's uniforms.
    #[test]
    fn fused_segment_spans_disabled_card_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut es = make_default(PresetTypeId::EDGE_STRETCH);
        es.enabled = false;
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, es, qm];

        let mut per_card = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[2].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
            return;
        }
        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused chain builds");

        for _ in 0..4 {
            run_once(&mut per_card, &device, &input, &effects, &pc);
            run_once(&mut fused, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused segment spanning a disabled card must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// Chain input at a DIFFERENT resolution than the canvas — the app feeds
    /// the chain a generator render target, which the resolution workstream
    /// can size below canvas. The fused kernel reads the chain source as an
    /// external (the cross-resolution sampling path); per-card resamples it
    /// node by node. An unfused QuadMirror in front normalizes resolution and
    /// would mask exactly this class, matching the order dependence reported.
    #[test]
    fn fused_segment_with_half_res_chain_input_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w / 2, h / 2);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let mut per_card = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
            return;
        }
        let mut fused = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("fused chain builds");

        for _ in 0..4 {
            run_once(&mut per_card, &device, &input, &effects, &pc);
            run_once(&mut fused, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused segment with half-res chain input must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// "Flash for a few frames then black" repro (2026-06-12, fusion OFF):
    /// Infrared ALONE on a STATIC input must produce a byte-identical frame
    /// every frame — it has no time dependence. The memo/hoisting path
    /// (gradient_ramp/mux/lut1d are pure+sticky) serves held LUT slots after
    /// the first frame; if a held slot is recycled/evicted/cleared the late
    /// frames go black while frame 0 was correct. Snapshot frame 0, run many
    /// frames, require the late frame to still match.
    #[test]
    fn infrared_alone_static_input_stays_stable_across_frames() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let effects = vec![ir];

        let mut cg = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("chain builds");

        // Frame 0 — the "flash" that looks correct.
        run_once(&mut cg, &device, &input, &effects, &pc);
        let frame0 = snapshot_output(&cg, &device, w, h);

        // Many more frames — the memo/sticky path is now serving held slots.
        for _ in 0..15 {
            run_once(&mut cg, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let drift = differ.compare(
            &device,
            &frame0.texture,
            cg.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            drift.over_count, 0,
            "Infrared on a static input must be frame-stable; a late frame \
             diverging from frame 0 is the flash-then-black bug: \
             max_abs={}, over={}/{}",
            drift.max_abs, drift.over_count, drift.total
        );
    }

    /// The on-stage blackout (2026-06-12): Infrared FOLLOWED BY another card,
    /// fusion off, static input. The chain plan's slot planner returns the
    /// sticky LUT resources' slots to its free pool at `free_after` (it only
    /// exempts persistent resources), so QuadMirror's intermediates share the
    /// LUT's physical texture and stomp it every frame — while the executor's
    /// memo skip keeps serving the latched slot. Infrared LAST works by
    /// accident (nothing runs after it to reuse the slot); this ordering is
    /// the one that goes black.
    #[test]
    fn infrared_before_quadmirror_stays_stable_across_frames() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let qm = make_default(PresetTypeId::QUAD_MIRROR);
        let effects = vec![ir, qm];

        let mut cg = PresetRuntime::try_build(
            &effects, &[], &primitives, &device, None, w, h, None, None,
        )
        .expect("chain builds");

        run_once(&mut cg, &device, &input, &effects, &pc);
        let frame0 = snapshot_output(&cg, &device, w, h);

        for _ in 0..15 {
            run_once(&mut cg, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let drift = differ.compare(
            &device,
            &frame0.texture,
            cg.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            drift.over_count, 0,
            "Infrared before QuadMirror on a static input must be frame-stable; \
             late-frame divergence is the on-stage flash-then-black bug: \
             max_abs={}, over={}/{}",
            drift.max_abs, drift.over_count, drift.total
        );
    }

    /// Membership gate: a rebuild whose ACTIVE CARD SET changed (a card
    /// toggled off) must NOT harvest — the trail holds the removed card's
    /// look, and latching blends would freeze it on screen with no escape
    /// (the on-stage artifact class from 2026-06-11). Same-set rebuilds keep
    /// carrying.
    #[test]
    fn toggle_rebuild_resets_state_same_set_rebuild_carries() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let fb = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        let mut cg = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut cg, "amount", 1.0);
        set_param(&mut cg, "gain", 1.1);
        let both = vec![fb.clone(), cg.clone()];
        // The toggled chain: ColorGrade disabled → not an active card.
        let mut cg_off = cg.clone();
        cg_off.enabled = false;
        let toggled = vec![fb.clone(), cg_off];

        let build = |effects: &[PresetInstance], prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(
                effects, &[], &primitives, &device, None, w, h, None, prior,
            )
            .expect("chain builds")
        };

        const WARM: usize = 6;
        // Donor accumulates a trail through BOTH cards, then the chain
        // rebuilds with ColorGrade toggled off.
        let mut donor = build(&both, None);
        for _ in 0..WARM {
            run_once(&mut donor, &device, &input, &both, &pc);
        }
        let mut after_toggle = build(&toggled, Some(&mut donor));
        run_once(&mut after_toggle, &device, &input, &toggled, &pc);

        // Oracle: the toggled chain built fresh (what a reset looks like).
        let mut fresh = build(&toggled, None);
        run_once(&mut fresh, &device, &input, &toggled, &pc);

        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            fresh.output_texture().unwrap(),
            after_toggle.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            r.over_count, 0,
            "a toggle rebuild must reset state (match a fresh build), not \
             carry the old trail: max_abs={}, over={}/{}",
            r.max_abs, r.over_count, r.total
        );
    }

    /// Upstream-prefix gate: moving a card UPSTREAM of a stateful card
    /// changes what feeds it — its carried trail would be a stale picture of
    /// the old chain (the 2026-06-11 reorder artifact). The rebuild must
    /// reset exactly that card: [FB, CG] reordered to [CG, FB] makes the
    /// harvested chain match a fresh [CG, FB] build.
    #[test]
    fn upstream_reorder_resets_stateful_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let fb = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        let mut cg = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut cg, "amount", 1.0);
        set_param(&mut cg, "gain", 1.1);
        let fb_first = vec![fb.clone(), cg.clone()];
        let cg_first = vec![cg.clone(), fb.clone()];

        let build = |effects: &[PresetInstance], prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(
                effects, &[], &primitives, &device, None, w, h, None, prior,
            )
            .expect("chain builds")
        };

        const WARM: usize = 6;
        let mut donor = build(&fb_first, None);
        for _ in 0..WARM {
            run_once(&mut donor, &device, &input, &fb_first, &pc);
        }
        let mut reordered = build(&cg_first, Some(&mut donor));
        run_once(&mut reordered, &device, &input, &cg_first, &pc);

        let mut fresh = build(&cg_first, None);
        run_once(&mut fresh, &device, &input, &cg_first, &pc);

        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            fresh.output_texture().unwrap(),
            reordered.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            r.over_count, 0,
            "an upstream reorder must reset the feedback card (match a fresh \
             build): max_abs={}, over={}/{}",
            r.max_abs, r.over_count, r.total
        );
    }
}

#[cfg(test)]
mod segment_prewarm_tests {
    //! Project-load segment prewarm shares `classify_segment_member` /
    //! `segment_run` with the chain build — these lock the shared pieces.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    #[test]
    fn segment_run_trims_transparents_and_collects_fuse_indices() {
        use SegmentMember::{Boundary, Fuse, Transparent};
        // Fuse, Transparent, Fuse, Transparent, Boundary: run ends at the
        // boundary, the trailing transparent is trimmed, fuse idxs = [0, 2].
        let members = [Fuse, Transparent, Fuse, Transparent, Boundary];
        let (j, fuse) = segment_run(&members, 0);
        assert_eq!(j, 3, "trailing transparent trimmed back to a plain card");
        assert_eq!(fuse, vec![0, 2]);
    }

    #[test]
    fn prewarm_classifies_stateless_cards_fuse_and_enqueues_without_panicking() {
        let primitives = PrimitiveRegistry::with_builtin();
        let a = make_default(PresetTypeId::COLOR_GRADE);
        let b = make_default(PresetTypeId::INVERT_COLORS);
        assert_eq!(
            classify_segment_member(&a, None, &primitives),
            SegmentMember::Fuse,
            "ColorGrade is a stateless ungrouped card — segment member"
        );
        // A watched card is a boundary — prewarm passes None so nothing is
        // watched at load, but the build-time exclusion must hold.
        assert_eq!(
            classify_segment_member(&a, Some(&a.id), &primitives),
            SegmentMember::Boundary
        );
        // Enqueue-only walk; in cfg(test) the segment lookup stays Pending
        // (no worker) — this locks that the walk itself is panic-free and
        // exercises the same card-list construction as the build.
        prewarm_chain_segments(&[a, b], &[], &primitives);
    }
}
