//! Which body slots of a Physics World are taken: the rule Shatter allocates
//! its pieces through. A slot is taken when a body is wired into the world's
//! `body_k` input (`crate::physics_members`); a body switched off keeps its
//! slot. A world takes any number of bodies. Its `bodies` count only sizes
//! its ports, so the edit that wires a slot past the count grows the count
//! with [`fit_world_bodies`].

use crate::effect_graph_def::{EffectGraphNode, EffectGraphWire};
use crate::physics_members::{Family, member_input};
use crate::scene_variadic::{WORLD_BODIES, WORLD_COPY_GROUPS};

/// A Physics World's taken body slots, one bit each.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BodySlots(Vec<u64>);

impl BodySlots {
    /// One pass over `wires`.
    pub fn of(wires: &[EffectGraphWire], world_id: u32) -> Self {
        let mut slots = Self::default();
        for wire in wires {
            if wire.to_node == world_id
                && let Some((Family::Body, slot)) = member_input(&wire.to_port)
            {
                slots.take(slot);
            }
        }
        slots
    }

    pub fn take(&mut self, slot: u32) {
        let word = slot as usize / 64;
        if self.0.len() <= word {
            self.0.resize(word + 1, 0);
        }
        self.0[word] |= 1 << (slot % 64);
    }

    fn is_taken(&self, slot: u32) -> bool {
        self.0
            .get(slot as usize / 64)
            .is_some_and(|word| word & (1 << (slot % 64)) != 0)
    }

    pub fn first_free(&self) -> u32 {
        self.free_slots().next().expect("the slots never run out")
    }

    /// The free slots, lowest first, without end.
    pub fn free_slots(&self) -> impl Iterator<Item = u32> + '_ {
        (0..).filter(move |&slot| !self.is_taken(slot))
    }
}

/// A world's `bodies` count: its saved value, or the default.
pub fn world_bodies(world: &EffectGraphNode) -> u32 {
    WORLD_BODIES.count(world)
}

/// Grow world `world_id`'s `bodies` and `copy_groups` counts so every member
/// number `wires` take has its ports. Every edit that wires a member calls
/// this once after wiring. A world already that big is left alone, so
/// nothing shrinks and a saved count is written only when it must change.
pub fn fit_world_bodies(nodes: &mut [EffectGraphNode], wires: &[EffectGraphWire], world_id: u32) {
    let Some(world) = nodes.iter_mut().find(|node| node.id == world_id) else {
        return;
    };
    let highest = |family: Family| {
        wires
            .iter()
            .filter(|wire| wire.to_node == world_id)
            .filter_map(|wire| {
                member_input(&wire.to_port)
                    .filter(|(found, _)| *found == family)
                    .map(|(_, k)| k)
            })
            .max()
    };
    if let Some(k) = highest(Family::Body) {
        WORLD_BODIES.grow_to_cover(world, k);
    }
    if let Some(k) = highest(Family::Copies) {
        WORLD_COPY_GROUPS.grow_to_cover(world, k);
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
    fn only_a_body_wired_into_the_world_takes_a_slot_and_slots_never_run_out() {
        let mut wires = vec![
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
        wires.extend((0..1_000).map(|slot| wire(1000 + slot, "body", 1, &format!("body_{slot}"))));
        assert_eq!(BodySlots::of(&wires, 1).first_free(), 1_000);
    }

    #[test]
    fn fitting_a_slot_grows_the_world_count_and_never_shrinks_it() {
        let mut nodes: Vec<EffectGraphNode> = vec![
            serde_json::from_value(serde_json::json!(
                {"id": 1, "nodeId": "world", "typeId": "node.physics_world"}
            ))
            .unwrap(),
        ];
        assert_eq!(world_bodies(&nodes[0]), PHYSICS_WORLD_DEFAULT_BODIES);
        let mut wires = vec![wire(10, "body", 1, "body_0")];
        fit_world_bodies(&mut nodes, &wires, 1);
        assert!(
            nodes[0].params.is_empty(),
            "a slot inside the count writes nothing"
        );
        wires.push(wire(11, "body", 1, "body_999"));
        fit_world_bodies(&mut nodes, &wires, 1);
        assert_eq!(world_bodies(&nodes[0]), 1_000);
        wires.truncate(1);
        fit_world_bodies(&mut nodes, &wires, 1);
        assert_eq!(
            world_bodies(&nodes[0]),
            1_000,
            "unwiring never shrinks the world"
        );
    }
}
