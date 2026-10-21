//! Compile a body's alias wires to its Physics World's numbered ports.
//!
//! An authored graph says "this object reads `body.pose`" and "this force
//! feeds `body.acceleration`"; the body's membership wire (`body.body` into
//! the world's `body_k` or `copies_k`) says which slot it is. The engine runs
//! on slot-indexed storage, so here each alias wire becomes the world's
//! numbered port for that membership (`world.pose_k`, `world.instances_k`,
//! `world.active_count_k`, `world.body_acceleration_k`,
//! `world.copies_acceleration_k`). It runs once when the graph is built,
//! after scene-modifier preparation and group flattening and before wires
//! connect; nothing here is per tick.
//!
//! A body with no membership is in no world. Its `pose` is its authored
//! transform, which is what a physics-off object shows. Its force, `instances`
//! and `active_count` have nothing to act on and are dropped.
//!
//! Wires already on the world's numbered ports pass through untouched: they
//! are the built graph's own shape, and the port grammar lives in
//! `manifold_core::physics_members`.

use std::borrow::Cow;

use ahash::AHashMap;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphWire};
use manifold_core::physics_members::{
    ACCELERATION_ALIAS, BODY_TYPE_ID, WorldMember, is_alias_output, world_member,
};

/// Alias chains (a body whose transform is another physics-off body's pose)
/// resolve one hop per pass; this bounds a cycle.
const MAX_HOPS: usize = 8;

fn touches_alias(def: &EffectGraphDef, is_body: &dyn Fn(u32) -> bool) -> bool {
    def.wires.iter().any(|wire| {
        (is_body(wire.from_node) && is_alias_output(&wire.from_port))
            || (is_body(wire.to_node) && wire.to_port == ACCELERATION_ALIAS)
    })
}

pub(crate) fn compile(def: &EffectGraphDef) -> Cow<'_, EffectGraphDef> {
    let bodies: AHashMap<u32, Option<WorldMember>> = def
        .nodes
        .iter()
        .filter(|node| node.type_id == BODY_TYPE_ID)
        .map(|node| (node.id, world_member(&def.nodes, &def.wires, node.id)))
        .collect();
    if bodies.is_empty() || !touches_alias(def, &|id| bodies.contains_key(&id)) {
        return Cow::Borrowed(def);
    }
    let mut out = def.clone();
    for _ in 0..MAX_HOPS {
        let source_of_transform: AHashMap<u32, (u32, String)> = out
            .wires
            .iter()
            .filter(|wire| bodies.contains_key(&wire.to_node) && wire.to_port == "transform")
            .map(|wire| (wire.to_node, (wire.from_node, wire.from_port.clone())))
            .collect();
        let mut changed = false;
        let mut wires = Vec::with_capacity(out.wires.len());
        for wire in &out.wires {
            match rewrite(wire, &bodies, &source_of_transform) {
                Rewritten::Kept => wires.push(wire.clone()),
                Rewritten::Replaced(replacement) => {
                    changed = true;
                    wires.push(replacement);
                }
                Rewritten::Dropped => changed = true,
            }
        }
        out.wires = wires;
        if !changed {
            break;
        }
    }
    Cow::Owned(out)
}

enum Rewritten {
    Kept,
    Replaced(EffectGraphWire),
    Dropped,
}

fn rewrite(
    wire: &EffectGraphWire,
    bodies: &AHashMap<u32, Option<WorldMember>>,
    source_of_transform: &AHashMap<u32, (u32, String)>,
) -> Rewritten {
    if let Some(member) = bodies.get(&wire.from_node)
        && is_alias_output(&wire.from_port)
    {
        return match member {
            Some(member) => match member.family.world_output(&wire.from_port, member.k) {
                Some(port) => Rewritten::Replaced(EffectGraphWire {
                    from_node: member.world,
                    from_port: port,
                    ..wire.clone()
                }),
                None => Rewritten::Dropped,
            },
            None if wire.from_port == "pose" => match source_of_transform.get(&wire.from_node) {
                Some((node, port)) => Rewritten::Replaced(EffectGraphWire {
                    from_node: *node,
                    from_port: port.clone(),
                    ..wire.clone()
                }),
                None => Rewritten::Dropped,
            },
            None => Rewritten::Dropped,
        };
    }
    if let Some(member) = bodies.get(&wire.to_node)
        && wire.to_port == ACCELERATION_ALIAS
    {
        return match member {
            Some(member) => Rewritten::Replaced(EffectGraphWire {
                to_node: member.world,
                to_port: member.family.acceleration_port(member.k),
                ..wire.clone()
            }),
            None => Rewritten::Dropped,
        };
    }
    Rewritten::Kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::EffectGraphNode;

    fn node(id: u32, type_id: &str) -> EffectGraphNode {
        serde_json::from_value(serde_json::json!({"id": id, "nodeId": format!("n{id}"), "typeId": type_id})).unwrap()
    }

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire { from_node, from_port: from_port.into(), to_node, to_port: to_port.into() }
    }

    fn def(nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphDef {
        EffectGraphDef { version: 2, name: None, description: None, preset_metadata: None, scene_modifiers: Vec::new(), nodes, wires }
    }

    #[test]
    fn a_member_s_alias_wires_take_its_numbered_ports() {
        let source = def(
            vec![
                node(1, "node.physics_world"),
                node(2, "node.rigid_body"),
                node(3, "node.scene_object"),
                node(4, "node.value"),
                node(5, "node.transform_3d"),
            ],
            vec![
                wire(5, "transform", 2, "transform"),
                wire(2, "body", 1, "body_7"),
                wire(2, "pose", 3, "transform"),
                wire(4, "field", 2, "acceleration"),
            ],
        );
        let built = compile(&source);
        let has = |from: u32, from_port: &str, to: u32, to_port: &str| {
            built.wires.iter().any(|w| w.from_node == from && w.from_port == from_port && w.to_node == to && w.to_port == to_port)
        };
        assert!(has(1, "pose_7", 3, "transform"));
        assert!(has(4, "field", 1, "body_acceleration_7"));
        assert!(has(2, "body", 1, "body_7"), "membership is the one numbered wire and stays");
        assert_eq!(built.wires.len(), 4);
    }

    #[test]
    fn a_copies_body_publishes_its_group_s_instances_and_count() {
        let source = def(
            vec![node(1, "node.physics_world"), node(2, "node.rigid_body"), node(3, "node.scene_object"), node(4, "node.value")],
            vec![
                wire(2, "body", 1, "copies_3"),
                wire(2, "instances", 3, "instances"),
                wire(2, "active_count", 3, "instance_count"),
                wire(4, "field", 2, "acceleration"),
                wire(2, "pose", 3, "transform"),
            ],
        );
        let built = compile(&source);
        let has = |from: u32, from_port: &str, to: u32, to_port: &str| {
            built.wires.iter().any(|w| w.from_node == from && w.from_port == from_port && w.to_node == to && w.to_port == to_port)
        };
        assert!(has(1, "instances_3", 3, "instances"));
        assert!(has(1, "active_count_3", 3, "instance_count"));
        assert!(has(4, "field", 1, "copies_acceleration_3"));
        assert!(!built.wires.iter().any(|w| w.to_port == "transform"), "a copy group has no single pose");
    }

    #[test]
    fn a_body_with_no_world_shows_its_authored_transform() {
        let source = def(
            vec![node(2, "node.rigid_body"), node(3, "node.scene_object"), node(4, "node.value"), node(5, "node.transform_3d")],
            vec![
                wire(5, "transform", 2, "transform"),
                wire(2, "pose", 3, "transform"),
                wire(4, "field", 2, "acceleration"),
            ],
        );
        let built = compile(&source);
        assert!(built.wires.iter().any(|w| w.from_node == 5 && w.to_node == 3 && w.to_port == "transform"));
        assert!(!built.wires.iter().any(|w| w.to_port == "acceleration"), "a force on a body in no world acts on nothing");
    }

    #[test]
    fn a_graph_with_no_alias_wire_is_borrowed_unchanged() {
        let source = def(
            vec![node(1, "node.physics_world"), node(2, "node.rigid_body"), node(3, "node.scene_object")],
            vec![wire(2, "body", 1, "body_0"), wire(1, "pose_0", 3, "transform")],
        );
        assert!(matches!(compile(&source), Cow::Borrowed(_)));
    }
}
