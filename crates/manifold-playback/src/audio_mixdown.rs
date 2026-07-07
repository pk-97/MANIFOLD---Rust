//! Offline audio mixdown for export.
//!
//! Renders every audio-layer clip in the export range into a single stereo WAV,
//! which the export muxer then muxes onto the encoded video. This is the export
//! counterpart to [`crate::audio_layer_playback::AudioLayerPlayback`] (live
//! playback): it must produce the SAME audio you hear, so the placement, warp,
//! gain, and solo/mute rules are mirrored from that module exactly.
//!
//! Per audio layer (design §5): a layer reaches the mix only when `master_hot`
//! — not muted, not silenced by another layer's solo, and not analysis-only.
//! Gain is the layer's linear gain. Per-clip placement matches live playback:
//! varispeed `warp_ratio`, the clip `in_point`, and the decoder `encoder_delay`
//! offset. Per-clip `is_muted` is intentionally NOT applied — live audio playback
//! doesn't apply it either (`AudioLayerPlayback::update` gates on layer flags
//! only), so applying it here would diverge from what the performer hears.
//!
//! Source samples are linearly interpolated, which folds the warp (varispeed)
//! and the source→output sample-rate conversion into one resample — the same
//! pitch-moves-with-rate behaviour as kira's `set_playback_rate`.

use std::fs::File;
use std::io::{BufWriter, Write};

use manifold_core::project::Project;
use manifold_core::tempo::{TempoMap, TempoMapConverter};
use manifold_core::units::Bpm;
use manifold_core::Beats;

use crate::audio_sync::preload_audio;

/// Export mix sample rate. 48 kHz is the standard render rate the layer taps
/// report and what FFmpeg muxes cleanly.
const OUT_SAMPLE_RATE: u32 = 48_000;

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
    let start_seconds = TempoMapConverter::beat_to_seconds(tempo_map, start_beat, bpm).0;
    let end_seconds = TempoMapConverter::beat_to_seconds(tempo_map, end_beat, bpm).0;
    let duration = (end_seconds - start_seconds).max(0.0);
    let out_sr = OUT_SAMPLE_RATE as f64;
    let num_frames = (duration * out_sr).round() as usize;
    if num_frames == 0 {
        return Ok(false);
    }

    // Audio layers have their own solo bus (design §5): any soloed audio layer
    // silences the others to master. Mirrors `AudioLayerPlayback::update`.
    let any_solo = project
        .timeline
        .layers
        .iter()
        .any(|l| l.is_audio() && l.is_solo);

    let project_bpm = project.settings.bpm.0;

    let mut left = vec![0.0f32; num_frames];
    let mut right = vec![0.0f32; num_frames];
    let mut wrote_any = false;

    for layer in project.timeline.layers.iter().filter(|l| l.is_audio()) {
        // `master_hot`: the layer reaches the speakers (and thus the export).
        let master_hot = !layer.is_muted && (!any_solo || layer.is_solo) && !layer.analysis_only;
        if !master_hot {
            continue;
        }
        let gain = layer.audio_gain_linear();

        for clip in layer.clips.iter().filter(|c| c.is_audio()) {
            let clip_start_sec =
                TempoMapConverter::beat_to_seconds(tempo_map, clip.start_beat, bpm).0;
            let clip_end_sec =
                TempoMapConverter::beat_to_seconds(tempo_map, clip.end_beat(), bpm).0;
            // Skip clips fully outside the export window.
            if clip_end_sec <= start_seconds || clip_start_sec >= end_seconds {
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

            // Output-frame span covering this clip's timeline window.
            let i_start =
                (((clip_start_sec - start_seconds) * out_sr).floor().max(0.0)) as usize;
            let i_end = (((clip_end_sec - start_seconds) * out_sr).ceil() as i64)
                .clamp(0, num_frames as i64) as usize;

            for i in i_start..i_end {
                let now = start_seconds + i as f64 / out_sr;
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
                left[i] += (l0 + (l1 - l0) * frac) * gain;
                right[i] += (r0 + (r1 - r0) * frac) * gain;
            }
            wrote_any = true;
        }
    }

    if !wrote_any {
        return Ok(false);
    }

    write_wav_i16(out_wav_path, OUT_SAMPLE_RATE, &left, &right)?;
    Ok(true)
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
            wav_path_str,
            Beats(0.0),
            Beats(20.0),
            Seconds(0.0),
            Seconds(2.0),
        ));

        project.timeline.layers.push(layer_normal);
        project.timeline.layers.push(layer_gain);
        project.timeline.layers.push(layer_muted);

        (project, dir)
    }

    /// Minimal self-cleaning temp-dir helper — no `tempfile` dependency.
    mod tempfile_dir {
        use std::path::{Path, PathBuf};

        pub struct TestDir(PathBuf);

        impl TestDir {
            pub fn new(prefix: &str) -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "{prefix}_{}_{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
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
}
