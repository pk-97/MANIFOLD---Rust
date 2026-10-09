//! One-time load completion for migrated effect user bindings.
//!
//! User-added effect bindings used to live in a parallel
//! `PresetInstance.user_param_bindings` Vec. The binding-storage
//! unification (`PRESET_UNIFICATION_PLAN.md` step 3) folds them into the
//! per-instance graph's `preset_metadata.bindings` (`user_added`), the
//! same single list generators already use. The on-disk JSON fold-in
//! lives in `manifold-io`'s v1.3→v1.4 migration — but that layer can't
//! build a preset's node graph (the topology is renderer-side, compiled
//! in). So when the JSON migration meets an effect with user bindings and
//! no per-instance graph, it can only emit a **metadata-only stub** graph
//! (the bindings + their specs, but no nodes).
//!
//! This pass completes those stubs at load: it lifts the effect's
//! canonical bundled topology into any graph that carries
//! `preset_metadata` but no nodes, preserving the migrated user-added
//! bindings/params on top. After this runs the effect renders exactly as
//! it did pre-migration (canonical topology) with the user bindings
//! addressing the canonical nodes by their stable, handle-stamped ids.
//!
//! Legacy-handle resolution itself is no longer this pass's job: a
//! pre-node-id binding deserializes through `BindingTarget`'s tolerant
//! reader, which upgrades the old `handleNode` form to `Node { node_id ==
//! handle }`, and the canonical preset nodes are stamped `node_id ==
//! handle`, so the migrated binding resolves against the lifted topology
//! with no extra step.
//!
//! Idempotent: a completed graph has nodes, so a second load (or a
//! re-save) finds nothing to lift.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;

use crate::load::catalog_source::preset_def as bundled_preset_def;

/// Complete every metadata-only stub graph produced by the v1.3→v1.4
/// user-binding fold-in. Walks master, layer, and clip effects — the same
/// surface [`Project::find_effect_by_id_mut`] covers. See module docs.
pub fn migrate_user_param_bindings_to_node_id(project: &mut Project) {
    for fx in &mut project.settings.master_effects {
        complete_stub_graph(fx);
    }
    for layer in &mut project.timeline.layers {
        if let Some(effects) = layer.effects.as_mut() {
            for fx in effects.iter_mut() {
                complete_stub_graph(fx);
            }
        }
        for clip in &mut layer.clips {
            for fx in &mut clip.effects {
                complete_stub_graph(fx);
            }
        }
    }
}

/// If `fx.graph` is a metadata-only stub (has `preset_metadata` but no
/// nodes), lift the effect's canonical topology underneath the migrated
/// metadata. No-op for graphs that already carry nodes (real per-instance
/// overrides, or already-completed stubs) and for `graph: None` effects.
fn complete_stub_graph(fx: &mut PresetInstance) {
    let needs_lift = fx
        .graph
        .as_ref()
        .is_some_and(|g| g.preset_metadata.is_some() && g.nodes.is_empty());
    if !needs_lift {
        return;
    }
    let effect_type = fx.effect_type().clone();
    let Some(canonical) = bundled_preset_def(&effect_type) else {
        // Effect type unknown to this build: leave the stub as-is so a
        // future load with the preset present can complete it. The
        // bindings stay inert, never silently dropped.
        return;
    };

    // Take the migrated metadata off the stub, then rebuild the graph from
    // the canonical topology with that metadata layered on top. The
    // canonical's own preset_metadata is the base (static params +
    // bindings); the migrated user-added entries append to it.
    let stub_meta = fx
        .graph
        .as_mut()
        .and_then(|g| g.preset_metadata.take())
        .expect("needs_lift checked preset_metadata is Some");

    let mut lifted: EffectGraphDef = canonical.as_ref().clone();
    match lifted.preset_metadata.as_mut() {
        Some(canon_meta) => {
            // Append only the user-added entries from the stub — the
            // canonical metadata already carries the static prefix.
            for b in stub_meta.bindings.into_iter().filter(|b| b.user_added) {
                if !canon_meta.bindings.iter().any(|x| x.id == b.id) {
                    // Pull the matching spec across too.
                    if let Some(spec) =
                        stub_meta.params.iter().find(|p| p.id == b.id).cloned()
                        && !canon_meta.params.iter().any(|p| p.id == spec.id)
                    {
                        canon_meta.params.push(spec);
                    }
                    canon_meta.bindings.push(b);
                }
            }
        }
        None => {
            // Canonical has no metadata (unusual) — adopt the stub's whole
            // metadata so the user bindings survive.
            lifted.preset_metadata = Some(stub_meta);
        }
    }
    fx.graph = Some(lifted);
}
