use crate::generator::Generator;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

/// Factory that maps GeneratorTypeId to concrete Generator instances.
/// Pipeline compilation happens at creation time (expensive — do at startup or first use).
///
/// All generators are registered via `inventory::submit!` in their implementation files.
pub struct GeneratorRegistry {
    target_format: GpuTextureFormat,
}

impl GeneratorRegistry {
    pub fn new(target_format: GpuTextureFormat) -> Self {
        Self { target_format }
    }

    /// Pre-compile all generator pipelines into the binary archive.
    /// Creates and immediately drops each generator — the compiled Metal pipeline
    /// binaries persist in the archive. Call at startup before `save_pipeline_archive()`.
    pub fn prewarm_all(&self, device: &GpuDevice) {
        let types: Vec<_> = inventory::iter::<super::registration::GeneratorFactory>
            .into_iter()
            .collect();
        log::info!("Pre-warming {} generator pipelines...", types.len());
        for factory in &types {
            let _ = (factory.create)(device);
        }
        log::info!("Generator pipeline pre-warm complete");
    }

    /// Create a new generator instance for the given type.
    pub fn create(
        &self,
        device: &GpuDevice,
        gen_type: &manifold_core::GeneratorTypeId,
    ) -> Option<Box<dyn Generator>> {
        let _fmt = self.target_format;
        for factory in inventory::iter::<super::registration::GeneratorFactory> {
            if factory.id == *gen_type {
                return Some((factory.create)(device));
            }
        }
        log::warn!("Generator type {:?} not yet implemented", gen_type);
        None
    }
}
