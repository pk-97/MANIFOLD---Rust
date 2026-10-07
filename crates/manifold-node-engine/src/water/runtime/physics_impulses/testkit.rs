//! Capture observations without exposing recipient or capture storage.
use crate::water::runtime::physics_impulses::{CapturedSceneImpulse, PreparedSceneImpulse};
#[cfg(feature = "gpu-proofs")]
use crate::water::physics_events::ImpulseTarget;
#[cfg(feature = "gpu-proofs")]
use manifold_core::NodeId;
use manifold_physics::{FieldValue, input::EventStamp};

impl PreparedSceneImpulse {
    #[doc(hidden)]
    #[cfg(feature = "gpu-proofs")]
    pub(crate) fn test_recipient_count(&self) -> usize { self.recipients.len() }
    #[doc(hidden)]
    #[cfg(feature = "gpu-proofs")]
    pub(crate) fn test_recipient_id(&self, index: usize) -> &NodeId { &self.recipients[index].id }
    #[doc(hidden)]
    #[cfg(feature = "gpu-proofs")]
    pub(crate) fn test_recipient_target(&self, index: usize) -> ImpulseTarget { self.recipients[index].target }
}
impl CapturedSceneImpulse {
    #[doc(hidden)]
    pub(crate) fn test_recipient_count(&self) -> usize { self.recipients.len() }
    #[doc(hidden)]
    pub(crate) fn test_stamp(&self, index: usize) -> EventStamp { self.stamps[index] }
    #[doc(hidden)]
    pub(crate) fn test_stamp_storage(&self) -> *const EventStamp { self.stamps.as_ptr() }
    #[doc(hidden)]
    pub(crate) fn test_field(&self) -> Option<&FieldValue> { self.field.as_ref() }
}
