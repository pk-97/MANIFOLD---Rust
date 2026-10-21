//! The one place a copied scene item's wires are made, for Duplicate (the
//! same graph) and Paste (the clipboard graph into the destination).
//!
//! A body's pose, instances, count and force are ordinary wires between the
//! copied nodes, or from an outside node into a copied one, so a plain wire
//! copy keeps them. The only wire that carries a number is the membership
//! wire into the Physics World; the copy never reuses the source's number but
//! takes the world's next free one ([`manifold_core::scene_variadic`]) and
//! grows the world's counts to cover it.

use manifold_core::effect_graph_def::{EffectGraphNode, EffectGraphWire};
use manifold_core::physics_body_slots::fit_world_bodies;
use manifold_core::physics_members::{MEMBER_OUTPUT, WORLD_TYPE_ID, member_input};
use manifold_core::scene_variadic::world_family;

/// How source wires map into the destination.
pub struct WireCopy<'a> {
    /// A source document id's destination id: a clone, or the node itself or
    /// its counterpart when it is shared. `None` refuses the copy.
    pub doc_id: &'a dyn Fn(u32) -> Option<u32>,
    /// Whether a destination node already existed before the copy (a shared
    /// Physics World, say). A wire into a port of such a node that is already
    /// wired keeps the node's own, so a copy never rewires the scene's setup.
    pub reused: &'a dyn Fn(u32) -> bool,
    /// The destination `to_port` of a wire whose port the copy moves (a render
    /// slot), or `None` to keep it.
    pub to_port: &'a dyn Fn(&EffectGraphWire) -> Option<String>,
}

/// Append the copies of `source` to `wires`.
pub fn copy_scene_item_wires(
    nodes: &mut [EffectGraphNode],
    wires: &mut Vec<EffectGraphWire>,
    source: &[EffectGraphWire],
    copy: &WireCopy<'_>,
) -> Result<(), String> {
    let mut worlds = Vec::new();
    for wire in source {
        let from_node = (copy.doc_id)(wire.from_node).ok_or("Copied source is missing")?;
        let to_node = (copy.doc_id)(wire.to_node).ok_or("Copied destination is missing")?;
        let mut to_port = (copy.to_port)(wire).unwrap_or_else(|| wire.to_port.clone());
        let is_world = nodes.iter().any(|node| node.id == to_node && node.type_id == WORLD_TYPE_ID);
        if is_world
            && wire.from_port == MEMBER_OUTPUT
            && let Some((family, _)) = member_input(&wire.to_port)
        {
            let number = world_family(family)
                .append_member(nodes, wires, to_node)
                .ok_or("Physics World has no free body slots")?;
            to_port = family.member_port(number);
            if !worlds.contains(&to_node) {
                worlds.push(to_node);
            }
        } else if (copy.reused)(to_node) && wires.iter().any(|w| w.to_node == to_node && w.to_port == to_port) {
            continue;
        }
        wires.push(EffectGraphWire { from_node, from_port: wire.from_port.clone(), to_node, to_port });
    }
    // Every number the world's wires take has its port, however it got wired.
    for world in worlds {
        fit_world_bodies(nodes, wires, world);
    }
    Ok(())
}
