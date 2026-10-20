//! Which body slots of a Physics World are taken: the one rule every
//! authoring path allocates through (enable, duplicate, paste, Shatter).

use crate::effect_graph_def::{EffectGraphDef, EffectGraphWire};
use crate::scene_surface::PHYSICS_WORLD_BODY_SLOTS;

const _: () = assert!(PHYSICS_WORLD_BODY_SLOTS <= u64::BITS, "one mask bit per body slot");

/// A Physics World's taken body slots, one bit each. A slot is taken when
/// anything feeds its body or field input or reads its pose, so a body that
/// is switched off keeps its slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BodySlots(u64);

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
        let slot = if wire.to_node == world_id {
            wire.to_port.strip_prefix("body_acceleration_").or_else(|| wire.to_port.strip_prefix("body_"))
        } else if wire.from_node == world_id {
            wire.from_port.strip_prefix("pose_")
        } else {
            None
        };
        if let Some(slot) = slot.and_then(|slot| slot.parse::<u32>().ok()) {
            self.take(slot);
        }
    }

    /// Marks `slot` taken; slots past the world's capacity are ignored.
    pub fn take(&mut self, slot: u32) {
        if slot < PHYSICS_WORLD_BODY_SLOTS {
            self.0 |= 1 << slot;
        }
    }

    pub fn first_free(self) -> Option<u32> {
        self.free_slots().next()
    }

    /// The free slots, lowest first.
    pub fn free_slots(self) -> impl Iterator<Item = u32> {
        (0..PHYSICS_WORLD_BODY_SLOTS).filter(move |slot| self.0 & (1 << slot) == 0)
    }

    pub fn free(self) -> u32 {
        PHYSICS_WORLD_BODY_SLOTS - self.0.count_ones()
    }
}

/// Every root-level Physics World's taken slots, from one pass over the
/// wires: what a structural sync reads for every object card at once.
#[derive(Debug, Clone, Default)]
pub struct WorldBodySlots(Vec<(u32, BodySlots)>);

impl WorldBodySlots {
    pub fn of(graph: &EffectGraphDef) -> Self {
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

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire {
            from_node,
            from_port: from_port.into(),
            to_node,
            to_port: to_port.into(),
        }
    }

    #[test]
    fn body_field_and_pose_wires_each_take_their_slot() {
        let wires = [
            wire(10, "body", 1, "body_0"),
            wire(11, "field", 1, "body_acceleration_2"),
            wire(1, "pose_3", 12, "transform"),
            wire(13, "body", 9, "body_1"),
        ];
        let slots = BodySlots::of(&wires, 1);
        assert_eq!(slots.free_slots().take(3).collect::<Vec<_>>(), [1, 4, 5]);
        assert_eq!(slots.first_free(), Some(1));
        assert_eq!(slots.free(), PHYSICS_WORLD_BODY_SLOTS - 3);
    }

    #[test]
    fn a_full_world_has_no_free_slot() {
        let wires: Vec<_> = (0..PHYSICS_WORLD_BODY_SLOTS).map(|slot| wire(100 + slot, "body", 1, &format!("body_{slot}"))).collect();
        let slots = BodySlots::of(&wires, 1);
        assert_eq!(slots.first_free(), None);
        assert_eq!(slots.free(), 0);
    }
}
