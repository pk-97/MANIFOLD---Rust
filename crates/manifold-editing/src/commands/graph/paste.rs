//! Paste-nodes command (copy/paste/duplicate within a graph level). Split out
//! of `graph.rs` in P2-G/S6 (pure move). `dedup_handle` is shared with the
//! scene DuplicateSceneObject command, so it stays `pub(super)` in
//! `graph/mod.rs` and is reached here via `super`.

use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::project::Project;

use crate::command::Command;

use super::{dedup_handle, descend_level, with_existing_target_graph_mut, with_target_graph_mut};

// ---------------------------------------------------------------------------
// Paste nodes (copy/paste/duplicate within a graph level)
// ---------------------------------------------------------------------------

/// Paste a set of copied nodes (and the wires among them) into the level at
/// `scope_path`. Each pasted node gets a fresh runtime id, a fresh stable
/// `NodeId`, a deduped handle, and an editor-position offset, so a copy never
/// collides with its source. A wire whose both endpoints are in the copied set
/// is re-anchored to the new ids; external wires are dropped (paste carries
/// internal connectivity only). Structural (the chain rebuilds); undo removes
/// exactly the pasted nodes and wires. Backs Cmd+V (paste) and copy-then-paste
/// duplication.
#[derive(Debug)]
pub struct PasteNodesCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    src_nodes: Vec<EffectGraphNode>,
    src_wires: Vec<EffectGraphWire>,
    offset: (f32, f32),
    catalog_default: EffectGraphDef,
    /// Minted on first execute, reused on redo so a pasted node's identity (and
    /// any binding later made against it) survives undo/redo: `(src id, new id,
    /// new node_id)`.
    remap: Option<Vec<(u32, u32, NodeId)>>,
}

impl PasteNodesCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        src_nodes: Vec<EffectGraphNode>,
        src_wires: Vec<EffectGraphWire>,
        offset: (f32, f32),
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            src_nodes,
            src_wires,
            offset,
            catalog_default,
            remap: None,
        }
    }
}
impl Command for PasteNodesCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let existing_remap = self.remap.clone();
        let src_nodes = &self.src_nodes;
        let src_wires = &self.src_wires;
        let offset = self.offset;
        let result = with_target_graph_mut(
            project,
            &self.target,
            &self.catalog_default,
            true,
            |def| {
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                // Fresh ids start past the level's current max; fresh node_ids
                // are minted once and reused on redo.
                let mut next_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
                let remap: Vec<(u32, u32, NodeId)> = existing_remap.unwrap_or_else(|| {
                    src_nodes
                        .iter()
                        .map(|sn| {
                            let new_id = next_id;
                            next_id += 1;
                            (sn.id, new_id, NodeId::new(manifold_core::short_id()))
                        })
                        .collect()
                });
                let mut taken: std::collections::HashSet<String> =
                    nodes.iter().filter_map(|n| n.handle.clone()).collect();
                for sn in src_nodes {
                    let Some((_, new_id, new_node_id)) =
                        remap.iter().find(|(orig, _, _)| *orig == sn.id)
                    else {
                        continue;
                    };
                    let mut node = sn.clone();
                    node.id = *new_id;
                    node.node_id = new_node_id.clone();
                    node.handle = sn.handle.as_deref().map(|h| dedup_handle(h, &mut taken));
                    node.editor_pos = Some(match sn.editor_pos {
                        Some((x, y)) => (x + offset.0, y + offset.1),
                        None => offset,
                    });
                    // The copy isn't card-exposed (its outer bindings address the
                    // original by node_id); start it un-exposed so no binding dangles.
                    node.exposed_params = Default::default();
                    nodes.push(node);
                }
                for sw in src_wires {
                    let from = remap.iter().find(|(o, _, _)| *o == sw.from_node);
                    let to = remap.iter().find(|(o, _, _)| *o == sw.to_node);
                    if let (Some((_, fid, _)), Some((_, tid, _))) = (from, to) {
                        wires.push(EffectGraphWire {
                            from_node: *fid,
                            from_port: sw.from_port.clone(),
                            to_node: *tid,
                            to_port: sw.to_port.clone(),
                        });
                    }
                }
                Some(remap)
            },
        )
        .flatten();
        if self.remap.is_none() {
            self.remap = result;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(remap) = self.remap.clone() else {
            return;
        };
        let new_ids: std::collections::HashSet<u32> =
            remap.iter().map(|(_, n, _)| *n).collect();
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                nodes.retain(|n| !new_ids.contains(&n.id));
                wires.retain(|w| {
                    !new_ids.contains(&w.from_node) && !new_ids.contains(&w.to_node)
                });
            }
        });
    }

    fn description(&self) -> &str {
        "Paste Nodes"
    }
}

