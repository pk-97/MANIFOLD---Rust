//! Fused-build shadow findings. A fused rebuild reports shadowed def params in
//! fused-kernel space, and each one must map back through the view's retarget
//! onto an entry of `shadow_baseline_entries()`. The reverse map must also
//! engage, or the check is vacuous.

use manifold_core::preset_def::PresetKind;
use {
    manifold_node_engine::exec::bound_graph::find_shadowed_def_params,
    manifold_node_engine::exec::bound_graph::shadow_baseline_entries,
    manifold_node_engine::exec::bound_graph::unretarget_shadow, manifold_node_engine::graph::Graph,
    manifold_node_engine::load::chain_spec::splice_def_into_chain,
    manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id,
    manifold_node_engine::param_binding::ResolvedBinding,
    manifold_node_engine::persistence::PrimitiveRegistry,
    manifold_nodes::bundled_presets::bundled_preset_type_ids,
};

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
        let Some(fused) =
            manifold_node_engine::freeze::install::fused_view_for(&base.canonical_def, &base)
        else {
            continue; // doesn't fuse — the unfused sweep above covers it
        };
        let mut graph = Graph::new();
        let source = graph.add_node_named(
            "source",
            Box::new(manifold_node_engine::scene::boundary_nodes::Source::new()),
        );
        let Some(splice) = splice_def_into_chain(
            &mut graph,
            (source, "out"),
            &fused.canonical_def,
            &primitives,
            &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default(),
        ) else {
            panic!("{}: fused def must splice", id.as_str());
        };
        let node_map: Vec<_> = splice
            .handles
            .iter()
            .filter_map(|(_, hid)| {
                graph
                    .get_node(*hid)
                    .map(|inst| (inst.node_id.clone(), *hid))
            })
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
