//! Scene-space recipient selections for authored physics impulses.

/// Live tick-start diagnostics. These counters are not a recorded physics take.
/// Counts only grow; the stage shows each increase once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneImpulseDiagnostics {
    pub started: u64,
    pub late: u64,
    /// Hits fired while the simulation was held (pause, Speed 0) or by a frame
    /// that failed; they never land, so resume never bursts.
    pub discarded: u64,
    /// Hits that could not be admitted at all.
    pub refused: u64,
    /// Why the newest refused hit was refused.
    pub last_refusal: Option<std::sync::Arc<str>>,
}

impl SceneImpulseDiagnostics {
    pub fn refuse(&mut self, reason: &str) {
        self.refused = self.refused.saturating_add(1);
        self.last_refusal = Some(reason.into());
    }
}

/// What a scene impulse runtime did with one Fire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImpulseFire {
    /// The parameter is not a scene impulse.
    NotImpulse,
    /// Every recipient admitted the hit; it lands on the next tick.
    Admitted,
    /// The scene's changed setup has not rendered yet, or a recipient is
    /// still preparing. Nothing was consumed; fire again after a render.
    NotReady,
}

const BODY_WORDS: usize = 4;

/// The bodies and copy groups of one Physics World that a hit or a coupled
/// liquid reaches: one bit per body slot and one per copy group. It is
/// derived at graph expansion and never saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RigidImpulseTargets {
    bodies: [u64; BODY_WORDS],
    groups: u32,
}

const _: () = assert!(
    RigidImpulseTargets::BODY_CAPACITY == crate::scene_surface::PHYSICS_WORLD_MAX_BODIES as usize,
    "the authored Physics World and the runtime body mask hold the same body count",
);
const _: () = assert!(
    RigidImpulseTargets::GROUP_CAPACITY == crate::scene_surface::PHYSICS_WORLD_MAX_COPY_GROUPS as usize,
    "the authored Physics World and the runtime group mask hold the same group count",
);

impl RigidImpulseTargets {
    /// Body slots the mask holds.
    pub const BODY_CAPACITY: usize = BODY_WORDS * u64::BITS as usize;
    /// Copy groups the mask holds.
    pub const GROUP_CAPACITY: usize = u32::BITS as usize;

    /// Body slot `index` alone; `None` past the mask.
    pub const fn body(index: usize) -> Option<Self> {
        let mut targets = Self { bodies: [0; BODY_WORDS], groups: 0 };
        if targets.insert_body(index) { Some(targets) } else { None }
    }

    /// Copy group `index` alone; `None` past the mask.
    pub const fn group(index: usize) -> Option<Self> {
        let mut targets = Self { bodies: [0; BODY_WORDS], groups: 0 };
        if targets.insert_group(index) { Some(targets) } else { None }
    }

    /// Select body slot `index`; false past the mask, which selects nothing.
    pub const fn insert_body(&mut self, index: usize) -> bool {
        if index >= Self::BODY_CAPACITY {
            return false;
        }
        self.bodies[index / 64] |= 1u64 << (index % 64);
        true
    }

    /// Select copy group `index`; false past the mask, which selects nothing.
    pub const fn insert_group(&mut self, index: usize) -> bool {
        if index >= Self::GROUP_CAPACITY {
            return false;
        }
        self.groups |= 1u32 << index;
        true
    }

    pub const fn is_empty(self) -> bool {
        let mut word = 0;
        while word < BODY_WORDS {
            if self.bodies[word] != 0 {
                return false;
            }
            word += 1;
        }
        self.groups == 0
    }

    pub const fn contains_body(self, index: usize) -> bool {
        index < Self::BODY_CAPACITY && (self.bodies[index / 64] & (1u64 << (index % 64))) != 0
    }

    pub const fn contains_group(self, index: usize) -> bool {
        index < Self::GROUP_CAPACITY && (self.groups & (1u32 << index)) != 0
    }

    /// Whether any copy group is selected.
    pub const fn has_groups(self) -> bool {
        self.groups != 0
    }

    pub const fn union(self, other: Self) -> Self {
        let mut bodies = [0; BODY_WORDS];
        let mut word = 0;
        while word < BODY_WORDS {
            bodies[word] = self.bodies[word] | other.bodies[word];
            word += 1;
        }
        Self { bodies, groups: self.groups | other.groups }
    }

    /// The selected body slots, lowest first.
    pub fn bodies(self) -> impl Iterator<Item = usize> {
        (0..Self::BODY_CAPACITY).filter(move |&index| self.contains_body(index))
    }

    /// The selected copy groups, lowest first.
    pub fn groups(self) -> impl Iterator<Item = usize> {
        (0..Self::GROUP_CAPACITY).filter(move |&index| self.contains_group(index))
    }
}

/// Which native simulation should receive a resolved graph impulse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImpulseTarget {
    Rigid(RigidImpulseTargets),
    Fluid,
    /// One captured field delivered to both participants of a shared worker.
    /// The source sequence is admitted once, even when both materials respond.
    FluidAndRigid(RigidImpulseTargets),
    /// Every live particle of a swarm feels the hit at its position, once, on
    /// the fire tick.
    Swarm,
}

impl ImpulseTarget {
    pub fn affects_fluid(self) -> bool {
        matches!(self, Self::Fluid | Self::FluidAndRigid(_))
    }

    pub fn rigid_targets(self) -> Option<RigidImpulseTargets> {
        match self {
            Self::Rigid(targets) | Self::FluidAndRigid(targets) => Some(targets),
            Self::Fluid | Self::Swarm => None,
        }
    }

    /// Merge authored selections which resolve to one shared native owner.
    /// A swarm owns its hits alone: only another selection of it merges in,
    /// and a swarm sharing an owner with water or bodies is refused.
    pub fn union(self, other: Self) -> Result<Self, String> {
        if self == Self::Swarm || other == Self::Swarm {
            return if self == other {
                Ok(Self::Swarm)
            } else {
                Err("Impulse: a swarm shares its owner with no other recipient".into())
            };
        }
        let fluid = self.affects_fluid() || other.affects_fluid();
        let rigid = match (self.rigid_targets(), other.rigid_targets()) {
            (Some(left), Some(right)) => Some(left.union(right)),
            (left, right) => left.or(right),
        };
        Ok(match (fluid, rigid) {
            (true, Some(targets)) => Self::FluidAndRigid(targets),
            (true, None) => Self::Fluid,
            (false, Some(targets)) => Self::Rigid(targets),
            (false, None) => unreachable!("each input has at least one recipient"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ImpulseTarget, RigidImpulseTargets};

    #[test]
    fn the_mask_holds_256_bodies_and_32_copy_groups() {
        assert_eq!(RigidImpulseTargets::BODY_CAPACITY, 256);
        assert_eq!(RigidImpulseTargets::GROUP_CAPACITY, 32);
        let mut targets = RigidImpulseTargets::default();
        assert!(targets.is_empty());
        for body in [0, 63, 64, 200, 255] {
            assert!(targets.insert_body(body));
        }
        assert!(!targets.insert_body(256), "past the mask selects nothing");
        assert!(targets.insert_group(31));
        assert!(!targets.insert_group(32));
        assert_eq!(targets.bodies().collect::<Vec<_>>(), [0, 63, 64, 200, 255]);
        assert_eq!(targets.groups().collect::<Vec<_>>(), [31]);
        assert!(!targets.contains_body(65) && !targets.contains_body(usize::MAX));
        assert!(!targets.contains_group(30) && !targets.contains_group(usize::MAX));
        assert_eq!(RigidImpulseTargets::body(256), None);
        assert_eq!(RigidImpulseTargets::group(32), None);
        let group = RigidImpulseTargets::group(2).unwrap();
        assert!(group.has_groups() && !group.contains_body(2));
    }

    #[test]
    fn union_preserves_each_recipient_class() {
        let rigid = RigidImpulseTargets::body(0).unwrap();
        let combined = ImpulseTarget::Rigid(rigid)
            .union(ImpulseTarget::FluidAndRigid(
                RigidImpulseTargets::body(200).unwrap().union(RigidImpulseTargets::group(5).unwrap()),
            ))
            .unwrap();
        let ImpulseTarget::FluidAndRigid(targets) = combined else {
            panic!("water and bodies stay one shared owner: {combined:?}");
        };
        assert_eq!(targets.bodies().collect::<Vec<_>>(), [0, 200]);
        assert_eq!(targets.groups().collect::<Vec<_>>(), [5]);
        assert_eq!(
            ImpulseTarget::Fluid.union(ImpulseTarget::Fluid),
            Ok(ImpulseTarget::Fluid)
        );
        assert_eq!(
            ImpulseTarget::Rigid(rigid).union(ImpulseTarget::Rigid(rigid)),
            Ok(ImpulseTarget::Rigid(rigid))
        );
        assert_eq!(ImpulseTarget::Swarm.union(ImpulseTarget::Swarm), Ok(ImpulseTarget::Swarm));
        assert!(ImpulseTarget::Swarm.union(ImpulseTarget::Fluid).is_err());
        assert!(ImpulseTarget::Rigid(rigid).union(ImpulseTarget::Swarm).is_err());
    }
}
