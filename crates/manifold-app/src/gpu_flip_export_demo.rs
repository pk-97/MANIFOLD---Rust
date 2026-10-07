//! Liquid seam P7a demo (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P7a (GPU FLIP on
//! the contract)): the shipped GPU FLIP Dam Break through the production export
//! at 60 and 30 fps. The frame at 5 s is the same pixels at both rates, and a
//! paused live transport keeps showing the same frame. Writes both videos,
//! the two 5 s frames, their difference and a paused frame to
//! `GPU_FLIP_DEMO_DIR` (`/tmp/gpu_flip_dam_break_l2` when unset).
#![cfg(all(test, feature = "journey-proofs", target_os = "macos"))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::unbounded;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::{Beats, Bpm, PresetTypeId};
use manifold_media::export_config::ExportConfig;
use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_renderer::headless_readback::{encode_rgba8_png, linear_to_srgb8, readback_raw_halves};

use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;

const PRESET: &str = "WaterDamBreakGpuFlip";
const BPM: f64 = 120.0;
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
/// The compared instant, in seconds.
const AT: u32 = 5;

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("GPU_FLIP_DEMO_DIR").map_or_else(|| PathBuf::from("/tmp/gpu_flip_dam_break_l2"), PathBuf::from);
    std::fs::create_dir_all(&dir).expect("demo directory");
    dir
}

fn project() -> Project {
    project_of(PRESET)
}

fn project_of(preset: &'static str) -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(BPM as f32);
    project.settings.output_width = WIDTH as i32;
    project.settings.output_height = HEIGHT as i32;
    let mut layer = Layer::new_generator("GPU FLIP Dam Break".into(), PresetTypeId::new(preset), 0);
    // A minute long, so the live run never reaches its end.
    layer.clips.push(TimelineClip::new_generator(Beats::ZERO, Beats(128.0)));
    project.timeline.layers.push(layer);
    project
}

fn halves(bytes: &[u8]) -> impl Iterator<Item = f32> + '_ {
    bytes.chunks_exact(2).map(|h| half::f16::from_bits(u16::from_le_bytes([h[0], h[1]])).to_f32())
}

/// Linear SDR halves as an sRGB PNG.
fn write_png(path: &Path, rgba16f: &[u8]) {
    let rgba: Vec<u8> = halves(rgba16f)
        .enumerate()
        .map(|(i, v)| if i % 4 == 3 { 255 } else { linear_to_srgb8(v) })
        .collect();
    std::fs::write(path, encode_rgba8_png(&rgba, WIDTH, HEIGHT)).expect("png written");
}

/// Export `0..=AT` s at `fps` through `ContentThread::run_export` and return
/// the linear SDR output of the frame at `AT` s, as encoded.
fn export(fps: u32, dir: &Path) -> Vec<u8> {
    let frames = AT * fps + 1;
    let path = dir.join(format!("dam_break_{fps}fps.mp4"));
    let seconds = f64::from(frames) / f64::from(fps);
    let cfg = ExportConfig {
        output_path: path.to_string_lossy().into_owned(),
        width: WIDTH,
        height: HEIGHT,
        fps: fps as f32,
        hdr: false,
        start_beat: 0.0,
        end_beat: seconds * BPM / 60.0,
        audio_path: None,
        audio_start_beat: 0.0,
        audio_encoder_delay: 0.0,
        split_at_markers: false,
    };
    let mut content = headless_content_thread(project(), WIDTH, HEIGHT);
    let (cmd_tx, cmd_rx) = unbounded();
    let (state_tx, state_rx) = unbounded();
    crate::scene_modifier_journey::warm_project(&mut content, &state_tx);
    let (observation_tx, observation_rx) = unbounded();
    let _observer = crate::content_export::install_export_observer(observation_tx, None);
    crate::content_export::capture_export_sdr_frame(frames - 1);
    let started = Instant::now();
    content.run_export(cfg, &cmd_rx, &state_tx);
    let took = started.elapsed();
    drop(cmd_tx);
    drop(state_tx);
    let finished = state_rx.try_iter().find_map(|state| state.export_finished).expect("the export finished");
    assert!(finished.success, "{fps} fps export failed: {}", finished.message);
    let observed: Vec<_> = observation_rx.try_iter().collect();
    assert_eq!(observed.len(), frames as usize, "{fps} fps export frame count");
    for frame in &observed {
        assert_eq!(frame.status, FrameRenderStatus::Complete, "{fps} fps frame {} status", frame.frame_idx);
    }
    let last = observed.last().expect("frames");
    assert!((last.time_seconds - f64::from(AT)).abs() < 1e-9, "{fps} fps last frame at {} s", last.time_seconds);
    println!(
        "GPU FLIP demo: {fps} fps export, {frames} frames in {:.1} s → {}",
        took.as_secs_f64(),
        path.display()
    );
    last.sdr_mapped_rgba16f.clone().expect("the frame at 5 s was read back")
}

#[test]
fn gpu_flip_dam_break_export_matches_across_frame_rates() {
    let dir = out_dir();
    let at_60 = export(60, &dir);
    let at_30 = export(30, &dir);
    assert_eq!(at_60.len(), at_30.len());
    write_png(&dir.join("dam_break_60fps_frame300.png"), &at_60);
    write_png(&dir.join("dam_break_30fps_frame150.png"), &at_30);
    let (mut differing, mut largest) = (0usize, 0.0f32);
    let mut diff = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for (a, b) in at_60.chunks_exact(8).zip(at_30.chunks_exact(8)) {
        let delta: Vec<f32> = halves(a).zip(halves(b)).map(|(x, y)| (x - y).abs()).collect();
        let worst = delta.iter().copied().fold(0.0, f32::max);
        if a != b {
            differing += 1;
            largest = largest.max(worst);
        }
        // Any difference shows: sixteen times brighter than it is, at least grey.
        let shade = if a == b { 0 } else { ((worst * 16.0).clamp(0.0, 1.0) * 255.0).max(64.0) as u8 };
        diff.extend_from_slice(&[shade, shade, shade, 255]);
    }
    std::fs::write(dir.join("dam_break_diff.png"), encode_rgba8_png(&diff, WIDTH, HEIGHT)).expect("diff written");
    println!(
        "GPU FLIP demo: frame 300 at 60 fps against frame 150 at 30 fps: {differing} of {} pixels differ, largest {largest:.3e}",
        WIDTH * HEIGHT
    );
    assert_eq!(differing, 0, "the frame at {AT} s differs between 60 and 30 fps");
}

/// The Dam Break as a project for the content-thread trace gate:
/// `MANIFOLD_RENDER_TRACE=1 cargo xtask perf-soak <dir>/gpu_flip_dam_break.manifold --seconds 60`
/// plays it with its whitewater, and no content frame may pass 20 ms.
#[test]
fn gpu_flip_dam_break_soak_project() {
    let path = out_dir().join("gpu_flip_dam_break.manifold");
    manifold_io::saver::save_project_v1(&project(), &path).expect("save the soak project");
    println!("GPU FLIP soak project → {}", path.display());
}

/// The project warmup builds every pipeline a liquid preset dispatches, so
/// its first played frames compile nothing. The warm frame runs zero liquid
/// ticks, so the tick region's nodes are first reached on stage
/// (BUG-jtod (GPU FLIP first-play stall)).
#[test]
fn liquid_presets_play_without_pipeline_compiles() {
    use manifold_core::cold_touch::{ColdTouchKind, cold_touch_count};
    let mut failures = Vec::new();
    for preset in [PRESET, "WaterDamBreakMatter", "WaterFloatingBoxMatter", "WaterStillPoolMatter"] {
        let mut content = headless_content_thread(project_of(preset), WIDTH, HEIGHT);
        let (state_tx, _state_rx) = unbounded();
        crate::scene_modifier_journey::warm_project(&mut content, &state_tx);
        let before = cold_touch_count(ColdTouchKind::PipelineCompile);
        content.handle_command(ContentCommand::Play);
        for _ in 0..120 {
            content.tick_frame(&state_tx);
        }
        content.content_pipeline.wait_for_render_complete();
        let compiles = cold_touch_count(ColdTouchKind::PipelineCompile) - before;
        if compiles > 0 {
            failures.push(format!("{preset}: {compiles}"));
        }
    }
    assert!(failures.is_empty(), "pipelines compiled while a warmed preset played: {failures:?}");
}

#[test]
fn gpu_flip_dam_break_paused_live_frames_hold() {
    let dir = out_dir();
    let mut content = headless_content_thread(project(), WIDTH, HEIGHT);
    let (state_tx, _state_rx) = unbounded();
    crate::scene_modifier_journey::warm_project(&mut content, &state_tx);
    let device = std::sync::Arc::clone(content.content_pipeline.native_gpu_for_tests().expect("native device"));
    let grab = |content: &crate::content_thread::ContentThread| {
        content.content_pipeline.wait_for_render_complete();
        readback_raw_halves(&device, content.content_pipeline.export_output_texture(), WIDTH, HEIGHT)
    };
    let differing = |a: &[u8], b: &[u8]| a.chunks_exact(8).zip(b.chunks_exact(8)).filter(|(x, y)| x != y).count();
    content.handle_command(ContentCommand::Play);
    let mut playing = Vec::new();
    for frame in 0..90 {
        content.tick_frame(&state_tx);
        if frame == 59 {
            playing = grab(&content);
            println!("GPU FLIP demo: playing frame 60 at {:.2} s", content.engine.current_time().0);
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    let later = grab(&content);
    println!("GPU FLIP demo: playing frame 90 at {:.2} s", content.engine.current_time().0);
    write_png(&dir.join("dam_break_playing_60.png"), &playing);
    write_png(&dir.join("dam_break_playing_90.png"), &later);
    let moved = differing(&playing, &later);
    println!("GPU FLIP demo: 30 playing frames changed {moved} of {} pixels", WIDTH * HEIGHT);
    assert!(moved > 0, "the liquid did not move while playing");
    content.handle_command(ContentCommand::Pause);
    content.tick_frame(&state_tx);
    let held = grab(&content);
    write_png(&dir.join("dam_break_paused.png"), &held);
    for frame in 0..3 {
        std::thread::sleep(Duration::from_millis(16));
        content.tick_frame(&state_tx);
        let changed = differing(&held, &grab(&content));
        println!("GPU FLIP demo: paused frame {} changed {changed} of {} pixels", frame + 1, WIDTH * HEIGHT);
        assert_eq!(changed, 0, "paused frame {} changed the picture", frame + 1);
    }
}

/// Export `frames` frames at 60 fps of the Dam Break at simulation resolution
/// 32 into a small output, `runs` times on one content thread, and return
/// the last frame's linear SDR output of each run.
fn small_exports(runs: usize, frames: u32, dir: &Path, tag: &str) -> Vec<Vec<u8>> {
    const W: u32 = 256;
    const H: u32 = 144;
    let mut project = project();
    project.settings.output_width = W as i32;
    project.settings.output_height = H as i32;
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let mut content = headless_content_thread(project, W, H);
    let (state_tx, state_rx) = unbounded();
    crate::scene_modifier_journey::set_generator_param(&mut content, &layer_id, "resolution", 32.0);
    crate::scene_modifier_journey::warm_project(&mut content, &state_tx);
    (0..runs)
        .map(|run| {
            let (cmd_tx, cmd_rx) = unbounded();
            let cfg = ExportConfig {
                output_path: dir.join(format!("determinism_{tag}_{run}.mp4")).to_string_lossy().into_owned(),
                width: W,
                height: H,
                fps: 60.0,
                hdr: false,
                start_beat: 0.0,
                end_beat: f64::from(frames) / 60.0 * BPM / 60.0,
                audio_path: None,
                audio_start_beat: 0.0,
                audio_encoder_delay: 0.0,
                split_at_markers: false,
            };
            let (observation_tx, observation_rx) = unbounded();
            let _observer = crate::content_export::install_export_observer(observation_tx, None);
            crate::content_export::capture_export_sdr_frame(frames - 1);
            content.run_export(cfg, &cmd_rx, &state_tx);
            drop(cmd_tx);
            let finished = state_rx.try_iter().find_map(|state| state.export_finished).expect("the export finished");
            assert!(finished.success, "{tag} run {run} failed: {}", finished.message);
            let observed: Vec<_> = observation_rx.try_iter().collect();
            assert_eq!(observed.len(), frames as usize, "{tag} run {run} frame count");
            for frame in &observed {
                assert_eq!(frame.status, FrameRenderStatus::Complete, "{tag} run {run} frame {} status", frame.frame_idx);
            }
            observed.last().and_then(|frame| frame.sdr_mapped_rgba16f.clone()).expect("the last frame was read back")
        })
        .collect()
}

/// Two exports of one project are the same pixels: two content threads, and a
/// second export on the first thread (export start reseeds the water, so an
/// earlier export or warm-up frames never leak into the next).
#[test]
fn gpu_flip_export_is_deterministic() {
    let dir = out_dir();
    let frames = 48;
    let first = small_exports(2, frames, &dir, "a");
    let other = small_exports(1, frames, &dir, "b");
    let reference = &first[0];
    assert!(reference.chunks_exact(8).any(|p| p != reference[..8].as_ref()), "the frame is uniform; nothing rendered");
    for (name, frame) in [("re-export on the same thread", &first[1]), ("export on a second thread", &other[0])] {
        let differing = reference.chunks_exact(8).zip(frame.chunks_exact(8)).filter(|(a, b)| a != b).count();
        println!("GPU FLIP determinism: {name}: {differing} pixels differ at frame {frames}");
        assert_eq!(differing, 0, "{name} differs from the first export");
    }
}
