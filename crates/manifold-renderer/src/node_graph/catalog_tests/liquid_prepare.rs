//! Install builds every liquid pipeline: a second `prepare_pipelines` pass, or
//! a second install on the same device, compiles nothing (BUG-jtod (first
//! played frame compiles pipelines)). Counts the device's own caches, so
//! tests running in parallel cannot disturb the numbers.

use std::path::Path;

use manifold_gpu::GpuTextureFormat;

use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;

const LIQUID_PRESETS: [&str; 4] = [
    "WaterDamBreakGpuFlip.json",
    "WaterDamBreakMatter.json",
    "WaterFloatingBoxMatter.json",
    "WaterStillPoolMatter.json",
];

fn cache_len(device: &manifold_gpu::GpuDevice) -> (usize, usize) {
    (device.compute_pipeline_cache_len(), device.render_pipeline_cache_len())
}

#[test]
fn liquid_prepare_pipelines_is_idempotent() {
    let _serial = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    for file in LIQUID_PRESETS {
        let device = manifold_node_engine::gpu::context::test_gpu_device("liquid_prepare_pipelines_is_idempotent");
        // Each preset must populate its own cold live cache.
        manifold_gpu::testkit::load_disk_shader_caches(&device);
        assert_eq!(cache_len(&device), (0, 0), "{file}: install starts cold");
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/generator-presets").join(file);
        let json = std::fs::read_to_string(path).expect("preset reads");
        let install = || {
            PresetRuntime::from_json_str_with_device(&json, &registry, device.clone(), 64, 64, GpuTextureFormat::Rgba16Float, None)
                .unwrap_or_else(|e| panic!("{file} installs: {e:?}"))
        };
        let mut runtime = install();
        let installed = cache_len(&device);
        for node in runtime.graph.nodes_mut() {
            node.node.prepare_pipelines(&device);
        }
        assert_eq!(cache_len(&device), installed, "{file}: a second prepare_pipelines pass compiled pipelines");
        let _again = install();
        assert_eq!(cache_len(&device), installed, "{file}: a second install compiled pipelines");
    }
}
