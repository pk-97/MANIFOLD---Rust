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
