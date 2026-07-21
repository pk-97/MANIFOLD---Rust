//! Group / ungroup / tint / rename commands for graph group nodes. Split out
//! of `graph.rs` in P2-G/S4 (pure move). The shared traversal + resolution
//! helpers `descend_level`, `collect_node_ids`, `resolve_target_instance` are
//! used across node_edit / expose / scene as well, so per the design's
//! "cross-module helpers stay pub(super)" rule they remain in `graph/mod.rs`
//! and are reached here via `super` (see queue S4 note).

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::project::Project;

use crate::command::Command;

use super::{
    collect_node_ids, descend_level, resolve_target_instance, with_existing_target_graph_mut,
    with_target_graph_mut,
};

/// Collapse a selection at `scope_path` into a single group node, via
/// [`manifold_core::group_edit::group_selection`]. Undo restores the level
/// wholesale (a structural transform touches many nodes/wires, so a level
/// snapshot is the clean reverse).
#[derive(Debug)]
pub struct GroupNodesCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    selected: Vec<u32>,
    handle: String,
    centroid: (f32, f32),
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before collapse. Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl GroupNodesCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        selected: Vec<u32>,
        handle: String,
        centroid: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            selected,
            handle,
            centroid,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for GroupNodesCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let selected: std::collections::BTreeSet<u32> = self.selected.iter().copied().collect();
        let handle = self.handle.clone();
        let centroid = self.centroid;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());
            match manifold_core::group_edit::group_selection(
                nodes.clone(),
                wires.clone(),
                &selected,
                &handle,
                centroid,
            ) {
                Ok((nn, nw)) => {
                    *nodes = nn;
                    *wires = nw;
                    Some(prev)
                }
                Err(e) => {
                    eprintln!("[manifold-editing] GroupNodes: {e:?}");
                    None
                }
            }
        });
        self.prev = result.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Group Nodes"
    }
}

/// Dissolve a group node at `scope_path` back into its level, via
/// [`manifold_core::group_edit::ungroup`]. The inverse of [`GroupNodesCommand`].
#[derive(Debug)]
pub struct UngroupNodeCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    group_node_id: u32,
    catalog_default: EffectGraphDef,
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl UngroupNodeCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        group_node_id: u32,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            group_node_id,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for UngroupNodeCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let group_node_id = self.group_node_id;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());
            match manifold_core::group_edit::ungroup(nodes.clone(), wires.clone(), group_node_id) {
                Ok((nn, nw)) => {
                    *nodes = nn;
                    *wires = nw;
                    Some(prev)
                }
                Err(e) => {
                    eprintln!("[manifold-editing] UngroupNode: {e:?}");
                    None
                }
            }
        });
        self.prev = result.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
    }

    fn description(&self) -> &str {
        "Ungroup Node"
    }
}

// ---------------------------------------------------------------------------
// Set group tint (cosmetic, non-structural)
// ---------------------------------------------------------------------------

/// Set (or clear) the accent colour of a group node at `scope_path`. Cosmetic
/// only — it never changes what runs, so it routes as a non-structural edit
/// (no chain rebuild). Undo restores the prior tint.
#[derive(Debug)]
pub struct SetGroupTintCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    group_node_id: u32,
    tint: Option<[f32; 4]>,
    catalog_default: EffectGraphDef,
    /// Pre-edit tint. `Some(prev)` once captured; outer `Option` distinguishes
    /// "not yet executed."
    prev: Option<Option<[f32; 4]>>,
}

impl SetGroupTintCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        group_node_id: u32,
        tint: Option<[f32; 4]>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            group_node_id,
            tint,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for SetGroupTintCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let id = self.group_node_id;
        let tint = self.tint;
        let captured =
            with_target_graph_mut(project, &self.target, &self.catalog_default, false, |def| {
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let group = nodes
                    .iter_mut()
                    .find(|n| n.id == id)
                    .and_then(|n| n.group.as_mut())?;
                let prev = group.tint;
                group.tint = tint;
                Some(prev)
            });
        if self.prev.is_none() {
            self.prev = captured.flatten();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.prev else {
            return;
        };
        let scope = self.scope_path.clone();
        let id = self.group_node_id;
        let _ = with_existing_target_graph_mut(project, &self.target, false, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope)
                && let Some(group) = nodes
                    .iter_mut()
                    .find(|n| n.id == id)
                    .and_then(|n| n.group.as_mut())
            {
                group.tint = prev;
            }
        });
    }

    fn description(&self) -> &str {
        "Set Group Tint"
    }
}
// ---------------------------------------------------------------------------
// Rename group (handle = namespace, so structural)
// ---------------------------------------------------------------------------

/// Rename a group node at `scope_path`. The handle is the group's namespace
/// (it prefixes inner handles at flatten time), so this is a structural edit.
/// Rejected as a no-op when the new handle is empty, contains `/`, or collides
/// with a sibling at the same level. Undo restores the prior handle.
#[derive(Debug)]
pub struct RenameGroupCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    group_node_id: u32,
    new_handle: String,
    catalog_default: EffectGraphDef,
    /// Pre-edit handle. `Some(prev)` once captured (the rename was applied);
    /// stays `None` when the rename was rejected or never executed.
    prev: Option<Option<String>>,
    /// D5 rename-sweep undo state (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2):
    /// `(param_id, prior_section)` for every card spec whose `section`
    /// followed this rename (it equaled the OLD group name and its binding
    /// target resolved inside the renamed group). Empty when nothing
    /// matched, or on a rejected/no-op rename. A hand-edited section (any
    /// other string) never lands here — it's untouched by the sweep.
    swept: Vec<(String, Option<String>)>,
}

impl RenameGroupCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        group_node_id: u32,
        new_handle: String,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            group_node_id,
            new_handle,
            catalog_default,
            prev: None,
            swept: Vec::new(),
        }
    }
}
impl Command for RenameGroupCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let id = self.group_node_id;
        let new_handle = self.new_handle.clone();
        // Guard against a repeated execute() (e.g. a defensive double-call
        // with no intervening undo) re-deriving `prev`/re-sweeping from an
        // already-renamed state — same guard shape the original code used
        // for `self.prev` alone.
        let first_time = self.prev.is_none();
        let captured =
            with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                // Reject invalid / colliding names — a rejected rename changes
                // nothing (the canvas keeps the old name).
                if new_handle.is_empty() || new_handle.contains('/') {
                    return None;
                }
                if nodes
                    .iter()
                    .any(|n| n.id != id && n.handle.as_deref() == Some(new_handle.as_str()))
                {
                    return None;
                }
                let node = nodes.iter_mut().find(|n| n.id == id)?;
                // Only groups carry a renamable namespace here.
                node.group.as_ref()?;
                let prev = node.handle.clone();
                node.handle = Some(new_handle.clone());
                // D5 sweep prep: every stable NodeId inside the renamed
                // group's subtree (any depth) — the "does this binding
                // target live inside the group we just renamed" test below.
                let mut inside = Vec::new();
                if let Some(body) = node.group.as_deref() {
                    collect_node_ids(&body.nodes, &mut inside);
                }
                Some((prev, inside))
            });
        let Some((prev, inside)) = captured.flatten() else {
            return;
        };
        if first_time {
            self.prev = Some(prev.clone());
        }
        if !first_time {
            // Sweep already ran on the genuine first execute; a repeated
            // call is a no-op past the handle write above.
            return;
        }

        // D5 rename-sweep: any card spec whose `section` equals the OLD
        // group name AND whose binding target resolves inside the renamed
        // group follows the rename — one undoable command, both writes.
        let Some(old_name) = prev else {
            // The group had no name before this rename — nothing could have
            // been sectioned under it.
            return;
        };
        let Some(inst) = resolve_target_instance(&self.target, project) else {
            return;
        };
        let target_ids: Vec<String> = inst
            .graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .map(|m| {
                m.bindings
                    .iter()
                    .filter(|b| match &b.target {
                        manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } => {
                            inside.contains(node_id)
                        }
                        manifold_core::effect_graph_def::BindingTarget::Composite { .. } => false,
                    })
                    .map(|b| b.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.swept.clear();
        for param_id in target_ids {
            if let Some(p) = inst.params.get_mut(&param_id)
                && p.spec.section.as_deref() == Some(old_name.as_str())
            {
                self.swept.push((param_id, p.spec.section.clone()));
                p.spec.section = Some(new_handle.clone());
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.swept.is_empty()
            && let Some(inst) = resolve_target_instance(&self.target, project)
        {
            for (param_id, prev_section) in self.swept.drain(..) {
                if let Some(p) = inst.params.get_mut(&param_id) {
                    p.spec.section = prev_section;
                }
            }
        }

        let Some(prev) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let id = self.group_node_id;
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope)
                && let Some(node) = nodes.iter_mut().find(|n| n.id == id)
            {
                node.handle = prev;
            }
        });
    }

    fn description(&self) -> &str {
        "Rename Group"
    }
}

