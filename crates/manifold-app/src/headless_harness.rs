//! Shared headless [`ContentThread`] construction — the same real
//! `PlaybackEngine` + renderer set + native Metal `ContentPipeline` wiring
//! `Application::resumed()` builds (`app.rs` ~2093-2278), minus every
//! UI-surface concern (no window, no IOSurface bridges, no MIDI device open,
//! no LED controller, no OSC listening). Originally lived inside
//! `journey_proof.rs` (test-only); extracted here (PERF_BUDGET_GATE_DESIGN.md
//! P1) so the headless `cargo xtask perf-soak` binary path can drive a real
//! `ContentThread` outside `#[cfg(test)]` — `journey_proof.rs` and the
//! BUG-035/037 regression guards now import [`headless_content_thread`] from
//! here instead of defining it locally. No behavior change: same struct
//! literal, same fields, same inert defaults.

use std::sync::Arc;

use manifold_core::project::Project;
use manifold_playback::engine::PlaybackEngine;

use crate::content_pipeline::ContentPipeline;
use crate::content_thread::ContentThread;
use manifold_core::Seconds;

/// Build a minimal but real headless [`ContentThread`]. See the module doc
/// for what's real vs. inert-defaulted.
pub(crate) fn headless_content_thread(project: Project, w: u32, h: u32) -> ContentThread {
    let native_device = Arc::new(manifold_gpu::GpuDevice::new());
    let gen_format = manifold_gpu::GpuTextureFormat::Rgba16Float;

    let renderers: Vec<Box<dyn manifold_playback::renderer::ClipRenderer>> = vec![
        Box::new(manifold_media::video_renderer::VideoRenderer::new(
            Arc::clone(&native_device),
            w,
            h,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            8,
        )),
        Box::new(manifold_media::image_renderer::ImageRenderer::new(
            Arc::clone(&native_device),
            w,
            h,
        )),
        Box::new(manifold_renderer::generator_renderer::GeneratorRenderer::new(
            Arc::clone(&native_device),
            w,
            h,
            gen_format,
            8,
        )),
    ];
    let mut engine = PlaybackEngine::new(renderers);
    engine.initialize(project);
    engine.set_live_clip_manager(manifold_playback::live_clip_manager::LiveClipManager::new());

    let mut content_pipeline = ContentPipeline::new(Box::new(
        manifold_renderer::layer_compositor::LayerCompositor::new(&native_device, w, h),
    ));
    content_pipeline.set_native_gpu(native_device);

    // Deliberately NOT `.start()`-ed (that call opens real MIDI devices) —
    // export/soak never reads MIDI input; the field just needs a value.
    let midi_input = manifold_playback::midi_input::MidiInputController::new();

    ContentThread {
        engine,
        editing_service: manifold_editing::service::EditingService::new(),
        content_pipeline,
        audio_layer_playback: None,
        percussion_orchestrator: manifold_playback::percussion_orchestrator::PercussionImportOrchestrator::new(
            None,
            String::new(),
        ),
        transport_controller: manifold_playback::transport_controller::TransportController::new(),
        gpu: manifold_renderer::gpu::GpuContext::new(),
        frame_count: 0,
        time_since_start: Seconds::ZERO,
        last_data_version: 0,
        midi_input,
        clip_launcher: manifold_playback::clip_launcher::ClipLauncher::new(),
        rendering_paused: false,
        timer: crate::frame_timer::FrameTimer::new(60.0),
        sync_arbiter: manifold_playback::sync::SyncArbiter::new(),
        osc_receiver: manifold_playback::osc_receiver::OscReceiver::new(),
        osc_sync: manifold_playback::osc_sync::OscSyncController::new(),
        osc_sender: manifold_playback::osc_sender::OscPositionSender::new(),
        osc_param_router: manifold_playback::osc_param_router::OscParamRouter::new(),
        ableton_bridge: manifold_playback::ableton_bridge::AbletonBridge::new(),
        ableton_active_last_frame: false,
        tempo_recorder: manifold_playback::tempo_recorder::TempoRecorder::new(),
        link_beat_offset: f64::NAN,
        led_controller: None,
        still_export: None,
        cached_midi_device_names: Vec::new(),
        last_midi_device_scan_time: Seconds(-10.0),
        cached_project_snapshot: None,
        watched_graph_target: None,
        preview_graph_node: None,
        node_preview_normalize: false,
        cached_graph_snapshot: None,
        mod_scratch: crate::content_state::ModulationSnapshot::empty(),
        audio_mod_runtime: crate::audio_mod_runtime::AudioModRuntime::default(),
        cached_midi_clock_position: Arc::from(""),
        cached_midi_clock_device: Arc::from(""),
        cached_perc_message: Arc::from(""),
        last_sent_midi_device_names: Arc::from([]),
        embedded_presets_fingerprint: 0,
        pending_undo_redo_event: None,
        #[cfg(feature = "profiling")]
        profiler: None,
    }
}
