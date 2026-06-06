//! Distributed generator factory registration via `inventory`.
//!
//! Each generator submits a `GeneratorFactory` alongside its `GeneratorMetadata`.
//! The generator registry collects these to build the create() lookup map.

use crate::generator::Generator;
use manifold_core::PresetTypeId;
use manifold_gpu::GpuDevice;

/// Factory entry for creating generator instances, submitted via `inventory::submit!`.
pub struct GeneratorFactory {
    pub id: PresetTypeId,
    pub create: fn(&GpuDevice) -> Box<dyn Generator>,
}

inventory::collect!(GeneratorFactory);
