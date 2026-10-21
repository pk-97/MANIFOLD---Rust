//! Which body slots of a Physics World are taken: the one rule every
//! authoring path allocates through (enable, duplicate, paste, Shatter).
//! A slot is taken when a body is wired into the world's `body_k` input
//! (`crate::physics_members`); a body switched off keeps its slot. A world
//! takes up to [`PHYSICS_WORLD_MAX_BODIES`] bodies. Its `bodies` count only
//! sizes its ports, so the edit that wires a slot past the count grows the
//! count with [`fit_world_bodies`]; a world with every slot taken refuses.

use crate::effect_graph_def::{EffectGraphNode, EffectGraphWire};
use crate::physics_members::{Family, member_input};
use crate::scene_surface::PHYSICS_WORLD_MAX_BODIES;
use crate::scene_variadic::{WORLD_BODIES, WORLD_COPY_GROUPS};

const WORDS: usize = (PHYSICS_WORLD_MAX_BODIES as usize).div_ceil(64);

/// A Physics World's taken body slots, one bit each. A slot is taken when a
/// body is wired into its `body_k` input, so a body that is switched off
/// keeps its slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BodySlots([u64; WORDS]);

impl BodySlots {
    /// One pass over `wires`.
    pub fn of(wires: &[EffectGraphWire], world_id: u32) -> Self {
        let mut slots = Self::default();
        for wire in wires {
            slots.take_wire(wire, world_id);
        }
        slots
    }

    fn take_wire(&mut self, wire: &EffectGraphWire, world_id: u32) {
        if wire.to_node == world_id
            && let Some((Family::Body, slot)) = member_input(&wire.to_port)
        {
            self.take(slot);
        }
    }

    /// Marks `slot` taken; slots past the most a world holds are ignored.
    pub fn take(&mut self, slot: u32) {
        if slot < PHYSICS_WORLD_MAX_BODIES {
            self.0[slot as usize / 64] |= 1 << (slot % 64);
        }
    }

    fn is_taken(self, slot: u32) -> bool {
        self.0[slot as usize / 64] & (1 << (slot % 64)) != 0
    }

    pub fn first_free(self) -> Option<u32> {
        self.free_slots().next()
    }

    /// The free slots, lowest first.
    pub fn free_slots(self) -> impl Iterator<Item = u32> {
        (0..PHYSICS_WORLD_MAX_BODIES).filter(move |&slot| !self.is_taken(slot))
    }

    pub fn free(self) -> u32 {
        PHYSICS_WORLD_MAX_BODIES - self.0.iter().map(|word| word.count_ones()).sum::<u32>()
    }

    /// The highest taken slot.
    pub fn highest(self) -> Option<u32> {
        (0..PHYSICS_WORLD_MAX_BODIES).rev().find(|&slot| self.is_taken(slot))
    }
}

/// A world's `bodies` count: its saved value, or the count every world had
/// before the count existed.
pub fn world_bodies(world: &EffectGraphNode) -> u32 {
    WORLD_BODIES.count(world)
}

/// Grow world `world_id`'s `bodies` and `copy_groups` counts so every member
/// number `wires` take has its ports. Every edit that wires a member calls
/// this once after wiring. A world already that big is left alone, so
/// nothing shrinks and a saved count is written only when it must change.
pub fn fit_world_bodies(nodes: &mut [EffectGraphNode], wires: &[EffectGraphWire], world_id: u32) {
    let Some(world) = nodes.iter_mut().find(|node| node.id == world_id) else { return };
    let highest = |family: Family| {
        wires
            .iter()
            .filter(|wire| wire.to_node == world_id)
            .filter_map(|wire| member_input(&wire.to_port).filter(|(found, _)| *found == family).map(|(_, k)| k))
            .max()
    };
    if let Some(k) = highest(Family::Body) {
        WORLD_BODIES.grow_to_cover(world, k);
    }
    if let Some(k) = highest(Family::Copies) {
        WORLD_COPY_GROUPS.grow_to_cover(world, k);
    }
}

/// Every root-level Physics World's taken slots, from one pass over the
/// wires: what a structural sync reads for every object card at once.
#[derive(Debug, Clone, Default)]
pub struct WorldBodySlots(Vec<(u32, BodySlots)>);

impl WorldBodySlots {
    pub fn of(graph: &crate::effect_graph_def::EffectGraphDef) -> Self {
        let mut worlds: Vec<(u32, BodySlots)> = graph
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.physics_world")
            .map(|node| (node.id, BodySlots::default()))
            .collect();
        for wire in &graph.wires {
            for (world, slots) in &mut worlds {
                slots.take_wire(wire, *world);
            }
        }
        Self(worlds)
    }

    pub fn get(&self, world_id: u32) -> Option<BodySlots> {
        self.0.iter().find(|(world, _)| *world == world_id).map(|(_, slots)| *slots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_surface::PHYSICS_WORLD_DEFAULT_BODIES;

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire {
            from_node,
            from_port: from_port.into(),
            to_node,
            to_port: to_port.into(),
        }
    }

    #[test]
    fn only_a_body_wired_into_the_world_takes_a_slot() {
        let wires = [
            wire(10, "body", 1, "body_0"),
            wire(11, "field", 1, "body_acceleration_2"),
            wire(1, "pose_3", 12, "transform"),
            wire(13, "body", 9, "body_1"),
            wire(14, "body", 1, "body_200"),
            wire(15, "body", 1, "copies_1"),
        ];
        let slots = BodySlots::of(&wires, 1);
        assert_eq!(
            slots.free_slots().take(3).collect::<Vec<_>>(),
            [1, 2, 3],
            "a force or pose wire names no slot, and copies are another family"
        );
        assert_eq!(slots.first_free(), Some(1));
        assert_eq!(slots.free(), PHYSICS_WORLD_MAX_BODIES - 2);
    }

    #[test]
    fn a_world_past_64_bodies_still_has_slots_and_a_full_world_has_none() {
        let mut wires: Vec<_> = (0..64).map(|slot| wire(1000 + slot, "body", 1, &format!("body_{slot}"))).collect();
        assert_eq!(BodySlots::of(&wires, 1).first_free(), Some(64), "the 64-body cap is lifted");
        wires.extend((64..PHYSICS_WORLD_MAX_BODIES).map(|slot| wire(1000 + slot, "body", 1, &format!("body_{slot}"))));
        let slots = BodySlots::of(&wires, 1);
        assert_eq!(slots.first_free(), None);
        assert_eq!(slots.free(), 0);
    }

    #[test]
    fn fitting_a_slot_grows_the_world_count_and_never_shrinks_it() {
        let mut nodes: Vec<EffectGraphNode> = vec![serde_json::from_value(serde_json::json!(
            {"id": 1, "nodeId": "world", "typeId": "node.physics_world"}
        ))
        .unwrap()];
        assert_eq!(world_bodies(&nodes[0]), PHYSICS_WORLD_DEFAULT_BODIES, "an old world has 64");
        let mut wires = vec![wire(10, "body", 1, "body_10")];
        fit_world_bodies(&mut nodes, &wires, 1);
        assert!(nodes[0].params.is_empty(), "a slot inside the count writes nothing");
        wires.push(wire(11, "body", 1, "body_64"));
        fit_world_bodies(&mut nodes, &wires, 1);
        assert_eq!(world_bodies(&nodes[0]), 65);
        wires.truncate(1);
        fit_world_bodies(&mut nodes, &wires, 1);
        assert_eq!(world_bodies(&nodes[0]), 65, "unwiring never shrinks the world");
        wires.push(wire(12, "body", 1, "body_255"));
        fit_world_bodies(&mut nodes, &wires, 1);
        assert_eq!(world_bodies(&nodes[0]), PHYSICS_WORLD_MAX_BODIES);
    }
}
