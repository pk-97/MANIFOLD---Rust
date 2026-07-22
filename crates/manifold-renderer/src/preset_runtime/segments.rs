//! Chain-fusion segment eligibility, run-scan, and the project-load
//! prewarm shared between the chain build and load-time warmup. Extracted
//! from preset_runtime.rs (Wave 3 P3-R, design D3).

use super::*;

/// Build the `(def, view)` slice for a fused segment, augmenting relight-on
/// members with DEFAULT knob values so the segment content key (and fused
/// WGSL) is knob-invariant.
pub(super) fn build_segment_cards(
    fuse_idxs: &[usize],
    active_effects: &[(usize, &PresetInstance)],
    primitives: &PrimitiveRegistry,
) -> Vec<(EffectGraphDef, &'static LoadedPresetView)> {
    let mut cards = Vec::with_capacity(fuse_idxs.len());
    for &k in fuse_idxs {
        let fx = active_effects[k].1;
        let view = loaded_preset_view_by_id(fx.effect_type()).expect("eligibility implies view");
        let def = if fx.relight_active() {
            crate::node_graph::relight::relight_augment(
                fx.graph.as_ref().unwrap_or(&view.canonical_def),
                primitives,
                &RelightParams::default(),
            )
        } else {
            fx.graph.as_ref().unwrap_or(&view.canonical_def).clone()
        };
        cards.push((def, view));
    }
    cards
}

/// Chain-fusion segment eligibility for one card (docs/CHAIN_FUSION_DESIGN.md).
/// Shared between the chain build and the project-load prewarm so the two can
/// never disagree about what forms a segment.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum SegmentMember {
    /// Never joins or spans a segment (watched / grouped / stateful /
    /// string-bound / no view).
    Boundary,
    /// Fusable segment member.
    Fuse,
    /// Currently skipped — splices nothing; transparent to a run.
    Transparent,
}

pub(super) fn classify_segment_member(
    fx: &PresetInstance,
    preview_effect: Option<&EffectId>,
    primitives: &PrimitiveRegistry,
) -> SegmentMember {
    if preview_effect == Some(&fx.id) || fx.group_id.is_some() {
        return SegmentMember::Boundary;
    }
    let Some(view) = loaded_preset_view_by_id(fx.effect_type()) else {
        return SegmentMember::Boundary;
    };
    if is_skipped_for(view.skip_mode, &view.type_id, fx) {
        return SegmentMember::Transparent;
    }
    if view
        .canonical_def
        .preset_metadata
        .as_ref()
        .is_some_and(|m| !m.string_bindings.is_empty())
    {
        return SegmentMember::Boundary;
    }
    let effective = fx.graph.as_ref().unwrap_or(&view.canonical_def);
    if crate::node_graph::freeze::segment::def_is_segment_stateless(effective, primitives) {
        SegmentMember::Fuse
    } else {
        SegmentMember::Boundary
    }
}

/// Scan one maximal segment run starting at `i` (caller guarantees
/// `members[i] == Fuse`): returns `(j, fuse_idxs)` — the exclusive end after
/// trimming trailing transparents, and the fusable indices within `[i, j)`.
pub(super) fn segment_run(members: &[SegmentMember], i: usize) -> (usize, Vec<usize>) {
    let mut j = i;
    while j < members.len() && members[j] != SegmentMember::Boundary {
        j += 1;
    }
    // Trim trailing transparents back into plain cards.
    while j > i && members[j - 1] == SegmentMember::Transparent {
        j -= 1;
    }
    let fuse_idxs = (i..j).filter(|&k| members[k] == SegmentMember::Fuse).collect();
    (j, fuse_idxs)
}

/// Project-load PREWARM (chain fusion): walk one chain's effect list with the
/// exact build-time segmentation and enqueue the background compile for every
/// segment that would form — so the first dispatch of a scene finds its fused
/// view Ready instead of rendering the opening seconds of the show per-card.
/// Enqueue-only and content-keyed: results land through the normal worker →
/// pump → generation-bump → rebuild path, duplicates dedupe in the pending
/// set, and an already-cached segment is a no-op. `preview_effect` is `None` —
/// nothing is watched at load.
pub fn prewarm_chain_segments(
    effects: &[PresetInstance],
    groups: &[EffectGroup],
    primitives: &PrimitiveRegistry,
) {
    use crate::node_graph::freeze::install as freeze_install;
    if !freeze_install::chain_fusion_enabled() {
        return;
    }
    let active = chain_active_effects(effects, groups);
    let members: Vec<SegmentMember> = active
        .iter()
        .map(|(_, fx)| classify_segment_member(fx, None, primitives))
        .collect();
    let mut i = 0;
    while i < active.len() {
        if members[i] != SegmentMember::Fuse {
            i += 1;
            continue;
        }
        let (j, fuse_idxs) = segment_run(&members, i);
        if fuse_idxs.len() >= 2 {
            let cards = build_segment_cards(&fuse_idxs, &active, primitives);
            let _ = freeze_install::fused_segment_view_for(&cards);
        }
        i = j.max(i + 1);
    }
}

/// Project-wide segment prewarm: enqueue background segment compiles for the
/// master chain and every layer chain in `project`. Call once at project load
/// (content thread) — by the time a scene's chain first dispatches, its fused
/// segments are compiled and gate-measured instead of the show opening
/// per-card. Builds its own registry (load-time, off the hot path).
pub fn prewarm_project_chain_segments(project: &manifold_core::project::Project) {
    use crate::node_graph::freeze::install as freeze_install;
    if !freeze_install::chain_fusion_enabled() {
        return;
    }
    let primitives = PrimitiveRegistry::with_builtin();
    static EMPTY_GROUPS: Vec<EffectGroup> = Vec::new();
    prewarm_chain_segments(
        &project.settings.master_effects,
        project.settings.master_effect_groups.as_ref().unwrap_or(&EMPTY_GROUPS),
        &primitives,
    );
    for layer in &project.timeline.layers {
        if let Some(effects) = layer.effects.as_ref() {
            prewarm_chain_segments(
                effects,
                layer.effect_groups.as_ref().unwrap_or(&EMPTY_GROUPS),
                &primitives,
            );
        }
    }
}
