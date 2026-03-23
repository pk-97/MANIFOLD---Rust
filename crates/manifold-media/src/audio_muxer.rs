//! FFmpeg-based audio post-muxing.
//!
//! After the Metal encoder writes a video-only MP4, this module uses FFmpeg
//! to mux an audio track with correct timing offset. Video is stream-copied
//! (no re-encode), audio is encoded to AAC 256kbps.
//!
//! Matches Unity VideoExporter.PostMuxAudio().

use std::fmt;
use std::path::Path;
use std::process::Command;

/// Errors from audio muxing.
#[derive(Debug)]
pub enum MuxError {
    /// FFmpeg binary not found at the given path.
    FfmpegNotFound(String),
    /// FFmpeg process failed to start.
    ProcessStart(std::io::Error),
    /// FFmpeg exited with non-zero code.
    FfmpegFailed { exit_code: i32, stderr: String },
    /// Audio file does not exist.
    AudioNotFound(String),
    /// Video file does not exist.
    VideoNotFound(String),
}

impl fmt::Display for MuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FfmpegNotFound(p) => write!(f, "ffmpeg not found at: {p}"),
            Self::ProcessStart(e) => write!(f, "failed to start ffmpeg: {e}"),
            Self::FfmpegFailed { exit_code, stderr } => {
                write!(f, "ffmpeg exited with code {exit_code}: {stderr}")
            }
            Self::AudioNotFound(p) => write!(f, "audio file not found: {p}"),
            Self::VideoNotFound(p) => write!(f, "video file not found: {p}"),
        }
    }
}

/// Audio muxer using FFmpeg subprocess.
pub struct AudioMuxer;

impl AudioMuxer {
    /// Post-mux audio into a video file.
    ///
    /// Stream-copies the video track (no re-encode) and encodes audio to AAC 256kbps.
    /// Uses `-itsoffset` for audio timing alignment.
    ///
    /// Matches Unity VideoExporter.PostMuxAudio() command construction.
    pub fn mux(
        ffmpeg_path: &str,
        video_path: &str,
        audio_path: &str,
        output_path: &str,
        audio_offset_seconds: f32,
    ) -> Result<(), MuxError> {
        if !Path::new(ffmpeg_path).exists() {
            return Err(MuxError::FfmpegNotFound(ffmpeg_path.to_string()));
        }
        if !Path::new(video_path).exists() {
            return Err(MuxError::VideoNotFound(video_path.to_string()));
        }
        if !Path::new(audio_path).exists() {
            return Err(MuxError::AudioNotFound(audio_path.to_string()));
        }

        log::info!(
            "[AudioMuxer] Muxing audio: offset={:.3}s, video={}, audio={} -> {}",
            audio_offset_seconds,
            video_path,
            audio_path,
            output_path,
        );

        // Build FFmpeg command matching Unity's PostMuxAudio():
        //   ffmpeg -y -i VIDEO -itsoffset OFFSET -i AUDIO
        //     -c:v copy -c:a aac -b:a 256k
        //     -map 0:v -map 1:a -shortest -movflags +faststart OUTPUT
        let mut cmd = Command::new(ffmpeg_path);
        cmd.arg("-y") // Overwrite output
            .arg("-i")
            .arg(video_path);

        // Apply audio offset
        if audio_offset_seconds.abs() > 0.001 {
            cmd.arg("-itsoffset")
                .arg(format!("{:.6}", audio_offset_seconds));
        }

        cmd.arg("-i")
            .arg(audio_path)
            .arg("-c:v")
            .arg("copy") // Stream-copy video (no re-encode)
            .arg("-c:a")
            .arg("aac")
            .arg("-b:a")
            .arg("256k")
            .arg("-map")
            .arg("0:v")
            .arg("-map")
            .arg("1:a")
            .arg("-shortest")
            .arg("-movflags")
            .arg("+faststart")
            .arg(output_path);

        let output = cmd.output().map_err(MuxError::ProcessStart)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);
            log::error!(
                "[AudioMuxer] FFmpeg failed (exit {}): {}",
                exit_code,
                stderr,
            );
            return Err(MuxError::FfmpegFailed { exit_code, stderr });
        }

        log::info!("[AudioMuxer] Audio mux complete -> {}", output_path);
        Ok(())
    }

    /// Resolve FFmpeg binary path.
    ///
    /// Search order (matches percussion_backend.rs pattern):
    /// 1. FFMPEG_PATH environment variable
    /// 2. Bundled runtime paths
    /// 3. System paths (Homebrew, /usr/local, /usr)
    pub fn resolve_ffmpeg(runtime_root: &str) -> Option<String> {
        // 1. Env var escape hatch
        if let Ok(p) = std::env::var("FFMPEG_PATH") {
            let p = p.trim().to_string();
            if !p.is_empty() && Path::new(&p).exists() {
                return Some(p);
            }
        }

        // 2. Bundled runtime paths
        let bundled = [
            Path::new(runtime_root).join("bin").join("ffmpeg"),
            Path::new(runtime_root).join("ffmpeg"),
        ];
        for candidate in &bundled {
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }

        // 3. System fallback
        let system = [
            "/opt/homebrew/bin/ffmpeg",
            "/usr/local/bin/ffmpeg",
            "/usr/bin/ffmpeg",
        ];
        for candidate in &system {
            if Path::new(candidate).exists() {
                return Some(candidate.to_string());
            }
        }

        None
    }
}
