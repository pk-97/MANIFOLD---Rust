//! Decodes audio files into raw interleaved PCM f32 samples for waveform rendering.
//!
//! Uses symphonia (same decoder kira uses internally) to decode any supported
//! audio format (WAV, AIFF, FLAC, MP3, AAC, OGG) into a flat f32 buffer.
//!
//! This is separate from kira's playback pipeline — kira handles real-time audio
//! playback, while this module provides sample access for visualization.

use kira::sound::static_sound::StaticSoundData;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decoded audio data: interleaved f32 samples + metadata.
pub struct DecodedAudio {
    /// Interleaved PCM samples (all channels interleaved: L R L R ...)
    pub samples: Vec<f32>,
    /// Sample rate in Hz (e.g. 44100, 48000)
    pub sample_rate: u32,
    /// Number of channels (1=mono, 2=stereo)
    pub channels: usize,
}

impl DecodedAudio {
    /// Extract interleaved PCM samples from kira's already-decoded StaticSoundData.
    /// This is a pure memory copy (no file I/O, no decoding) — takes single-digit ms
    /// even for long tracks. Kira's Frame is always stereo (left, right).
    pub fn from_static_sound_data(data: &StaticSoundData) -> Self {
        let num_frames = data.frames.len();
        let mut samples = Vec::with_capacity(num_frames * 2);
        for frame in data.frames.iter() {
            samples.push(frame.left);
            samples.push(frame.right);
        }
        DecodedAudio {
            samples,
            sample_rate: data.sample_rate,
            channels: 2,
        }
    }
}

/// Decode an audio file into raw interleaved PCM f32 samples.
///
/// Supports WAV, AIFF, FLAC, MP3, AAC, OGG via symphonia.
/// Returns samples + sample_rate + channel count.
pub fn decode_audio_to_pcm(path: &str) -> Result<DecodedAudio, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open audio file '{}': {}", path, e))?;

    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // Provide a hint based on file extension
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("Failed to probe audio format: {}", e))?;

    let mut format = probed.format;

    // Find the first audio track
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| "No audio track found".to_string())?;

    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| "Unknown sample rate".to_string())?;
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(2);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Failed to create decoder: {}", e))?;

    let mut all_samples: Vec<f32> = Vec::new();

    // Decode all packets
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break; // End of stream
            }
            Err(e) => {
                log::warn!("[AudioDecoder] Packet read error (continuing): {}", e);
                break;
            }
        };

        // Skip packets from other tracks
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(symphonia::core::errors::Error::DecodeError(msg)) => {
                log::warn!("[AudioDecoder] Decode error (skipping packet): {}", msg);
                continue;
            }
            Err(e) => {
                log::warn!("[AudioDecoder] Fatal decode error: {}", e);
                break;
            }
        };

        // Convert decoded audio buffer to interleaved f32
        let spec = *decoded.spec();
        let num_frames = decoded.frames();
        let num_channels = spec.channels.count();

        if num_frames == 0 || num_channels == 0 {
            continue;
        }

        let mut sample_buf = SampleBuffer::<f32>::new(num_frames as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);
        all_samples.extend_from_slice(sample_buf.samples());
    }

    if all_samples.is_empty() {
        return Err("Decoded zero samples".to_string());
    }

    let total_frames = all_samples.len() / channels;
    log::info!(
        "[AudioDecoder] Decoded '{}': {} frames, {}ch, {}Hz ({:.1}s)",
        std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        total_frames,
        channels,
        sample_rate,
        total_frames as f32 / sample_rate as f32,
    );

    Ok(DecodedAudio {
        samples: all_samples,
        sample_rate,
        channels,
    })
}
