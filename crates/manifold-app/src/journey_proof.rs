//! Headless release-journey export harness (feature `journey-proofs`,
//! macOS-only). P3 of `docs/OFFLINE_AUDIO_REACTIVE_EXPORT_DESIGN.md`, extended
//! per the release-journey queue item: a standing, feature-gated end-to-end
//! proof that offline audio-reactive export actually moves pixels, not just a
//! green unit test (`prove-render-path-before-claiming-visual-win`).
//!
//! ## What this drives
//!
//! [`run_headless_export`] builds a minimal but REAL [`ContentThread`] (same
//! `PlaybackEngine` + renderers + native Metal `ContentPipeline` construction
//! as `Application::resumed()` in `app.rs` ~2093-2278, minus every UI-only
//! concern: no window, no IOSurface preview/atlas bridges, no MIDI device
//! open (`midi_input.start()` never called), no LED output, no OSC listening)
//! and calls the real, unmodified [`ContentThread::run_export`]
//! (`content_export.rs:25`) — the exact path a live export takes. This
//! harness never re-implements the frame loop or re-encodes; it only
//! constructs the thread and its channels, then hands control to the
//! production export path.
//!
//! ## External dependency
//!
//! Frame extraction and audio-stream verification shell out to `ffmpeg` /
//! `ffprobe` (resolved the same way `AudioMuxer::resolve_ffmpeg` does, falling
//! back to `/opt/homebrew/bin/ffmpeg` / `/opt/homebrew/bin/ffprobe`, present
//! on this machine). Like `gpu-proofs`, this feature is deliberate-run only —
//! minutes-long (GPU render + video encode + ffmpeg frame extraction), never
//! part of the default sweep.
//!
//! ## Artifacts
//!
//! Every proof writes its export + extracted frames under
//! `target/journey-proofs/<test-name>/` (stable, not cleaned up — the
//! orchestrator reads the PNGs directly per the acceptance-demo L2 bar).
//!
//! Every item below exists solely to serve the `#[cfg(test)]` proofs at the
//! bottom of this file — there is no non-test caller by design (this harness
//! is deliberate-run only, like `gpu-proofs`). `#![cfg(test)]` on the whole
//! module (rather than a per-item `#[allow(dead_code)]`) makes that
//! structural: under a plain `cargo build`/`cargo clippy` (no `--tests`) the
//! module compiles empty — invisible to the default sweep and to clippy —
//! and materializes only when compiled as tests (`cargo test`/`cargo clippy
//! --tests`, both with `--features journey-proofs`).
#![cfg(test)]

use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};

use manifold_core::clip::TimelineClip;
use manifold_core::effects::{ParamId, ParameterDriver};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{AudioBand, AudioFeature, AudioFeatureKind, AudioSend, ParameterAudioMod};
use manifold_core::{BeatDivision, Beats, Bpm, DriverWaveform, PresetTypeId, Seconds};
use manifold_media::export_config::ExportConfig;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
// Headless `ContentThread` construction now lives in `headless_harness.rs`
// (PERF_BUDGET_GATE_DESIGN.md P1) — shared with the non-test `perf-soak`
// binary path, which can't reach a `#[cfg(test)]` item.
use crate::headless_harness::headless_content_thread;

/// Drive one real export through the production path: build a headless
/// content thread, call the real `ContentThread::run_export` (never a
/// reimplementation), and return the finished output path.
///
/// `run_export` sends periodic `ContentState` progress on `state_tx`
/// (`content_export.rs:510-512`, every 10 frames) and the app's real channels
/// are `crossbeam_channel::unbounded` (`app.rs:2016-2017` — NOT the bounded-4
/// channel this phase's brief warned to check for; verified by reading the
/// construction site directly, per the mechanism-question oracle rule).
/// `send()` on an unbounded channel never blocks, so no separate draining
/// thread is required for correctness — but a background drain keeps the
/// harness from silently growing an unread channel across a multi-hundred-
/// frame export, and is how we observe the `ExportFinishedEvent` outside
/// `run_export`'s own borrow of `state_tx`.
fn run_headless_export(project: Project, cfg: ExportConfig) -> Result<PathBuf, String> {
    let mut ct = headless_content_thread(project, cfg.width, cfg.height);

    let (cmd_tx, cmd_rx): (Sender<ContentCommand>, Receiver<ContentCommand>) =
        crossbeam_channel::unbounded();
    let (state_tx, state_rx) = crossbeam_channel::unbounded::<ContentState>();

    let drain = std::thread::Builder::new()
        .name("journey-proof-drain".into())
        .spawn(move || {
            let mut finished = None;
            // BUG-083 verification: log every progress snapshot the real
            // export path sends, so a run of this proof
            // (`--features journey-proofs -- --nocapture`) is a direct,
            // observable oracle that `is_exporting`/`export_progress`/
            // `export_status` climb during a real export — not just that the
            // fields are non-zero somewhere, but that the exact snapshots
            // the UI's header consumer reads actually progress.
            while let Ok(state) = state_rx.recv() {
                if state.is_exporting {
                    println!(
                        "[journey-proof] export progress: {:.1}% — {}",
                        state.export_progress * 100.0,
                        state.export_status
                    );
                }
                if let Some(ev) = state.export_finished {
                    finished = Some(ev);
                    break;
                }
            }
            finished
        })
        .expect("spawn drain thread");

    ct.run_export(cfg, &cmd_rx, &state_tx);

    // Keep cmd_tx alive across the call above (run_export only ever reads
    // it); drop explicitly afterward so the drain thread's `recv()` can't
    // hang if `run_export` somehow returned without sending a finished event.
    drop(cmd_tx);
    drop(state_tx);

    let finished = drain.join().map_err(|_| "journey-proof drain thread panicked".to_string())?;
    match finished {
        Some(ev) if ev.success => Ok(PathBuf::from(ev.output_path)),
        Some(ev) => Err(format!("export failed: {}", ev.message)),
        None => Err("export produced no ExportFinishedEvent".to_string()),
    }
}

/// A tiny export config: 320x180 SDR at `fps`, full content range (resolved
/// from the project's clip extents — `content_export.rs`'s
/// `content_range_beats()` fallback), no HDR, no explicit range override.
fn tiny_export_config(output_path: &Path, fps: f32) -> ExportConfig {
    ExportConfig {
        output_path: output_path.to_string_lossy().into_owned(),
        width: 320,
        height: 180,
        fps,
        hdr: false,
        start_beat: 0.0,
        end_beat: 0.0,
        audio_path: None,
        audio_start_beat: 0.0,
        audio_encoder_delay: 0.0,
    }
}

// ─── ffmpeg / ffprobe helpers ───────────────────────────────────────────────

fn resolve_ffprobe() -> Option<String> {
    if let Ok(p) = std::env::var("FFPROBE_PATH")
        && !p.is_empty()
        && Path::new(&p).exists()
    {
        return Some(p);
    }
    for candidate in ["/opt/homebrew/bin/ffprobe", "/usr/local/bin/ffprobe", "/usr/bin/ffprobe"] {
        if Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Extract every frame of `video_path` as PNGs into `out_dir` (created if
/// needed), sorted by name. Never re-encodes or re-renders — a plain `ffmpeg`
/// subprocess call reading the file `run_headless_export` already produced.
fn extract_frames_to_pngs(video_path: &Path, out_dir: &Path) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let ffmpeg = manifold_media::audio_muxer::AudioMuxer::resolve_ffmpeg("")
        .ok_or_else(|| "ffmpeg not found (see journey_proof.rs module header)".to_string())?;
    let pattern = out_dir.join("frame_%04d.png");
    let status = std::process::Command::new(&ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(video_path)
        .arg(&pattern)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;
    if !status.success() {
        return Err(format!("ffmpeg frame-extract exited with {status}"));
    }
    let mut frames: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .map_err(|e| format!("read_dir {}: {e}", out_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("png"))
        .collect();
    frames.sort();
    if frames.is_empty() {
        return Err(format!("ffmpeg produced no frames in {}", out_dir.display()));
    }
    Ok(frames)
}

/// Mean Rec.709-ish luma (`image`'s `to_luma8`) of one PNG frame, normalized
/// to `[0, 1]`.
fn frame_mean_luma(png_path: &Path) -> Result<f32, String> {
    let img = image::open(png_path).map_err(|e| format!("decode {}: {e}", png_path.display()))?;
    let gray = img.to_luma8();
    let (sum, count) = gray.pixels().fold((0u64, 0u64), |(s, c), p| (s + p.0[0] as u64, c + 1));
    if count == 0 {
        return Err(format!("{} decoded to zero pixels", png_path.display()));
    }
    Ok((sum as f32 / count as f32) / 255.0)
}

/// Per-frame mean luma across every extracted frame, in order.
fn luma_series(frames: &[PathBuf]) -> Result<Vec<f32>, String> {
    frames.iter().map(|p| frame_mean_luma(p)).collect()
}

/// Whether `path` (a muxed export) carries an audio stream at all — closes
/// the standing "mixdown unverified on a real export" debt
/// (`project_audio_layer_export_mixdown` memory / `audio_mixdown.rs` doc).
fn ffprobe_has_audio_stream(path: &Path) -> Result<bool, String> {
    let ffprobe = resolve_ffprobe().ok_or_else(|| "ffprobe not found".to_string())?;
    let output = std::process::Command::new(&ffprobe)
        .args(["-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .map_err(|e| format!("spawn ffprobe: {e}"))?;
    Ok(!output.stdout.is_empty())
}

// ─── Fixture builders ───────────────────────────────────────────────────────

const CLICK_BPM: f32 = 120.0;
const CLICK_BEATS: usize = 8;
const CLICK_FPS: f32 = 24.0;
/// Generator param this whole harness drives: StarField's `brightness`
/// (`assets/generator-presets/StarField.json`, id `StarField`) is the single
/// scalar the §2.5-equivalent audit found that both (a) has a wide range
/// (0..4, default 1.5) and (b) is wired as the LAST op in its graph
/// (`... => scale_offset brightness`, NODE_CATALOG.md §6.1) — a flat
/// multiplicative gain on the whole rendered frame, so driving it end-to-end
/// changes mean frame luma directly and unambiguously. No fused/bundled
/// primitive involved; this harness only sets an existing param's audio-mod
/// binding, same as any performer would from the inspector.
const DRIVEN_PARAM: &str = "brightness";

pub(crate) fn star_field_generator_layer(index: i32) -> Layer {
    let mut layer = Layer::new("Stars".to_string(), LayerType::Generator, index);
    let pid = PresetTypeId::from_string("StarField".to_string());
    layer.change_generator_type(pid);
    layer.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(CLICK_BEATS as f64)));
    layer
}

/// Write a deterministic 120 BPM, 8-beat (4s), 48kHz mono click-track WAV: a
/// sharp 10ms decaying-tone burst on every beat, silence between. Fixed
/// content (no RNG, no wall clock) — `export_is_deterministic_in_features`
/// depends on the SAME bytes feeding both runs.
fn write_click_track_wav(path: &Path) {
    const SAMPLE_RATE: u32 = 48_000;
    let beat_secs = 60.0 / CLICK_BPM;
    let total_secs = CLICK_BEATS as f32 * beat_secs;
    const CLICK_SECS: f32 = 0.010;
    const CLICK_TONE_HZ: f32 = 1000.0;

    let total_samples = (SAMPLE_RATE as f32 * total_secs) as usize;
    let click_samples = (SAMPLE_RATE as f32 * CLICK_SECS) as usize;
    let mut samples = vec![0.0f32; total_samples];

    for beat in 0..CLICK_BEATS {
        let onset = (beat as f32 * beat_secs * SAMPLE_RATE as f32) as usize;
        for i in 0..click_samples {
            let idx = onset + i;
            if idx >= samples.len() {
                break;
            }
            // Linear decay envelope over the burst so it reads as a "click"
            // (a full-scale square gate would sound like a pop and stress
            // the analyzer differently than a live percussive transient).
            let env = 1.0 - (i as f32 / click_samples as f32);
            let t = i as f32 / SAMPLE_RATE as f32;
            samples[idx] = 0.9 * env * (2.0 * std::f32::consts::PI * CLICK_TONE_HZ * t).sin();
        }
    }

    write_wav_i16_mono(path, SAMPLE_RATE, &samples);
}

/// Minimal RIFF/WAVE mono 16-bit PCM writer — the same shape as
/// `audio_mixdown.rs`'s private `write_wav_i16` (stereo), mono here since the
/// click track has no stereo content to speak of.
fn write_wav_i16_mono(path: &Path, sample_rate: u32, samples: &[f32]) {
    use std::io::Write;
    let channels: u16 = 1;
    let bits: u16 = 16;
    let block_align: u16 = channels * bits / 8;
    let byte_rate: u32 = sample_rate * block_align as u32;
    let data_bytes: u32 = (samples.len() as u32) * block_align as u32;
    let riff_size: u32 = 36 + data_bytes;

    let file = std::fs::File::create(path)
        .unwrap_or_else(|e| panic!("create click-track wav {}: {e}", path.display()));
    let mut w = std::io::BufWriter::new(file);
    w.write_all(b"RIFF").unwrap();
    w.write_all(&riff_size.to_le_bytes()).unwrap();
    w.write_all(b"WAVE").unwrap();
    w.write_all(b"fmt ").unwrap();
    w.write_all(&16u32.to_le_bytes()).unwrap();
    w.write_all(&1u16.to_le_bytes()).unwrap();
    w.write_all(&channels.to_le_bytes()).unwrap();
    w.write_all(&sample_rate.to_le_bytes()).unwrap();
    w.write_all(&byte_rate.to_le_bytes()).unwrap();
    w.write_all(&block_align.to_le_bytes()).unwrap();
    w.write_all(&bits.to_le_bytes()).unwrap();
    w.write_all(b"data").unwrap();
    w.write_all(&data_bytes.to_le_bytes()).unwrap();
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        w.write_all(&v.to_le_bytes()).unwrap();
    }
    w.flush().unwrap();
}

/// One audio layer (the click track), one layer-fed send, one generator layer
/// whose `brightness` param carries an ENABLED audio-mod binding to that
/// send's Full-band amplitude. `click_wav_path` must already exist on disk.
fn audio_reactive_project(click_wav_path: &str) -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(CLICK_BPM);

    let mut audio_layer = Layer::new_audio("Click".to_string(), 0);
    let audio_layer_id = audio_layer.layer_id.clone();
    audio_layer.clips.push(TimelineClip::new_audio(
        click_wav_path.to_string(),
        Beats(0.0),
        Beats(CLICK_BEATS as f64),
        Seconds(0.0),
        Seconds((CLICK_BEATS as f32 * (60.0 / CLICK_BPM)) as f64),
    ));

    // Layer-fed send (D2: NOT capture-fed — `channels` stays empty) so
    // `AudioSend::has_capture()` is false and `is_layer_fed()` is true.
    let mut send = AudioSend::new("Click Send");
    send.source.layers.push(audio_layer_id);
    let send_id = send.id.clone();

    let mut gen_layer = star_field_generator_layer(1);
    let mut audio_mod = ParameterAudioMod::new(
        ParamId::from(DRIVEN_PARAM),
        send_id,
        AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
    );
    audio_mod.enabled = true;
    gen_layer.gen_params_mut().expect("generator layer must carry gen_params").audio_mods_mut().push(audio_mod);

    project.timeline.layers.push(audio_layer);
    project.timeline.layers.push(gen_layer);
    project.audio_setup.sends.push(send);

    project
}

/// Same generator layer and driven param as [`audio_reactive_project`], but
/// with NO audio at all — `brightness` is driven by a free-running LFO
/// instead. Proves the "camera-LFO-in-export" seam (any continuous driver
/// modulates through export, not just audio-mod) on the same cheap param.
fn lfo_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(CLICK_BPM);

    let mut gen_layer = star_field_generator_layer(0);
    // Half-note period = 2 beats = 1s at 120 BPM -> 4 full cycles across the
    // 4s/8-beat export, so the mean-crossing assertion has real margin.
    let driver = ParameterDriver::new(DRIVEN_PARAM, BeatDivision::Half, DriverWaveform::Sine);
    gen_layer.gen_params_mut().expect("generator layer must carry gen_params").drivers = Some(vec![driver]);

    project.timeline.layers.push(gen_layer);
    project
}

// ─── Proofs ─────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "journey-proofs"))]
mod tests {
    use super::*;

    fn out_dir(name: &str) -> PathBuf {
        let dir = PathBuf::from("target/journey-proofs").join(name);
        std::fs::create_dir_all(&dir).expect("create journey-proofs out dir");
        dir
    }

    /// Click-frame vs gap-frame window indices within one beat, at
    /// [`CLICK_FPS`]/[`CLICK_BPM`] (2 beats/sec -> `frames_per_beat` frames
    /// per beat). Click window = the first third of the beat (attack +
    /// early release); gap window = the last third (fully settled — default
    /// `AudioModShape::release_ms` is 120ms, well under a 500ms beat).
    fn click_and_gap_frames(total_frames: usize) -> (Vec<usize>, Vec<usize>) {
        let frames_per_beat = (CLICK_FPS / (CLICK_BPM / 60.0)).round() as usize; // 24/2 = 12
        let third = frames_per_beat / 3; // 4
        let mut click = Vec::new();
        let mut gap = Vec::new();
        for beat in 0..CLICK_BEATS {
            let base = beat * frames_per_beat;
            for i in 0..third {
                if base + i < total_frames {
                    click.push(base + i);
                }
            }
            for i in (frames_per_beat - third)..frames_per_beat {
                if base + i < total_frames {
                    gap.push(base + i);
                }
            }
        }
        (click, gap)
    }

    fn mean_at(series: &[f32], indices: &[usize]) -> f32 {
        let sum: f32 = indices.iter().map(|&i| series[i]).sum();
        sum / indices.len() as f32
    }

    /// P3's render-path proof (per `prove-render-path-before-claiming-visual-
    /// win`): export a fixture project with a param bound to a band envelope
    /// over a click-track audio layer, extract frames, assert the bound
    /// region visibly changes across beat boundaries and settles between
    /// them — a moving export, not a green unit test.
    #[test]
    fn audio_reactive_export_moves() {
        let dir = out_dir("audio_reactive_export_moves");
        let click_wav = dir.join("click.wav");
        write_click_track_wav(&click_wav);
        let project = audio_reactive_project(click_wav.to_str().unwrap());
        assert_eq!(
            project.analysis_consumed_sends().len(),
            1,
            "the fixture's audio-mod binding must make its send analysis-consumed"
        );

        let video_path = dir.join("export.mp4");
        let cfg = tiny_export_config(&video_path, CLICK_FPS);
        let produced = run_headless_export(project, cfg).expect("export should succeed");
        assert_eq!(produced, video_path);

        assert!(
            ffprobe_has_audio_stream(&video_path).expect("ffprobe"),
            "exported video must carry an audio stream — closes the standing \
             'mixdown unverified on a real export' debt \
             (project_audio_layer_export_mixdown memory / audio_mixdown.rs docs)"
        );

        let frames = extract_frames_to_pngs(&video_path, &dir.join("frames")).expect("frame extract");
        let luma = luma_series(&frames).expect("luma series");
        println!("[journey-proof] audio_reactive_export_moves: {} frames, luma={:?}", luma.len(), luma);

        let (click_idx, gap_idx) = click_and_gap_frames(luma.len());
        let click_mean = mean_at(&luma, &click_idx);
        let gap_mean = mean_at(&luma, &gap_idx);
        println!("[journey-proof] click_mean={click_mean} gap_mean={gap_mean}");

        // Ratio, not absolute delta: StarField is sparse bright points on a
        // black field, so mean-frame luma is tiny in absolute terms (observed
        // ~1e-4..2e-3) even though `brightness` swings the full 0..4 param
        // range. A relative threshold is invariant to that baseline. Observed
        // click/gap ratio on this fixture is ~6-7x; 2x is comfortably below
        // that (tolerant of encoder noise) while still failing hard if the
        // param is frozen (ratio == 1).
        assert!(
            click_mean > gap_mean * 2.0,
            "click frames must read markedly brighter than gap frames: \
             click_mean={click_mean} gap_mean={gap_mean} (ratio={:.2})",
            click_mean / gap_mean
        );
        let (min, max) = luma.iter().fold((f32::MAX, f32::MIN), |(mn, mx), &v| (mn.min(v), mx.max(v)));
        assert!(
            max > min * 2.0,
            "luma series must not be constant: min={min} max={max} (ratio={:.2})",
            max / min
        );
    }

    /// Round-trip gate (DESIGN_DOC_STANDARD §5, from BUG-036): SAVE the
    /// audio-reactive fixture through the real project-io save path, RELOAD
    /// it through the real load path, export the RELOADED project. Bindings
    /// must modulate AFTER reload, not just after creation — the create-path
    /// is only half a gate for stateful features.
    #[test]
    fn audio_reactive_survives_save_reload() {
        let dir = out_dir("audio_reactive_survives_save_reload");
        let click_wav = dir.join("click.wav");
        write_click_track_wav(&click_wav);
        let project = audio_reactive_project(click_wav.to_str().unwrap());

        let manifold_path = dir.join("project.manifold");
        manifold_io::saver::save_project_v1(&project, &manifold_path).expect("save_project_v1");
        let reloaded = manifold_io::loader::load_project(&manifold_path).expect("load_project");

        assert_eq!(
            reloaded.analysis_consumed_sends().len(),
            1,
            "the audio-mod binding must survive the real save/load round trip"
        );

        let video_path = dir.join("export.mp4");
        let cfg = tiny_export_config(&video_path, CLICK_FPS);
        run_headless_export(reloaded, cfg).expect("export of the RELOADED project should succeed");

        let frames = extract_frames_to_pngs(&video_path, &dir.join("frames")).expect("frame extract");
        let luma = luma_series(&frames).expect("luma series");
        println!(
            "[journey-proof] audio_reactive_survives_save_reload: {} frames, luma={:?}",
            luma.len(),
            luma
        );

        let (click_idx, gap_idx) = click_and_gap_frames(luma.len());
        let click_mean = mean_at(&luma, &click_idx);
        let gap_mean = mean_at(&luma, &gap_idx);
        println!("[journey-proof] click_mean={click_mean} gap_mean={gap_mean}");
        assert!(
            click_mean > gap_mean * 2.0,
            "post-reload: click frames must still read markedly brighter than gap frames \
             (modulation must survive reload, not just creation): \
             click_mean={click_mean} gap_mean={gap_mean} (ratio={:.2})",
            click_mean / gap_mean
        );
    }

    /// The "camera-LFO-in-export" seam, proven on the same cheap param: no
    /// audio at all, a free-running LFO must still visibly oscillate the
    /// exported frames.
    #[test]
    fn lfo_modulation_exports() {
        let dir = out_dir("lfo_modulation_exports");
        let project = lfo_project();

        let video_path = dir.join("export.mp4");
        let cfg = tiny_export_config(&video_path, CLICK_FPS);
        run_headless_export(project, cfg).expect("export should succeed");

        let frames = extract_frames_to_pngs(&video_path, &dir.join("frames")).expect("frame extract");
        let luma = luma_series(&frames).expect("luma series");
        println!("[journey-proof] lfo_modulation_exports: {} frames, luma={:?}", luma.len(), luma);

        let mean: f32 = luma.iter().sum::<f32>() / luma.len() as f32;
        let (min, max) = luma.iter().fold((f32::MAX, f32::MIN), |(mn, mx), &v| (mn.min(v), mx.max(v)));
        println!("[journey-proof] mean={mean} min={min} max={max}");
        // Ratio, not absolute delta — see audio_reactive_export_moves for why
        // (StarField's mean luma is tiny in absolute terms regardless of
        // `brightness`'s param-range swing).
        assert!(max > min * 2.0, "LFO-driven export must swing: min={min} max={max} (ratio={:.2})", max / min);

        let crossings = luma.windows(2).filter(|w| (w[0] - mean) * (w[1] - mean) < 0.0).count();
        println!("[journey-proof] mean crossings={crossings}");
        assert!(
            crossings >= 4,
            "a 4-cycle sine LFO over the export must cross its own mean several times, got {crossings}"
        );
    }

    /// D4 (design doc): same project + range + fps -> the same feature
    /// sequence, at the artifact level. Runs the audio-reactive export
    /// twice from the same click-track WAV and compares per-frame luma.
    #[test]
    fn export_is_deterministic_in_features() {
        let dir = out_dir("export_is_deterministic_in_features");
        let click_wav = dir.join("click.wav");
        write_click_track_wav(&click_wav);
        let wav_path = click_wav.to_str().unwrap();

        let video_a = dir.join("export_a.mp4");
        run_headless_export(audio_reactive_project(wav_path), tiny_export_config(&video_a, CLICK_FPS))
            .expect("export a should succeed");
        let frames_a = extract_frames_to_pngs(&video_a, &dir.join("frames_a")).expect("frames a");
        let luma_a = luma_series(&frames_a).expect("luma a");

        let video_b = dir.join("export_b.mp4");
        run_headless_export(audio_reactive_project(wav_path), tiny_export_config(&video_b, CLICK_FPS))
            .expect("export b should succeed");
        let frames_b = extract_frames_to_pngs(&video_b, &dir.join("frames_b")).expect("frames b");
        let luma_b = luma_series(&frames_b).expect("luma b");

        assert_eq!(luma_a.len(), luma_b.len(), "both runs must produce the same frame count");
        let max_diff = luma_a
            .iter()
            .zip(luma_b.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        println!("[journey-proof] export_is_deterministic_in_features: max_diff={max_diff}");
        // Tolerant of encoder-quantization jitter (H.264 CRF is deterministic
        // given identical input frames, but this guards against any residual
        // GPU float nondeterminism) while still failing hard on real drift.
        assert!(max_diff < 0.01, "two runs of the same export diverged: max per-frame luma diff {max_diff}");
    }
}
