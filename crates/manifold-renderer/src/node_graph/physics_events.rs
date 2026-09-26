//! Graph-facing adapters for native physics impulse events.

use manifold_physics::FieldValue;
use manifold_physics::input::AppliedEvent;

use crate::node_graph::physics::{ResolvedRigidImpulse, RigidImpulseTargets};

/// Which native simulation should receive a resolved graph impulse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImpulseTarget {
    Rigid(RigidImpulseTargets),
    Fluid,
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

/// Convert a fluid-native receipt without allocating or dropping it.
pub(crate) fn map_fluid_receipt(
    event: AppliedEvent<FieldValue>,
    consume: &mut dyn FnMut(AppliedEvent<ResolvedNodeImpulse>),
) {
    let AppliedEvent {
        source,
        applied,
        lateness,
        value: field,
    } = event;
    consume(AppliedEvent {
        source,
        applied,
        lateness,
        value: ResolvedNodeImpulse {
            field,
            target: ImpulseTarget::Fluid,
        },
    });
}
