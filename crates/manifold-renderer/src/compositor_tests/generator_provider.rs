use manifold_gpu::GpuTextureFormat;

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
