//! WATER_SIMULATION_DESIGN.md "Transport pause / water speed zero": live water
//! with preview time debt renders bit-identical frames while the transport is
//! held. Water Basin meshes on the CPU (the Add Fluid shape: the fluid node
//! drives its own mesh); the GPU dam break meshes through the Liquid Surface
//! group, so the sort, blob, volume and marching-cubes chain is covered too.

use manifold_core::params::ParamManifest;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::readback_tonemapped_rgba8;
use manifold_renderer::node_graph::{PrimitiveRegistry, physics::PhysicsStepScope};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

use crate::harness;

const PRESETS: [(&str, &str); 2] = [
    ("WaterBasin", include_str!("../../assets/generator-presets/WaterBasin.json")),
    ("WaterDamBreakGpu", include_str!("../../assets/generator-presets/WaterDamBreakGpu.json")),
];
const W: u32 = 320;
const H: u32 = 180;
/// Unpaced frames: far faster than the solver, so preview builds time debt.
const PLAY_FRAMES: i64 = 120;
/// Long enough for the worker to finish several batches of that debt.
const PAUSE: std::time::Duration = std::time::Duration::from_secs(2);

fn context(frame: i64, time: f64) -> PresetContext {
    PresetContext {
        time,
        beat: time * 2.0,
        dt: 1.0 / 60.0,
        width: W,
        height: H,
        output_width: W,
        output_height: H,
        aspect: W as f32 / H as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

#[test]
fn fluid_paused_transport_renders_identical_frames() {
    let harness = harness::shared();
    let device = &harness.device;
    let target = RenderTarget::new(device, W, H, GpuTextureFormat::Rgba16Float, "fluid-pause");
    let params = ParamManifest::default();
    for (name, preset) in PRESETS {
        let mut runtime = PresetRuntime::from_json_str_with_device(
            preset,
            &PrimitiveRegistry::with_builtin(),
            std::sync::Arc::clone(device),
            W,
            H,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .unwrap_or_else(|error| panic!("{name} builds: {error:?}"));
        let mut render = |frame: i64, time: f64| {
            let _live = PhysicsStepScope::for_render(false);
            let mut encoder = device.create_encoder("fluid pause");
            {
                let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
                runtime.render(&mut gpu, &target.texture, &context(frame, time), &params);
            }
            encoder.commit_and_wait_profiled(device);
            readback_tonemapped_rgba8(device, &target.texture, W, H)
        };
        let mut frame = 0;
        let mut first_play = None;
        let mut played = None;
        while frame < PLAY_FRAMES {
            frame += 1;
            let pixels = render(frame, frame as f64 / 60.0);
            first_play.get_or_insert_with(|| pixels.clone());
            played = Some(pixels);
        }
        assert!(played != first_play, "{name}: the water must move while playing");
        let held_time = PLAY_FRAMES as f64 / 60.0;
        frame += 1;
        let paused = render(frame, held_time);
        let started = std::time::Instant::now();
        while started.elapsed() < PAUSE {
            frame += 1;
            assert!(render(frame, held_time) == paused, "{name}: paused water moved at frame {frame}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
