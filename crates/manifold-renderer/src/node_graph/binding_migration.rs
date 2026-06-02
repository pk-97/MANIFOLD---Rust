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
//! Old override defs may carry nodes without ids (they predate node-id
//! targeting); those get ids minted here and persisted, since the
//! override lives in the project file. The canonical bundled presets
//! ship pre-stamped, so `graph: None` instances resolve directly.
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
    // override if it diverged (minting ids for any node that lacks one
    // first, so the map has ids to hand out and they persist), else the
    // canonical bundled preset.
    let effect_type = fx.effect_type().clone();
    let handle_to_id = match fx.graph.as_mut() {
        Some(def) => {
            ensure_node_ids(def);
            handle_id_map(def)
        }
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

/// Mint a [`NodeId`] for any node in `def` — recursively, including group
/// bodies — that lacks one. Old override defs predate node-id targeting,
/// so their nodes load with empty ids; a migrated override needs stable
/// ids for its bindings to resolve against (and to persist on next save).
fn ensure_node_ids(def: &mut EffectGraphDef) {
    ensure_node_ids_in(&mut def.nodes);
}

fn ensure_node_ids_in(nodes: &mut [EffectGraphNode]) {
    for node in nodes {
        if node.node_id.is_empty() {
            node.node_id = NodeId::new(manifold_core::short_id());
        }
        if let Some(group) = node.group.as_mut() {
            ensure_node_ids_in(&mut group.nodes);
        }
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
