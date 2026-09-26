//! SCENE_MODIFIER_RT_DESIGN.md section 5.4 (P5): explicit per-frame validity.
//!
//! A GPU completion fence can only say "no fault"; it cannot say "this frame
//! traced the geometry the raster drew". `FrameRenderStatus` is the
//! allocation-free validity signal every renderer encoder wrapper carries:
//! nodes merge pending/failed into the wrapper as they evaluate, and the
//! pipeline/export boundary reads the merged result. `EffectNodeContext::
//! error` stays the diagnostic channel; this is the rejectable fact.

/// Validity of one produced frame. `merge` severity is Failed >
/// PendingGeometry > Complete; the first failure wins and a later success
/// never clears it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameRenderStatus {
    /// Every input was current and every update encoded successfully.
    #[default]
    Complete,
    /// A geometry source was not ready this frame (async content in
    /// flight). Warmup treats this as incomplete preparation; export
    /// rejects it like a failure — pending at the export boundary is an
    /// error, not permission to evaluate again.
    PendingGeometry,
    /// The frame is invalid and cannot be made valid by a later success.
    Failed(FrameRenderFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameRenderFailure {
    /// An object's geometry failed validation (counts, strides, weights).
    InvalidGeometry,
    /// A surface-rendering scratch resource failed admission or allocation.
    SurfaceAllocation,
    /// The RT update needed preparation the resident set did not cover.
    RtNeedsPreparation,
    /// An RT admission/allocation failure refused the update.
    RtAllocation,
    /// Encoding the RT update itself failed.
    RtEncode,
}

impl FrameRenderStatus {
    /// Fold `next` into `self`: failure beats pending beats complete; the
    /// FIRST failure is kept when several land in one frame.
    pub fn merge(&mut self, next: Self) {
        *self = match (*self, next) {
            (failed @ Self::Failed(_), _) => failed,
            (_, failed @ Self::Failed(_)) => failed,
            (pending @ Self::PendingGeometry, _) => pending,
            (_, pending @ Self::PendingGeometry) => pending,
            _ => Self::Complete,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_severity_and_first_failure_wins() {
        let mut status = FrameRenderStatus::Complete;
        status.merge(FrameRenderStatus::Complete);
        assert_eq!(status, FrameRenderStatus::Complete);
        status.merge(FrameRenderStatus::PendingGeometry);
        assert_eq!(status, FrameRenderStatus::PendingGeometry);
        // A later success cannot clear pending.
        status.merge(FrameRenderStatus::Complete);
        assert_eq!(status, FrameRenderStatus::PendingGeometry);
        status.merge(FrameRenderStatus::Failed(FrameRenderFailure::RtEncode));
        assert_eq!(
            status,
            FrameRenderStatus::Failed(FrameRenderFailure::RtEncode)
        );
        // First failure wins over a later one.
        status.merge(FrameRenderStatus::Failed(FrameRenderFailure::InvalidGeometry));
        assert_eq!(
            status,
            FrameRenderStatus::Failed(FrameRenderFailure::RtEncode)
        );
        // Failure beats pending regardless of order.
        let mut other = FrameRenderStatus::Failed(FrameRenderFailure::RtAllocation);
        other.merge(FrameRenderStatus::PendingGeometry);
        assert_eq!(
            other,
            FrameRenderStatus::Failed(FrameRenderFailure::RtAllocation)
        );
    }
}
