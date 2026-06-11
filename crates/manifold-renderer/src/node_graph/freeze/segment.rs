//! Cross-card chain fusion — concatenated segment defs (design:
//! `docs/CHAIN_FUSION_DESIGN.md`).
//!
//! A chain is already ONE runtime graph; the per-seam round-trip exists only
//! because fusion runs per card def. This module builds the **concatenated
//! def** for a segment of adjacent cards so the existing freeze pipeline
//! (`partition_regions` → codegen → retarget) sees both sides of every seam
//! in one document. A region that spans a seam is then just a region.
//!
//! Namespacing: node ids (`EffectGraphNode.id`, u32) are document-scoped wire
//! keys and stable `node_id`s are only unique per def, so card *i*'s nodes are
//! remapped to fresh u32 ids and every `node_id` / handle is prefixed with
//! `c{i}.`. Positional prefixes make [A,B] and [B,A] distinct content keys for
//! free. Two cards of the same type (two Blooms) concatenate cleanly.
//!
//! Seam stitching: card i's `FinalOutput` and card i+1's `Source` both
//! disappear; the producer that fed `FinalOutput.in` re-anchors every wire
//! that fanned out of `Source.out`. Card 0's `Source` and the last card's
//! `FinalOutput` survive as the segment's own boundaries, so the result is a
//! perfectly ordinary effect def — fusable, buildable, cacheable through every
//! existing door.

use ahash::AHashMap;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphWire};

use crate::node_graph::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID};

/// Prefix applied to card `i`'s stable node ids inside a concatenated segment
/// def. The chain builder uses the same scheme to retarget each card's
/// bindings into the segment's namespace.
pub fn card_prefix(card_index: usize) -> String {
    format!("c{card_index}.")
}

/// Concatenate ≥2 adjacent cards' (flattened) defs into one segment def.
///
/// Each input def must be a Transform-shaped effect: exactly one
/// `system.source` and exactly one `system.final_output`, with the output fed
/// by exactly one wire. Group nodes are flattened here (same as
/// `fuse_canonical_def_masked` does), so callers can pass raw canonical or
/// edited defs. Returns `None` on any malformed card — fail-closed, the chain
/// renders per-card exactly as today.
pub fn concat_defs(cards: &[&EffectGraphDef]) -> Option<EffectGraphDef> {
    if cards.len() < 2 {
        return None;
    }

    let mut nodes = Vec::new();
    let mut wires: Vec<EffectGraphWire> = Vec::new();
    let mut next_id: u32 = 0;
    // (node u32 id, port) the previous card's output producer — where the next
    // card's Source fan-out re-anchors.
    let mut prev_tail: Option<(u32, String)> = None;

    for (ci, card) in cards.iter().enumerate() {
        let flat = manifold_core::flatten::flatten_groups(card).ok()?;
        let prefix = card_prefix(ci);
        let first = ci == 0;
        let last = ci == cards.len() - 1;

        let source_doc_id = single_node_of_type(&flat, SOURCE_TYPE_ID)?;
        let final_doc_id = single_node_of_type(&flat, FINAL_OUTPUT_TYPE_ID)?;
        // The card's output endpoint: the one wire feeding FinalOutput.in.
        let tail_wire = {
            let mut feeds = flat.wires.iter().filter(|w| w.to_node == final_doc_id);
            let w = feeds.next()?;
            if feeds.next().is_some() {
                return None; // malformed: FinalOutput fed twice
            }
            w.clone()
        };

        // Remap this card's nodes to fresh u32 ids, dropping the boundaries
        // that the stitch removes.
        let mut id_map: AHashMap<u32, u32> = AHashMap::default();
        for n in &flat.nodes {
            let drop_source = !first && n.id == source_doc_id;
            let drop_final = !last && n.id == final_doc_id;
            if drop_source || drop_final {
                continue;
            }
            let mut node = n.clone();
            node.id = next_id;
            id_map.insert(n.id, next_id);
            next_id += 1;
            if !node.node_id.is_empty() {
                node.node_id = NodeId::new(format!("{prefix}{}", node.node_id.as_str()));
            } else {
                // A pre-migration doc without stamped ids can't be namespaced
                // unambiguously — refuse, render per-card.
                return None;
            }
            if let Some(h) = &node.handle {
                node.handle = Some(format!("{prefix}{h}"));
            }
            nodes.push(node);
        }

        // Rewire. A wire's endpoints map through id_map; wires touching a
        // dropped boundary re-anchor:
        //   from == dropped Source  → from = prev card's tail endpoint
        //   to   == dropped Final   → recorded as this card's tail, not emitted
        for w in &flat.wires {
            if !last && w.to_node == final_doc_id {
                continue; // the tail wire — recorded below, not emitted
            }
            let (from_node, from_port) = if !first && w.from_node == source_doc_id {
                let (n, p) = prev_tail.clone()?;
                (n, p)
            } else {
                (*id_map.get(&w.from_node)?, w.from_port.clone())
            };
            let to_node = *id_map.get(&w.to_node)?;
            wires.push(EffectGraphWire {
                from_node,
                from_port,
                to_node,
                to_port: w.to_port.clone(),
            });
        }

        if !last {
            // This card's output producer in segment ids. A passthrough card
            // (Source wired straight to FinalOutput) keeps the previous tail.
            prev_tail = if !first && tail_wire.from_node == source_doc_id {
                Some(prev_tail.clone()?)
            } else {
                Some((*id_map.get(&tail_wire.from_node)?, tail_wire.from_port.clone()))
            };
        }
    }

    Some(EffectGraphDef {
        version: cards[0].version,
        name: None,
        description: None,
        // Anonymous: segment identity is the content key, and per-card outer
        // params / bindings stay on each card's own EffectSlot.
        preset_metadata: None,
        nodes,
        wires,
    })
}

fn single_node_of_type(def: &EffectGraphDef, type_id: &str) -> Option<u32> {
    let mut it = def.nodes.iter().filter(|n| n.type_id == type_id);
    let id = it.next()?.id;
    if it.next().is_some() {
        return None;
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::freeze::region::partition_regions;
    use crate::node_graph::PrimitiveRegistry;

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    fn card(json: &str) -> EffectGraphDef {
        serde_json::from_str(json).expect("parse card def")
    }

    const CARD_A: &str = r#"{
        "version": 1, "name": "cardA", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.gain", "nodeId": "gain" },
            { "id": 2, "typeId": "node.contrast", "nodeId": "contrast" },
            { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
        ]
    }"#;

    const CARD_B: &str = r#"{
        "version": 1, "name": "cardB", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.saturation", "nodeId": "sat" },
            { "id": 2, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
        ]
    }"#;

    /// The headline structural claim: two pointwise cards concatenate into a
    /// def whose region finder produces ONE region spanning the seam — the
    /// seam round-trip is gone at the partition level.
    #[test]
    fn two_pointwise_cards_concat_into_one_region() {
        let a = card(CARD_A);
        let b = card(CARD_B);
        let seg = concat_defs(&[&a, &b]).expect("concat builds");

        // Boundaries: exactly one Source (card 0's) and one FinalOutput
        // (card 1's) survive.
        assert_eq!(
            seg.nodes.iter().filter(|n| n.type_id == "system.source").count(),
            1
        );
        assert_eq!(
            seg.nodes.iter().filter(|n| n.type_id == "system.final_output").count(),
            1
        );

        let regions = partition_regions(&seg, &registry());
        assert_eq!(regions.len(), 1, "the seam must not split the region");
        let member_ids: Vec<&str> = {
            let by_doc: std::collections::BTreeMap<u32, &str> = seg
                .nodes
                .iter()
                .map(|n| (n.id, n.node_id.as_str()))
                .collect();
            regions[0].members.iter().map(|m| by_doc[&m.doc_id]).collect()
        };
        assert_eq!(
            member_ids,
            vec!["c0.gain", "c0.contrast", "c1.sat"],
            "the region spans both cards' atoms in chain order"
        );
    }

    /// Namespacing: the same card twice — duplicate stable node ids on input —
    /// yields unique, positional ids in the segment def.
    #[test]
    fn same_card_twice_namespaces_node_ids() {
        let a = card(CARD_A);
        let seg = concat_defs(&[&a, &a]).expect("concat builds");
        let mut ids: Vec<&str> = seg.nodes.iter().map(|n| n.node_id.as_str()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "all node_ids unique after namespacing");
        assert!(seg.nodes.iter().any(|x| x.node_id.as_str() == "c0.gain"));
        assert!(seg.nodes.iter().any(|x| x.node_id.as_str() == "c1.gain"));
        // u32 wire keys are unique too (serde would not catch this).
        let mut doc_ids: Vec<u32> = seg.nodes.iter().map(|n| n.id).collect();
        let n = doc_ids.len();
        doc_ids.sort_unstable();
        doc_ids.dedup();
        assert_eq!(doc_ids.len(), n);
    }

    /// Fail-closed: a card without a FinalOutput (malformed) refuses to
    /// concatenate rather than producing a broken segment.
    #[test]
    fn malformed_card_refuses() {
        let a = card(CARD_A);
        let broken = card(
            r#"{
            "version": 1, "name": "broken", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.gain", "nodeId": "gain" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" }
            ]
        }"#,
        );
        assert!(concat_defs(&[&a, &broken]).is_none());
        assert!(concat_defs(&[&a]).is_none(), "a single card is not a segment");
    }
}
