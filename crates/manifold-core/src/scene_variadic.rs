//! Ordered variadic inputs: a node with `prefix0`, `prefix1`, ... inputs and a
//! count param that sizes them. A member's number is its order and sits on
//! exactly one wire. Adding a member always takes the lowest number no wire
//! uses and grows the owner's count so the port exists; nothing renumbers.

use crate::effect_graph_def::{EffectGraphNode, EffectGraphWire, SerializedParamValue};
use crate::physics_members::Family;
use crate::scene_surface::{
    PHYSICS_WORLD_DEFAULT_BODIES, PHYSICS_WORLD_DEFAULT_COPY_GROUPS, PHYSICS_WORLD_MAX_BODIES,
    PHYSICS_WORLD_MAX_COPY_GROUPS,
};

/// One ordered family on an owner node.
#[derive(Debug, Clone, Copy)]
pub struct Variadic {
    pub prefix: &'static str,
    /// The owner param whose value is how many ports of the family exist.
    pub count_param: &'static str,
    /// The count when none is saved.
    pub default_count: u32,
    pub max: u32,
}

/// A Physics World's body family.
pub const WORLD_BODIES: Variadic = Variadic {
    prefix: "body_",
    count_param: "bodies",
    default_count: PHYSICS_WORLD_DEFAULT_BODIES,
    max: PHYSICS_WORLD_MAX_BODIES,
};

/// A Physics World's copy-group family.
pub const WORLD_COPY_GROUPS: Variadic = Variadic {
    prefix: "copies_",
    count_param: "copy_groups",
    default_count: PHYSICS_WORLD_DEFAULT_COPY_GROUPS,
    max: PHYSICS_WORLD_MAX_COPY_GROUPS,
};

/// The world family a body of `family` joins.
pub fn world_family(family: Family) -> Variadic {
    match family {
        Family::Body => WORLD_BODIES,
        Family::Copies => WORLD_COPY_GROUPS,
    }
}

impl Variadic {
    fn taken(&self, wires: &[EffectGraphWire], owner: u32, k: u32) -> bool {
        let port = format!("{}{k}", self.prefix);
        wires.iter().any(|wire| wire.to_node == owner && wire.to_port == port)
    }

    /// The lowest number no wire into `owner` uses; `None` when all `max` are
    /// taken.
    pub fn next_free(&self, wires: &[EffectGraphWire], owner: u32) -> Option<u32> {
        (0..self.max).find(|&k| !self.taken(wires, owner, k))
    }

    /// How many numbers below `max` no wire into `owner` uses.
    pub fn free(&self, wires: &[EffectGraphWire], owner: u32) -> u32 {
        (0..self.max).filter(|&k| !self.taken(wires, owner, k)).count() as u32
    }

    /// The owner's saved count, or the family's default.
    pub fn count(&self, owner: &EffectGraphNode) -> u32 {
        match owner.params.get(self.count_param) {
            Some(SerializedParamValue::Float { value }) if value.is_finite() && *value >= 0.0 => value.round() as u32,
            Some(SerializedParamValue::Int { value }) => u32::try_from(*value).unwrap_or(self.default_count),
            _ => self.default_count,
        }
    }

    /// Grow the owner's count so number `k` has its port. A count already that
    /// big is left alone, so nothing shrinks and a count is written only when
    /// it must change.
    pub fn grow_to_cover(&self, owner: &mut EffectGraphNode, k: u32) {
        let needed = k + 1;
        if self.count(owner) < needed {
            owner.params.insert(self.count_param.to_string(), SerializedParamValue::Float { value: needed as f32 });
        }
    }

    /// Take the next free number on `owner` and grow its count to cover it.
    /// The caller wires the returned number. `None` when the family is full or
    /// the owner is missing.
    pub fn append_member(&self, nodes: &mut [EffectGraphNode], wires: &[EffectGraphWire], owner: u32) -> Option<u32> {
        let k = self.next_free(wires, owner)?;
        let node = nodes.iter_mut().find(|node| node.id == owner)?;
        self.grow_to_cover(node, k);
        Some(k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> EffectGraphNode {
        serde_json::from_value(serde_json::json!({"id": 1, "nodeId": "world", "typeId": "node.physics_world"})).unwrap()
    }

    fn member(from: u32, port: &str) -> EffectGraphWire {
        EffectGraphWire { from_node: from, from_port: "body".into(), to_node: 1, to_port: port.into() }
    }

    #[test]
    fn append_takes_the_lowest_free_number_and_never_renumbers() {
        let mut nodes = vec![world()];
        let wires = vec![member(10, "body_0"), member(11, "body_2")];
        assert_eq!(WORLD_BODIES.append_member(&mut nodes, &wires, 1), Some(1));
        assert!(nodes[0].params.is_empty(), "number 1 is inside the default count");
        assert_eq!(WORLD_BODIES.free(&wires, 1), PHYSICS_WORLD_MAX_BODIES - 2);
    }

    #[test]
    fn append_past_the_count_grows_it_and_a_full_family_refuses() {
        let mut nodes = vec![world()];
        let mut wires: Vec<_> = (0..64).map(|k| member(100 + k, &format!("body_{k}"))).collect();
        assert_eq!(WORLD_BODIES.append_member(&mut nodes, &wires, 1), Some(64));
        assert_eq!(WORLD_BODIES.count(&nodes[0]), 65);
        wires.extend((64..PHYSICS_WORLD_MAX_BODIES).map(|k| member(100 + k, &format!("body_{k}"))));
        assert_eq!(WORLD_BODIES.append_member(&mut nodes, &wires, 1), None);
    }

    #[test]
    fn a_copy_group_takes_the_next_group_number() {
        let mut nodes = vec![world()];
        let wires = vec![member(10, "copies_0")];
        assert_eq!(world_family(Family::Copies).append_member(&mut nodes, &wires, 1), Some(1));
        assert_eq!(WORLD_COPY_GROUPS.count(&nodes[0]), 2);
    }
}
