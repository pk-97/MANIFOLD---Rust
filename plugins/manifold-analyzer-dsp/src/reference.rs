//! Offline reference-track analysis for the MS plot overlay.
//!
//! Streaming path: decode an audio file (WAV / MP3 / FLAC / AAC / M4A /
//! OGG / AIFF) via
//! symphonia, run it through the same BH-windowed FFT the real-time
//! plugin uses (one pass per FFT size in the live plugin's dropdown so
//! the per-bin distribution overlays the live MS without binwidth
//! offset), maintain bounded per-bin quantile histograms, then reduce
//! to low/mid/high percentile envelopes at every FFT bin. Integrated LUFS is computed via the same BS.1770 meter so the
//! GUI can gain-match the ref to the live mix.
//!
//! Nothing here runs on the audio thread; the analysis is kicked off from
//! the GUI thread on file-pick and typically runs on a worker thread.
//! New persisted envelopes keep complete per-FFT-bin percentile triples for
//! accurate overlays; the older ~1 K log-grid representation remains readable.
//!
//! LAME tag parsing lives here too — used to learn a per-file lowpass
//! cutoff (e.g. 16 kHz for 128 kbps MP3) so the band doesn't misleadingly
//! taper off at the codec's brickwall.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::de::Error as DeError;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};

use crate::median::QuantizedHistogram;
use crate::{Analyzer, LoudnessMeter, MIN_DB};

/// Number of log-spaced display points stored per envelope. Chosen to cover
/// the pixel density of a typical analyzer window (~1 K wide).
pub const REF_POINTS: usize = 1024;

/// Low edge of the log-frequency grid the envelope is sampled on.
pub const REF_FREQ_MIN: f32 = 10.0;

/// High edge of the grid. Covers up to 96 kHz sample-rate Nyquist; for
/// lower-rate sources we just clip above their Nyquist at draw time.
pub const REF_FREQ_MAX: f32 = 48_000.0;

/// Overlap used for offline analysis. This matches the live analyzer's
/// 90-percent overlap and keeps reference frame timing comparable to live.
pub const REF_OVERLAP_RATIO: f32 = 0.9;

/// EWMA release time constant for the offline `Analyzer`. Rise is instant,
/// matching the live peak-style analyzer; decay is 200 ms.
pub const REF_AVG_MS: f32 = 200.0;

/// Percentile bounds for the band. 10 / 50 / 90 is the mastering-tool
/// norm — `low`/`high` represent "the typical 80 % of spectral content"
/// without reacting to silence or rare transients at either extreme;
/// `mid` is the median, drawn as the bold "this is the centre of the
/// distribution" line a mix should target.
pub const REF_PERCENTILE_LOW: f32 = 0.10;
pub const REF_PERCENTILE_MID: f32 = 0.50;
pub const REF_PERCENTILE_HIGH: f32 = 0.90;

/// Per-FFT-size envelope. The same source audio is analysed at every
/// FFT size the live plugin offers so the GUI can pick the matching
/// envelope at draw time and curves overlay the live MS without any
/// binwidth offset (per-bin dB scales with `10·log₁₀(N)` for broadband).
#[derive(Clone, Debug)]
pub struct RefEnvelopeAtFft {
    pub fft_size: usize,
    /// Triples of approximate `[low_db, mid_db, high_db]` (10 / 50 / 90
    /// percentile estimates) at each of `REF_POINTS` log-spaced frequencies spanning
    /// `[REF_FREQ_MIN, REF_FREQ_MAX]`.
    pub bounds: Vec<[f32; 3]>,
    /// Per-FFT-bin percentile triples, including bins that are not represented
    /// by the persisted log-spaced display grid. Empty bins contain the
    /// `MIN_DB` floor triple. This preserves the full-resolution history for
    /// overlays that need to read a specific FFT bin. The field is kept in
    /// memory as triples, but serialized as a compact compressed f32 payload.
    pub bin_bounds: Vec<[f32; 3]>,
}

const BIN_BOUNDS_VERSION: u8 = 1;
const BIN_BOUNDS_COMPONENTS: usize = 3;
const BIN_BOUNDS_MAX_COUNT: usize = 1_048_576;
const BIN_BOUNDS_MAX_COMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const BIN_BOUNDS_MAX_RAW_BYTES: usize = BIN_BOUNDS_MAX_COUNT * BIN_BOUNDS_COMPONENTS * 4;

#[derive(Debug, Serialize, Deserialize)]
struct CompactBinBounds {
    version: u8,
    count: usize,
    data: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum BinBoundsOnDisk {
    Compact(CompactBinBounds),
    Legacy(Vec<[f32; 3]>),
}

impl Serialize for RefEnvelopeAtFft {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("RefEnvelopeAtFft", 3)?;
        state.serialize_field("fft_size", &self.fft_size)?;
        // New analyses have all full-resolution bins, so the old 1024-point
        // grid would only duplicate data. Retain it when it is present for
        // compatibility with states written by older versions.
        if !self.bounds.is_empty() {
            state.serialize_field("bounds", &self.bounds)?;
        }
        if !self.bin_bounds.is_empty() {
            state.serialize_field("binBounds", &compact_bin_bounds(&self.bin_bounds))?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for RefEnvelopeAtFft {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            fft_size: usize,
            #[serde(default)]
            bounds: Vec<[f32; 3]>,
            #[serde(default, rename = "binBounds")]
            bin_bounds: Option<BinBoundsOnDisk>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let bin_bounds = match wire.bin_bounds {
            None => Vec::new(),
            Some(BinBoundsOnDisk::Legacy(bounds)) => {
                validate_bin_bounds_count(bounds.len()).map_err(D::Error::custom)?;
                bounds
            }
            Some(BinBoundsOnDisk::Compact(compact)) => {
                decode_compact_bin_bounds(wire.fft_size, compact).map_err(D::Error::custom)?
            }
        };
        Ok(Self {
            fft_size: wire.fft_size,
            bounds: wire.bounds,
            bin_bounds,
        })
    }
}

fn expected_bin_count(fft_size: usize) -> Option<usize> {
    fft_size.checked_div(2)?.checked_add(1)
}

fn validate_bin_bounds_size(fft_size: usize, count: usize) -> Result<(), String> {
    validate_bin_bounds_count(count)?;
    if count == 0 {
        return Ok(());
    }
    let expected = expected_bin_count(fft_size).unwrap_or(0);
    if count != expected {
        return Err(format!(
            "binBounds count {count} does not match FFT size {fft_size} ({expected})"
        ));
    }
    Ok(())
}

fn validate_bin_bounds_count(count: usize) -> Result<(), String> {
    if count > BIN_BOUNDS_MAX_COUNT {
        return Err(format!("binBounds count {count} exceeds limit"));
    }
    Ok(())
}

fn compact_bin_bounds(bounds: &[[f32; 3]]) -> CompactBinBounds {
    let mut raw = Vec::with_capacity(bounds.len() * BIN_BOUNDS_COMPONENTS * 4);
    for triple in bounds {
        for value in triple {
            raw.extend_from_slice(&value.to_le_bytes());
        }
    }
    let data = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    CompactBinBounds {
        version: BIN_BOUNDS_VERSION,
        count: bounds.len(),
        data,
    }
}

fn decode_compact_bin_bounds(
    fft_size: usize,
    compact: CompactBinBounds,
) -> Result<Vec<[f32; 3]>, String> {
    if compact.version != BIN_BOUNDS_VERSION {
        return Err(format!("unsupported binBounds version {}", compact.version));
    }
    validate_bin_bounds_size(fft_size, compact.count)?;
    if compact.data.len() > BIN_BOUNDS_MAX_COMPRESSED_BYTES {
        return Err("compressed binBounds payload exceeds limit".into());
    }
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(
        &compact.data,
        BIN_BOUNDS_MAX_RAW_BYTES,
    )
    .map_err(|err| format!("invalid compressed binBounds payload: {err:?}"))?;
    let expected_bytes = compact
        .count
        .checked_mul(BIN_BOUNDS_COMPONENTS * 4)
        .ok_or_else(|| "binBounds size overflow".to_string())?;
    if expected_bytes > BIN_BOUNDS_MAX_RAW_BYTES || raw.len() != expected_bytes {
        return Err(format!(
            "binBounds payload has {} bytes, expected {expected_bytes}",
            raw.len()
        ));
    }
    let mut bounds = Vec::with_capacity(compact.count);
    for chunk in raw.chunks_exact(BIN_BOUNDS_COMPONENTS * 4) {
        bounds.push([
            f32::from_le_bytes(chunk[0..4].try_into().unwrap()),
            f32::from_le_bytes(chunk[4..8].try_into().unwrap()),
            f32::from_le_bytes(chunk[8..12].try_into().unwrap()),
        ]);
    }
    Ok(bounds)
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RefEnvelope {
    /// One entry per FFT size analysed. Always non-empty after a
    /// successful `analyze_ref_file`. Empty after a failed deserialize
    /// of an older on-disk format — caller should treat that as
    /// "no usable analysis" and prompt a re-load.
    #[serde(default)]
    pub per_fft: Vec<RefEnvelopeAtFft>,
}

impl RefEnvelope {
    pub fn empty() -> Self {
        Self {
            per_fft: Vec::new(),
        }
    }

    /// Pick the envelope whose FFT size matches `fft_size` exactly, or
    /// the nearest available one if no exact match. Returns `None` only
    /// when `per_fft` is empty (e.g. legacy save deserialised without
    /// envelope data).
    pub fn for_fft(&self, fft_size: usize) -> Option<&RefEnvelopeAtFft> {
        self.per_fft.iter().min_by_key(|e| {
            let a = e.fft_size as i64;
            let b = fft_size as i64;
            (a - b).unsigned_abs()
        })
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

/// Run the full offline analysis — decode, FFT stats at every requested
/// FFT size, LUFS, LAME cutoff. `fft_sizes` should list every size the
/// live plugin offers in its FFT-Size dropdown; the worker analyses
/// each so the GUI can pick the matching envelope at draw time and the
/// ref overlays the live MS with no binwidth fudge. Blocks the calling
/// thread; intended to run on a worker.
pub fn analyze_ref_file(path: &Path, fft_sizes: &[usize]) -> Result<RefAnalysis, RefError> {
    let lowpass_hz = read_lame_lowpass(path);
    let mut meter = None;
    let mut first_pass = None;
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut mid = Vec::new();
    let mut side = Vec::new();
    let spool = if fft_sizes.len() > 1 {
        Some(BufWriter::new(tempfile::tempfile().map_err(RefError::Io)?))
    } else {
        None
    };
    let mut spool = spool;

    let (source_sr, decoded_frames) = decode_file(path, |interleaved, channels, sr| {
        if meter.is_none() {
            let mut loudness = LoudnessMeter::new(sr);
            loudness.set_deferred_aggregation(true);
            meter = Some(loudness);
        }
        if first_pass.is_none() {
            first_pass = fft_sizes
                .first()
                .copied()
                .map(|n_fft| ReferenceFftPass::new(sr, n_fft));
        }
        let frame_count = interleaved.len() / channels.max(1);
        left.resize(frame_count, 0.0);
        right.resize(frame_count, 0.0);
        mid.resize(frame_count, 0.0);
        side.resize(frame_count, 0.0);
        for frame in 0..frame_count {
            let base = frame * channels;
            let l = interleaved[base];
            let r = if channels >= 2 {
                interleaved[base + 1]
            } else {
                l
            };
            left[frame] = l;
            right[frame] = r;
            mid[frame] = 0.5 * (l + r);
            side[frame] = 0.5 * (l - r);
        }
        if let Some(spool) = spool.as_mut() {
            for (&mid_sample, &side_sample) in mid.iter().zip(&side) {
                spool
                    .write_all(&mid_sample.to_le_bytes())
                    .map_err(RefError::Io)?;
                spool
                    .write_all(&side_sample.to_le_bytes())
                    .map_err(RefError::Io)?;
            }
        }
        meter
            .as_mut()
            .expect("meter initialized on first packet")
            .process(&left, &right);
        if let Some(pass) = first_pass.as_mut() {
            pass.process(&mid, &side)?;
        }
        Ok(())
    })?;

    let source_sr = source_sr.max(1.0);
    let duration_secs = decoded_frames as f32 / source_sr;
    if decoded_frames < (source_sr * 0.5) as usize {
        return Err(RefError::TooShort);
    }
    let mut mid_per_fft = Vec::with_capacity(fft_sizes.len());
    let mut side_per_fft = Vec::with_capacity(fft_sizes.len());
    if let Some(pass) = first_pass {
        mid_per_fft.push(pass.mid.finish(source_sr));
        side_per_fft.push(pass.side.finish(source_sr));
    }
    let mut spool = spool
        .map(|spool| {
            spool
                .into_inner()
                .map_err(|err| RefError::Io(err.into_error()))
        })
        .transpose()?;
    let mut spool = spool.take().map(BufReader::new);
    for &n_fft in fft_sizes.iter().skip(1) {
        let Some(spool) = spool.as_mut() else {
            return Err(RefError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "reference replay spool is unavailable",
            )));
        };
        let mut pass = ReferenceFftPass::new(source_sr, n_fft);
        replay_spool(spool, &mut pass, decoded_frames)?;
        mid_per_fft.push(pass.mid.finish(source_sr));
        side_per_fft.push(pass.side.finish(source_sr));
    }
    let mid_env = RefEnvelope {
        per_fft: mid_per_fft,
    };
    let side_env = RefEnvelope {
        per_fft: side_per_fft,
    };

    let loudness = meter
        .as_mut()
        .expect("meter initialized when audio was decoded");
    loudness.finalize_aggregation();
    let loudness = loudness.snapshot();

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

struct ReferenceFftPass {
    mid: SpectrumPass,
    side: SpectrumPass,
}

impl ReferenceFftPass {
    fn new(sr: f32, n_fft: usize) -> Self {
        Self {
            mid: SpectrumPass::new(sr, n_fft),
            side: SpectrumPass::new(sr, n_fft),
        }
    }

    fn process(&mut self, mid: &[f32], side: &[f32]) -> Result<(), RefError> {
        self.mid.process(mid)?;
        self.side.process(side)
    }
}

fn replay_spool(
    spool: &mut BufReader<File>,
    pass: &mut ReferenceFftPass,
    frame_count: usize,
) -> Result<(), RefError> {
    const CHUNK_FRAMES: usize = 8192;
    let mut bytes = vec![0u8; CHUNK_FRAMES * 2 * std::mem::size_of::<f32>()];
    let mut mid = vec![0.0f32; CHUNK_FRAMES];
    let mut side = vec![0.0f32; CHUNK_FRAMES];
    spool.seek(SeekFrom::Start(0)).map_err(RefError::Io)?;
    let mut frames_read = 0usize;
    while frames_read < frame_count {
        let frames = (frame_count - frames_read).min(CHUNK_FRAMES);
        let byte_count = frames * 2 * std::mem::size_of::<f32>();
        spool
            .read_exact(&mut bytes[..byte_count])
            .map_err(RefError::Io)?;
        for frame in 0..frames {
            let offset = frame * 8;
            mid[frame] = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            side[frame] = f32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        }
        pass.process(&mid[..frames], &side[..frames])?;
        frames_read += frames;
    }
    Ok(())
}

struct SpectrumPass {
    analyzer: Analyzer,
    histograms: Vec<QuantizedHistogram>,
    fft_size: usize,
}

impl SpectrumPass {
    fn new(sr: f32, n_fft: usize) -> Self {
        let mut analyzer = Analyzer::new(sr, n_fft);
        analyzer.set_overlap_ratio(REF_OVERLAP_RATIO);
        analyzer.set_attack_release_ms(0.0, REF_AVG_MS);
        Self {
            analyzer,
            histograms: vec![QuantizedHistogram::default(); n_fft / 2 + 1],
            fft_size: n_fft,
        }
    }

    fn process(&mut self, samples: &[f32]) -> Result<(), RefError> {
        let histograms = &mut self.histograms;
        let mut error = None;
        self.analyzer.process_mono(samples, |average| {
            for (histogram, &value) in histograms.iter_mut().zip(average) {
                // Silence below the display floor has no useful distinction,
                // but non-finite values must still be rejected by the histogram.
                let value = if value.is_finite() {
                    value.max(MIN_DB)
                } else {
                    value
                };
                if let Err(err) = histogram.add(value) {
                    error = Some(RefError::Decode(format!("reference spectrum: {err}")));
                    break;
                }
            }
        });
        error.map_or(Ok(()), Err)
    }

    fn finish(self, _sr: f32) -> RefEnvelopeAtFft {
        RefEnvelopeAtFft {
            fft_size: self.fft_size,
            bounds: Vec::new(),
            bin_bounds: dense_bin_bounds(&self.histograms),
        }
    }
}

fn dense_bin_bounds(samples: &[QuantizedHistogram]) -> Vec<[f32; 3]> {
    samples
        .iter()
        .map(|bounds| bounds.estimates().unwrap_or([MIN_DB; 3]))
        .collect()
}

// ---------------------------------------------------------------------
// symphonia decode
// ---------------------------------------------------------------------

fn decode_file<F>(path: &Path, mut on_frames: F) -> Result<(f32, usize), RefError>
where
    F: FnMut(&[f32], usize, f32) -> Result<(), RefError>,
{
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
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
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| RefError::Decode(format!("make decoder: {e}")))?;

    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut sample_spec = None;
    let mut decoded_frames = 0usize;

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
        let decoded_rate = spec.rate as f32;
        if (decoded_rate - sample_rate).abs() > f32::EPSILON {
            return Err(RefError::Decode(format!(
                "sample rate changed from {sample_rate} Hz to {decoded_rate} Hz"
            )));
        }
        if sample_buf.as_ref().is_none_or(|buf| {
            buf.capacity() < decoded.capacity() * spec.channels.count() || sample_spec != Some(spec)
        }) {
            sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
            sample_spec = Some(spec);
        }
        let buf = sample_buf.as_mut().expect("sample_buf just set");
        buf.copy_interleaved_ref(decoded);
        let frames = buf.samples();
        let frame_channels = spec.channels.count().max(1);
        let n_frames = frames.len() / frame_channels;
        on_frames(frames, frame_channels, sample_rate)?;
        decoded_frames = decoded_frames.saturating_add(n_frames);
    }

    Ok((sample_rate, decoded_frames))
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
    use std::path::{Path, PathBuf};

    use serde::de::{DeserializeSeed, IntoDeserializer, MapAccess};
    use serde::Deserialize;

    use super::*;

    #[test]
    fn retaining_every_fft_bin_preserves_the_reference_envelope() {
        let mut pass = SpectrumPass::new(48000.0, 32768);
        let signal: Vec<f32> = (0..48000)
            .map(|i| (std::f32::consts::TAU * 997.0 * i as f32 / 48000.0).sin() * 0.25)
            .collect();
        pass.process(&signal).unwrap();
        let envelope = pass.finish(48000.0);
        assert_eq!(envelope.bin_bounds.len(), 32768 / 2 + 1);
        assert!(envelope.bin_bounds.iter().any(|bound| bound[1] > MIN_DB));
    }

    #[test]
    fn replayed_spool_matches_serial_processing_across_packet_boundaries() {
        let frame_count = 20_000;
        let mid: Vec<f32> = (0..frame_count)
            .map(|frame| {
                let t = frame as f32 / 48_000.0;
                (std::f32::consts::TAU * 997.0 * t).sin() * 0.25
                    + (std::f32::consts::TAU * 2_113.0 * t).sin() * 0.1
            })
            .collect();
        let side: Vec<f32> = (0..frame_count)
            .map(|frame| {
                let t = frame as f32 / 48_000.0;
                (std::f32::consts::TAU * 431.0 * t).cos() * 0.2
            })
            .collect();

        let mut serial = ReferenceFftPass::new(48_000.0, 1_024);
        let mut offset = 0;
        while offset < frame_count {
            let end = (offset + 137).min(frame_count);
            serial.process(&mid[offset..end], &side[offset..end]).unwrap();
            offset = end;
        }

        let mut spool = BufWriter::new(tempfile::tempfile().unwrap());
        for (&mid_sample, &side_sample) in mid.iter().zip(&side) {
            spool.write_all(&mid_sample.to_le_bytes()).unwrap();
            spool.write_all(&side_sample.to_le_bytes()).unwrap();
        }
        let file = spool.into_inner().unwrap();
        let mut replay = ReferenceFftPass::new(48_000.0, 1_024);
        replay_spool(&mut BufReader::new(file), &mut replay, frame_count).unwrap();

        assert_eq!(
            serial.mid.finish(48_000.0).bin_bounds,
            replay.mid.finish(48_000.0).bin_bounds
        );
        assert_eq!(
            serial.side.finish(48_000.0).bin_bounds,
            replay.side.finish(48_000.0).bin_bounds
        );
    }

    #[test]
    fn empty_envelope_has_no_data() {
        let e = RefEnvelope::empty();
        assert!(e.per_fft.is_empty());
        assert!(e.for_fft(4096).is_none());
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

    fn fixture_path(extension: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "manifold-reference-{}-{}.{}",
            std::process::id(),
            extension,
            extension
        ))
    }

    fn pcm_samples(sample_rate: u32, frames: usize) -> Vec<i16> {
        (0..frames)
            .map(|frame| {
                let phase = frame as f32 * 440.0 * std::f32::consts::TAU / sample_rate as f32;
                (phase.sin() * 8_000.0) as i16
            })
            .collect()
    }

    fn write_wav(path: &Path) {
        let sample_rate = 8_000u32;
        let samples = pcm_samples(sample_rate, sample_rate as usize);
        let mut pcm = Vec::with_capacity(samples.len() * 4);
        for sample in samples {
            pcm.extend_from_slice(&sample.to_le_bytes());
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
        let mut bytes = Vec::with_capacity(44 + pcm.len());
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36u32 + pcm.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 4).to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&pcm);
        std::fs::write(path, bytes).unwrap();
    }

    fn write_aiff(path: &Path) {
        let sample_rate = 8_000u32;
        let samples = pcm_samples(sample_rate, sample_rate as usize);
        let mut pcm = Vec::with_capacity(samples.len() * 4);
        for sample in samples {
            pcm.extend_from_slice(&sample.to_be_bytes());
            pcm.extend_from_slice(&sample.to_be_bytes());
        }
        let mut bytes = Vec::with_capacity(54 + pcm.len());
        bytes.extend_from_slice(b"FORM");
        bytes.extend_from_slice(&(46u32 + pcm.len() as u32).to_be_bytes());
        bytes.extend_from_slice(b"AIFF");
        bytes.extend_from_slice(b"COMM");
        bytes.extend_from_slice(&18u32.to_be_bytes());
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&(sample_rate).to_be_bytes());
        bytes.extend_from_slice(&16u16.to_be_bytes());
        // 80-bit IEEE extended 8000 Hz: exponent 13, mantissa 8000/2^13.
        bytes.extend_from_slice(&0x400cu16.to_be_bytes());
        bytes.extend_from_slice(&(u64::from(sample_rate) << 50).to_be_bytes());
        bytes.extend_from_slice(b"SSND");
        bytes.extend_from_slice(&(8u32 + pcm.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&pcm);
        std::fs::write(path, bytes).unwrap();
    }

    fn assert_decoded_fixture(path: &Path) {
        let analysis = analyze_ref_file(path, &[64]).unwrap();
        assert_eq!(analysis.source_sample_rate, 8_000.0);
        assert!((analysis.duration_secs - 1.0).abs() < 0.01);
        assert_eq!(analysis.mid.per_fft.len(), 1);
        assert!(analysis.mid.per_fft[0].bounds.is_empty());
        assert!(analysis.mid.per_fft[0]
            .bin_bounds
            .iter()
            .any(|bound| bound[1] > MIN_DB));
    }

    #[test]
    fn synthetic_wav_reference_decodes_and_analyzes() {
        let path = fixture_path("wav");
        write_wav(&path);
        assert_decoded_fixture(&path);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn synthetic_aiff_reference_decodes_and_analyzes() {
        let path = fixture_path("aiff");
        write_aiff(&path);
        assert_decoded_fixture(&path);
        let _ = std::fs::remove_file(path);
    }

    fn write_float_wav_18khz(path: &Path) {
        let sample_rate = 48_000u32;
        let frames = sample_rate as usize;
        let mut pcm = Vec::with_capacity(frames * 2 * 4);
        for frame in 0..frames {
            let sample = (std::f32::consts::TAU * 18_000.0 * frame as f32
                / sample_rate as f32)
                .sin()
                * 0.25;
            pcm.extend_from_slice(&sample.to_le_bytes());
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
        let mut bytes = Vec::with_capacity(44 + pcm.len());
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36u32 + pcm.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2 * 4).to_le_bytes());
        bytes.extend_from_slice(&8u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&pcm);
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn stereo_float_wav_18khz_retains_full_bin_peak_at_each_fft_size() {
        let path = std::env::temp_dir().join(format!(
            "manifold-reference-{}-18khz.wav",
            std::process::id()
        ));
        write_float_wav_18khz(&path);
        let analysis = analyze_ref_file(&path, &[8192, 32768]).unwrap();
        for envelope in &analysis.mid.per_fft {
            let bin = (18_000.0 * envelope.fft_size as f32 / 48_000.0).round() as usize;
            assert_eq!(envelope.bin_bounds.len(), envelope.fft_size / 2 + 1);
            let peak_db = envelope.bin_bounds[bin][1];
            assert!(
                (peak_db + 12.041).abs() < 1.1,
                "{} FFT bin {bin} peak {peak_db} dB",
                envelope.fft_size
            );
        }
        let _ = std::fs::remove_file(path);
    }

    struct LegacyEnvelopeMap {
        field: usize,
    }

    impl<'de> MapAccess<'de> for LegacyEnvelopeMap {
        type Error = serde::de::value::Error;

        fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
        where
            K: DeserializeSeed<'de>,
        {
            let key = match self.field {
                0 => "fft_size",
                1 => "bounds",
                _ => return Ok(None),
            };
            seed.deserialize(key.into_deserializer()).map(Some)
        }

        fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
        where
            V: DeserializeSeed<'de>,
        {
            let value = match self.field {
                0 => seed.deserialize(8192usize.into_deserializer()),
                1 => seed.deserialize(vec![vec![MIN_DB; 3]].into_deserializer()),
                _ => unreachable!("value requested after map end"),
            };
            self.field += 1;
            value
        }
    }

    #[test]
    fn legacy_envelope_without_bin_bounds_deserializes_with_empty_default() {
        let map = serde::de::value::MapAccessDeserializer::new(LegacyEnvelopeMap { field: 0 });
        let envelope = RefEnvelopeAtFft::deserialize(map).unwrap();
        assert_eq!(envelope.fft_size, 8192);
        assert_eq!(envelope.bounds.len(), 1);
        assert!(envelope.bin_bounds.is_empty());
    }

    #[test]
    fn compact_bin_bounds_round_trip_and_omit_redundant_grid() {
        let envelope = RefEnvelopeAtFft {
            fft_size: 8,
            bounds: Vec::new(),
            bin_bounds: vec![
                [MIN_DB, MIN_DB, MIN_DB],
                [-12.5, -3.25, 4.75],
                [1.0, 2.0, 3.0],
                [4.0, 5.0, 6.0],
                [7.0, 8.0, 9.0],
            ],
        };
        let value = serde_json::to_value(&envelope).unwrap();
        assert!(value.get("bounds").is_none());
        assert_eq!(value["binBounds"]["version"], BIN_BOUNDS_VERSION);
        assert_eq!(value["binBounds"]["count"], 5);
        let decoded: RefEnvelopeAtFft = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.bin_bounds, envelope.bin_bounds);
    }

    #[test]
    fn compact_bin_bounds_are_smaller_than_legacy_json() {
        let bin_bounds: Vec<[f32; 3]> = (0..8193)
            .map(|index| {
                let value = (index as f32 * 0.013).sin() * 80.0 - 100.0;
                [value, value + 0.25, value + 0.5]
            })
            .collect();
        let compact = serde_json::to_vec(&RefEnvelopeAtFft {
            fft_size: 16_384,
            bounds: Vec::new(),
            bin_bounds: bin_bounds.clone(),
        })
        .unwrap();
        let legacy = serde_json::to_vec(&serde_json::json!({
            "fft_size": 16_384,
            "binBounds": bin_bounds,
        }))
        .unwrap();
        assert!(
            compact.len() < legacy.len(),
            "compact {} bytes, legacy {} bytes",
            compact.len(),
            legacy.len()
        );
    }

    #[test]
    fn legacy_dense_bin_bounds_still_deserialize() {
        let value = serde_json::json!({
            "fft_size": 8,
            "bounds": [[1.0, 2.0, 3.0]],
            "binBounds": [
                [-240.0, -240.0, -240.0],
                [-12.5, -3.25, 4.75],
                [1.0, 2.0, 3.0],
                [4.0, 5.0, 6.0],
                [7.0, 8.0, 9.0]
            ]
        });
        let decoded: RefEnvelopeAtFft = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.bounds.len(), 1);
        assert_eq!(decoded.bin_bounds.len(), 5);
        assert_eq!(decoded.bin_bounds[1], [-12.5, -3.25, 4.75]);
    }

    #[test]
    fn compact_bin_bounds_reject_invalid_size_before_decompression() {
        let value = serde_json::json!({
            "fft_size": 8,
            "binBounds": {
                "version": BIN_BOUNDS_VERSION,
                "count": BIN_BOUNDS_MAX_COUNT + 1,
                "data": []
            }
        });
        let error = serde_json::from_value::<RefEnvelopeAtFft>(value).unwrap_err();
        assert!(error.to_string().contains("exceeds limit"));
    }

    #[test]
    fn vorbis_codec_is_registered_when_feature_enabled() {
        use symphonia::core::codecs::CODEC_TYPE_VORBIS;

        assert!(symphonia::default::get_codecs()
            .get_codec(CODEC_TYPE_VORBIS)
            .is_some());
    }
}
