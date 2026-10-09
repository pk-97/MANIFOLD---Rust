//! Graph-facing adapters for native physics impulse events.

use manifold_physics::FieldValue;
use manifold_physics::input::AppliedEvent;

use crate::scene::impulse::ImpulseTarget;
use crate::water::physics::ResolvedRigidImpulse;

/// An owned, resolved impulse ready for native fixed-tick admission.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
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
