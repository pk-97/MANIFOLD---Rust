//! One-time load migration: backfill `UserParamBinding.node_id`.
//!
//! Pre-node-id projects addressed inner-graph user bindings by handle.
//! That handle is captured on load into
//! [`UserParamBinding::legacy_node_handle`](manifold_core::effects::UserParamBinding::legacy_node_handle);
//! this pass resolves it to the inner node's stable [`NodeId`] against
//! the graph the effect actually renders with — the per-instance
//! override ([`EffectInstance::graph`]) if it diverged, otherwise the
//! canonical bundled preset for the effect's type.
//!
//! Node identity itself is normalized earlier: override-def nodes get
//! `node_id == handle` from [`manifold_core::project::Project`]'s
//! load-time normalization, and the canonical bundled presets ship
//! pre-stamped. So both graphs already map handle → id by the time this
//! runs — it only has to copy the right id onto each binding.
//!
//! This is a versioned load upgrade, not a runtime fallback: it runs
//! once when a project is loaded, the runtime resolver only ever reads
//! `node_id`, and a resolved binding clears its legacy handle so the
//! upgrade never re-triggers after a save. A handle that can't be
//! resolved (effect type unknown to this build, or the node was deleted)
//! is left untouched — the binding stays inert, not silently dropped, so
//! a future load with the right graph present can still recover it.

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode};
use manifold_core::effects::EffectInstance;
use manifold_core::project::Project;

use crate::node_graph::bundled_presets::bundled_preset_def;

/// Backfill `node_id` on every user binding in the project that still
/// carries a pre-node-id handle. Walks master, layer, and clip effects —
/// the same surface [`Project::find_effect_by_id_mut`] covers. See module
/// docs.
pub fn migrate_user_param_bindings_to_node_id(project: &mut Project) {
    for fx in &mut project.settings.master_effects {
        migrate_effect(fx);
    }
    for layer in &mut project.timeline.layers {
        if let Some(effects) = layer.effects.as_mut() {
            for fx in effects.iter_mut() {
                migrate_effect(fx);
            }
        }
        for clip in &mut layer.clips {
            for fx in &mut clip.effects {
                migrate_effect(fx);
            }
        }
    }
}

fn migrate_effect(fx: &mut EffectInstance) {
    // Nothing to do unless some binding carries an unresolved legacy
    // handle. Keeps the common (already-migrated / no-user-bindings)
    // case free of any graph lookup.
    let needs_migration = fx
        .user_param_bindings
        .iter()
        .any(|b| b.node_id.is_empty() && b.legacy_node_handle.is_some());
    if !needs_migration {
        // Clear any stale legacy handles on already-resolved bindings so a
        // re-save drops the dead `nodeHandle` key.
        for b in &mut fx.user_param_bindings {
            if !b.node_id.is_empty() {
                b.legacy_node_handle = None;
            }
        }
        return;
    }

    // Resolve handles against the graph this instance renders with: the
    // per-instance override if it diverged, else the canonical bundled
    // preset. Both already carry `node_id == handle` on every node —
    // overrides via `Project::normalize_override_node_ids` at core load,
    // bundled presets via the on-disk stamp — so this is a pure read.
    let effect_type = fx.effect_type().clone();
    let handle_to_id = match fx.graph.as_ref() {
        Some(def) => handle_id_map(def),
        None => match bundled_preset_def(&effect_type) {
            Some(def) => handle_id_map(def),
            // Effect type unknown to this build: leave the bindings
            // untouched for a future load that has the preset.
            None => return,
        },
    };

    for b in &mut fx.user_param_bindings {
        if !b.node_id.is_empty() {
            b.legacy_node_handle = None;
            continue;
        }
        let Some(handle) = b.legacy_node_handle.as_deref() else {
            continue;
        };
        if let Some(id) = handle_to_id.get(handle) {
            b.node_id = id.clone();
            b.legacy_node_handle = None;
        }
        // Unresolved handle: leave both fields as-is (inert binding,
        // recoverable on a future load). No silent data loss.
    }
}

/// Build a `handle → NodeId` map over every node in the def, recursing
/// into group bodies. Keyed by the node's own (unprefixed) handle, which
/// is what a pre-node-id binding stored. Pre-node-id data is flat (groups
/// postdate it), so collisions across group boundaries don't arise in
/// practice; if one ever did, the first node wins — best-effort, never a
/// panic.
fn handle_id_map(def: &EffectGraphDef) -> ahash::AHashMap<String, NodeId> {
    let mut map = ahash::AHashMap::default();
    collect_handle_ids(&def.nodes, &mut map);
    map
}

fn collect_handle_ids(nodes: &[EffectGraphNode], map: &mut ahash::AHashMap<String, NodeId>) {
    for node in nodes {
        if let Some(handle) = node.handle.as_deref()
            && !node.node_id.is_empty()
        {
            map.entry(handle.to_string())
                .or_insert_with(|| node.node_id.clone());
        }
        if let Some(group) = node.group.as_ref() {
            collect_handle_ids(&group.nodes, map);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::EffectTypeId;
    use manifold_core::effects::{EffectInstance, ParamConvert, UserParamBinding};

    /// A pre-node-id user binding: empty `node_id`, target carried in
    /// `legacy_node_handle` (the shape an old project deserializes into).
    fn legacy_binding(handle: &str, inner: &str) -> UserParamBinding {
        UserParamBinding {
            id: format!("user.{handle}.{inner}.1"),
            label: inner.to_string(),
            node_id: NodeId::default(),
            legacy_node_handle: Some(handle.to_string()),
            inner_param: inner.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
        }
    }

    #[test]
    fn graph_none_binding_resolves_against_canonical_preset() {
        // Bloom is a `graph: None` effect: its graph IS the bundled
        // preset, whose nodes are stamped with `nodeId == handle`. A
        // legacy binding to handle "blur" must come out targeting the
        // canonical node's id (== "blur" by the stamp convention).
        let mut project = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.user_param_bindings.push(legacy_binding("blur", "radius"));
        project.settings.master_effects.push(fx);

        migrate_user_param_bindings_to_node_id(&mut project);

        let b = &project.settings.master_effects[0].user_param_bindings[0];
        let canonical = bundled_preset_def(&EffectTypeId::BLOOM).expect("Bloom preset present");
        let blur = canonical
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("blur"))
            .expect("Bloom has a `blur` node");
        assert!(!blur.node_id.is_empty(), "preset node must be stamped");
        assert_eq!(b.node_id, blur.node_id, "binding now targets the node id");
        assert_eq!(b.legacy_node_handle, None, "legacy handle cleared");
    }

    #[test]
    fn graph_override_binding_resolves_against_overrides_own_nodes() {
        // A `graph: Some(def)` override: the binding must resolve against
        // the OVERRIDE's nodes, not the bundled preset. Core load
        // normalization has already stamped `node_id == handle` on the
        // override node (simulated here), so the migration just copies it
        // onto the binding.
        use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION, EffectGraphNode};
        use std::collections::{BTreeMap, BTreeSet};

        let node = EffectGraphNode {
            id: 0,
            node_id: NodeId::new("softblur"), // normalized at core load
            type_id: "node.blur".to_string(),
            handle: Some("softblur".to_string()),
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![node],
            wires: vec![],
        };
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.graph = Some(def);
        // "softblur" exists only on the override, never on Bloom's preset.
        fx.user_param_bindings
            .push(legacy_binding("softblur", "radius"));
        let mut project = Project::default();
        project.settings.master_effects.push(fx);

        migrate_user_param_bindings_to_node_id(&mut project);

        let b = &project.settings.master_effects[0].user_param_bindings[0];
        assert_eq!(b.node_id, "softblur", "binding targets the override node id");
        assert_eq!(b.legacy_node_handle, None);
    }

    #[test]
    fn already_migrated_binding_is_left_alone_and_handle_cleared() {
        // A binding that already carries a node id (post-cutover save)
        // must not be touched, except to drop any stale legacy handle.
        let mut project = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        let mut b = legacy_binding("blur", "radius");
        b.node_id = NodeId::new("explicit_id");
        fx.user_param_bindings.push(b);
        project.settings.master_effects.push(fx);

        migrate_user_param_bindings_to_node_id(&mut project);

        let b = &project.settings.master_effects[0].user_param_bindings[0];
        assert_eq!(b.node_id, "explicit_id", "explicit id preserved");
        assert_eq!(b.legacy_node_handle, None, "stale legacy handle cleared");
    }

    #[test]
    fn unresolved_handle_is_left_inert_not_dropped() {
        // A handle that matches no node in the graph: the binding stays
        // unmigrated (empty id, handle retained) so a future load can
        // recover it. Never silently dropped.
        let mut project = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.user_param_bindings
            .push(legacy_binding("ghost_node", "radius"));
        project.settings.master_effects.push(fx);

        migrate_user_param_bindings_to_node_id(&mut project);

        let b = &project.settings.master_effects[0].user_param_bindings[0];
        assert!(b.node_id.is_empty(), "unresolved binding stays inert");
        assert_eq!(
            b.legacy_node_handle.as_deref(),
            Some("ghost_node"),
            "handle retained for future recovery"
        );
    }
}
