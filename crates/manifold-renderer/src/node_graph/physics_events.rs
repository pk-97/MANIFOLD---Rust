//! Graph-facing adapters for native physics impulse events.

use manifold_physics::FieldValue;
use manifold_physics::input::AppliedEvent;

use crate::node_graph::physics::{ResolvedRigidImpulse, RigidImpulseTargets};

/// Which native simulation should receive a resolved graph impulse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImpulseTarget {
    Rigid(RigidImpulseTargets),
    Fluid,
    /// One captured field delivered to both participants of a shared worker.
    /// The source sequence is admitted once, even when both materials respond.
    FluidAndRigid(RigidImpulseTargets),
}

impl ImpulseTarget {
    pub fn affects_fluid(self) -> bool {
        matches!(self, Self::Fluid | Self::FluidAndRigid(_))
    }

    pub fn rigid_targets(self) -> Option<RigidImpulseTargets> {
        match self {
            Self::Rigid(targets) | Self::FluidAndRigid(targets) => Some(targets),
            Self::Fluid => None,
        }
    }

    /// Merge authored selections which resolve to one shared native owner.
    pub(crate) fn union(self, other: Self) -> Self {
        let fluid = self.affects_fluid() || other.affects_fluid();
        let rigid = match (self.rigid_targets(), other.rigid_targets()) {
            (Some(left), Some(right)) => Some(RigidImpulseTargets {
                bodies: left.bodies | right.bodies,
                copies: left.copies || right.copies,
            }),
            (left, right) => left.or(right),
        };
        match (fluid, rigid) {
            (true, Some(targets)) => Self::FluidAndRigid(targets),
            (true, None) => Self::Fluid,
            (false, Some(targets)) => Self::Rigid(targets),
            (false, None) => unreachable!("each input has at least one recipient"),
        }
    }
}

/// An owned, resolved impulse ready for native fixed-tick admission.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedNodeImpulse {
    pub field: FieldValue,
    pub target: ImpulseTarget,
}

/// Convert a rigid-native receipt without allocating or dropping it.
pub(crate) fn map_rigid_receipt(
    event: AppliedEvent<ResolvedRigidImpulse>,
    consume: &mut dyn FnMut(AppliedEvent<ResolvedNodeImpulse>),
) {
    let AppliedEvent {
        source,
        applied,
        lateness,
        value: ResolvedRigidImpulse { field, targets },
    } = event;
    consume(AppliedEvent {
        source,
        applied,
        lateness,
        value: ResolvedNodeImpulse {
            field,
            target: ImpulseTarget::Rigid(targets),
        },
    });
}
