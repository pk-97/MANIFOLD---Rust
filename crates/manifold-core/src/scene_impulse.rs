//! Scene-space recipient selections for authored physics impulses.

use std::sync::Arc;

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

/// The bodies and copy groups of one Physics World that a hit or a coupled
/// liquid reaches: one bit per body slot and one per copy group, as many as
/// the world has. It is derived at graph expansion and never saved; cloning
/// it shares the bits, so a hit carries it without allocating.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RigidImpulseTargets {
    /// Bit `k` of word `k / 64`; no trailing zero word, so equal selections
    /// compare equal.
    bodies: Arc<[u64]>,
    groups: Arc<[u64]>,
}

fn with_bit(words: &[u64], index: usize) -> Arc<[u64]> {
    let mut words = words.to_vec();
    if words.len() <= index / 64 {
        words.resize(index / 64 + 1, 0);
    }
    words[index / 64] |= 1u64 << (index % 64);
    words.into()
}

fn has_bit(words: &[u64], index: usize) -> bool {
    words.get(index / 64).is_some_and(|word| word & (1u64 << (index % 64)) != 0)
}

fn union_words(left: &[u64], right: &[u64]) -> Arc<[u64]> {
    let (long, short) = if left.len() >= right.len() { (left, right) } else { (right, left) };
    let mut words = long.to_vec();
    for (word, other) in words.iter_mut().zip(short) {
        *word |= other;
    }
    words.into()
}

fn set_bits(words: &[u64]) -> impl Iterator<Item = usize> + '_ {
    words.iter().enumerate().flat_map(|(at, &word)| {
        (0..64).filter(move |bit| word & (1u64 << bit) != 0).map(move |bit| at * 64 + bit)
    })
}

impl RigidImpulseTargets {
    /// Body slot `index` alone. Always `Some`: the mask holds any slot.
    pub fn body(index: usize) -> Option<Self> {
        Some(Self { bodies: with_bit(&[], index), groups: Arc::default() })
    }

    /// Copy group `index` alone. Always `Some`: the mask holds any group.
    pub fn group(index: usize) -> Option<Self> {
        Some(Self { bodies: Arc::default(), groups: with_bit(&[], index) })
    }

    /// Select body slot `index`. Graph expansion only: it reallocates.
    pub fn insert_body(&mut self, index: usize) -> bool {
        self.bodies = with_bit(&self.bodies, index);
        true
    }

    /// Select copy group `index`. Graph expansion only: it reallocates.
    pub fn insert_group(&mut self, index: usize) -> bool {
        self.groups = with_bit(&self.groups, index);
        true
    }

    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty() && self.groups.is_empty()
    }

    pub fn contains_body(&self, index: usize) -> bool {
        has_bit(&self.bodies, index)
    }

    pub fn contains_group(&self, index: usize) -> bool {
        has_bit(&self.groups, index)
    }

    /// Whether any copy group is selected.
    pub fn has_groups(&self) -> bool {
        !self.groups.is_empty()
    }

    pub fn union(self, other: Self) -> Self {
        Self { bodies: union_words(&self.bodies, &other.bodies), groups: union_words(&self.groups, &other.groups) }
    }

    /// The selected body slots, lowest first.
    pub fn bodies(&self) -> impl Iterator<Item = usize> + '_ {
        set_bits(&self.bodies)
    }

    /// The selected copy groups, lowest first.
    pub fn groups(&self) -> impl Iterator<Item = usize> + '_ {
        set_bits(&self.groups)
    }
}

/// Which native simulation should receive a resolved graph impulse.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub fn affects_fluid(&self) -> bool {
        matches!(self, Self::Fluid | Self::FluidAndRigid(_))
    }

    pub fn rigid_targets(&self) -> Option<RigidImpulseTargets> {
        match self {
            Self::Rigid(targets) | Self::FluidAndRigid(targets) => Some(targets.clone()),
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
    fn the_mask_holds_any_body_and_copy_group() {
        let mut targets = RigidImpulseTargets::default();
        assert!(targets.is_empty());
        for body in [0, 63, 64, 200, 255, 256, 5_000] {
            assert!(targets.insert_body(body));
        }
        assert!(targets.insert_group(31));
        assert!(targets.insert_group(32));
        assert_eq!(targets.bodies().collect::<Vec<_>>(), [0, 63, 64, 200, 255, 256, 5_000]);
        assert_eq!(targets.groups().collect::<Vec<_>>(), [31, 32]);
        assert!(!targets.contains_body(65) && !targets.contains_body(usize::MAX));
        assert!(!targets.contains_group(30) && !targets.contains_group(usize::MAX));
        let group = RigidImpulseTargets::group(2).unwrap();
        assert!(group.has_groups() && !group.contains_body(2));
        assert_eq!(
            RigidImpulseTargets::body(3).unwrap().union(RigidImpulseTargets::body(300).unwrap()),
            RigidImpulseTargets::body(300).unwrap().union(RigidImpulseTargets::body(3).unwrap()),
            "a selection compares by what it selects"
        );
    }

    #[test]
    fn union_preserves_each_recipient_class() {
        let rigid = RigidImpulseTargets::body(0).unwrap();
        let combined = ImpulseTarget::Rigid(rigid.clone())
            .union(ImpulseTarget::FluidAndRigid(
                RigidImpulseTargets::body(200).unwrap().union(RigidImpulseTargets::group(5).unwrap()),
            ))
            .unwrap();
        let ImpulseTarget::FluidAndRigid(targets) = &combined else {
            panic!("water and bodies stay one shared owner: {combined:?}");
        };
        assert_eq!(targets.bodies().collect::<Vec<_>>(), [0, 200]);
        assert_eq!(targets.groups().collect::<Vec<_>>(), [5]);
        assert_eq!(
            ImpulseTarget::Fluid.union(ImpulseTarget::Fluid),
            Ok(ImpulseTarget::Fluid)
        );
        assert_eq!(
            ImpulseTarget::Rigid(rigid.clone()).union(ImpulseTarget::Rigid(rigid.clone())),
            Ok(ImpulseTarget::Rigid(rigid.clone()))
        );
        assert_eq!(ImpulseTarget::Swarm.union(ImpulseTarget::Swarm), Ok(ImpulseTarget::Swarm));
        assert!(ImpulseTarget::Swarm.union(ImpulseTarget::Fluid).is_err());
        assert!(ImpulseTarget::Rigid(rigid).union(ImpulseTarget::Swarm).is_err());
    }
}
