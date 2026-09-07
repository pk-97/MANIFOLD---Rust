use std::path::PathBuf;
use std::sync::Arc;

use manifold_core::preset_def::PresetKind;
use manifold_playback::renderer::ClipRenderer;
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::preset_loader::set_project_presets;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

static TRACES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct ProbeLog;
impl log::Log for ProbeLog {
    fn enabled(&self, _: &log::Metadata) -> bool { true }
    fn log(&self, record: &log::Record) {
        let text = record.args().to_string();
        if text.contains("[RT-DIAG] trace frame=") {
            TRACES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if record.level() <= log::Level::Warn || text.contains("[RT-DIAG]") || text.contains("[RT-REPRO]") {
            eprintln!("{text}");
        }
    }
    fn flush(&self) {}
}
static LOGGER: ProbeLog = ProbeLog;

#[test]
fn warmup_probe_rt_repro_project() {
    let Some(project_path) = std::env::var_os("MANIFOLD_REPRO_PROJECT") else {
        eprintln!("warmup probe skipped: MANIFOLD_REPRO_PROJECT is unset");
        return;
    };
    log::set_logger(&LOGGER).expect("probe logger");
    log::set_max_level(log::LevelFilter::Info);
    assert!(manifold_gpu::gpu_fault::diagnostics_enabled(), "enable MANIFOLD_GPU_DIAGNOSTICS=1 to count actual RT dispatches");
    let path = PathBuf::from(project_path);
    eprintln!("loading reproduction project {}", path.display());

    let project = manifold_io::loader::load_project_with(&path, |embedded| {
        let mut effects = Vec::new();
        let mut generators = Vec::new();
        for preset in embedded {
            let Some(id) = preset.id() else { continue };
            let entry = (
                id.as_str().to_owned(),
                serde_json::to_string(&preset.def).expect("embedded preset JSON"),
                preset.origin,
            );
            match preset.kind {
                PresetKind::Effect => effects.push(entry),
                PresetKind::Generator => generators.push(entry),
            }
        }
        set_project_presets(effects, generators);
    })
    .expect("load reproduction project");
    let layer = project
        .timeline
        .layers
        .iter()
        .find(|layer| layer.layer_id.as_str() == "53b8a585")
        .expect("reproduction layer 53b8a585");
    assert_eq!(layer.generator_type().as_str(), "cc0_oomurasaki_azalea_r_x_pulchrum#2", "expected project azalea generator");
    assert_eq!(
        project.settings.rt_quality.realtime.shadows,
        manifold_core::settings::RtQualityTier::UltraLow
    );
    assert_eq!(
        project.settings.rt_quality.realtime.ray_resolution,
        manifold_core::settings::RtRayResolution::Quarter
    );

    let device = Arc::new(GpuDevice::new());
    let mut renderer = GeneratorRenderer::new(device, 1080, 1920, GpuTextureFormat::Rgba16Float, 0);
    renderer.set_rt_quality(&project.settings.rt_quality.realtime);
    let initial_faults = manifold_gpu::gpu_fault::fault_count();
    let outcome = renderer.prewarm_layer(
        layer,
        manifold_core::WarmupBudget {
            per_layer: std::time::Duration::from_secs(30),
            per_layer_frames: 48,
            total: std::time::Duration::from_secs(30),
        },
    );
    let fault_count = manifold_gpu::gpu_fault::fault_count();
    eprintln!("warmup layer={} outcome={outcome:?} fault_count={fault_count}", layer.layer_id);
    let traces = TRACES.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!("RT dispatches={traces}");
    assert!(traces > 0, "inconclusive: no RT dispatch occurred");
    assert_ne!(outcome, manifold_core::WarmupOutcome::GpuFailed);
    assert_eq!(fault_count, initial_faults, "GPU fault during RT warmup");
}
