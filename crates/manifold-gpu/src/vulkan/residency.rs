//! Residency sets are a Metal idea; Vulkan keeps every allocation resident,
//! so the manager has nothing to track.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

use crate::GpuResidencyStats;

pub struct GpuResidencyManager {
    _reserved: (),
}

impl GpuResidencyManager {
    /// Apply queued registration changes.
    pub fn drain(&mut self) {
        todo!("vulkan P1")
    }

    pub fn stats(&self) -> GpuResidencyStats {
        todo!("vulkan P1")
    }
}
