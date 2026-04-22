//! Offline reference-track analysis for the MS plot overlay.
//!
//! One-shot path: decode an audio file (WAV / MP3 / FLAC / AAC / M4A) via
//! symphonia, run it through the same BH-windowed 16 384-point FFT the
//! real-time plugin uses, collect per-bin dB samples over the whole track,
//! then reduce to low/high percentile envelopes at a fixed log-spaced
//! frequency grid. Integrated LUFS is computed via the same BS.1770 meter
//! so the GUI can gain-match the ref to the live mix.
//!
//! Nothing here runs on the audio thread; the analysis is kicked off from
//! the GUI thread on file-pick and typically runs on a worker thread.
//! Storage is downsampled (~1 K points per side) so persisted plugin
//! state stays small enough for DAW projects.
//!
//! LAME tag parsing lives here too — used to learn a per-file lowpass
//! cutoff (e.g. 16 kHz for 128 kbps MP3) so the band doesn't misleadingly
//! taper off at the codec's brickwall.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Analyzer, LoudnessMeter, LoudnessSnapshot, MIN_DB};

/// Number of log-spaced points stored per envelope. Chosen to cover the
/// pixel density of a typical analyzer window (~1 K wide) without eating
/// plugin-state space (~8 KB per envelope, ~32 KB per slot).
pub const REF_POINTS: usize = 1024;

/// Low edge of the log-frequency grid the envelope is sampled on.
pub const REF_FREQ_MIN: f32 = 10.0;

/// High edge of the grid. Covers up to 96 kHz sample-rate Nyquist; for
/// lower-rate sources we just clip above their Nyquist at draw time.
pub const REF_FREQ_MAX: f32 = 48_000.0;

/// FFT size used for the offline analysis. Matches the real-time plugin
/// so the bin width and window shape are identical — makes the band
/// directly comparable to the live curve.
pub const REF_FFT_SIZE: usize = 16_384;

/// Overlap used for offline analysis. 50 % keeps memory bounded for long
/// tracks (e.g. a 4 min song is ~2 K frames at 48 k) while still giving
/// plenty of samples per bin for a stable percentile.
pub const REF_OVERLAP_RATIO: f32 = 0.5;

/// EWMA averaging time constant for the offline `Analyzer`. Matches the
/// plugin's 200 ms value so the offline per-frame dB values are drawn
/// from the same distribution as what the live curve ever shows.
pub const REF_AVG_MS: f32 = 200.0;

/// Percentile bounds for the band. 10 / 90 is the iZotope / Ozone norm —
/// the band represents "the typical 80 % of spectral content" without
/// reacting to silence or rare transients at either extreme.
pub const REF_PERCENTILE_LOW: f32 = 0.10;
pub const REF_PERCENTILE_HIGH: f32 = 0.90;

/// Low/high dB percentile per log-spaced frequency slot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefEnvelope {
    /// Pairs of `[low_db, high_db]` at each of `REF_POINTS` log-spaced
    /// frequencies spanning `[REF_FREQ_MIN, REF_FREQ_MAX]`.
    pub bounds: Vec<[f32; 2]>,
}

impl RefEnvelope {
    pub fn empty() -> Self {
        Self {
            bounds: vec![[MIN_DB, MIN_DB]; REF_POINTS],
        }
    }
}

/// Complete analysis result for a single reference track.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefAnalysis {
    pub mid: RefEnvelope,
    pub side: RefEnvelope,
    /// BS.1770 integrated loudness of the whole file. Used to shift the
    /// band vertically so the comparison is loudness-matched with the
    /// live mix.
    pub integrated_lufs: f32,
    /// Loudness Range (LRA) in LU over the whole file.
    pub lra_lu: f32,
    /// Monotonic max of BS.1770 short-term (3 s) loudness across the
    /// file. Used to derive DR = ST max − Integrated for the ref.
    pub short_term_max_lufs: f32,
    /// Monotonic max of 4× oversampled true peak across the file.
    pub true_peak_max_dbtp: f32,
    /// Monotonic max of raw sample peak across the file.
    pub sample_peak_max_db: f32,
    /// Monotonic max of the 300 ms-smoothed RMS across the file.
    pub rms_max_db: f32,
    /// MP3 LAME-tag lowpass in Hz, if detected. Display should fade the
    /// band to transparent above this frequency so codec brickwall
    /// artefacts don't read as "ref has no high end".
    pub lowpass_hz: Option<f32>,
    pub source_sample_rate: f32,
    pub duration_secs: f32,
}

impl RefAnalysis {
    /// DR derived the same way the live meter does: short-term max
    /// minus integrated. Returns 0.0 if either input is below the
    /// "no signal" sentinel.
    pub fn dr_lu(&self) -> f32 {
        if self.short_term_max_lufs > MIN_DB + 1.0 && self.integrated_lufs > MIN_DB + 1.0 {
            self.short_term_max_lufs - self.integrated_lufs
        } else {
            0.0
        }
    }

    /// PLR derived like the live meter: true-peak max minus integrated.
    pub fn plr_lu(&self) -> f32 {
        if self.true_peak_max_dbtp > MIN_DB + 1.0 && self.integrated_lufs > MIN_DB + 1.0 {
            self.true_peak_max_dbtp - self.integrated_lufs
        } else {
            0.0
        }
    }
}

/// What went wrong when analysing a file.
#[derive(Debug)]
pub enum RefError {
    Io(std::io::Error),
    Decode(String),
    NoAudio,
    TooShort,
}

impl std::fmt::Display for RefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefError::Io(e) => write!(f, "I/O error: {e}"),
            RefError::Decode(s) => write!(f, "decode failed: {s}"),
            RefError::NoAudio => write!(f, "file has no audio track"),
            RefError::TooShort => write!(f, "file is too short to analyse (< 0.5 s)"),
        }
    }
}

impl std::error::Error for RefError {}

/// Run the full offline analysis — decode, FFT stats, LUFS, LAME cutoff.
/// Blocks the calling thread; intended to run on a worker.
pub fn analyze_ref_file(path: &Path) -> Result<RefAnalysis, RefError> {
    let lowpass_hz = read_lame_lowpass(path);
    let (samples_l, samples_r, source_sr) = decode_file(path)?;
    let duration_secs = samples_l.len() as f32 / source_sr.max(1.0);

    if samples_l.len() < (source_sr * 0.5) as usize {
        return Err(RefError::TooShort);
    }

    let (mid_env, side_env) = run_fft_stats(&samples_l, &samples_r, source_sr);
    let loudness = run_loudness_aggregates(&samples_l, &samples_r, source_sr);

    Ok(RefAnalysis {
        mid: mid_env,
        side: side_env,
        integrated_lufs: loudness.integrated_lufs,
        lra_lu: loudness.lra_lu,
        short_term_max_lufs: loudness.short_term_max_lufs,
        true_peak_max_dbtp: loudness.true_peak_max_dbtp,
        sample_peak_max_db: loudness.sample_peak_max_db,
        rms_max_db: loudness.rms_max_db,
        lowpass_hz,
        source_sample_rate: source_sr,
        duration_secs,
    })
}

fn run_fft_stats(l: &[f32], r: &[f32], sr: f32) -> (RefEnvelope, RefEnvelope) {
    let mut mid_a = Analyzer::new(sr, REF_FFT_SIZE);
    mid_a.set_overlap_ratio(REF_OVERLAP_RATIO);
    mid_a.set_averaging_ms(REF_AVG_MS);
    let mut side_a = Analyzer::new(sr, REF_FFT_SIZE);
    side_a.set_overlap_ratio(REF_OVERLAP_RATIO);
    side_a.set_averaging_ms(REF_AVG_MS);

    // Match the live `Analyzer`'s positive-half bin count (includes Nyquist).
    let num_bins = REF_FFT_SIZE / 2 + 1;
    let mut mid_samples: Vec<Vec<f32>> = (0..num_bins).map(|_| Vec::new()).collect();
    let mut side_samples: Vec<Vec<f32>> = (0..num_bins).map(|_| Vec::new()).collect();

    let chunk = 4096;
    let mut mid_buf = vec![0.0f32; chunk];
    let mut side_buf = vec![0.0f32; chunk];
    let mut i = 0;
    while i < l.len() {
        let end = (i + chunk).min(l.len());
        let n = end - i;
        for k in 0..n {
            let lk = l[i + k];
            let rk = r[i + k];
            mid_buf[k] = 0.5 * (lk + rk);
            side_buf[k] = 0.5 * (lk - rk);
        }
        mid_a.process_mono(&mid_buf[..n], |avg| {
            for (bin, v) in avg.iter().enumerate() {
                mid_samples[bin].push(*v);
            }
        });
        side_a.process_mono(&side_buf[..n], |avg| {
            for (bin, v) in avg.iter().enumerate() {
                side_samples[bin].push(*v);
            }
        });
        i = end;
    }

    let mid_env = collapse_to_log_grid(&mut mid_samples, sr);
    let side_env = collapse_to_log_grid(&mut side_samples, sr);
    (mid_env, side_env)
}

/// Reduce per-FFT-bin sample vectors to a `REF_POINTS` log-spaced grid.
///
/// Each FFT bin is reduced once to its [low, high] percentile pair, then
/// every log-spaced display point linearly interpolates between the two
/// FFT bins straddling its frequency. This eliminates the visible
/// "staircase" at low frequency where one 2.7 Hz bin spans many log
/// slots — adjacent slots now ramp smoothly between the percentiles of
/// the FFT bins on either side, instead of all snapping to the same
/// nearest bin's value. At high frequencies where each log slot covers
/// many bins, the fractional part is small and interpolation degenerates
/// gracefully to "use the closer bin's percentile."
///
/// Percentile-of-mixed-distributions ≠ mix-of-percentiles, but for visual
/// envelope display the linear blend is the right trade-off: it removes
/// the bin-aligned discontinuities that read as "broken" without
/// introducing a smoothing kernel that would distort actual content.
fn collapse_to_log_grid(samples: &mut [Vec<f32>], sr: f32) -> RefEnvelope {
    let bin_hz = sr / REF_FFT_SIZE as f32;
    let log_min = REF_FREQ_MIN.ln();
    let log_max = REF_FREQ_MAX.ln();
    let denom = (REF_POINTS - 1) as f32;

    // Pass 1: percentile cache per bin. `None` = bin was empty / above
    // source Nyquist. Storage is freed as we go since long tracks stash
    // ~70 MB in `samples` at 3 min.
    let mut bin_percentiles: Vec<Option<[f32; 2]>> = Vec::with_capacity(samples.len());
    for bin_samples in samples.iter_mut() {
        if bin_samples.is_empty() {
            bin_percentiles.push(None);
            continue;
        }
        bin_samples.sort_by(|a, b| a.total_cmp(b));
        let lo = percentile_from_sorted(bin_samples, REF_PERCENTILE_LOW);
        let hi = percentile_from_sorted(bin_samples, REF_PERCENTILE_HIGH);
        bin_percentiles.push(Some([lo.min(hi), lo.max(hi)]));
        bin_samples.clear();
        bin_samples.shrink_to_fit();
    }

    // Pass 2: linear interpolation between the two FFT bins straddling
    // each log slot's frequency. Both endpoints must have data; if either
    // is missing (above source Nyquist) the slot falls back to MIN_DB so
    // the curve cleanly stops at the source band edge instead of fading
    // into the noise floor.
    let mut bounds = Vec::with_capacity(REF_POINTS);
    for p in 0..REF_POINTS {
        let t = p as f32 / denom;
        let freq = (log_min + t * (log_max - log_min)).exp();
        let bin_f = (freq / bin_hz).max(0.0);
        let bin_lo = bin_f.floor() as usize;
        let bin_hi = bin_lo + 1;
        let frac = bin_f - bin_lo as f32;
        let pair_lo = bin_percentiles.get(bin_lo).and_then(|c| *c);
        let pair_hi = bin_percentiles.get(bin_hi).and_then(|c| *c);
        let pair = match (pair_lo, pair_hi) {
            (Some(a), Some(b)) => [
                a[0] + (b[0] - a[0]) * frac,
                a[1] + (b[1] - a[1]) * frac,
            ],
            // Past the last valid bin → silence floor.
            (None, _) | (_, None) => [MIN_DB, MIN_DB],
        };
        bounds.push(pair);
    }
    RefEnvelope { bounds }
}

fn percentile_from_sorted(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return MIN_DB;
    }
    let idx = (p * (sorted.len() - 1) as f32).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Offline loudness pass: feed the whole file through the same
/// `LoudnessMeter` the runtime uses, then pull every single-number
/// aggregate out of its final snapshot. Without a block sink attached
/// the meter runs integrated + LRA gating in-line, so LRA and the
/// max-hold readouts all settle to their true full-file values by
/// the last sample.
fn run_loudness_aggregates(l: &[f32], r: &[f32], sr: f32) -> LoudnessSnapshot {
    let mut meter = LoudnessMeter::new(sr);
    let chunk = 8192;
    let mut i = 0;
    while i < l.len() {
        let end = (i + chunk).min(l.len());
        meter.process(&l[i..end], &r[i..end]);
        i = end;
    }
    meter.snapshot()
}

// ---------------------------------------------------------------------
// symphonia decode
// ---------------------------------------------------------------------

fn decode_file(path: &Path) -> Result<(Vec<f32>, Vec<f32>, f32), RefError> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = File::open(path).map_err(RefError::Io)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| RefError::Decode(format!("probe: {e}")))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or(RefError::NoAudio)?
        .clone();
    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| RefError::Decode("no sample rate".into()))? as f32;
    let channels = codec_params
        .channels
        .ok_or_else(|| RefError::Decode("no channels".into()))?;
    let n_channels = channels.count().max(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| RefError::Decode(format!("make decoder: {e}")))?;

    let mut samples_l: Vec<f32> = Vec::new();
    let mut samples_r: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(RefError::Decode(format!("next_packet: {e}"))),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(RefError::Decode(format!("decode: {e}"))),
        };
        let spec = *decoded.spec();
        if sample_buf.is_none() {
            sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        }
        let buf = sample_buf.as_mut().expect("sample_buf just set");
        buf.copy_interleaved_ref(decoded);
        let frames = buf.samples();
        let n_frames = frames.len() / n_channels;
        for f in 0..n_frames {
            let base = f * n_channels;
            let left = frames[base];
            let right = if n_channels >= 2 { frames[base + 1] } else { left };
            samples_l.push(left);
            samples_r.push(right);
        }
    }

    Ok((samples_l, samples_r, sample_rate))
}

// ---------------------------------------------------------------------
// LAME tag (MP3 lowpass)
// ---------------------------------------------------------------------

/// Parse the LAME info tag from an MP3 file's first frame and return the
/// encoder's lowpass cutoff in Hz. Returns `None` for non-MP3 files,
/// missing tags, or older LAME versions that leave the field zeroed.
///
/// Covers ~95 % of MP3s in the wild (modern consumer encoders all use
/// LAME). Other encoders get no correction, which is the right default.
pub fn read_lame_lowpass(path: &Path) -> Option<f32> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if ext != "mp3" {
        return None;
    }
    let mut file = File::open(path).ok()?;

    // Skip ID3v2 header if present. Size field is 4 synchsafe bytes
    // (7 bits each, MSB must be zero) big-endian.
    let mut header = [0u8; 10];
    file.read_exact(&mut header).ok()?;
    let start_offset: u64 = if &header[..3] == b"ID3" {
        let size = ((header[6] & 0x7F) as u32) << 21
            | ((header[7] & 0x7F) as u32) << 14
            | ((header[8] & 0x7F) as u32) << 7
            | (header[9] & 0x7F) as u32;
        10 + size as u64
    } else {
        0
    };
    file.seek(SeekFrom::Start(start_offset)).ok()?;

    // Scan the first ~16 KB for the LAME tag. The tag lives at a fixed
    // offset inside the first MPEG frame (varies by MPEG version /
    // channel mode), so a short linear scan is simplest and robust.
    let mut buf = vec![0u8; 16 * 1024];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);

    // Look for "LAME" magic. Require it to be preceded (within 200
    // bytes) by "Xing" or "Info" — the VBR header marker — so random
    // "LAME" occurrences inside embedded artwork or tags don't match.
    let lame_idx = find_lame_magic(&buf)?;
    // Byte at offset +10 from the "LAME" magic is the lowpass value in
    // 100 Hz units. Zero means "not set" (older encoders or CBR).
    let lowpass_byte = *buf.get(lame_idx + 10)?;
    if lowpass_byte == 0 {
        return None;
    }
    Some(lowpass_byte as f32 * 100.0)
}

fn find_lame_magic(buf: &[u8]) -> Option<usize> {
    let mut last_marker: Option<usize> = None;
    let mut i = 0;
    while i + 4 <= buf.len() {
        let window = &buf[i..i + 4];
        if window == b"Xing" || window == b"Info" {
            last_marker = Some(i);
        }
        if window == b"LAME" {
            if let Some(m) = last_marker {
                if i.saturating_sub(m) <= 200 {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_envelope_has_correct_shape() {
        let e = RefEnvelope::empty();
        assert_eq!(e.bounds.len(), REF_POINTS);
        assert!(e.bounds.iter().all(|b| b[0] == MIN_DB && b[1] == MIN_DB));
    }

    #[test]
    fn percentile_from_sorted_is_monotone() {
        let xs: Vec<f32> = (0..100).map(|n| n as f32).collect();
        assert!(percentile_from_sorted(&xs, 0.10) < percentile_from_sorted(&xs, 0.90));
        assert_eq!(percentile_from_sorted(&xs, 0.00), 0.0);
        assert_eq!(percentile_from_sorted(&xs, 1.00), 99.0);
    }

    #[test]
    fn lame_magic_requires_marker_nearby() {
        // "LAME" with no Xing/Info nearby: rejected.
        let buf = b"xxxxxxxxLAMExxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        assert!(find_lame_magic(buf).is_none());
        // "Xing" followed by "LAME" within 200 bytes: accepted.
        let mut buf2 = Vec::new();
        buf2.extend_from_slice(b"Xing");
        buf2.extend_from_slice(&[0u8; 50]);
        buf2.extend_from_slice(b"LAME");
        buf2.extend_from_slice(&[0u8; 20]);
        let idx = find_lame_magic(&buf2).expect("should find LAME");
        assert_eq!(&buf2[idx..idx + 4], b"LAME");
    }
}
