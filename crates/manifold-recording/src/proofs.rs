//! Feature-gated proof-harness support (`recording-proofs`): the pattern
//! writer that bakes a decodable frame index into pixels via the real GPU
//! path, and the ffprobe/ffmpeg oracle that reads it back out of a recorded
//! file. See docs/LIVE_RECORDING_PROOFS_DESIGN.md §4.
//!
//! Everything here is test/harness infrastructure — it is not part of the
//! production recording path (that stays in `session.rs`/`recording_thread.rs`
//! untouched).

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use manifold_gpu::{
    GpuBinding, GpuComputePipeline, GpuDevice, GpuEncoder, GpuTexture, GpuTextureDesc,
    GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

/// Number of blocks in the frame-index pattern (2 sync blocks + 24 index
/// bits). Must match `NUM_BLOCKS` in `shaders/test_pattern.wgsl`.
const NUM_BLOCKS: u32 = 26;
/// Number of frame-index bits encoded (blocks 2..26). Must match
/// `INDEX_BITS` in `shaders/test_pattern.wgsl`.
const INDEX_BITS: u32 = 24;

// ---------------------------------------------------------------------
// Headless GPU device (mirrors manifold-renderer's `test_device()`,
// crates/manifold-renderer/src/lib.rs).
// ---------------------------------------------------------------------

/// Process-wide serialization lock for the proof suite's GPU + AVAssetWriter
/// work. `GpuDevice` construction (~200-500ms) and the ProRes hardware
/// encoder are shared, contended resources; running the suite's tests
/// concurrently (cargo test's default per-binary thread parallelism) risks
/// exactly the nondeterministic flakiness `manifold-renderer`'s
/// `GPU_TEST_LOCK` was added to avoid. Plain `std::sync::Mutex` (not
/// `parking_lot`) — no new dependency beyond the design's specified
/// `serde_json` optional dep.
static GPU_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the process-wide GPU/encoder serialization lock for the
/// remainder of the calling test. Hold the returned guard for the test's
/// full body.
pub fn gpu_guard() -> MutexGuard<'static, ()> {
    GPU_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Shared headless `GpuDevice` for the proof suite. `GpuDevice::new()` warms
/// Metal pipeline state; tests only need *a* working device, never a fresh
/// one. Callers should hold [`gpu_guard`] for the test's duration so the
/// shared device (and the native ProRes/HEVC hardware encoder) isn't driven
/// concurrently by other tests in the same binary.
pub fn test_device() -> Arc<GpuDevice> {
    static SHARED: OnceLock<Arc<GpuDevice>> = OnceLock::new();
    SHARED.get_or_init(|| Arc::new(GpuDevice::new())).clone()
}

/// Allocate a fresh Rgba16Float texture usable as both a compute-shader
/// write target and a read source — the harness's stand-in for the
/// compositor's linear-light output texture. Same usage flags as
/// `TextureRingPool` (texture_pool.rs).
pub fn synthetic_source_texture(device: &GpuDevice, width: u32, height: u32) -> GpuTexture {
    let desc = GpuTextureDesc {
        width,
        height,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::SHADER_WRITE
            | GpuTextureUsage::COPY_SRC,
        label: "RecordingProofSource",
        mip_levels: 1,
    };
    device.create_texture(&desc)
}

// ---------------------------------------------------------------------
// Pattern writer
// ---------------------------------------------------------------------

/// Dispatches `test_pattern.wgsl` to bake a frame-index block code into an
/// Rgba16Float texture. Single-dispatch shape, mirrors `FormatConverter`.
pub struct PatternWriter {
    pipeline: GpuComputePipeline,
}

impl PatternWriter {
    pub fn new(device: &GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/test_pattern.wgsl"),
            "cs_main",
            "Recording Proof Test Pattern",
        );
        Self { pipeline }
    }

    /// Dispatch the pattern write for `frame_index` into `dest` (an
    /// Rgba16Float texture). Call in the same command buffer, before
    /// `LiveRecordingSession::encode_format_conversion` — the harness's
    /// transcription of the production capture sequence
    /// (content_pipeline.rs:2547-2621).
    pub fn encode(&self, encoder: &mut GpuEncoder, dest: &GpuTexture, frame_index: u32) {
        // Params { frame_index: u32, width: u32, _pad0: u32, _pad1: u32 } —
        // packed to a 16-byte multiple (the naga uniform-layout rule; see
        // e.g. node_graph/freeze/diff.rs's DiffParams for the same
        // convention). No bytemuck dependency — plain byte copies.
        let mut params = [0u8; 16];
        params[0..4].copy_from_slice(&frame_index.to_ne_bytes());
        params[4..8].copy_from_slice(&dest.width.to_ne_bytes());

        encoder.dispatch_compute(
            &self.pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: &params,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: dest,
                },
            ],
            [dest.width.div_ceil(16), dest.height.div_ceil(16), 1],
            "Recording Proof Test Pattern",
        );
    }
}

// ---------------------------------------------------------------------
// ffprobe/ffmpeg oracle (D3 — hard-required, no silent skip)
// ---------------------------------------------------------------------

/// Report decoded from an independent ffprobe/ffmpeg pass over a recorded
/// file — the proof suite's oracle, deliberately outside our own code (D3).
#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub video_frame_count: u64,
    pub video_duration_s: f64,
    pub audio_duration_s: Option<f64>,
    /// Video packet PTS, stream order (raw ffprobe integer timescale ticks).
    pub pts: Vec<i64>,
    /// Decoded block-pattern frame indices, stream order. Empty unless
    /// `decode_indices` was passed to [`probe`].
    pub frame_indices: Vec<u32>,
}

/// Probe a recorded file with ffprobe/ffmpeg. Hard-errors (never silently
/// skips) if the tools are missing — see docs/LIVE_RECORDING_PROOFS_DESIGN.md
/// D3 and `feedback_no_silent_fallbacks_or_interim_stopgaps`.
pub fn probe(path: &Path, decode_indices: bool) -> Result<ProbeReport, String> {
    require_tool("ffprobe")?;
    if decode_indices {
        require_tool("ffmpeg")?;
    }

    let streams = probe_streams(path)?;
    let (codec, width, height, video_duration_s) = parse_video_stream(&streams)?;
    let audio_duration_s = parse_audio_duration(&streams);
    let pts = probe_pts(path)?;
    let video_frame_count = pts.len() as u64;

    let frame_indices = if decode_indices {
        decode_frame_indices(path, width, height)?
    } else {
        Vec::new()
    };

    Ok(ProbeReport {
        codec,
        width,
        height,
        video_frame_count,
        video_duration_s,
        audio_duration_s,
        pts,
        frame_indices,
    })
}

fn require_tool(name: &str) -> Result<(), String> {
    let found = Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if found {
        Ok(())
    } else {
        Err(format!(
            "[recording-proofs] `{name}` not found on PATH. The recording proof \
             suite requires ffmpeg/ffprobe as an independent verification oracle \
             (docs/LIVE_RECORDING_PROOFS_DESIGN.md D3) — install with \
             `brew install ffmpeg`."
        ))
    }
}

fn probe_streams(path: &Path) -> Result<serde_json::Value, String> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe spawn failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe -show_streams failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("ffprobe JSON parse failed: {e}"))
}

fn parse_video_stream(streams: &serde_json::Value) -> Result<(String, u32, u32, f64), String> {
    let arr = streams["streams"]
        .as_array()
        .ok_or("ffprobe output has no 'streams' array")?;
    let video = arr
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or("no video stream in ffprobe output")?;

    let codec = video["codec_name"]
        .as_str()
        .ok_or("video stream missing codec_name")?
        .to_string();
    let width = video["width"].as_u64().ok_or("video stream missing width")? as u32;
    let height = video["height"]
        .as_u64()
        .ok_or("video stream missing height")? as u32;
    let duration = video["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or("video stream missing duration")?;

    Ok((codec, width, height, duration))
}

fn parse_audio_duration(streams: &serde_json::Value) -> Option<f64> {
    let arr = streams["streams"].as_array()?;
    let audio = arr.iter().find(|s| s["codec_type"] == "audio")?;
    audio["duration"].as_str().and_then(|s| s.parse::<f64>().ok())
}

fn probe_pts(path: &Path) -> Result<Vec<i64>, String> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe spawn failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe packet=pts failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut pts = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: i64 = line
            .parse()
            .map_err(|e| format!("ffprobe pts parse failed on '{line}': {e}"))?;
        pts.push(value);
    }
    Ok(pts)
}

/// Decode the block pattern back out of the file via ffmpeg rawvideo, one
/// center-pixel sample per block per frame, threshold at 128 — see the
/// pattern spec in `shaders/test_pattern.wgsl`.
fn decode_frame_indices(path: &Path, width: u32, height: u32) -> Result<Vec<u32>, String> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        // -fps_mode passthrough: decode exactly the packets in the file,
        // one rawvideo frame per decoded frame, with NO retiming. Without
        // this, ffmpeg's default CFR frame-rate matching drops/duplicates
        // frames whenever decoded PTS spacing deviates from the container's
        // nominal r_frame_rate — exactly what the adversarial-timing test
        // deliberately produces (empirically verified: default decode of a
        // 600-frame file with a clustered near-duplicate-PTS run produced
        // 720 rawvideo frames instead of 600; passthrough produced exactly
        // 600). This is a decode-side artifact, not an encoder bug — the
        // file's own packet-level PTS are correct and strictly increasing.
        .args([
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ])
        .output()
        .map_err(|e| format!("ffmpeg spawn failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffmpeg rawvideo decode failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let frame_bytes = (width as usize) * (height as usize);
    if frame_bytes == 0 {
        return Err("decode_frame_indices: zero-sized frame".into());
    }
    let block_width = width / NUM_BLOCKS;
    if block_width == 0 {
        return Err("decode_frame_indices: width too small for block pattern".into());
    }

    let data = output.stdout;
    if data.len() % frame_bytes != 0 {
        log::warn!(
            "[recording-proofs] rawvideo decode: {} bytes is not a multiple of \
             frame size {frame_bytes} — trailing partial frame dropped",
            data.len()
        );
    }

    let row = (height / 2) as usize;
    let mut indices = Vec::with_capacity(data.len() / frame_bytes);
    for frame in data.chunks_exact(frame_bytes) {
        let row_start = row * width as usize;
        let mut index: u32 = 0;
        for bit in 0..INDEX_BITS {
            let block = 2 + bit;
            let center_x = (block * block_width + block_width / 2) as usize;
            let px = frame[row_start + center_x];
            let white = px >= 128;
            index = (index << 1) | u32::from(white);
        }
        indices.push(index);
    }
    Ok(indices)
}
