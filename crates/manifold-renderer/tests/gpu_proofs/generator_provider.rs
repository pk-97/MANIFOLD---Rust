use std::sync::Arc;

use manifold_core::{PresetTypeId, params::ParamManifest};
use manifold_foundation::cold_touch::{ColdTouchKind, cold_touch_count};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_node_engine::{
    gpu::{gpu_encoder::GpuEncoder, render_target::RenderTarget},
    runtime::{generator_provider::generator_provider, preset_context::PresetContext},
};

use manifold_renderer::{generator_renderer::GeneratorRenderer, generators::registry::GeneratorRegistry};

const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
const WORKER: &str = "MANIFOLD_GENERATOR_PROVIDER_PROOF_WORKER";

// Like the RT instancing probe, re-enter exactly one test in a fresh process.
// The parent never opens a device; the child inherits the GPU gate queue.
fn isolated(test: &str) -> Option<String> {
    let module = module_path!().split_once("::").expect("crate prefix").1;
    let name = format!("{module}::{test}");
    if std::env::var(WORKER).as_deref() == Ok(name.as_str()) {
        return None;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env(WORKER, &name)
        .env("MANIFOLD_LOG_REBUILD_REASON", "1")
        .output()
        .expect("spawn isolated generator provider proof");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 probe log");
    assert!(output.status.success(), "{name} failed:\n{}\n{stderr}", String::from_utf8_lossy(&output.stdout));
    assert!(stderr.contains("generator-provider-proof-complete"), "child must execute its proof: {stderr}");
    Some(stderr)
}

fn cache_counts(device: &GpuDevice) -> (usize, usize) {
    (device.compute_pipeline_cache_len(), device.render_pipeline_cache_len())
}

fn compiles() -> u64 {
    cold_touch_count(ColdTouchKind::PipelineCompile)
}

fn cold_device_with_shader_cache(label: &str) -> Arc<GpuDevice> {
    let device = Arc::new(GpuDevice::new_queued(label));
    // Match the app/headless harness: reuse source-keyed translation and Metal
    // binaries, not live pipeline objects. Every cache miss still records a
    // PipelineCompile before consulting these disk caches, so missing provider
    // requests cannot be hidden by a previous process warming the disk cache.
    let cache = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"))
        .join("Library/Caches/com.latentspace.manifold");
    std::fs::create_dir_all(&cache).expect("shader cache directory");
    let before = compiles();
    device.load_pipeline_archive(&cache.join("pipeline_cache.metallib"));
    device.load_msl_cache(&cache.join("msl_cache"));
    assert_eq!(cache_counts(&device), (0, 0), "disk caches must not warm the device");
    assert_eq!(compiles(), before, "loading disk caches must not request pipelines");
    device
}

#[test]
fn registered_prewarm_preserves_the_direct_pipeline_cache() {
    if let Some(log) = isolated("registered_prewarm_preserves_the_direct_pipeline_cache") {
        let sequence = |start: &str, end: &str| -> Vec<String> {
            log.split_once(start).expect("start marker").1
                .split_once(end).expect("end marker").0.lines()
                .filter(|line| line.starts_with("[pipeline-compile]"))
                .map(str::to_owned).collect()
        };
        let provider = sequence("provider-prewarm-start", "provider-prewarm-end");
        let direct = sequence("direct-prewarm-start", "direct-prewarm-end");
        // Bokeh closes the explicitly ordered prewarms. The following atom
        // sweep walks a fresh AHashMap, whose iteration order is unspecified.
        let ordered_end = |lines: &[String]| {
            lines.iter().position(|line| line == "[pipeline-compile] label=node.bokeh_gather")
                .expect("last explicitly ordered prewarm") + 1
        };
        assert_eq!(
            provider[..ordered_end(&provider)], direct[..ordered_end(&direct)],
            "explicit cold compilation order must match",
        );
        return;
    }

    let provider_device = cold_device_with_shader_cache("generator provider parity");
    let provider = generator_provider();
    let registry = GeneratorRegistry::new(FORMAT);

    eprintln!("provider-prewarm-start");
    let before = compiles();
    (provider.prewarm)(&provider_device, FORMAT);
    let provider_compiles = compiles() - before;
    eprintln!("provider-prewarm-end");
    let provider_counts = cache_counts(&provider_device);

    let direct_device = cold_device_with_shader_cache("generator direct baseline");
    eprintln!("direct-prewarm-start");
    let before = compiles();
    registry.prewarm_all(&direct_device);
    let direct_compiles = compiles() - before;
    eprintln!("direct-prewarm-end");
    let direct_counts = cache_counts(&direct_device);
    assert!(direct_counts.0 > 0 && direct_counts.1 > 0, "nonempty compute and render baselines");
    assert_eq!(provider_counts, direct_counts, "provider must populate the entire cold cache");
    assert_eq!(provider_compiles, direct_compiles, "include pipelines outside the two caches");

    // Cache entries never evict: equal cardinalities plus D minus P empty
    // proves equal key sets, even if two different sets have the same size.
    let before = compiles();
    registry.prewarm_all(&provider_device);
    assert_eq!(cache_counts(&provider_device), provider_counts, "direct warming found a missing key");
    assert_eq!(compiles(), before, "direct warming after provider must compile nothing");
    // The direct replay above compiled nothing, so this remains the device
    // warmed solely by the provider. Do not pay for another full prewarm.
    provider_prewarm_first_generator_frame_compiles_nothing(provider_device);
    eprintln!("generator-provider-proof-complete");
}

fn provider_prewarm_first_generator_frame_compiles_nothing(device: Arc<GpuDevice>) {
    let provider = generator_provider();
    let mut generator = (provider.create)(
        device.clone(), FORMAT, &PresetTypeId::new("Plasma"), None,
        16, 16, false, None, None,
    ).expect("bundled Plasma generator");
    let target = RenderTarget::new(&device, 16, 16, FORMAT, "provider first frame");
    let ctx = PresetContext {
        time: 0.0, beat: 0.0, dt: 1.0 / 60.0,
        width: 16, height: 16, output_width: 16, output_height: 16,
        aspect: 1.0, owner_key: 0, is_clip_level: false,
        frame_count: 0, anim_progress: 0.0, trigger_count: 0,
    };
    let before = compiles();
    let cache_before = cache_counts(&device);
    let mut encoder = device.create_encoder("provider first frame");
    generator.render(
        &mut GpuEncoder::new(&mut encoder, &device), &target.texture,
        &ctx, &ParamManifest::default(),
    );
    encoder.commit_and_wait_completed();
    assert!(generator.errors().is_empty(), "first frame must render: {:?}", generator.errors());
    assert_eq!(compiles(), before, "first generator frame must compile no pipeline");
    assert_eq!(cache_counts(&device), cache_before);
}

#[test]
fn new_unwarmed_does_not_compile_generator_pipelines() {
    if isolated("new_unwarmed_does_not_compile_generator_pipelines").is_some() {
        return;
    }
    let device = Arc::new(GpuDevice::new_queued("generator new_unwarmed"));
    assert_eq!(cache_counts(&device), (0, 0));
    let before = compiles();
    let _renderer = GeneratorRenderer::new_unwarmed(device.clone(), 16, 16, FORMAT, 0);
    assert_eq!(cache_counts(&device), (0, 0));
    assert_eq!(compiles(), before, "new_unwarmed must make no compile requests");
    eprintln!("generator-provider-proof-complete");
}
