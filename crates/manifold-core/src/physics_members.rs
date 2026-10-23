//! How a body belongs to a Physics World, in one place.
//!
//! A body joins a world through one numbered wire: `body.body` into the
//! world's `body_k` (a plain body) or `copies_k` (a copy group's prototype).
//! The number is the body's solve order and nothing else names it. Everything
//! else lives on the body: a scene object reads `body.pose` (or a copy
//! group's `body.instances` and `body.active_count`), and a per-body force
//! wires into `body.acceleration`. The world's own member-side ports
//! (`pose_k`, `instances_k`, `active_count_k`, `body_acceleration_k`,
//! `copies_acceleration_k`) exist only in the built graph: the compile
//! (`manifold_node_engine::load::physics_members`) rewrites each body alias
//! wire to the numbered port of that body's membership, so the engine still
//! runs on slot-indexed storage and nothing parses a port name per tick.
//!
//! This table is the only place that knows the port grammar. Every authored
//! reader asks [`world_member`] or [`member_input`].

use std::sync::Arc;

use crate::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};

/// One body or copy group a world left out: the world input it is wired to
/// (`body_k`, `copies_k`), the param the refusal concerns (a body param, or
/// `pos_x`, `rot_x` or `scale_x` on the transform feeding the body; `None`
/// when none does), and why. Shared strings, so reading a standing refusal
/// every frame allocates nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyRefusalRow {
    pub input: Arc<str>,
    pub param: Option<&'static str>,
    pub reason: Arc<str>,
}

pub const WORLD_TYPE_ID: &str = "node.physics_world";
pub const BODY_TYPE_ID: &str = "node.rigid_body";
/// The body output that carries membership.
pub const MEMBER_OUTPUT: &str = "body";
/// The body input a per-body force wires into.
pub const ACCELERATION_ALIAS: &str = "acceleration";

/// The two ways a body joins a world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    /// A plain body, solved in the order of its number.
    Body,
    /// The prototype of a copy group.
    Copies,
}

/// One body-side output alias and the world output family it rewrites to.
struct Alias {
    body_port: &'static str,
    world_prefix: &'static str,
}

struct Declared {
    member_prefix: &'static str,
    acceleration_prefix: &'static str,
    aliases: &'static [Alias],
}

static BODY: Declared = Declared {
    member_prefix: "body_",
    acceleration_prefix: "body_acceleration_",
    aliases: &[Alias { body_port: "pose", world_prefix: "pose_" }],
};

static COPIES: Declared = Declared {
    member_prefix: "copies_",
    acceleration_prefix: "copies_acceleration_",
    aliases: &[
        Alias { body_port: "instances", world_prefix: "instances_" },
        Alias { body_port: "active_count", world_prefix: "active_count_" },
    ],
};

impl Family {
    pub const ALL: [Family; 2] = [Family::Body, Family::Copies];

    fn declared(self) -> &'static Declared {
        match self {
            Family::Body => &BODY,
            Family::Copies => &COPIES,
        }
    }

    /// The world input a member of number `k` wires into.
    pub fn member_port(self, k: u32) -> String {
        format!("{}{k}", self.declared().member_prefix)
    }

    /// The world input that takes member `k`'s force, in the built graph.
    pub fn acceleration_port(self, k: u32) -> String {
        format!("{}{k}", self.declared().acceleration_prefix)
    }

    /// The body outputs that stand for the world's per-member outputs.
    pub fn alias_outputs(self) -> impl Iterator<Item = &'static str> {
        self.declared().aliases.iter().map(|alias| alias.body_port)
    }

    /// The world output member `k`'s alias `body_port` rewrites to.
    pub fn world_output(self, body_port: &str, k: u32) -> Option<String> {
        self.declared()
            .aliases
            .iter()
            .find(|alias| alias.body_port == body_port)
            .map(|alias| format!("{}{k}", alias.world_prefix))
    }

    /// The member number in a world input name of this family's member ports.
    fn member_index(self, port: &str) -> Option<u32> {
        number_after(port, self.declared().member_prefix)
    }
}

fn number_after(port: &str, prefix: &str) -> Option<u32> {
    let digits = port.strip_prefix(prefix)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The family and number a world input name carries as a membership port:
/// `body_3` is `(Body, 3)`, `copies_0` is `(Copies, 0)`. A force port such as
/// `body_acceleration_3` is not one.
pub fn member_input(port: &str) -> Option<(Family, u32)> {
    Family::ALL.into_iter().find_map(|family| family.member_index(port).map(|k| (family, k)))
}

/// Whether `port` is a body alias output (`pose`, `instances`, `active_count`).
pub fn is_alias_output(port: &str) -> bool {
    Family::ALL.into_iter().any(|family| family.alias_outputs().any(|alias| alias == port))
}

/// The family whose world output family `port` is, with the member number.
pub fn member_side_output(port: &str) -> Option<(Family, u32)> {
    Family::ALL.into_iter().find_map(|family| {
        family
            .declared()
            .aliases
            .iter()
            .find_map(|alias| number_after(port, alias.world_prefix))
            .map(|k| (family, k))
    })
}

/// The family and number of a world's per-member force input.
pub fn member_side_input(port: &str) -> Option<(Family, u32)> {
    Family::ALL
        .into_iter()
        .find_map(|family| number_after(port, family.declared().acceleration_prefix).map(|k| (family, k)))
}

/// A body's place in a world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldMember {
    pub world: u32,
    pub family: Family,
    pub k: u32,
}

/// The world `body` joins, from one level's nodes and wires (a flattened
/// graph, or one group level). `None` when no wire carries its membership.
pub fn world_member(nodes: &[EffectGraphNode], wires: &[EffectGraphWire], body: u32) -> Option<WorldMember> {
    wires.iter().filter(|wire| wire.from_node == body && wire.from_port == MEMBER_OUTPUT).find_map(|wire| {
        let (family, k) = member_input(&wire.to_port)?;
        nodes
            .iter()
            .any(|node| node.id == wire.to_node && node.type_id == WORLD_TYPE_ID)
            .then_some(WorldMember { world: wire.to_node, family, k })
    })
}

/// Every membership wire into `world`, as `(body, family, k)`.
pub fn members_of(wires: &[EffectGraphWire], world: u32) -> Vec<(u32, Family, u32)> {
    wires
        .iter()
        .filter(|wire| wire.to_node == world && wire.from_port == MEMBER_OUTPUT)
        .filter_map(|wire| member_input(&wire.to_port).map(|(family, k)| (wire.from_node, family, k)))
        .collect()
}

/// A wire the built graph uses but an authored graph must not carry: out of a
/// world's per-member outputs, or into its per-member force inputs.
pub fn member_side_wires(def: &EffectGraphDef) -> Vec<(u32, EffectGraphWire)> {
    fn level(nodes: &[EffectGraphNode], wires: &[EffectGraphWire], scope: u32, out: &mut Vec<(u32, EffectGraphWire)>) {
        let is_world = |id: u32| nodes.iter().any(|node| node.id == id && node.type_id == WORLD_TYPE_ID);
        for wire in wires {
            let from = is_world(wire.from_node) && member_side_output(&wire.from_port).is_some();
            let to = is_world(wire.to_node) && member_side_input(&wire.to_port).is_some();
            if from || to {
                out.push((scope, wire.clone()));
            }
        }
        for node in nodes {
            if let Some(group) = node.group.as_deref() {
                level(&group.nodes, &group.wires, node.id, out);
            }
        }
    }
    let mut out = Vec::new();
    level(&def.nodes, &def.wires, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, type_id: &str) -> EffectGraphNode {
        serde_json::from_value(serde_json::json!({"id": id, "nodeId": format!("n{id}"), "typeId": type_id})).unwrap()
    }

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire { from_node, from_port: from_port.into(), to_node, to_port: to_port.into() }
    }

    #[test]
    fn the_table_names_each_family_s_ports() {
        assert_eq!(Family::Body.member_port(3), "body_3");
        assert_eq!(Family::Copies.member_port(0), "copies_0");
        assert_eq!(Family::Body.acceleration_port(7), "body_acceleration_7");
        assert_eq!(Family::Copies.acceleration_port(2), "copies_acceleration_2");
        assert_eq!(Family::Body.world_output("pose", 4).as_deref(), Some("pose_4"));
        assert_eq!(Family::Copies.world_output("instances", 1).as_deref(), Some("instances_1"));
        assert_eq!(Family::Copies.world_output("active_count", 1).as_deref(), Some("active_count_1"));
        assert_eq!(Family::Body.world_output("instances", 1), None);
    }

    #[test]
    fn a_membership_port_is_not_a_force_port() {
        assert_eq!(member_input("body_12"), Some((Family::Body, 12)));
        assert_eq!(member_input("copies_5"), Some((Family::Copies, 5)));
        assert_eq!(member_input("body_acceleration_12"), None);
        assert_eq!(member_input("copies_acceleration_0"), None);
        assert_eq!(member_input("body_"), None);
        assert_eq!(member_input("copies"), None);
        assert_eq!(member_side_input("body_acceleration_12"), Some((Family::Body, 12)));
        assert_eq!(member_side_output("pose_3"), Some((Family::Body, 3)));
        assert_eq!(member_side_output("active_count_0"), Some((Family::Copies, 0)));
        assert_eq!(member_side_output("physics_ms"), None);
    }

    #[test]
    fn world_member_finds_a_plain_body_and_a_copies_body() {
        let nodes = [node(1, WORLD_TYPE_ID), node(2, BODY_TYPE_ID), node(3, BODY_TYPE_ID), node(4, BODY_TYPE_ID)];
        let wires = [
            wire(2, "body", 1, "body_5"),
            wire(3, "body", 1, "copies_2"),
            wire(1, "pose_5", 9, "transform"),
        ];
        assert_eq!(world_member(&nodes, &wires, 2), Some(WorldMember { world: 1, family: Family::Body, k: 5 }));
        assert_eq!(world_member(&nodes, &wires, 3), Some(WorldMember { world: 1, family: Family::Copies, k: 2 }));
        assert_eq!(world_member(&nodes, &wires, 4), None, "a body with no membership wire is in no world");
        assert_eq!(members_of(&wires, 1), [(2, Family::Body, 5), (3, Family::Copies, 2)]);
    }

    #[test]
    fn world_member_resolves_a_body_inside_an_import_group() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 2,
            "nodes": [
                {"id": 1, "nodeId": "world", "typeId": "node.physics_world"},
                {"id": 10, "nodeId": "import", "handle": "import", "typeId": "system.group", "group": {
                    "nodes": [
                        {"id": 11, "nodeId": "body", "typeId": "node.rigid_body"},
                        {"id": 12, "nodeId": "out", "typeId": "system.group_output"}
                    ],
                    "wires": [{"fromNode": 11, "fromPort": "body", "toNode": 12, "toPort": "body"}],
                    "interface": {"inputs": [], "outputs": [{"name": "body", "portType": "RigidBody"}]}
                }}
            ],
            "wires": [{"fromNode": 10, "fromPort": "body", "toNode": 1, "toPort": "body_3"}]
        }))
        .unwrap();
        let flat = crate::flatten::flatten_groups(&def).unwrap();
        let body = flat.nodes.iter().find(|node| node.node_id.as_str() == "body").unwrap().id;
        let world = flat.nodes.iter().find(|node| node.type_id == WORLD_TYPE_ID).unwrap().id;
        assert_eq!(
            world_member(&flat.nodes, &flat.wires, body),
            Some(WorldMember { world, family: Family::Body, k: 3 }),
            "the group's body output flattens to the body's own membership wire"
        );
    }

    #[test]
    fn member_side_wires_are_found_at_every_level() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 2,
            "nodes": [
                {"id": 1, "nodeId": "world", "typeId": "node.physics_world"},
                {"id": 2, "nodeId": "object", "typeId": "node.scene_object"},
                {"id": 3, "nodeId": "body", "typeId": "node.rigid_body"}
            ],
            "wires": [
                {"fromNode": 3, "fromPort": "body", "toNode": 1, "toPort": "body_0"},
                {"fromNode": 1, "fromPort": "pose_0", "toNode": 2, "toPort": "transform"},
                {"fromNode": 2, "fromPort": "object", "toNode": 1, "toPort": "body_acceleration_0"}
            ]
        }))
        .unwrap();
        let found = member_side_wires(&def);
        assert_eq!(found.len(), 2);
        assert!(member_side_wires(&EffectGraphDef { wires: vec![wire(3, "body", 1, "body_0")], ..def }).is_empty());
    }
}
