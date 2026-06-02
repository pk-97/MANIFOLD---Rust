//! Group editing — the pure-data restructuring behind the editor's
//! collapse-to-group and ungroup gestures, operating on **one graph level** of
//! an [`EffectGraphDef`] (the nodes + wires visible at the current editor
//! scope).
//!
//! See `docs/NODE_GROUPS_UI_DESIGN.md`. Three operations:
//!
//! - [`infer_interface`] — given a selection, work out which wires cross its
//!   boundary and therefore which input/output ports the group needs. This is
//!   the "magic" of collapse; isolating it here makes it previewable (the UI
//!   can show the inferred ports before committing) and provable.
//! - [`group_selection`] — collapse a selection into a single group node whose
//!   body holds the selected nodes plus `system.group_input`/`group_output`
//!   boundary nodes, re-anchoring the crossing wires to the new ports.
//! - [`ungroup`] — the inverse: inline a group node's body back into the level.
//!
//! All of it is pure data — no GPU, no renderer, no registry — so it unit-tests
//! exactly like [`crate::flatten`]. The load-bearing property is the round trip:
//! `ungroup(group_selection(level)) ≅ level` (up to node-id renumbering). The
//! authoritative port *types* are filled in later by the snapshot layer (which
//! has the registry); [`InterfacePortDef::port_type`] is left blank here.

use std::collections::{BTreeMap, BTreeSet};

use crate::effect_graph_def::{
    EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID,
    GroupDef, GroupInterface, InterfacePortDef,
};
use crate::id::NodeId;

/// One inferred boundary port: its name plus the inner endpoint it binds to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredPort {
    pub name: String,
    pub inner_node: u32,
    pub inner_port: String,
}

/// The interface a selection would expose if collapsed: inputs (inner sinks fed
/// from outside) and outputs (inner sources feeding outside).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredInterface {
    pub inputs: Vec<InferredPort>,
    pub outputs: Vec<InferredPort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupEditError {
    /// Collapse was asked to group an empty selection.
    EmptySelection,
    /// The group handle contains the reserved `/` namespace delimiter.
    ReservedHandleChar { handle: String },
    /// A selected / target id isn't present at this level.
    UnknownNode { node_id: u32 },
    /// `ungroup` was pointed at a node that isn't a group.
    NotAGroup { node_id: u32 },
}

/// Inspect the wires crossing `selected`'s boundary and derive the ports a
/// collapsed group would expose. One input port per distinct inner sink fed
/// from outside; one output port per distinct inner source feeding outside.
/// Port names come from the inner port name, deduplicated. Input and output
/// names live in separate namespaces (a group may have input `x` and output
/// `x`). Deterministic: keyed and ordered by `(inner_node, inner_port)`.
pub fn infer_interface(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    selected: &BTreeSet<u32>,
) -> InferredInterface {
    let _ = nodes; // ports are derived purely from crossing wires
    let mut input_keys: BTreeSet<(u32, String)> = BTreeSet::new();
    let mut output_keys: BTreeSet<(u32, String)> = BTreeSet::new();
    for w in wires {
        let from_sel = selected.contains(&w.from_node);
        let to_sel = selected.contains(&w.to_node);
        match (from_sel, to_sel) {
            (false, true) => {
                input_keys.insert((w.to_node, w.to_port.clone()));
            }
            (true, false) => {
                output_keys.insert((w.from_node, w.from_port.clone()));
            }
            _ => {}
        }
    }

    let mut used = BTreeSet::new();
    let inputs = input_keys
        .into_iter()
        .map(|(n, p)| InferredPort {
            name: unique_name(&p, &mut used),
            inner_node: n,
            inner_port: p,
        })
        .collect();

    let mut used_out = BTreeSet::new();
    let outputs = output_keys
        .into_iter()
        .map(|(n, p)| InferredPort {
            name: unique_name(&p, &mut used_out),
            inner_node: n,
            inner_port: p,
        })
        .collect();

    InferredInterface { inputs, outputs }
}

/// Collapse `selected` into a single group node named `handle`, placed at
/// `centroid`. Returns the rewritten level (the group node replaces the
/// selection; crossing wires re-anchor to the group's inferred ports; the
/// selected nodes + boundary nodes + internal/boundary wires form the body).
///
/// Param carry-over (inner `exposed_params` → interface params) is deferred to
/// the interface-editing phase; the new group starts with no interface params.
pub fn group_selection(
    nodes: Vec<EffectGraphNode>,
    wires: Vec<EffectGraphWire>,
    selected: &BTreeSet<u32>,
    handle: &str,
    centroid: (f32, f32),
) -> Result<(Vec<EffectGraphNode>, Vec<EffectGraphWire>), GroupEditError> {
    if selected.is_empty() {
        return Err(GroupEditError::EmptySelection);
    }
    if handle.contains('/') {
        return Err(GroupEditError::ReservedHandleChar {
            handle: handle.to_string(),
        });
    }
    for id in selected {
        if !nodes.iter().any(|n| n.id == *id) {
            return Err(GroupEditError::UnknownNode { node_id: *id });
        }
    }

    let iface = infer_interface(&nodes, &wires, selected);
    let in_name: BTreeMap<(u32, String), String> = iface
        .inputs
        .iter()
        .map(|p| ((p.inner_node, p.inner_port.clone()), p.name.clone()))
        .collect();
    let out_name: BTreeMap<(u32, String), String> = iface
        .outputs
        .iter()
        .map(|p| ((p.inner_node, p.inner_port.clone()), p.name.clone()))
        .collect();

    let max_id = nodes.iter().map(|n| n.id).max().unwrap_or(0);
    let group_node_id = max_id + 1;
    let gi_id = max_id + 2;
    let go_id = max_id + 3;

    // ── body ──
    let mut body_nodes: Vec<EffectGraphNode> =
        nodes.iter().filter(|n| selected.contains(&n.id)).cloned().collect();
    body_nodes.push(sentinel_node(gi_id, GROUP_INPUT_TYPE_ID));
    body_nodes.push(sentinel_node(go_id, GROUP_OUTPUT_TYPE_ID));

    let mut body_wires: Vec<EffectGraphWire> = wires
        .iter()
        .filter(|w| selected.contains(&w.from_node) && selected.contains(&w.to_node))
        .cloned()
        .collect();
    for p in &iface.inputs {
        body_wires.push(wire(gi_id, &p.name, p.inner_node, &p.inner_port));
    }
    for p in &iface.outputs {
        body_wires.push(wire(p.inner_node, &p.inner_port, go_id, &p.name));
    }

    let interface = GroupInterface {
        inputs: iface
            .inputs
            .iter()
            .map(|p| InterfacePortDef {
                name: p.name.clone(),
                port_type: String::new(),
            })
            .collect(),
        outputs: iface
            .outputs
            .iter()
            .map(|p| InterfacePortDef {
                name: p.name.clone(),
                port_type: String::new(),
            })
            .collect(),
        params: Vec::new(),
    };

    let group_nd = EffectGraphNode {
        id: group_node_id,
        node_id: NodeId::new(crate::short_id()),
        type_id: GROUP_TYPE_ID.to_string(),
        handle: Some(handle.to_string()),
        params: BTreeMap::new(),
        exposed_params: BTreeSet::new(),
        editor_pos: Some(centroid),
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: Some(Box::new(GroupDef {
            interface,
            nodes: body_nodes,
            wires: body_wires,
        })),
    };

    // ── parent ──
    let mut parent_nodes: Vec<EffectGraphNode> =
        nodes.iter().filter(|n| !selected.contains(&n.id)).cloned().collect();
    parent_nodes.push(group_nd);

    let mut parent_wires: Vec<EffectGraphWire> = Vec::new();
    for w in &wires {
        match (selected.contains(&w.from_node), selected.contains(&w.to_node)) {
            (false, false) => parent_wires.push(w.clone()),
            (false, true) => {
                let name = &in_name[&(w.to_node, w.to_port.clone())];
                parent_wires.push(wire(w.from_node, &w.from_port, group_node_id, name));
            }
            (true, false) => {
                let name = &out_name[&(w.from_node, w.from_port.clone())];
                parent_wires.push(wire(group_node_id, name, w.to_node, &w.to_port));
            }
            (true, true) => {} // internal — lives in the body
        }
    }

    Ok((parent_nodes, parent_wires))
}

/// Inline the group node `group_node_id`'s body back into this level, dropping
/// its boundary nodes and re-anchoring the wires that touched the group to what
/// they connected to inside it. Inner node ids are renumbered fresh; colliding
/// handles are deduplicated.
pub fn ungroup(
    nodes: Vec<EffectGraphNode>,
    wires: Vec<EffectGraphWire>,
    group_node_id: u32,
) -> Result<(Vec<EffectGraphNode>, Vec<EffectGraphWire>), GroupEditError> {
    let group_nd = nodes
        .iter()
        .find(|n| n.id == group_node_id)
        .ok_or(GroupEditError::UnknownNode {
            node_id: group_node_id,
        })?;
    let body = group_nd
        .group
        .as_deref()
        .ok_or(GroupEditError::NotAGroup {
            node_id: group_node_id,
        })?;

    let gi_id = body
        .nodes
        .iter()
        .find(|n| n.type_id == GROUP_INPUT_TYPE_ID)
        .map(|n| n.id);
    let go_id = body
        .nodes
        .iter()
        .find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)
        .map(|n| n.id);

    // Fresh ids for inlined inner nodes; dedup handles against the parent.
    let max_id = nodes.iter().map(|n| n.id).max().unwrap_or(0);
    let mut next = max_id + 1;
    let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
    let mut used_handles: BTreeSet<String> = nodes
        .iter()
        .filter(|n| n.id != group_node_id)
        .filter_map(|n| n.handle.clone())
        .collect();

    let mut inlined: Vec<EffectGraphNode> = Vec::new();
    for bn in &body.nodes {
        if Some(bn.id) == gi_id || Some(bn.id) == go_id {
            continue;
        }
        let new_id = next;
        next += 1;
        remap.insert(bn.id, new_id);
        let mut clone = bn.clone();
        clone.id = new_id;
        if let Some(h) = clone.handle.clone() {
            clone.handle = Some(unique_name(&h, &mut used_handles));
        }
        inlined.push(clone);
    }

    // Boundary routing from the body wires.
    let mut input_map: BTreeMap<String, Vec<(u32, String)>> = BTreeMap::new();
    let mut output_map: BTreeMap<String, (u32, String)> = BTreeMap::new();
    let mut inner_wires: Vec<EffectGraphWire> = Vec::new();
    for w in &body.wires {
        if Some(w.from_node) == gi_id {
            if let Some(&to) = remap.get(&w.to_node) {
                input_map
                    .entry(w.from_port.clone())
                    .or_default()
                    .push((to, w.to_port.clone()));
            }
        } else if Some(w.to_node) == go_id {
            if let Some(&from) = remap.get(&w.from_node) {
                output_map.insert(w.to_port.clone(), (from, w.from_port.clone()));
            }
        } else if let (Some(&from), Some(&to)) =
            (remap.get(&w.from_node), remap.get(&w.to_node))
        {
            inner_wires.push(wire(from, &w.from_port, to, &w.to_port));
        }
    }

    // ── rebuild level ──
    let mut out_nodes: Vec<EffectGraphNode> =
        nodes.iter().filter(|n| n.id != group_node_id).cloned().collect();
    out_nodes.extend(inlined);

    let mut out_wires: Vec<EffectGraphWire> = inner_wires;
    for w in &wires {
        if w.from_node == group_node_id {
            if let Some((pn, pp)) = output_map.get(&w.from_port) {
                out_wires.push(wire(*pn, pp, w.to_node, &w.to_port));
            }
        } else if w.to_node == group_node_id {
            if let Some(consumers) = input_map.get(&w.to_port) {
                for (cn, cp) in consumers {
                    out_wires.push(wire(w.from_node, &w.from_port, *cn, cp));
                }
            }
        } else {
            out_wires.push(w.clone());
        }
    }

    Ok((out_nodes, out_wires))
}

// ── helpers ──

fn unique_name(base: &str, used: &mut BTreeSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let mut i = 2u32;
    loop {
        let cand = format!("{base}_{i}");
        if used.insert(cand.clone()) {
            return cand;
        }
        i += 1;
    }
}

fn sentinel_node(id: u32, type_id: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(crate::short_id()),
        type_id: type_id.to_string(),
        handle: None,
        params: BTreeMap::new(),
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Handle-space node set + handle-keyed wire set, for renumbering-independent
    /// structural comparison.
    type Canonical = (BTreeSet<String>, BTreeSet<(String, String, String, String)>);

    fn node(id: u32, handle: &str) -> EffectGraphNode {
        let mut n = sentinel_node(id, "node.atom");
        n.handle = Some(handle.to_string());
        n
    }

    fn sel(ids: &[u32]) -> BTreeSet<u32> {
        ids.iter().copied().collect()
    }

    /// Canonical handle-space form so two levels compare independent of node-id
    /// renumbering: the set of node handles + the set of wires keyed by handle.
    fn canonical(
        nodes: &[EffectGraphNode],
        wires: &[EffectGraphWire],
    ) -> Canonical {
        let key: BTreeMap<u32, String> = nodes
            .iter()
            .map(|n| {
                (
                    n.id,
                    n.handle.clone().unwrap_or_else(|| format!("{}#{}", n.type_id, n.id)),
                )
            })
            .collect();
        let node_keys: BTreeSet<String> = key.values().cloned().collect();
        let wire_keys = wires
            .iter()
            .map(|w| {
                (
                    key[&w.from_node].clone(),
                    w.from_port.clone(),
                    key[&w.to_node].clone(),
                    w.to_port.clone(),
                )
            })
            .collect();
        (node_keys, wire_keys)
    }

    // a.out -> b.in ; b.out -> c.in
    fn abc() -> (Vec<EffectGraphNode>, Vec<EffectGraphWire>) {
        (
            vec![node(0, "a"), node(1, "b"), node(2, "c")],
            vec![wire(0, "out", 1, "in"), wire(1, "out", 2, "in")],
        )
    }

    #[test]
    fn infers_one_in_one_out_for_a_middle_node() {
        let (n, w) = abc();
        let iface = infer_interface(&n, &w, &sel(&[1]));
        assert_eq!(iface.inputs.len(), 1);
        assert_eq!(iface.outputs.len(), 1);
        assert_eq!(iface.inputs[0].inner_node, 1);
        assert_eq!(iface.inputs[0].inner_port, "in");
        assert_eq!(iface.outputs[0].inner_port, "out");
    }

    #[test]
    fn infers_no_ports_for_fully_internal_selection() {
        let (n, w) = abc();
        // Selecting everything: no wire crosses the boundary.
        let iface = infer_interface(&n, &w, &sel(&[0, 1, 2]));
        assert!(iface.inputs.is_empty());
        assert!(iface.outputs.is_empty());
    }

    #[test]
    fn dedups_repeated_port_names() {
        // Two inner nodes both sinking on "in" from outside.
        let nodes = vec![node(0, "src"), node(1, "x"), node(2, "y")];
        let wires = vec![wire(0, "out", 1, "in"), wire(0, "out", 2, "in")];
        let iface = infer_interface(&nodes, &wires, &sel(&[1, 2]));
        let names: Vec<_> = iface.inputs.iter().map(|p| p.name.clone()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"in".to_string()));
        assert!(names.contains(&"in_2".to_string()));
    }

    #[test]
    fn group_selection_replaces_selection_with_a_group_node() {
        let (n, w) = abc();
        let (pn, pw) = group_selection(n, w, &sel(&[1]), "g", (10.0, 20.0)).unwrap();

        // Parent: a, c, and a group node "g".
        let g = pn.iter().find(|x| x.handle.as_deref() == Some("g")).unwrap();
        assert_eq!(g.type_id, GROUP_TYPE_ID);
        let body = g.group.as_deref().unwrap();
        assert_eq!(g.editor_pos, Some((10.0, 20.0)));
        // Body holds b + the two boundary nodes.
        assert!(body.nodes.iter().any(|x| x.handle.as_deref() == Some("b")));
        assert!(body.nodes.iter().any(|x| x.type_id == GROUP_INPUT_TYPE_ID));
        assert!(body.nodes.iter().any(|x| x.type_id == GROUP_OUTPUT_TYPE_ID));
        assert_eq!(body.interface.inputs.len(), 1);
        assert_eq!(body.interface.outputs.len(), 1);
        // Parent wires now route through the group node.
        let in_name = &body.interface.inputs[0].name;
        let out_name = &body.interface.outputs[0].name;
        let g_id = g.id;
        assert!(pw.iter().any(|x| x.to_node == g_id && &x.to_port == in_name));
        assert!(pw.iter().any(|x| x.from_node == g_id && &x.from_port == out_name));
    }

    #[test]
    fn group_then_ungroup_is_identity() {
        let (n, w) = abc();
        let original = canonical(&n, &w);

        let (pn, pw) = group_selection(n, w, &sel(&[1]), "g", (0.0, 0.0)).unwrap();
        let g_id = pn.iter().find(|x| x.handle.as_deref() == Some("g")).unwrap().id;
        let (un, uw) = ungroup(pn, pw, g_id).unwrap();

        assert_eq!(canonical(&un, &uw), original, "ungroup must invert group");
    }

    #[test]
    fn group_then_ungroup_is_identity_for_multi_node_selection() {
        // src -> m1.in ; m1.out -> m2.in ; m2.out -> sink.in. Group {m1, m2}.
        let nodes = vec![node(0, "src"), node(1, "m1"), node(2, "m2"), node(3, "sink")];
        let wires = vec![
            wire(0, "out", 1, "in"),
            wire(1, "out", 2, "in"),
            wire(2, "out", 3, "in"),
        ];
        let original = canonical(&nodes, &wires);

        let (pn, pw) = group_selection(nodes, wires, &sel(&[1, 2]), "mid", (0.0, 0.0)).unwrap();
        // Internal wire m1->m2 must live in the body, not the parent.
        let g = pn.iter().find(|x| x.handle.as_deref() == Some("mid")).unwrap();
        assert!(
            g.group
                .as_deref()
                .unwrap()
                .wires
                .iter()
                .any(|x| x.from_node == 1 && x.to_node == 2)
        );
        let g_id = g.id;
        let (un, uw) = ungroup(pn, pw, g_id).unwrap();
        assert_eq!(canonical(&un, &uw), original);
    }

    #[test]
    fn nested_group_then_ungroup_is_identity() {
        // Group a single node, then group the resulting group node again.
        let (n, w) = abc();
        let (pn, pw) = group_selection(n, w, &sel(&[1]), "inner", (0.0, 0.0)).unwrap();
        let inner_id = pn.iter().find(|x| x.handle.as_deref() == Some("inner")).unwrap().id;
        let before = canonical(&pn, &pw);

        // Wrap the inner group again.
        let (pn2, pw2) =
            group_selection(pn, pw, &sel(&[inner_id]), "outer", (0.0, 0.0)).unwrap();
        let outer_id = pn2.iter().find(|x| x.handle.as_deref() == Some("outer")).unwrap().id;
        let (un, uw) = ungroup(pn2, pw2, outer_id).unwrap();
        assert_eq!(canonical(&un, &uw), before, "ungroup of a nested group inverts");
    }

    #[test]
    fn errors() {
        let (n, w) = abc();
        assert_eq!(
            group_selection(n.clone(), w.clone(), &sel(&[]), "g", (0.0, 0.0)),
            Err(GroupEditError::EmptySelection)
        );
        assert!(matches!(
            group_selection(n.clone(), w.clone(), &sel(&[1]), "bad/name", (0.0, 0.0)),
            Err(GroupEditError::ReservedHandleChar { .. })
        ));
        assert!(matches!(
            group_selection(n.clone(), w.clone(), &sel(&[99]), "g", (0.0, 0.0)),
            Err(GroupEditError::UnknownNode { node_id: 99 })
        ));
        // ungroup on a plain node.
        assert!(matches!(
            ungroup(n, w, 0),
            Err(GroupEditError::NotAGroup { node_id: 0 })
        ));
    }

    /// Like `canonical`, but strips the group-instance prefix the flattener adds
    /// to inner handles (`g/b` → `b`), so a grouped-then-flattened graph
    /// compares against its ungrouped equivalent.
    fn canonical_stripped(
        nodes: &[EffectGraphNode],
        wires: &[EffectGraphWire],
    ) -> Canonical {
        let strip = |h: &str| h.rsplit('/').next().unwrap_or(h).to_string();
        let key: BTreeMap<u32, String> = nodes
            .iter()
            .map(|n| {
                (
                    n.id,
                    n.handle
                        .as_deref()
                        .map(strip)
                        .unwrap_or_else(|| format!("{}#{}", n.type_id, n.id)),
                )
            })
            .collect();
        let node_keys = key.values().cloned().collect();
        let wire_keys = wires
            .iter()
            .map(|w| {
                (
                    key[&w.from_node].clone(),
                    w.from_port.clone(),
                    key[&w.to_node].clone(),
                    w.to_port.clone(),
                )
            })
            .collect();
        (node_keys, wire_keys)
    }

    #[test]
    fn collapsing_does_not_change_the_flattened_topology() {
        use crate::effect_graph_def::{EFFECT_GRAPH_VERSION, EffectGraphDef};
        use crate::flatten::flatten_groups;

        let mk = |nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>| EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes,
            wires,
        };

        let (n, w) = abc();
        let flat_original = flatten_groups(&mk(n.clone(), w.clone())).unwrap();

        let (pn, pw) = group_selection(n, w, &sel(&[1]), "g", (0.0, 0.0)).unwrap();
        let flat_grouped = flatten_groups(&mk(pn, pw)).unwrap();

        // Same runtime topology, modulo the handle prefix the flattener adds.
        assert_eq!(
            canonical_stripped(&flat_grouped.nodes, &flat_grouped.wires),
            canonical_stripped(&flat_original.nodes, &flat_original.wires),
            "collapse must not change what the runtime executes"
        );
    }
}
