//! Corpus gate for BUG-1l7f (imported-def param no-op): the set of shipped
//! presets that bake a node param their own card binding throws away is exactly
//! `SHADOW_BASELINE`, no more and no fewer.
//!
//! `BoundGraph::new` plants every card binding's declared default onto its
//! target node, so a def-baked value on a card-owned param is dead on arrival —
//! silently, with no error and no wrong pixel until much later. The detector is
//! `find_shadowed_def_params`, and the ten known offenders are allowlisted in
//! product code so they don't drown the live diagnostic log. This test is what
//! keeps that allowlist honest in both directions: a NEW preset with the
//! disagreement fails, and so does an allowlist entry that no longer reproduces
//! (i.e. one that was cleaned up, or renamed out from under the entry).
//!
//! Fixing a new failure means picking which value is the truth: either change
//! the binding's `default_value` (and its matching `ParamSpecDef`) to the baked
//! value, or delete the baked node param. Both routes end with one number, not
//! two.

use manifold_core::preset_def::PresetKind;
use manifold_renderer::node_graph::{
    Graph, PrimitiveRegistry, ResolvedBinding, ShadowedDefParam, bundled_preset_def,
    bundled_preset_type_ids, find_shadowed_def_params, loaded_preset_view_by_id,
    shadow_baseline_entries, splice_def_into_chain, unretarget_shadow,
};

/// Effects splice into a chain, so build the graph the way the chain builder
/// does — a bare `Source` upstream, then the canonical def — and resolve the
/// view's static card bindings against the spliced node map. Same two inputs
/// `PresetRuntime::try_build` hands the detector, minus the GPU.
fn effect_findings(type_id: &manifold_core::PresetTypeId) -> Vec<ShadowedDefParam> {
    let Some(view) = loaded_preset_view_by_id(type_id) else {
        return Vec::new();
    };
    let primitives = PrimitiveRegistry::with_builtin();
    let mut graph = Graph::new();
    let source = graph.add_node_named(
        "source",
        Box::new(manifold_renderer::node_graph::Source::new()),
    );
    let Some(splice) = splice_def_into_chain(
        &mut graph,
        (source, "out"),
        &view.canonical_def,
        &primitives,
        None,
    ) else {
        panic!("{}: canonical def must splice", type_id.as_str());
    };
    let node_map: Vec<_> = splice
        .handles
        .iter()
        .filter_map(|(_, id)| graph.get_node(*id).map(|inst| (inst.node_id.clone(), *id)))
        .collect();
    let bindings: Vec<ResolvedBinding> = view
        .bindings
        .iter()
        .filter_map(|b| ResolvedBinding::from_static(b, &node_map))
        .collect();
    find_shadowed_def_params(&view.canonical_def, &bindings, &graph)
}

/// Generators build through the real `PresetRuntime::from_def` (mock executor, no
/// GPU) and report their findings off the runtime itself, so this half of the
/// sweep exercises the production build path rather than a copy of it.
fn generator_findings(type_id: &manifold_core::PresetTypeId) -> Vec<ShadowedDefParam> {
    let Some(def) = bundled_preset_def(type_id) else {
        return Vec::new();
    };
    let primitives = PrimitiveRegistry::with_builtin();
    let Ok(runtime) =
        manifold_renderer::preset_runtime::PresetRuntime::from_def(def.clone(), &primitives, None)
    else {
        // Load failures are another test's business.
        return Vec::new();
    };
    runtime.shadowed_def_params().cloned().collect()
}

#[test]
fn bundled_preset_card_binding_shadows_match_the_baseline_exactly() {
    let mut found: Vec<(String, ShadowedDefParam)> = Vec::new();
    for id in bundled_preset_type_ids(PresetKind::Effect) {
        found.extend(
            effect_findings(&id)
                .into_iter()
                .map(|f| (id.as_str().to_string(), f)),
        );
    }
    for id in bundled_preset_type_ids(PresetKind::Generator) {
        found.extend(
            generator_findings(&id)
                .into_iter()
                .map(|f| (id.as_str().to_string(), f)),
        );
    }

    let baseline = shadow_baseline_entries();
    let unlisted: Vec<String> = found
        .iter()
        .filter(|(preset, f)| {
            !baseline
                .iter()
                .any(|(p, n, param)| p == preset && *n == f.node_id && *param == f.param)
        })
        .map(|(preset, f)| format!("{preset}: {f}"))
        .collect();
    let stale: Vec<String> = baseline
        .iter()
        .filter(|(p, n, param)| {
            !found
                .iter()
                .any(|(preset, f)| *p == preset && *n == f.node_id && *param == f.param)
        })
        .map(|(p, n, param)| format!("{p}: {n}.{param}"))
        .collect();

    assert!(
        unlisted.is_empty(),
        "{} preset param(s) are baked on a node AND owned by a card binding that \
         overwrites them at build (BUG-1l7f). The node value never renders — pick \
         one number, don't add these to SHADOW_BASELINE:\n{}",
        unlisted.len(),
        unlisted.join("\n"),
    );
    assert!(
        stale.is_empty(),
        "{} SHADOW_BASELINE entr(ies) no longer reproduce — the preset was cleaned \
         up or the node/param was renamed. Delete them from SHADOW_BASELINE in \
         node_graph/bound_graph.rs:\n{}",
        stale.len(),
        stale.join("\n"),
    );
}

/// The fused build of a baselined preset reports shadow findings in fused-kernel
/// space (`fused_region_0.n5_amount`), which never matched the baseline's
/// original names (`grade_mix.amount`) — so every fused rebuild of ColorGrade
/// re-logged known dead residue as a fresh `[chain-error]` (the graph-editor
/// spam of 2026-08). Every fused-space finding must map back through the view's
/// retarget onto a baseline entry, and the reverse map must actually engage.
#[test]
fn fused_effect_shadow_findings_map_back_to_the_baseline() {
    let primitives = PrimitiveRegistry::with_builtin();
    let baseline = shadow_baseline_entries();
    let mut saw_retargeted = false;
    for id in bundled_preset_type_ids(PresetKind::Effect) {
        let Some(base) = loaded_preset_view_by_id(&id) else {
            continue;
        };
        let Some(fused) = manifold_renderer::node_graph::freeze::install::fused_view_for(
            &base.canonical_def,
            base,
        ) else {
            continue; // doesn't fuse — the unfused sweep above covers it
        };
        let mut graph = Graph::new();
        let source = graph.add_node_named(
            "source",
            Box::new(manifold_renderer::node_graph::Source::new()),
        );
        let Some(splice) = splice_def_into_chain(
            &mut graph,
            (source, "out"),
            &fused.canonical_def,
            &primitives,
            None,
        ) else {
            panic!("{}: fused def must splice", id.as_str());
        };
        let node_map: Vec<_> = splice
            .handles
            .iter()
            .filter_map(|(_, hid)| graph.get_node(*hid).map(|inst| (inst.node_id.clone(), *hid)))
            .collect();
        let bindings: Vec<ResolvedBinding> = fused
            .bindings
            .iter()
            .filter_map(|b| ResolvedBinding::from_static(b, &node_map))
            .collect();
        for finding in find_shadowed_def_params(&fused.canonical_def, &bindings, &graph) {
            let canonical = unretarget_shadow(&finding, &fused.fused_retarget);
            if canonical != finding {
                saw_retargeted = true;
            }
            assert!(
                baseline.iter().any(|(p, n, param)| *p == id.as_str()
                    && *n == canonical.node_id
                    && *param == canonical.param),
                "{}: fused build reports `{finding}` which maps back to `{}`.{} — not a \
                 baseline entry, so this would log a fresh [chain-error] on every fused \
                 rebuild. Pick one number (see the sweep test above) or extend the baseline.",
                id.as_str(),
                canonical.node_id,
                canonical.param,
            );
        }
    }
    assert!(
        saw_retargeted,
        "no fused finding exercised the reverse map — did everything stop fusing?"
    );
}
