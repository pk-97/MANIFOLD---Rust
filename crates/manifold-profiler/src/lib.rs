//! MANIFOLD profiler — structured performance capture for analysis.
//!
//! Records per-frame timing data from the content thread during real execution.
//! Dumps session data as JSONL (one frame per line) plus a summary JSON file.
//! Designed for machine consumption: Claude reads the output to identify
//! bottlenecks and recommend targeted optimizations.
//!
//! Activated via the backtick (`) key when compiled with the `profiling` feature
//! on manifold-app. Zero runtime cost when not recording.

use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

// ─── Session Metadata ──────────────────────────────────────────────

/// Top-level session info written to session.json.
#[derive(Debug, Clone, Serialize)]
pub struct SessionMetadata {
    pub project_name: String,
    pub project_path: String,
    pub resolution: (u32, u32),
    pub target_fps: f32,
    pub frame_budget_ms: f32,
    pub gpu_name: String,
    pub start_time: String,
    pub duration_seconds: f64,
    pub total_frames: u64,
}

// ─── Per-Frame Data ────────────────────────────────────────────────

/// One frame's worth of profiling data.
#[derive(Debug, Clone, Serialize)]
pub struct FrameRecord {
    pub index: u64,
    pub beat: f32,
    pub bar: u32,
    pub wall_time_ms: f64,
    pub budget_exceeded: bool,
    pub content_thread: ContentTimings,
    pub active_clips: usize,
    pub active_layers: usize,
}

/// CPU timing breakdown for a single content thread tick.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ContentTimings {
    pub total_ms: f64,
    pub midi_input_ms: f64,
    pub sync_controllers_ms: f64,
    pub engine_tick_ms: f64,
    pub render_content_ms: f64,
    pub gpu_poll_ms: f64,
    pub cleanup_ms: f64,
}

// ─── Summary ───────────────────────────────────────────────────────

/// Aggregated statistics computed at dump time.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub frames_over_budget: u64,
    pub worst_frame: Option<WorstFrame>,
    pub mean_frame_ms: f64,
    pub p95_frame_ms: f64,
    pub p99_frame_ms: f64,
    pub max_frame_ms: f64,
    pub phase_aggregates: PhaseAggregates,
    pub hotspots: Vec<Hotspot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorstFrame {
    pub index: u64,
    pub ms: f64,
    pub beat: f32,
    pub bar: u32,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PhaseAggregates {
    pub midi_input: PercentileStat,
    pub sync_controllers: PercentileStat,
    pub engine_tick: PercentileStat,
    pub render_content: PercentileStat,
    pub gpu_poll: PercentileStat,
    pub cleanup: PercentileStat,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PercentileStat {
    pub mean_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hotspot {
    pub beat_range: (f32, f32),
    pub bar_range: (u32, u32),
    pub mean_frame_ms: f64,
    pub frames_over_budget: u64,
    pub total_frames: u64,
}

// ─── Profile Session ───────────────────────────────────────────────

/// Manages a single profiling session. Created when recording starts,
/// collects frame data, dumps to disk when recording stops.
pub struct ProfileSession {
    project_name: String,
    project_path: String,
    resolution: (u32, u32),
    target_fps: f32,
    gpu_name: String,
    frames: Vec<FrameRecord>,
    start_instant: Instant,
    start_time_str: String,
    recording: bool,
}

impl ProfileSession {
    /// Start a new profiling session.
    pub fn new(
        project_name: String,
        project_path: String,
        resolution: (u32, u32),
        target_fps: f32,
        gpu_name: String,
    ) -> Self {
        // ISO 8601 timestamp from system time
        let now = std::time::SystemTime::now();
        let since_epoch = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let secs = since_epoch.as_secs();
        // Simple timestamp: YYYY-MM-DD_HHMMSS (approximate from epoch)
        let start_time_str = format_timestamp(secs);

        // Pre-allocate for ~5 minutes at 60fps
        let capacity = (target_fps as usize * 300).max(1024);

        Self {
            project_name,
            project_path,
            resolution,
            target_fps,
            gpu_name,
            frames: Vec::with_capacity(capacity),
            start_instant: Instant::now(),
            start_time_str,
            recording: true,
        }
    }

    /// Whether this session is actively recording.
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// Number of frames recorded so far.
    pub fn frame_count(&self) -> u64 {
        self.frames.len() as u64
    }

    /// Record a single frame's data. Only call when is_recording() is true.
    pub fn record_frame(&mut self, record: FrameRecord) {
        if self.recording {
            self.frames.push(record);
        }
    }

    /// Stop recording and dump the session to disk.
    /// Returns the output directory path on success.
    pub fn stop_and_dump(&mut self) -> Result<PathBuf, String> {
        self.recording = false;
        let duration = self.start_instant.elapsed().as_secs_f64();

        let metadata = SessionMetadata {
            project_name: self.project_name.clone(),
            project_path: self.project_path.clone(),
            resolution: self.resolution,
            target_fps: self.target_fps,
            frame_budget_ms: 1000.0 / self.target_fps,
            gpu_name: self.gpu_name.clone(),
            start_time: self.start_time_str.clone(),
            duration_seconds: duration,
            total_frames: self.frames.len() as u64,
        };

        let summary = self.compute_summary(&metadata);
        self.write_output(&metadata, &summary)
    }

    /// Compute aggregated summary from recorded frames.
    fn compute_summary(&self, metadata: &SessionMetadata) -> SessionSummary {
        if self.frames.is_empty() {
            return SessionSummary {
                frames_over_budget: 0,
                worst_frame: None,
                mean_frame_ms: 0.0,
                p95_frame_ms: 0.0,
                p99_frame_ms: 0.0,
                max_frame_ms: 0.0,
                phase_aggregates: PhaseAggregates::default(),
                hotspots: Vec::new(),
            };
        }

        let budget = metadata.frame_budget_ms as f64;
        let n = self.frames.len();

        // Collect wall times for percentile calculation
        let mut wall_times: Vec<f64> = self.frames.iter().map(|f| f.wall_time_ms).collect();
        wall_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let frames_over_budget = self.frames.iter().filter(|f| f.wall_time_ms > budget).count() as u64;

        let worst = self.frames.iter().max_by(|a, b| {
            a.wall_time_ms.partial_cmp(&b.wall_time_ms).unwrap_or(std::cmp::Ordering::Equal)
        });

        let worst_frame = worst.map(|f| WorstFrame {
            index: f.index,
            ms: f.wall_time_ms,
            beat: f.beat,
            bar: f.bar,
        });

        // Phase aggregates
        let phase_aggregates = PhaseAggregates {
            midi_input: percentile_stat(&self.frames, |f| f.content_thread.midi_input_ms),
            sync_controllers: percentile_stat(&self.frames, |f| f.content_thread.sync_controllers_ms),
            engine_tick: percentile_stat(&self.frames, |f| f.content_thread.engine_tick_ms),
            render_content: percentile_stat(&self.frames, |f| f.content_thread.render_content_ms),
            gpu_poll: percentile_stat(&self.frames, |f| f.content_thread.gpu_poll_ms),
            cleanup: percentile_stat(&self.frames, |f| f.content_thread.cleanup_ms),
        };

        // Hotspot detection: find contiguous bar ranges where >50% of frames exceed budget
        let hotspots = self.detect_hotspots(budget);

        SessionSummary {
            frames_over_budget,
            worst_frame,
            mean_frame_ms: wall_times.iter().sum::<f64>() / n as f64,
            p95_frame_ms: percentile_value(&wall_times, 0.95),
            p99_frame_ms: percentile_value(&wall_times, 0.99),
            max_frame_ms: wall_times.last().copied().unwrap_or(0.0),
            phase_aggregates,
            hotspots,
        }
    }

    /// Detect timeline regions where performance is consistently bad.
    fn detect_hotspots(&self, budget_ms: f64) -> Vec<Hotspot> {
        if self.frames.is_empty() {
            return Vec::new();
        }

        // Group frames by bar
        let mut bar_frames: Vec<(u32, Vec<&FrameRecord>)> = Vec::new();
        for frame in &self.frames {
            if let Some(last) = bar_frames.last_mut()
                && last.0 == frame.bar
            {
                last.1.push(frame);
                continue;
            }
            bar_frames.push((frame.bar, vec![frame]));
        }

        // Find bars where >50% of frames exceed budget
        let mut hotspots = Vec::new();
        let mut current_hotspot: Option<(u32, u32, Vec<&FrameRecord>)> = None;

        for (bar, frames) in &bar_frames {
            let over = frames.iter().filter(|f| f.wall_time_ms > budget_ms).count();
            let is_hot = over as f64 / frames.len() as f64 > 0.5;

            if is_hot {
                if let Some(ref mut h) = current_hotspot {
                    h.1 = *bar;
                    h.2.extend(frames.iter());
                } else {
                    current_hotspot = Some((*bar, *bar, frames.clone()));
                }
            } else if let Some(h) = current_hotspot.take() {
                let beat_min = h.2.iter().map(|f| f.beat).fold(f32::MAX, f32::min);
                let beat_max = h.2.iter().map(|f| f.beat).fold(f32::MIN, f32::max);
                let mean = h.2.iter().map(|f| f.wall_time_ms).sum::<f64>() / h.2.len() as f64;
                let over_count = h.2.iter().filter(|f| f.wall_time_ms > budget_ms).count() as u64;
                hotspots.push(Hotspot {
                    beat_range: (beat_min, beat_max),
                    bar_range: (h.0, h.1),
                    mean_frame_ms: mean,
                    frames_over_budget: over_count,
                    total_frames: h.2.len() as u64,
                });
            }
        }

        // Flush any trailing hotspot
        if let Some(h) = current_hotspot {
            let beat_min = h.2.iter().map(|f| f.beat).fold(f32::MAX, f32::min);
            let beat_max = h.2.iter().map(|f| f.beat).fold(f32::MIN, f32::max);
            let mean = h.2.iter().map(|f| f.wall_time_ms).sum::<f64>() / h.2.len() as f64;
            let over_count = h.2.iter().filter(|f| f.wall_time_ms > budget_ms).count() as u64;
            hotspots.push(Hotspot {
                beat_range: (beat_min, beat_max),
                bar_range: (h.0, h.1),
                mean_frame_ms: mean,
                frames_over_budget: over_count,
                total_frames: h.2.len() as u64,
            });
        }

        hotspots
    }

    /// Write session.json, frames.jsonl, and summary.json to disk.
    fn write_output(
        &self,
        metadata: &SessionMetadata,
        summary: &SessionSummary,
    ) -> Result<PathBuf, String> {
        // Output directory: profiling_sessions/<timestamp>_<project_name>/
        let sanitized_name = self.project_name.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
        let dir_name = format!("{}_{}", self.start_time_str, sanitized_name);
        let output_dir = PathBuf::from("profiling_sessions").join(dir_name);

        std::fs::create_dir_all(&output_dir)
            .map_err(|e| format!("Failed to create output directory: {}", e))?;

        // session.json
        let session_path = output_dir.join("session.json");
        let session_json = serde_json::to_string_pretty(metadata)
            .map_err(|e| format!("Failed to serialize session metadata: {}", e))?;
        std::fs::write(&session_path, session_json)
            .map_err(|e| format!("Failed to write session.json: {}", e))?;

        // summary.json
        let summary_path = output_dir.join("summary.json");
        let summary_json = serde_json::to_string_pretty(summary)
            .map_err(|e| format!("Failed to serialize summary: {}", e))?;
        std::fs::write(&summary_path, summary_json)
            .map_err(|e| format!("Failed to write summary.json: {}", e))?;

        // frames.jsonl — one JSON object per line
        let frames_path = output_dir.join("frames.jsonl");
        let mut file = std::io::BufWriter::new(
            std::fs::File::create(&frames_path)
                .map_err(|e| format!("Failed to create frames.jsonl: {}", e))?,
        );
        for frame in &self.frames {
            let line = serde_json::to_string(frame)
                .map_err(|e| format!("Failed to serialize frame: {}", e))?;
            writeln!(file, "{}", line)
                .map_err(|e| format!("Failed to write frame line: {}", e))?;
        }
        file.flush().map_err(|e| format!("Failed to flush frames.jsonl: {}", e))?;

        Ok(output_dir)
    }
}

// ─── Helpers ───────────────────────────────────────────────────────

/// Compute percentile statistics for a given field extractor.
fn percentile_stat(frames: &[FrameRecord], extract: impl Fn(&FrameRecord) -> f64) -> PercentileStat {
    if frames.is_empty() {
        return PercentileStat::default();
    }
    let mut values: Vec<f64> = frames.iter().map(&extract).collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    PercentileStat {
        mean_ms: values.iter().sum::<f64>() / values.len() as f64,
        p95_ms: percentile_value(&values, 0.95),
        p99_ms: percentile_value(&values, 0.99),
        max_ms: values.last().copied().unwrap_or(0.0),
    }
}

/// Get the value at a given percentile from a sorted slice.
fn percentile_value(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 * pct) as usize).min(sorted.len() - 1);
    sorted[idx]
}

/// Format a unix timestamp as YYYY-MM-DD_HHMMSS.
fn format_timestamp(epoch_secs: u64) -> String {
    // Simple epoch → date conversion (no chrono dependency)
    let days = epoch_secs / 86400;
    let time_of_day = epoch_secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since 1970-01-01 → year/month/day
    let (year, month, day) = days_to_date(days);
    format!("{:04}-{:02}-{:02}_{:02}{:02}{:02}", year, month, day, hours, minutes, seconds)
}

/// Convert days since epoch to (year, month, day).
fn days_to_date(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let months = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1;
    for m in months {
        if days < m {
            break;
        }
        days -= m;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}
