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


#[cfg(test)]
use std::collections::BTreeMap;
use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire,
};
#[cfg(test)]
use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::project::Project;

mod card_owned_write;
mod card_pin;
pub use card_pin::{KeepOnCard, SetCardPinCommand};
mod slider_range;
pub use slider_range::SetSliderRangeCommand;
mod node_edit;
pub use node_edit::*;
mod expose;
pub use expose::*;
mod groups;
pub use groups::*;
mod instance_snapshot;
pub(crate) use instance_snapshot::{prune_by_param_id, prune_instance_params};
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
    project.with_graph_for_target_mut(target, Some(catalog_default), structural, f)
}

/// One scene per layer (D17): `added` more render-scene nodes would give the
/// layer's generator graph a second scene. Checked before any mutation so a
/// refused edit leaves the graph and the undo stack untouched. A project that
/// already holds two loads unchanged; only new additions are refused.
pub(super) fn refuses_second_scene(
    project: &Project,
    target: &GraphTarget,
    catalog_default: &EffectGraphDef,
    added: usize,
) -> bool {
    if added == 0 || !matches!(target, GraphTarget::Generator(_)) {
        return false;
    }
    let Some(owner) = project.graph_target_owner(target) else {
        return false;
    };
    let existing = owner.graph.as_ref().unwrap_or(catalog_default);
    manifold_core::effect_graph_def::render_scene_count(&existing.nodes) + added > 1
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
    project.with_graph_for_target_mut(target, None, structural, f)
}

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

/// Where in a graph a command writes, so an edit inside a scene layer's
/// sealed Scene node is refused before it runs, whoever sent it
/// (SCENE_DOCUMENT_DESIGN.md ruling 18 (One writer per region)).
#[derive(Debug, Clone)]
pub struct GraphEditSite {
    pub target: GraphTarget,
    /// Group-node ids from the root to the level the edit writes.
    pub scope: Vec<u32>,
    pub reach: SiteReach,
}

#[derive(Debug, Clone)]
pub enum SiteReach {
    /// Adds to the level or rewires or moves nodes on it; moving and wiring
    /// from the Scene node is the user's.
    Level,
    /// Edits, removes or regroups these nodes of the level.
    Nodes(Vec<u32>),
    /// Rewrites what the scene compiler owns on a scene layer (the whole
    /// graph, or the card it describes); refused there with this reason.
    SceneLayer(&'static str),
}

impl GraphEditSite {
    pub(super) fn new(target: &GraphTarget, scope: &[u32], reach: SiteReach) -> Self {
        Self { target: target.clone(), scope: scope.to_vec(), reach }
    }
}

/// Why `site` must not be written: it lies inside a scene layer's Scene node,
/// it removes or regroups that node, or it rewrites what the compiler owns.
pub fn sealed_refusal(project: &Project, site: &GraphEditSite) -> Option<&'static str> {
    use manifold_core::scene_node::{INSIDE_SCENE_NODE, SCENE_NODE_STAYS, is_scene_node, scope_enters_scene_node};
    let owner = project.graph_target_owner(&site.target)?;
    owner.scene.as_ref()?;
    if matches!(site.target, GraphTarget::SceneModifier { .. }) {
        return Some(INSIDE_SCENE_NODE);
    }
    let graph = owner.graph.as_ref()?;
    if let SiteReach::SceneLayer(reason) = site.reach {
        return Some(reason);
    }
    if scope_enters_scene_node(&graph.nodes, &site.scope) {
        return Some(INSIDE_SCENE_NODE);
    }
    let SiteReach::Nodes(ids) = &site.reach else {
        return None;
    };
    let mut level = graph.nodes.as_slice();
    for hop in &site.scope {
        level = level.iter().find(|n| n.id == *hop).and_then(|n| n.group.as_deref()).map(|body| body.nodes.as_slice())?;
    }
    level.iter().any(|n| ids.contains(&n.id) && is_scene_node(n)).then_some(SCENE_NODE_STAYS)
}

/// Navigate to the node + wire vectors of the sub-graph at `scope` — a list of
/// group-node ids to descend into (empty = the document root). Returns `None`
/// if a hop doesn't resolve to a group, or enters a sealed Scene node, which
/// only the scene compiler writes. The mutable handles let a command both
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
            if manifold_core::scene_node::is_scene_node(group) {
                return None;
            }
            let body = group.group.as_deref_mut()?;
            descend_level(&mut body.nodes, &mut body.wires, rest)
        }
    }
}

/// Resolve the display name (`handle`) of the innermost group named by
/// `scope` — the group whose name an exposed param's card `section` is
/// stamped with (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md section 2 D5). `scope` is a
/// path of group-node ids from the document root; the LAST id is the
/// innermost group. Returns `None` for a top-level node (empty scope), or if
/// any hop doesn't resolve to a named group (an anonymous boundary node has
/// `handle: None` — matches D5's "top-level nodes get `None`" for that edge
/// case too, rather than a panic).
pub(super) fn innermost_group_display_name(
    nodes: &[EffectGraphNode],
    scope: &[u32],
) -> Option<String> {
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

/// Resolve the `&mut PresetInstance` a [`GraphTarget`] addresses — same match
/// every `graph.rs` command uses (mirrors `ToggleNodeParamExposeCommand`'s
/// identical resolve for its mirror step). Used by rename commands' D5 card-
/// section sweep, which needs the manifest (`.params`) alongside the graph —
/// outside `with_target_graph_mut`'s narrower `&mut EffectGraphDef` view.
/// Free function (not a method) so both [`RenameGroupCommand`] and
/// the scene rename share one implementation.
pub(super) fn resolve_target_instance<'p>(
    target: &GraphTarget,
    project: &'p mut Project,
) -> Option<&'p mut manifold_core::effects::PresetInstance> {
    match target {
        GraphTarget::Effect(effect_id) => project.find_effect_by_id_mut(effect_id),
        GraphTarget::Generator(layer_id) => project
            .timeline
            .find_layer_by_id_mut(layer_id)
            .and_then(|(_, layer)| layer.gen_params_mut()),
        GraphTarget::SceneModifier { .. } => None,
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
