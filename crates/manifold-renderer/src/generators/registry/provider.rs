//! The catalog is the single provider; the existing registry owns its order and policy.

use super::GeneratorRegistry;
use manifold_core::{PresetTypeId, effect_graph_def::EffectGraphDef, effects::RelightParams, params::ParamManifest};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_node_engine::runtime::{PresetRuntime, generator_provider::GeneratorProvider};
use std::sync::Arc;

fn prewarm(device: &Arc<GpuDevice>, format: GpuTextureFormat) {
    GeneratorRegistry::new(format).prewarm_all(device);
}

fn create(
    device: Arc<GpuDevice>,
    format: GpuTextureFormat,
    gen_type: &PresetTypeId,
    override_def: Option<&EffectGraphDef>,
    width: u32,
    height: u32,
    is_watched: bool,
    manifest: Option<&ParamManifest>,
    relight: Option<&RelightParams>,
) -> Option<Box<PresetRuntime>> {
    GeneratorRegistry::new(format).create_with_override(
        device, gen_type, override_def, width, height, is_watched, manifest, relight,
    )
}

inventory::submit! {
    GeneratorProvider { prewarm, create }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_the_single_generator_provider() {
        let provider = manifold_node_engine::runtime::generator_provider::generator_provider();
        assert!(std::ptr::fn_addr_eq(provider.prewarm, prewarm as fn(&Arc<GpuDevice>, GpuTextureFormat)));
        assert!(std::ptr::fn_addr_eq(provider.create, create as manifold_node_engine::runtime::generator_provider::CreateGenerator));
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn registered_prewarm_preserves_the_direct_pipeline_cache() {
        let device = manifold_gpu::testkit::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = GeneratorRegistry::new(format);
        registry.prewarm_all(&device.arc());
        let before = (device.compute_pipeline_cache_len(), device.render_pipeline_cache_len());
        let provider = manifold_node_engine::runtime::generator_provider::generator_provider();
        (provider.prewarm)(&device.arc(), format);
        assert_eq!(before, (device.compute_pipeline_cache_len(), device.render_pipeline_cache_len()));
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn new_unwarmed_does_not_compile_generator_pipelines() {
        let device = manifold_gpu::testkit::test_device();
        let before = (device.compute_pipeline_cache_len(), device.render_pipeline_cache_len());
        let _renderer = crate::generator_renderer::GeneratorRenderer::new_unwarmed(
            device.arc(), 16, 16, GpuTextureFormat::Rgba16Float, 0,
        );
        assert_eq!(before, (device.compute_pipeline_cache_len(), device.render_pipeline_cache_len()));
    }
}
