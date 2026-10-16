//! Chain-fusion segment eligibility, run-scan, and the project-load
//! prewarm shared between the chain build and load-time warmup. Extracted
//! from preset_runtime.rs (Wave 3 P3-R, design D3).

use std::sync::Arc;

use super::*;

/// Build the `(def, view)` slice for a fused segment, augmenting relight-on
/// members with DEFAULT knob values so the segment content key (and fused
/// WGSL) is knob-invariant.
pub(super) fn build_segment_cards(
    fuse_idxs: &[usize],
    active_effects: &[(usize, &PresetInstance)],
    primitives: &PrimitiveRegistry,
) -> Vec<(EffectGraphDef, Arc<LoadedPresetView>)> {
    let mut cards = Vec::with_capacity(fuse_idxs.len());
    for &k in fuse_idxs {
        let fx = active_effects[k].1;
        let view = loaded_preset_view_by_id(fx.effect_type()).expect("eligibility implies view");
        let def = if fx.relight_active() {
            crate::load::augmentation::relight_augment(
                fx.graph.as_ref().unwrap_or(view.canonical_def.as_ref()),
                primitives,
                &RelightParams::default(),
            )
        } else {
            fx.graph
                .as_ref()
                .unwrap_or(view.canonical_def.as_ref())
                .clone()
        };
        cards.push((def, view));
    }
    cards
}

/// A fused segment that cannot be built with every member's params live. The
/// chain build refuses the segment by its cache key and rebuilds its cards one
/// by one.
pub(crate) struct SegmentRefusal {
    pub(crate) key: u64,
    effect_ids: Vec<EffectId>,
    reason: String,
}

impl SegmentRefusal {
    pub(crate) fn new(key: u64, members: &[&PresetInstance], reason: String) -> Self {
        Self { key, effect_ids: members.iter().map(|fx| fx.id.clone()).collect(), reason }
    }
}

impl std::fmt::Display for SegmentRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<&str> = self.effect_ids.iter().map(|id| id.as_str()).collect();
        write!(f, "segment of effects [{}] refused: {}", ids.join(", "), self.reason)
    }
}

/// Every member card's static and user bindings, resolved against the segment
/// splice (`graph` + `node_map`), in member order. `Err` names each binding
/// that did not resolve; the caller refuses the segment rather than build a
/// member with params dropped.
pub(crate) fn resolve_segment_bindings(
    view: &crate::freeze::install::SegmentView,
    members: &[&PresetInstance],
    graph: &Graph,
    node_map: &[(NodeId, NodeInstanceId)],
) -> Result<Vec<Vec<ResolvedBinding>>, String> {
    let mut resolved = Vec::with_capacity(members.len());
    let mut missing: Vec<String> = Vec::new();
    for (seg_idx, fx) in members.iter().enumerate() {
        let prefix = crate::freeze::segment::card_prefix(seg_idx);
        let Some(card_static) = view.card_bindings.get(seg_idx) else {
            missing.push(format!("{} has no bindings in the segment", fx.id.as_str()));
            continue;
        };
        let user_bindings = fx.user_param_bindings();
        let mut bindings = Vec::with_capacity(card_static.len() + user_bindings.len());
        for b in card_static {
            match ResolvedBinding::from_static(b, node_map) {
                Some(rb) => bindings.push(rb),
                None => missing.push(format!("{} param {}", fx.id.as_str(), b.id.as_ref())),
            }
        }
        for core in user_bindings.iter() {
            // Repoint into the segment namespace: a fused-away target goes
            // through the segment retarget map; a surviving target resolves
            // by prefixing its stable node id.
            let prefixed_key = (format!("{prefix}{}", core.node_id.as_str()), core.inner_param.clone());
            let mut c = core.clone();
            if let Some((fused_id, field)) = view.retarget.get(&prefixed_key) {
                c.node_id = fused_id.clone();
                c.inner_param = field.clone();
                c.convert = crate::freeze::install::convert_for_fused_field(c.convert);
            } else {
                c.node_id = NodeId::new(prefixed_key.0.clone());
            }
            match ResolvedBinding::from_user(&c, graph, node_map) {
                Some(rb) => bindings.push(rb),
                None => missing.push(format!("{} user param {}", fx.id.as_str(), core.id)),
            }
        }
        resolved.push(bindings);
    }
    if missing.is_empty() {
        Ok(resolved)
    } else {
        Err(format!("bindings did not resolve: {}", missing.join("; ")))
    }
}

manifold_core::testkit_visible! {
/// Chain-fusion segment eligibility for one card (docs/CHAIN_FUSION_DESIGN.md).
/// Shared between the chain build and the project-load prewarm so the two can
/// never disagree about what forms a segment.
#[derive(Clone, Copy, PartialEq, Debug)]
#[doc(hidden)]
pub(crate) enum SegmentMember {
    /// Never joins or spans a segment (watched / grouped / stateful /
    /// string-bound / no view).
    Boundary,
    /// Fusable segment member.
    Fuse,
}
}

manifold_core::testkit_visible! {
#[doc(hidden)]
pub(crate) fn classify_segment_member(
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
    if view
        .canonical_def
        .preset_metadata
        .as_ref()
        .is_some_and(|m| !m.string_bindings.is_empty())
    {
        return SegmentMember::Boundary;
    }
    let effective = fx.graph.as_ref().unwrap_or(view.canonical_def.as_ref());
    if crate::freeze::segment::def_is_segment_stateless(effective, primitives) {
        SegmentMember::Fuse
    } else {
        SegmentMember::Boundary
    }
}
}

manifold_core::testkit_visible! {
/// Scan one maximal segment run starting at `i` (caller guarantees
/// `members[i] == Fuse`): returns `(j, fuse_idxs)` — the exclusive end
/// and the fusable indices within `[i, j)`.
#[doc(hidden)]
pub(crate) fn segment_run(members: &[SegmentMember], i: usize) -> (usize, Vec<usize>) {
    let mut j = i;
    while j < members.len() && members[j] != SegmentMember::Boundary {
        j += 1;
    }
    let fuse_idxs = (i..j).filter(|&k| members[k] == SegmentMember::Fuse).collect();
    (j, fuse_idxs)
}
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
    use crate::freeze::install as freeze_install;
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
    use crate::freeze::install as freeze_install;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::freeze::install::{self, SegmentLookup, SegmentView};
    use crate::param_binding::{ParamBinding, ParamConvert, ParamId, ParamTarget};
    use std::borrow::Cow;

    fn binding(id: &'static str, node: &str) -> ParamBinding {
        ParamBinding {
            id: ParamId::from(id),
            label: Cow::Borrowed("Param"),
            default_value: 0.0,
            target: ParamTarget::Node { node_id: NodeId::new(node), param: Cow::Borrowed("gain") },
            convert: ParamConvert::Float,
            scale: 1.0,
            offset: 0.0,
            min: 0.0,
            max: 1.0,
            curve: manifold_core::macro_bank::MacroCurve::Linear,
            invert: false,
            default_mirrors_node_param: false,
        }
    }

    fn passthrough_def(name: &str) -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 3, "name": name,
            "nodes": [
                {"id": 0, "nodeId": "source", "typeId": "system.source"},
                {"id": 1, "nodeId": "final_output", "typeId": "system.final_output"}
            ],
            "wires": [{"fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in"}]
        }))
        .expect("passthrough def parses")
    }

    fn view_of(def: &EffectGraphDef, type_id: &'static str) -> Arc<LoadedPresetView> {
        Arc::new(LoadedPresetView {
            type_id: PresetTypeId::new(type_id),
            canonical_def: Arc::new(def.clone()),
            bindings: vec![binding("gain", "map")],
            fused_retarget: Default::default(),
            mesh_rules: Default::default(),
        })
    }

    /// A segment member whose binding has no node in the fused splice refuses
    /// the whole segment, naming the effect, and the next lookup of that
    /// segment is Refused, so the build splices every member on its own with
    /// its own bindings. A neighbour can never leave a card's params dead.
    #[test]
    fn unresolvable_member_binding_refuses_the_segment() {
        let members = [
            PresetInstance::new(PresetTypeId::new("SegA")),
            PresetInstance::new(PresetTypeId::new("SegB")),
        ];
        let member_refs: Vec<&PresetInstance> = members.iter().collect();
        let graph = Graph::new();
        let node_map = vec![(NodeId::new("c0.map"), NodeInstanceId(0))];
        let mut view = SegmentView {
            def: passthrough_def("segment"),
            card_bindings: vec![vec![binding("gain", "c0.map")], vec![binding("gain", "c1.map")]],
            retarget: Default::default(),
            mesh_rules: Default::default(),
        };

        let err = resolve_segment_bindings(&view, &member_refs, &graph, &node_map)
            .expect_err("a member binding with no node must refuse the segment");
        assert!(err.contains(members[1].id.as_str()), "refusal names the member: {err}");
        assert!(!err.contains(members[0].id.as_str()), "only the broken member is named: {err}");

        view.card_bindings[1] = vec![binding("gain", "c0.map")];
        let resolved = resolve_segment_bindings(&view, &member_refs, &graph, &node_map)
            .expect("every binding resolves");
        assert_eq!(resolved.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1]);

        let defs = [passthrough_def("refused-a"), passthrough_def("refused-b")];
        let cards: Vec<(EffectGraphDef, Arc<LoadedPresetView>)> = defs
            .iter()
            .zip(["SegA", "SegB"])
            .map(|(d, t)| (d.clone(), view_of(d, t)))
            .collect();
        let refs: Vec<_> = cards.iter().map(|(d, v)| (d, v.as_ref())).collect();
        install::refuse_segment(install::segment_key(&refs));
        assert!(
            matches!(install::fused_segment_view_for(&cards), SegmentLookup::Refused),
            "a refused segment must build its cards one by one",
        );
    }

    /// The segment and fused-effect cache keys cover the bindings a compiled
    /// view carries: the same graph with a different binding list is a
    /// different entry, so no card inherits another card's bindings.
    #[test]
    fn fused_cache_keys_cover_bindings() {
        let def = passthrough_def("keyed");
        let a = view_of(&def, "KeyA");
        let b = view_of(&def, "KeyA");
        assert_eq!(
            install::fused_effect_cache_key(&def, &a),
            install::fused_effect_cache_key(&def, &b),
            "equal inputs share an entry",
        );
        let mut scaled = view_of(&def, "KeyA");
        Arc::get_mut(&mut scaled).expect("unique").bindings[0].scale = 2.0;
        assert_ne!(
            install::fused_effect_cache_key(&def, &a),
            install::fused_effect_cache_key(&def, &scaled),
        );
        assert_ne!(
            install::fused_effect_cache_key(&def, &a),
            install::fused_effect_cache_key(&def, &view_of(&def, "KeyC")),
        );
        assert_ne!(
            install::segment_key(&[(&def, a.as_ref()), (&def, a.as_ref())]),
            install::segment_key(&[(&def, a.as_ref()), (&def, scaled.as_ref())]),
        );
    }
}
