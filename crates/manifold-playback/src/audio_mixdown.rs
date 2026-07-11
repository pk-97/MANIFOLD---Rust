//! Offline audio mixdown for export.
//!
//! Renders every audio-layer clip in the export range into a stereo master
//! (plus, per D3, a mono downmix and per-layer mono taps for the offline
//! audio-reactive analysis driver — see
//! `docs/OFFLINE_AUDIO_REACTIVE_EXPORT_DESIGN.md`). The stereo master is what
//! the export muxer muxes onto the encoded video. This is the export
//! counterpart to [`crate::audio_layer_playback::AudioLayerPlayback`] (live
//! playback): it must produce the SAME audio you hear, so the placement, warp,
//! gain, and solo/mute rules are mirrored from that module exactly.
//!
//! Per audio layer (design §5): a layer reaches the **master mix** only when
//! `master_hot` — not muted, not silenced by another layer's solo, and not
//! analysis-only. A layer's **tap** (its own mono, exposed via
//! `ExportAudio::per_layer_mono` for layers requested in `tapped_layers`) is
//! gated by `tap_hot` instead — the same mute/solo gate, but NOT cut by
//! `analysis_only`, mirroring `AudioLayerPlayback::update`
//! (`audio_layer_playback.rs:225-232`): an analysis-only layer stays silent to
//! the master while its tap stays hot, exactly like the live sub-track's
//! output-volume gate (`master_hot`) vs its per-voice tap gate (`tap_hot`).
//! Gain is the layer's linear gain, applied identically to both destinations.
//! Per-clip placement matches live playback: varispeed `warp_ratio`, the clip
//! `in_point`, and the decoder `encoder_delay` offset. Per-clip `is_muted` is
//! intentionally NOT applied — live audio playback doesn't apply it either
//! (`AudioLayerPlayback::update` gates on layer flags only), so applying it
//! here would diverge from what the performer hears.
//!
//! Source samples are linearly interpolated, which folds the warp (varispeed)
//! and the source→output sample-rate conversion into one resample — the same
//! pitch-moves-with-rate behaviour as kira's `set_playback_rate`.

use std::fs::File;
use std::io::{BufWriter, Write};

use ahash::AHashMap;
use manifold_core::id::LayerId;
use manifold_core::project::Project;
use manifold_core::tempo::{TempoMap, TempoMapConverter};
use manifold_core::units::Bpm;
use manifold_core::Beats;

use crate::audio_sync::preload_audio;

/// Export mix sample rate. 48 kHz is the standard render rate the layer taps
/// report and what FFmpeg muxes cleanly.
const OUT_SAMPLE_RATE: u32 = 48_000;

/// The render core behind export audio: the export-rendered mix plus the
/// analysis-consumer buffers derived from the SAME rendered frames (design
/// seam brief — "one render, two consumers, no drift between what is heard
/// and what is analyzed").
pub struct ExportAudio {
    /// [`OUT_SAMPLE_RATE`] (48kHz).
    pub sample_rate: u32,
    /// Stereo master, whole render including the pre-roll (design D3).
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// `(left[i] + right[i]) * 0.5` for every frame — the analysis consumer's
    /// downmix of the exact same rendered frames the master WAV is built from.
    pub master_mono: Vec<f32>,
    /// One mono buffer per requested (audio) layer in `tapped_layers`, gated
    /// by `tap_hot` (see module docs) rather than `master_hot` — present for
    /// every requested audio layer even when that layer never goes hot (all
    /// zeros then, mirroring the live tap always receiving silence rather
    /// than being omitted).
    pub per_layer_mono: AHashMap<LayerId, Vec<f32>>,
    /// Samples of pre-roll (design D3) prepended to every buffer above.
    /// Exactly [`OUT_SAMPLE_RATE`] (1 second) whenever the range is
    /// non-empty; 0 for an empty range.
    pub pre_roll_samples: usize,
    /// Whether any master-hot clip actually overlapped the ORIGINAL
    /// `[start_beat, end_beat)` window (not the pre-roll extension). Mirrors
    /// the old `render_export_mix`'s `Ok(false)` ("nothing to mux") signal.
    pub audible_in_range: bool,
}

/// Render the audio-layer mix for `[start_beat, end_beat)`, prefixed by a
/// 1-second pre-roll (design D3), returning the stereo master plus the
/// analysis-consumer mono buffers. See [`ExportAudio`] and the module docs for
/// the master/tap hotness rules.
///
/// `tapped_layers` are the (audio) layers whose own mono should also be
/// rendered (only meaningful contribution: layers referenced by a layer-fed
/// send in P2's offline driver). Each unique clip is decoded once and its
/// interpolated samples fan out to whichever of the master / per-layer
/// buffers are hot for it — one render pass, no second decode or resample
/// loop over the same clip.
pub fn render_export_audio(
    project: &Project,
    start_beat: Beats,
    end_beat: Beats,
    bpm: Bpm,
    tempo_map: &mut TempoMap,
    tapped_layers: &[LayerId],
) -> Result<ExportAudio, String> {
    let start_seconds = TempoMapConverter::beat_to_seconds(tempo_map, start_beat, bpm).0;
    let end_seconds = TempoMapConverter::beat_to_seconds(tempo_map, end_beat, bpm).0;
    let duration = (end_seconds - start_seconds).max(0.0);
    let out_sr = OUT_SAMPLE_RATE as f64;
    let main_frames = (duration * out_sr).round() as usize;
    if main_frames == 0 {
        return Ok(ExportAudio {
            sample_rate: OUT_SAMPLE_RATE,
            left: Vec::new(),
            right: Vec::new(),
            master_mono: Vec::new(),
            per_layer_mono: AHashMap::new(),
            pre_roll_samples: 0,
            audible_in_range: false,
        });
    }

    // D3: always render 1s of pre-roll ahead of the range, so analyzers
    // (P2) are settled by frame 0. Exactly OUT_SAMPLE_RATE samples — a clean
    // 1-second prefix, no rounding drift relative to the main-range math below.
    let pre_roll_samples = OUT_SAMPLE_RATE as usize;
    let total_frames = pre_roll_samples + main_frames;
    let render_start_seconds = start_seconds - pre_roll_samples as f64 / out_sr;

    // Audio layers have their own solo bus (design §5): any soloed audio layer
    // silences the others to master. Mirrors `AudioLayerPlayback::update`.
    let any_solo = project
        .timeline
        .layers
        .iter()
        .any(|l| l.is_audio() && l.is_solo);

    let project_bpm = project.settings.bpm.0;

    let mut left = vec![0.0f32; total_frames];
    let mut right = vec![0.0f32; total_frames];
    let mut audible_in_range = false;

    // Scratch stereo accumulators for tapped layers, downmixed to mono at the
    // end via the same `(l+r)*0.5` rule as `master_mono` — so a tapped
    // layer's buffer is bit-identical to rendering that layer alone through
    // the master path.
    let mut per_layer_lr: AHashMap<LayerId, (Vec<f32>, Vec<f32>)> = AHashMap::new();

    for layer in project.timeline.layers.iter().filter(|l| l.is_audio()) {
        // Tap/master gating mirrors `AudioLayerPlayback::update`
        // (audio_layer_playback.rs:225-232): `tap_hot` feeds the layer's
        // post-fader send tap (drives analysis) and is NOT cut by
        // `analysis_only` — only mute/solo gate it. `master_hot` additionally
        // requires `!analysis_only`, since analysis-only layers exist
        // precisely to feed analysis "silently" without reaching the master
        // mix.
        let tap_hot = !layer.is_muted && (!any_solo || layer.is_solo);
        let master_hot = tap_hot && !layer.analysis_only;
        let is_tapped = tapped_layers.contains(&layer.layer_id);

        if is_tapped {
            per_layer_lr
                .entry(layer.layer_id.clone())
                .or_insert_with(|| (vec![0.0f32; total_frames], vec![0.0f32; total_frames]));
        }

        // Nothing to contribute from this layer to either destination.
        if !(master_hot || (is_tapped && tap_hot)) {
            continue;
        }

        let gain = layer.audio_gain_linear();

        for clip in layer.clips.iter().filter(|c| c.is_audio()) {
            let clip_start_sec =
                TempoMapConverter::beat_to_seconds(tempo_map, clip.start_beat, bpm).0;
            let clip_end_sec =
                TempoMapConverter::beat_to_seconds(tempo_map, clip.end_beat(), bpm).0;
            // Skip clips fully outside the render window (export range + pre-roll).
            if clip_end_sec <= render_start_seconds || clip_start_sec >= end_seconds {
                continue;
            }

            let pre = match preload_audio(&clip.audio_file_path, Beats::ZERO) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!(
                        "[audio_mixdown] decode failed for '{}': {e} — clip skipped",
                        clip.audio_file_path
                    );
                    continue;
                }
            };
            let frames = &pre.sound_data.frames;
            if frames.is_empty() {
                continue;
            }
            let src_sr = pre.sound_data.sample_rate as f64;
            let file_dur = pre.clip_duration.0;
            let encoder_delay = pre.encoder_delay.0;
            let ratio = clip.warp_ratio(project_bpm) as f64;
            let in_point = clip.in_point.0;

            // `audible_in_range` tracks the ORIGINAL [start_seconds,
            // end_seconds) window only (not the pre-roll extension), so a
            // clip sitting entirely in the pre-roll second doesn't flip it —
            // matches the old `render_export_mix`'s `wrote_any` semantics
            // exactly (tied to master-hot layers, set once decode + frames
            // succeed, regardless of whether any sample actually lands).
            if master_hot && clip_end_sec > start_seconds && clip_start_sec < end_seconds {
                audible_in_range = true;
            }

            // Output-frame span covering this clip's window, relative to the
            // render start (pre-roll included).
            let i_start = (((clip_start_sec - render_start_seconds) * out_sr)
                .floor()
                .max(0.0)) as usize;
            let i_end = (((clip_end_sec - render_start_seconds) * out_sr).ceil() as i64)
                .clamp(0, total_frames as i64) as usize;

            for i in i_start..i_end {
                let now = render_start_seconds + i as f64 / out_sr;
                // Source position the playhead is over (matches live playback):
                // wall-clock since the clip start, scaled by the warp ratio,
                // offset into the file by the in-point + encoder priming silence.
                let src_pos = (now - clip_start_sec) * ratio + in_point + encoder_delay;
                if src_pos < 0.0 || src_pos >= file_dur {
                    continue;
                }
                let src_idx = src_pos * src_sr;
                let i0 = src_idx.floor() as usize;
                let frac = (src_idx - i0 as f64) as f32;
                let (l0, r0) = frame_at(frames, i0);
                let (l1, r1) = frame_at(frames, i0 + 1);
                let l_val = l0 + (l1 - l0) * frac;
                let r_val = r0 + (r1 - r0) * frac;
                if master_hot {
                    left[i] += l_val * gain;
                    right[i] += r_val * gain;
                }
                if is_tapped
                    && tap_hot
                    && let Some((pl, pr)) = per_layer_lr.get_mut(&layer.layer_id)
                {
                    pl[i] += l_val * gain;
                    pr[i] += r_val * gain;
                }
            }
        }
    }

    // master_mono is the analysis-consumer downmix of the same rendered
    // frames the master WAV is built from (design seam brief).
    let master_mono: Vec<f32> = left
        .iter()
        .zip(right.iter())
        .map(|(l, r)| (l + r) * 0.5)
        .collect();

    let per_layer_mono: AHashMap<LayerId, Vec<f32>> = per_layer_lr
        .into_iter()
        .map(|(id, (pl, pr))| {
            let mono: Vec<f32> = pl.iter().zip(pr.iter()).map(|(l, r)| (l + r) * 0.5).collect();
            (id, mono)
        })
        .collect();

    Ok(ExportAudio {
        sample_rate: OUT_SAMPLE_RATE,
        left,
        right,
        master_mono,
        per_layer_mono,
        pre_roll_samples,
        audible_in_range,
    })
}

/// Write `audio`'s master stereo (pre-roll stripped) to `out_wav_path`.
/// Returns `Ok(true)` if written, `Ok(false)` if `audio` has no post-pre-roll
/// frames or nothing was audible in range (caller should write nothing).
pub fn write_export_wav(audio: &ExportAudio, out_wav_path: &str) -> Result<bool, String> {
    if !audio.audible_in_range || audio.left.len() <= audio.pre_roll_samples {
        return Ok(false);
    }
    write_wav_i16(
        out_wav_path,
        audio.sample_rate,
        &audio.left[audio.pre_roll_samples..],
        &audio.right[audio.pre_roll_samples..],
    )?;
    Ok(true)
}

/// Render the audio-layer mix for `[start_beat, end_beat)` into a stereo WAV at
/// `out_wav_path`. Returns `Ok(true)` if any audio was written (the caller then
/// muxes the WAV), `Ok(false)` if there were no audio clips in range (export
/// proceeds video-only). Decode failures on individual clips are logged and that
/// clip is skipped — an honest partial silence, never a stand-in.
pub fn render_export_mix(
    project: &Project,
    start_beat: Beats,
    end_beat: Beats,
    bpm: Bpm,
    tempo_map: &mut TempoMap,
    out_wav_path: &str,
) -> Result<bool, String> {
    let audio = render_export_audio(project, start_beat, end_beat, bpm, tempo_map, &[])?;
    write_export_wav(&audio, out_wav_path)
}

/// Stereo sample at frame index `i` (silence past the end), as `(left, right)`.
#[inline]
fn frame_at(frames: &[kira::Frame], i: usize) -> (f32, f32) {
    match frames.get(i) {
        Some(f) => (f.left, f.right),
        None => (0.0, 0.0),
    }
}

/// Write interleaved 16-bit PCM stereo WAV. Minimal RIFF/WAVE writer (no extra
/// dependency); samples are clamped to [-1, 1] before quantization.
fn write_wav_i16(path: &str, sample_rate: u32, left: &[f32], right: &[f32]) -> Result<(), String> {
    let num_frames = left.len().min(right.len());
    let channels: u16 = 2;
    let bits: u16 = 16;
    let block_align: u16 = channels * bits / 8;
    let byte_rate: u32 = sample_rate * block_align as u32;
    let data_bytes: u32 = (num_frames as u32) * block_align as u32;
    let riff_size: u32 = 36 + data_bytes;

    let file = File::create(path).map_err(|e| format!("create WAV '{path}': {e}"))?;
    let mut w = BufWriter::new(file);

    let mut hdr = || -> std::io::Result<()> {
        w.write_all(b"RIFF")?;
        w.write_all(&riff_size.to_le_bytes())?;
        w.write_all(b"WAVE")?;
        // fmt chunk (PCM)
        w.write_all(b"fmt ")?;
        w.write_all(&16u32.to_le_bytes())?;
        w.write_all(&1u16.to_le_bytes())?; // PCM
        w.write_all(&channels.to_le_bytes())?;
        w.write_all(&sample_rate.to_le_bytes())?;
        w.write_all(&byte_rate.to_le_bytes())?;
        w.write_all(&block_align.to_le_bytes())?;
        w.write_all(&bits.to_le_bytes())?;
        // data chunk
        w.write_all(b"data")?;
        w.write_all(&data_bytes.to_le_bytes())?;
        for i in 0..num_frames {
            let l = (left[i].clamp(-1.0, 1.0) * 32767.0) as i16;
            let r = (right[i].clamp(-1.0, 1.0) * 32767.0) as i16;
            w.write_all(&l.to_le_bytes())?;
            w.write_all(&r.to_le_bytes())?;
        }
        Ok(())
    };
    hdr().map_err(|e| format!("write WAV '{path}': {e}"))?;
    w.flush().map_err(|e| format!("flush WAV '{path}': {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::clip::TimelineClip;
    use manifold_core::layer::Layer;
    use manifold_core::Seconds;
    use std::f32::consts::PI;

    /// FNV-1a 64-bit hash — inline, no dependency, good enough to catch any
    /// byte-level drift in the written WAV.
    fn fnv1a_hash(bytes: &[u8]) -> u64 {
        const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
        const PRIME: u64 = 0x100000001b3;
        let mut hash = OFFSET_BASIS;
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(PRIME);
        }
        hash
    }

    /// Writes a small stereo WAV fixture (440Hz tone + 3Hz slow component, so
    /// the resample path has real signal to interpolate) at `sample_rate` —
    /// deliberately != 48kHz so the mixdown's resample path actually runs.
    fn write_test_fixture_wav(path: &std::path::Path, sample_rate: u32, duration_secs: f32) {
        let n = (sample_rate as f32 * duration_secs) as usize;
        let mut left = vec![0.0f32; n];
        let mut right = vec![0.0f32; n];
        for i in 0..n {
            let t = i as f32 / sample_rate as f32;
            let s = 0.5 * (2.0 * PI * 440.0 * t).sin() + 0.2 * (2.0 * PI * 3.0 * t).sin();
            left[i] = s;
            right[i] = s * 0.9; // slightly different right channel so L/R aren't identical
        }
        write_wav_i16(path.to_str().unwrap(), sample_rate, &left, &right)
            .expect("write test fixture wav");
    }

    /// Builds the fixture project used by the pre/post-refactor byte-identity
    /// gate: three audio layers (normal gain, non-unity gain with a
    /// non-integer-beat clip start, and one muted layer that must not
    /// contribute), each with one clip backed by a 44.1kHz fixture file.
    /// Returns `(project, fixture_dir)` — the fixture dir must outlive the
    /// call to `render_export_mix`/`render_export_audio`.
    fn build_fixture_project() -> (Project, tempfile_dir::TestDir) {
        let dir = tempfile_dir::TestDir::new("manifold_audio_mixdown_test");
        let wav_path = dir.path().join("tone.wav");
        write_test_fixture_wav(&wav_path, 44_100, 2.0);
        let wav_path_str = wav_path.to_str().unwrap().to_string();

        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);

        let mut layer_normal = Layer::new_audio("Normal".to_string(), 0);
        layer_normal.clips.push(TimelineClip::new_audio(
            wav_path_str.clone(),
            Beats(0.0),
            Beats(20.0),
            Seconds(0.0),
            Seconds(2.0),
        ));

        let mut layer_gain = Layer::new_audio("Gain".to_string(), 1);
        layer_gain.audio_gain_db = 6.0; // non-unity gain
        layer_gain.clips.push(TimelineClip::new_audio(
            wav_path_str.clone(),
            Beats(0.5), // non-integer beat start
            Beats(19.5),
            Seconds(0.0),
            Seconds(2.0),
        ));

        let mut layer_muted = Layer::new_audio("Muted".to_string(), 2);
        layer_muted.is_muted = true;
        layer_muted.clips.push(TimelineClip::new_audio(
            wav_path_str.clone(),
            Beats(0.0),
            Beats(20.0),
            Seconds(0.0),
            Seconds(2.0),
        ));

        // Analysis-only: silent to master, but its tap must stay hot (mirrors
        // AudioLayerPlayback::update's tap_hot / master_hot split).
        let mut layer_analysis_only = Layer::new_audio("AnalysisOnly".to_string(), 3);
        layer_analysis_only.analysis_only = true;
        layer_analysis_only.clips.push(TimelineClip::new_audio(
            wav_path_str,
            Beats(0.0),
            Beats(20.0),
            Seconds(0.0),
            Seconds(2.0),
        ));

        project.timeline.layers.push(layer_normal);
        project.timeline.layers.push(layer_gain);
        project.timeline.layers.push(layer_muted);
        project.timeline.layers.push(layer_analysis_only);

        (project, dir)
    }

    /// Minimal self-cleaning temp-dir helper — no `tempfile` dependency.
    mod tempfile_dir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        pub struct TestDir(PathBuf);

        impl TestDir {
            pub fn new(prefix: &str) -> Self {
                // `pid + nanosecond timestamp` alone is NOT a unique key: this
                // process's wall clock does not actually resolve to
                // nanoseconds (measured ~96% collision rate over 200k calls
                // in a tight loop on this machine), and several tests in this
                // module call `build_fixture_project()` — sharing this same
                // prefix — from different threads at near-identical wall
                // time when `cargo test` fans them out in parallel. A
                // collision here means two tests' `TestDir`s resolve to the
                // SAME directory: they race writing/reading the same
                // `tone.wav`, and the first `TestDir` to drop deletes the
                // directory out from under the other, corrupting or nuking
                // the second test's fixture and producing intermittent exact
                // float-equality failures unrelated to real mixdown behavior
                // (BUG-106 / BUG-090 / BUG-074). Fix: add a per-process
                // atomic sequence number so two calls can never collide
                // regardless of clock resolution — same pattern already
                // used by `percussion_backend.rs::build_temp_config_path`.
                static SEQ: AtomicU64 = AtomicU64::new(0);
                let seq = SEQ.fetch_add(1, Ordering::Relaxed);
                let dir = std::env::temp_dir().join(format!(
                    "{prefix}_{}_{}_{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0),
                    seq
                ));
                std::fs::create_dir_all(&dir).expect("create test fixture dir");
                Self(dir)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TestDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn render_export_mix_byte_identity_fixture() {
        let (project, _dir) = build_fixture_project();
        let mut tempo_map = TempoMap::default();
        let out_dir = tempfile_dir::TestDir::new("manifold_audio_mixdown_out");
        let out_path = out_dir.path().join("mix.wav");

        let wrote = render_export_mix(
            &project,
            Beats(2.0),
            Beats(6.0),
            Bpm(120.0),
            &mut tempo_map,
            out_path.to_str().unwrap(),
        )
        .expect("render_export_mix should succeed");
        assert!(wrote, "expected audio in range to produce a WAV");

        let bytes = std::fs::read(&out_path).expect("read written wav");
        let hash = fnv1a_hash(&bytes);
        // Recorded from the unmodified pre-refactor code (P1 gate literal —
        // do NOT update this to make a refactor pass; a changed hash means
        // the refactor changed behavior).
        assert_eq!(
            hash, 0xaa873b48d8b143e1,
            "mixdown byte-identity gate: WAV bytes changed (hash was {hash:#x})"
        );
    }

    #[test]
    fn render_export_mix_empty_range_returns_ok_false() {
        let (project, _dir) = build_fixture_project();
        let mut tempo_map = TempoMap::default();
        let out_dir = tempfile_dir::TestDir::new("manifold_audio_mixdown_empty_out");
        let out_path = out_dir.path().join("empty.wav");

        let wrote = render_export_mix(
            &project,
            Beats(1000.0),
            Beats(1004.0),
            Bpm(120.0),
            &mut tempo_map,
            out_path.to_str().unwrap(),
        )
        .expect("render_export_mix should succeed on empty range");
        assert!(!wrote, "expected no audio in an out-of-range window");
    }

    /// Finds a layer's id in the fixture project by name (layers get a fresh
    /// random `LayerId` on construction, so tests look them up by name).
    fn layer_id_named(project: &Project, name: &str) -> LayerId {
        project
            .timeline
            .layers
            .iter()
            .find(|l| l.name == name)
            .unwrap_or_else(|| panic!("fixture layer '{name}' not found"))
            .layer_id
            .clone()
    }

    #[test]
    fn render_export_audio_pre_roll_is_exactly_one_second() {
        let (project, _dir) = build_fixture_project();
        let mut tempo_map = TempoMap::default();
        let audio =
            render_export_audio(&project, Beats(2.0), Beats(6.0), Bpm(120.0), &mut tempo_map, &[])
                .expect("render_export_audio should succeed");
        assert_eq!(audio.pre_roll_samples, 48_000);
        assert_eq!(audio.left.len(), audio.pre_roll_samples + 96_000); // 2s @ 48kHz main range
        assert_eq!(audio.right.len(), audio.left.len());
        assert_eq!(audio.master_mono.len(), audio.left.len());
    }

    #[test]
    fn render_export_audio_master_mono_is_lr_downmix() {
        let (project, _dir) = build_fixture_project();
        let mut tempo_map = TempoMap::default();
        let audio =
            render_export_audio(&project, Beats(2.0), Beats(6.0), Bpm(120.0), &mut tempo_map, &[])
                .expect("render_export_audio should succeed");
        for i in 0..audio.left.len() {
            let expected = (audio.left[i] + audio.right[i]) * 0.5;
            assert_eq!(audio.master_mono[i], expected, "mismatch at frame {i}");
        }
    }

    #[test]
    fn render_export_audio_tapped_layer_matches_rendering_alone() {
        let (project, _dir) = build_fixture_project();
        let normal_id = layer_id_named(&project, "Normal");

        let mut tempo_map = TempoMap::default();
        let audio = render_export_audio(
            &project,
            Beats(2.0),
            Beats(6.0),
            Bpm(120.0),
            &mut tempo_map,
            std::slice::from_ref(&normal_id),
        )
        .expect("render_export_audio should succeed");

        // Render a project containing ONLY the "Normal" layer; its master_mono
        // (gain 1.0, no other layers to contend with) must equal the tapped
        // per-layer mono from the full project.
        let mut solo_project = Project::default();
        solo_project.settings.bpm = project.settings.bpm;
        solo_project.timeline.layers.push(
            project
                .timeline
                .layers
                .iter()
                .find(|l| l.layer_id == normal_id)
                .unwrap()
                .clone(),
        );
        let mut solo_tempo_map = TempoMap::default();
        let solo_audio = render_export_audio(
            &solo_project,
            Beats(2.0),
            Beats(6.0),
            Bpm(120.0),
            &mut solo_tempo_map,
            &[],
        )
        .expect("solo render_export_audio should succeed");

        let tapped = audio
            .per_layer_mono
            .get(&normal_id)
            .expect("tapped layer entry present");
        assert_eq!(tapped.len(), solo_audio.master_mono.len());
        for (i, (&tapped_sample, &solo_sample)) in
            tapped.iter().zip(solo_audio.master_mono.iter()).enumerate()
        {
            assert_eq!(
                tapped_sample, solo_sample,
                "tapped layer mono diverges from solo render at frame {i}"
            );
        }
    }

    #[test]
    fn render_export_audio_analysis_only_layer_taps_but_never_hits_master() {
        let (project, _dir) = build_fixture_project();
        let analysis_id = layer_id_named(&project, "AnalysisOnly");

        let mut tempo_map = TempoMap::default();
        let audio = render_export_audio(
            &project,
            Beats(2.0),
            Beats(6.0),
            Bpm(120.0),
            &mut tempo_map,
            std::slice::from_ref(&analysis_id),
        )
        .expect("render_export_audio should succeed");

        let tapped = audio
            .per_layer_mono
            .get(&analysis_id)
            .expect("analysis-only layer must still appear in per_layer_mono");
        let has_signal = tapped.iter().any(|&s| s.abs() > 1e-6);
        assert!(
            has_signal,
            "analysis-only layer's tap must carry real signal (tap_hot, not cut by analysis_only)"
        );

        // The same project rendered WITHOUT the analysis-only layer's
        // contribution should be identical to the full project's master —
        // i.e. the analysis-only layer never reaches left/right.
        let mut without_analysis_only = Project::default();
        without_analysis_only.settings.bpm = project.settings.bpm;
        for layer in &project.timeline.layers {
            if layer.name != "AnalysisOnly" {
                without_analysis_only.timeline.layers.push(layer.clone());
            }
        }
        let mut baseline_tempo_map = TempoMap::default();
        let baseline = render_export_audio(
            &without_analysis_only,
            Beats(2.0),
            Beats(6.0),
            Bpm(120.0),
            &mut baseline_tempo_map,
            &[],
        )
        .expect("baseline render_export_audio should succeed");

        assert_eq!(audio.left, baseline.left, "analysis-only layer altered master left");
        assert_eq!(audio.right, baseline.right, "analysis-only layer altered master right");
    }

    #[test]
    fn render_export_audio_pre_roll_only_clip_is_not_audible_in_range() {
        // A project with a single clip that sits ENTIRELY inside the pre-roll
        // second (before the export range starts) must report
        // `audible_in_range == false` — matching the old wrapper's Ok(false).
        let dir = tempfile_dir::TestDir::new("manifold_audio_mixdown_preroll_only");
        let wav_path = dir.path().join("tone.wav");
        write_test_fixture_wav(&wav_path, 44_100, 2.0);

        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        let mut layer = Layer::new_audio("PreRollOnly".to_string(), 0);
        // At 120bpm, 1 beat == 0.5s. The export range is beats [4, 8) ==
        // seconds [2.0, 4.0); the pre-roll therefore covers [1.0, 2.0). Place
        // a clip at beats [2.0, 3.5) == seconds [1.0, 1.75): fully inside the
        // pre-roll second, ending before the range even starts.
        layer.clips.push(TimelineClip::new_audio(
            wav_path.to_str().unwrap().to_string(),
            Beats(2.0),
            Beats(1.5),
            Seconds(0.0),
            Seconds(2.0),
        ));
        project.timeline.layers.push(layer);

        let mut tempo_map = TempoMap::default();
        let audio =
            render_export_audio(&project, Beats(4.0), Beats(8.0), Bpm(120.0), &mut tempo_map, &[])
                .expect("render_export_audio should succeed");
        assert!(
            !audio.audible_in_range,
            "clip lying entirely in the pre-roll second must not set audible_in_range"
        );
    }
}
