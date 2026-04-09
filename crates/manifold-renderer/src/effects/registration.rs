//! Distributed effect factory registration via `inventory`.
//!
//! Each effect submits an `EffectFactory` alongside its `EffectMetadata`.
//! The effect registry collects these to build processors at startup.

use crate::effect::PostProcessEffect;
use manifold_core::EffectTypeId;
use manifold_gpu::GpuDevice;

/// Factory entry for creating effect processor instances, submitted via `inventory::submit!`.
pub struct EffectFactory {
    pub id: EffectTypeId,
    pub create: fn(&GpuDevice) -> Box<dyn PostProcessEffect>,
}

inventory::collect!(EffectFactory);
