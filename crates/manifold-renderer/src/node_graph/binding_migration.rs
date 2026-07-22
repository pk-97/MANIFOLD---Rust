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

use crate::node_graph::bundled_presets::bundled_preset_def;

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

    let mut lifted: EffectGraphDef = canonical.clone();
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

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
    };
    use manifold_core::effects::{PresetInstance, ParamConvert};

    /// Build a metadata-only stub graph carrying one user-added binding —
    /// the exact shape the v1.3→v1.4 JSON fold-in emits when an effect had
    /// `userParamBindings` but no per-instance graph.
    fn stub_with_user_binding(node_handle: &str, inner: &str, id: &str) -> EffectGraphDef {
        let meta = PresetMetadata {
            id: PresetTypeId::new(""),
            display_name: String::new(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: vec![ParamSpecDef {
                id: id.to_string(),
                name: inner.to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 0.0,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: Vec::new(),
                format_string: None,
                osc_suffix: String::new(),
                curve: Default::default(),
                invert: false,
                is_angle: false,
                is_trigger_gate: false,
                wraps: false,
                section: None,
                card_visible: true,
            }],
            bindings: vec![BindingDef {
                id: id.to_string(),
                label: inner.to_string(),
                default_value: 0.0,
                // `node_id == handle` — the convention the canonical
                // preset stamp uses, so this resolves after the lift.
                target: BindingTarget::Node {
                    node_id: manifold_core::NodeId::new(node_handle),
                    param: inner.to_string(),
                },
                convert: ParamConvert::Float,
                user_added: true,
                scale: 1.0,
                offset: 0.0,
            }],
            skip_mode: Default::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        };
        EffectGraphDef {
            version: 0,
            name: None,
            description: None,
            preset_metadata: Some(meta),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    #[test]
    fn stub_graph_is_completed_with_canonical_topology() {
        // Bloom is a `graph: None` effect. A migrated user binding for it
        // arrives as a metadata-only stub; the lift must restore Bloom's
        // canonical nodes while keeping the user binding.
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.graph = Some(stub_with_user_binding("blur", "radius", "user.blur.radius.1"));
        project.settings.master_effects.push(fx);

        migrate_user_param_bindings_to_node_id(&mut project);

        let g = project.settings.master_effects[0]
            .graph
            .as_ref()
            .expect("graph present");
        assert!(!g.nodes.is_empty(), "canonical topology lifted in");
        let meta = g.preset_metadata.as_ref().expect("metadata present");
        assert!(
            meta.bindings.iter().any(|b| b.id == "user.blur.radius.1" && b.user_added),
            "user binding preserved on the lifted graph"
        );
        // The user binding's value slot still resolves by id.
        assert!(
            project.settings.master_effects[0]
                .user_param_bindings()
                .iter()
                .any(|b| b.id == "user.blur.radius.1"),
            "user binding enumerates from the lifted graph"
        );
    }

    #[test]
    fn already_completed_graph_is_left_alone() {
        // A graph that already has nodes (real override, or a re-loaded
        // completed stub) must not be re-lifted — idempotency.
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        let canonical = bundled_preset_def(&PresetTypeId::BLOOM)
            .expect("Bloom preset present")
            .clone();
        let node_count = canonical.nodes.len();
        fx.graph = Some(canonical);
        project.settings.master_effects.push(fx);

        migrate_user_param_bindings_to_node_id(&mut project);

        assert_eq!(
            project.settings.master_effects[0].graph.as_ref().unwrap().nodes.len(),
            node_count,
            "completed graph untouched"
        );
    }

    #[test]
    fn graph_none_effect_is_left_alone() {
        // No graph, no migration — a plain effect stays `graph: None`.
        let mut project = Project::default();
        let fx = PresetInstance::new(PresetTypeId::BLOOM);
        project.settings.master_effects.push(fx);

        migrate_user_param_bindings_to_node_id(&mut project);

        assert!(project.settings.master_effects[0].graph.is_none());
    }
}
