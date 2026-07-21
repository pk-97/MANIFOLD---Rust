//! Graph mutation commands — Phase 3 of per-card divergence,
//! generalized to support both effect graphs and generator graphs.
//!
//! Each command operates on the `EffectGraphDef` that a
//! [`manifold_core::GraphTarget`] points at. Targets resolve to:
//!
//! - [`GraphTarget::Effect`] → [`PresetInstance::graph`] with
//!   `PresetInstance::graph_version` as the version counter.
//! - [`GraphTarget::Generator`] → [`crate::commands::graph::Layer::generator_graph`]
//!   (via `Project::timeline::find_layer_by_id_mut`) with
//!   `Layer::generator_graph_version` as the version counter.
//!
//! Commands lift a `None` graph to a clone of the supplied catalog
//! default on first edit, apply the mutation, then bump the target's
//! version counter so the renderer detects the change. Reverse state
//! for undo/redo is stored on each command instance.
//!
//! Phase 3 of the per-card-divergence plan in
//! `docs/NODE_GRAPH_SYSTEM.md`.

use std::collections::BTreeMap;

use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::project::Project;

mod node_edit;
pub use node_edit::*;
mod expose;
pub use expose::*;
mod groups;
pub use groups::*;
mod scene;
pub use scene::*;
mod modifiers;
pub use modifiers::*;
mod paste;
pub use paste::*;

#[cfg(test)]
mod test_support;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a [`GraphTarget`] to a mutable [`EffectGraphDef`] inside
/// `project`, lifting a `None` graph to a clone of `catalog_default`
/// on first edit. Runs `f` against the def, then bumps the target's
/// version counter so the renderer notices the change.
///
/// Returns `Some(R)` from `f`, or `None` if the target no longer
/// resolves (effect / layer was deleted between command creation and
/// execution — both possible across undo/redo cycles).
/// `structural` decides which version counter advances: `true` for an edit
/// that changes topology (node/wire add or remove) → bumps the structure
/// version → forces a chain rebuild; `false` for a value- or position-only edit
/// → bumps only the snapshot version → the renderer applies it in place with no
/// rebuild and no state reset.
pub(super) fn with_target_graph_mut<F, R>(
    project: &mut Project,
    target: &GraphTarget,
    catalog_default: &EffectGraphDef,
    structural: bool,
    f: F,
) -> Option<R>
where
    F: FnOnce(&mut EffectGraphDef) -> R,
{
    project.with_preset_graph_mut(target, |host| {
        let def = host
            .graph_def_mut()
            .get_or_insert_with(|| catalog_default.clone());
        let r = f(def);
        if structural {
            host.bump_graph_structure_version();
        } else {
            host.bump_graph_version();
        }
        r
    })
}

/// Variant of [`with_target_graph_mut`] that doesn't lift the graph
/// from `None` — `f` only runs if the target already has a `Some(def)`.
/// Used by undo paths that mutate an already-edited graph; the catalog
/// default isn't needed because if the graph is `None` there's nothing
/// to undo.
pub(super) fn with_existing_target_graph_mut<F, R>(
    project: &mut Project,
    target: &GraphTarget,
    structural: bool,
    f: F,
) -> Option<R>
where
    F: FnOnce(&mut EffectGraphDef) -> R,
{
    project
        .with_preset_graph_mut(target, |host| {
            let def = host.graph_def_mut().as_mut()?;
            let r = f(def);
            if structural {
                host.bump_graph_structure_version();
            } else {
                host.bump_graph_version();
            }
            Some(r)
        })
        .flatten()
}

/// Refresh the target's live `ParamManifest` from its just-mutated graph
/// metadata (BUG-295). `with_target_graph_mut`/`with_existing_target_graph_mut`
/// bump `graph_version`/`graph_structure_version` — a different counter the
/// renderer watches for chain rebuilds — but never touch
/// `PresetInstance::params` itself, so a command that stamps a freshly-minted
/// node's exposures into `preset_metadata.params` (or restores a prior
/// `preset_metadata` on undo) leaves the panel's live manifest stale until a
/// save+reload round trip. Called after every scene-structural command that
/// touches `preset_metadata` at runtime — see call sites below. A no-op if
/// the target no longer resolves (effect/layer deleted).
pub(super) fn refresh_target_manifest(project: &mut Project, target: &GraphTarget) {
    project.with_preset_graph_mut(target, |host| host.refresh_manifest_from_graph());
}

/// Helper for the Revert command: take the target's current
/// `Option<EffectGraphDef>` (consuming it; leaves `None` in place) and
/// return what was there. Bumps the version counter.
pub(super) fn take_target_graph(
    project: &mut Project,
    target: &GraphTarget,
) -> Option<Option<EffectGraphDef>> {
    project.with_preset_graph_mut(target, |host| {
        let prev = host.graph_def_mut().take();
        host.bump_graph_structure_version();
        prev
    })
}

/// Helper for the Revert command: install a given graph (or `None`)
/// at the target, bumping the version counter.
pub(super) fn install_target_graph(
    project: &mut Project,
    target: &GraphTarget,
    graph: Option<EffectGraphDef>,
) {
    project.with_preset_graph_mut(target, |host| {
        *host.graph_def_mut() = graph;
        host.bump_graph_structure_version();
    });
}

// ---------------------------------------------------------------------------
// Add Graph Node
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Group / Ungroup
// ---------------------------------------------------------------------------

/// Navigate to the node + wire vectors of the sub-graph at `scope` — a list of
/// group-node ids to descend into (empty = the document root). Returns `None`
/// if a hop doesn't resolve to a group. The mutable handles let a command both
/// read the level (snapshot for undo) and replace it (apply the transform).
pub(super) fn descend_level<'a>(
    nodes: &'a mut Vec<EffectGraphNode>,
    wires: &'a mut Vec<EffectGraphWire>,
    scope: &[u32],
) -> Option<(&'a mut Vec<EffectGraphNode>, &'a mut Vec<EffectGraphWire>)> {
    match scope.split_first() {
        None => Some((nodes, wires)),
        Some((gid, rest)) => {
            let group = nodes.iter_mut().find(|n| n.id == *gid)?;
            let body = group.group.as_deref_mut()?;
            descend_level(&mut body.nodes, &mut body.wires, rest)
        }
    }
}

/// Resolve the display name (`handle`) of the innermost group named by
/// `scope` — the group whose name an exposed param's card `section` is
/// stamped with (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D5). `scope` is a
/// path of group-node ids from the document root; the LAST id is the
/// innermost group. Returns `None` for a top-level node (empty scope), or if
/// any hop doesn't resolve to a named group (an anonymous boundary node has
/// `handle: None` — matches D5's "top-level nodes get `None`" for that edge
/// case too, rather than a panic).
pub(super) fn innermost_group_display_name(nodes: &[EffectGraphNode], scope: &[u32]) -> Option<String> {
    let mut level = nodes;
    let mut name = None;
    for gid in scope {
        let node = level.iter().find(|n| n.id == *gid)?;
        name = node.handle.clone();
        level = node.group.as_deref()?.nodes.as_slice();
    }
    name
}

/// Collect every populated stable [`NodeId`] within `nodes` and all nested
/// group bodies, at any depth — used by `RenameGroupCommand`'s D5
/// section-sweep to test "does this binding's target live inside the group
/// we just renamed." Includes nested groups' own ids (a binding could in
/// principle target a group node directly), not just leaves.
pub(super) fn collect_node_ids(nodes: &[EffectGraphNode], out: &mut Vec<NodeId>) {
    for n in nodes {
        if !n.node_id.is_empty() {
            out.push(n.node_id.clone());
        }
        if let Some(body) = n.group.as_deref() {
            collect_node_ids(&body.nodes, out);
        }
    }
}


// ---------------------------------------------------------------------------
// Add Scene Object / Add Scene Light
// (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D7/D7a, P5)
// ---------------------------------------------------------------------------

/// Build a plain (non-group, non-boundary) node for the scene-build gestures
/// below — same 12-field shape `AddGraphNodeCommand`/`group_edit::group_selection`
/// use, factored out so the two commands below don't repeat the struct literal
/// four times.
pub(super) fn scene_build_node(id: u32, type_id: &str, handle: Option<String>, params: BTreeMap<String, SerializedParamValue>) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(manifold_core::short_id()),
        type_id: type_id.to_string(),
        handle,
        params,
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

pub(super) fn scene_build_wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

/// Resolve the `&mut PresetInstance` a [`GraphTarget`] addresses — same match
/// every `graph.rs` command uses (mirrors `ToggleNodeParamExposeCommand`'s
/// identical resolve for its mirror step). Used by rename commands' D5 card-
/// section sweep, which needs the manifest (`.params`) alongside the graph —
/// outside `with_target_graph_mut`'s narrower `&mut EffectGraphDef` view.
/// Free function (not a method) so both [`RenameGroupCommand`] and
/// [`RenameSceneObjectCommand`] share one implementation.
pub(super) fn resolve_target_instance<'p>(
    target: &GraphTarget,
    project: &'p mut Project,
) -> Option<&'p mut manifold_core::effects::PresetInstance> {
    match target {
        GraphTarget::Effect(effect_id) => project.find_effect_by_id_mut(effect_id),
        GraphTarget::Generator(layer_id) => {
            project.timeline.find_layer_by_id_mut(layer_id).map(|(_, layer)| layer.gen_params_or_init())
        }
    }
}


/// `base`, else `base_2`, `base_3`, … — the first form not already in `taken`.
/// Inserts the chosen handle into `taken` so a batch paste stays collision-free.
pub(super) fn dedup_handle(base: &str, taken: &mut std::collections::HashSet<String>) -> String {
    if !taken.contains(base) {
        taken.insert(base.to_string());
        return base.to_string();
    }
    let mut i = 2u32;
    loop {
        let cand = format!("{base}_{i}");
        if !taken.contains(&cand) {
            taken.insert(cand.clone());
            return cand;
        }
        i += 1;
    }
}
