//! MANIFOLD profiler — structured performance capture for analysis.
//!
//! Records per-frame timing data from the content thread during real execution.
//! Dumps session data as JSONL (one frame per line) plus a summary JSON file.
//! Designed for machine consumption: Claude reads the output to identify
//! bottlenecks and recommend targeted optimizations.
//!
//! Activated via the backtick (`) key when compiled with the `profiling` feature
//! on manifold-app. Zero runtime cost when not recording.

pub mod compare;

use serde::{Deserialize, Serialize};
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
    /// Per-pass GPU timing from timestamp queries (empty if unavailable).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gpu_passes: Vec<GpuPassRecord>,
    /// Active clips this frame with generator type info.
    pub active_clips: Vec<ActiveClipInfo>,
    /// Active effects this frame with type and parameter info.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub active_effects: Vec<ActiveEffectInfo>,
    pub active_layer_count: usize,
    /// Total GPU passes this frame.
    pub gpu_pass_count: u32,
    /// Sum of all GPU pass durations (ms).
    pub gpu_total_ms: f64,
    /// Per-layer state (opacity, mute, solo).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layer_states: Vec<LayerState>,
    /// Number of content thread ticks missed (frame drops).
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub missed_frames: u64,
    /// Profiler buffer readback overhead in ms.
    pub profiler_overhead_ms: f64,
    /// GPU memory estimate for this frame.
    pub memory: MemorySnapshot,
}

fn is_zero_u64(v: &u64) -> bool { *v == 0 }

/// GPU pass timing from wgpu timestamp queries.
#[derive(Debug, Clone, Serialize)]
pub struct GpuPassRecord {
    pub name: String,
    pub ms: f64,
    /// Absolute begin timestamp in nanoseconds (frame-relative GPU clock).
    pub begin_ns: f64,
    /// Absolute end timestamp in nanoseconds (frame-relative GPU clock).
    pub end_ns: f64,
    /// Output texture width (0 if unknown).
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub width: u32,
    /// Output texture height (0 if unknown).
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub height: u32,
    /// Whether this is a compute pass (vs render).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_compute: bool,
}

/// Named parameter with human-readable name.
#[derive(Debug, Clone, Serialize)]
pub struct NamedParam {
    pub name: String,
    pub value: f32,
}

/// Info about an active clip in this frame.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveClipInfo {
    pub clip_id: String,
    pub generator_type: String,
    pub layer_index: i32,
    /// Animation progress [0..1] within the clip.
    pub anim_progress: f32,
    /// Live modulated generator parameters with names.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gen_params: Vec<NamedParam>,
}

/// Info about an active effect in this frame.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveEffectInfo {
    pub effect_type: String,
    /// "clip:<clip_id>", "layer:<index>", or "master"
    pub scope: String,
    /// Effect group ID (for wet/dry grouping).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Live modulated parameters with names.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<NamedParam>,
}

/// Per-layer state snapshot for a single frame.
#[derive(Debug, Clone, Serialize)]
pub struct LayerState {
    pub index: i32,
    pub opacity: f32,
    pub is_muted: bool,
    pub is_solo: bool,
}

fn is_zero_u32(v: &u32) -> bool { *v == 0 }

// ─── Timeline Snapshot ─────────────────────────────────────────────

/// Project timeline structure dumped at session start for cross-referencing.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineSnapshot {
    pub bpm: f32,
    pub time_signature: i32,
    pub resolution: (u32, u32),
    pub layers: Vec<LayerSnapshot>,
    pub master_effects: Vec<EffectSnapshot>,
}

/// Layer state at session start.
#[derive(Debug, Clone, Serialize)]
pub struct LayerSnapshot {
    pub index: i32,
    pub generator_type: String,
    pub blend_mode: String,
    pub is_muted: bool,
    pub clips: Vec<ClipSnapshot>,
    pub effects: Vec<EffectSnapshot>,
}

/// Clip state at session start.
#[derive(Debug, Clone, Serialize)]
pub struct ClipSnapshot {
    pub id: String,
    pub start_beat: f32,
    pub duration_beats: f32,
    pub generator_type: String,
    pub effect_count: usize,
}

/// Effect state at session start.
#[derive(Debug, Clone, Serialize)]
pub struct EffectSnapshot {
    pub effect_type: String,
    pub enabled: bool,
}

/// GPU memory snapshot for a frame.
#[derive(Debug, Clone, Serialize, Default)]
pub struct MemorySnapshot {
    pub estimated_texture_bytes: u64,
    pub render_target_count: u32,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub frames_over_budget: u64,
    pub worst_frame: Option<WorstFrame>,
    pub mean_frame_ms: f64,
    pub p95_frame_ms: f64,
    pub p99_frame_ms: f64,
    pub max_frame_ms: f64,
    pub phase_aggregates: PhaseAggregates,
    /// Per-GPU-pass aggregated stats (e.g. "generator:fluid_sim" → mean/p95/max).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gpu_pass_aggregates: Vec<GpuPassAggregate>,
    pub hotspots: Vec<Hotspot>,

    // ── Extended analysis ─────────────────────────────────────────
    /// Frame pacing / jitter analysis.
    pub jitter: JitterAnalysis,
    /// Thermal degradation detection (first 10% vs last 10%).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thermal: Option<ThermalAnalysis>,
    /// First-use spike detection (shader compilation).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub first_use_spikes: Vec<FirstUseSpike>,
    /// Idle (0 clips) vs active (1+ clips) comparison.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_vs_active: Option<IdleActiveComparison>,
    /// GPU pass count stats.
    pub pass_count: PassCountStats,
    /// Automated actionable recommendations.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recommendations: Vec<String>,
}

/// Frame pacing / jitter analysis.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JitterAnalysis {
    pub mean_dt_ms: f64,
    pub stddev_dt_ms: f64,
    /// Coefficient of variation (stddev / mean). >0.3 = high jitter.
    pub coefficient_of_variation: f64,
    /// Frames where dt > 1.5x target frame time.
    pub frames_with_significant_jitter: u64,
}

/// Thermal throttling detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalAnalysis {
    pub first_10pct_mean_ms: f64,
    pub last_10pct_mean_ms: f64,
    /// last / first. >1.15 = likely throttled.
    pub degradation_ratio: f64,
    pub likely_throttled: bool,
}

/// Shader compilation spike on first use of a GPU pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirstUseSpike {
    pub pass_name: String,
    pub first_use_frame: u64,
    pub first_use_ms: f64,
    /// Mean excluding the first occurrence.
    pub steady_state_mean_ms: f64,
    pub spike_ratio: f64,
}

/// Baseline (idle) vs loaded (active) comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleActiveComparison {
    pub idle_mean_ms: f64,
    pub active_mean_ms: f64,
    pub overhead_ms: f64,
    pub idle_frame_count: u64,
    pub active_frame_count: u64,
}

/// GPU pass count statistics.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PassCountStats {
    pub mean_pass_count: f64,
    pub max_pass_count: u32,
    pub mean_gpu_total_ms: f64,
    /// mean_gpu_total / frame_budget * 100.
    pub gpu_budget_usage_pct: f64,
}

/// Aggregated GPU timing for a specific pass label across all frames.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuPassAggregate {
    pub name: String,
    pub mean_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub frame_count: u64,
    /// Frame index where this pass first appeared.
    pub first_seen_frame: u64,
    /// Mean excluding the first occurrence (steady-state performance).
    pub steady_state_mean_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorstFrame {
    pub index: u64,
    pub ms: f64,
    pub beat: f32,
    pub bar: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PhaseAggregates {
    pub midi_input: PercentileStat,
    pub sync_controllers: PercentileStat,
    pub engine_tick: PercentileStat,
    pub render_content: PercentileStat,
    pub gpu_poll: PercentileStat,
    pub cleanup: PercentileStat,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PercentileStat {
    pub mean_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    timeline_snapshot: Option<TimelineSnapshot>,
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
            timeline_snapshot: None,
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

    /// Set the timeline snapshot (called once at session start).
    pub fn set_timeline_snapshot(&mut self, snapshot: TimelineSnapshot) {
        self.timeline_snapshot = Some(snapshot);
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
                gpu_pass_aggregates: Vec::new(),
                hotspots: Vec::new(),
                jitter: JitterAnalysis::default(),
                thermal: None,
                first_use_spikes: Vec::new(),
                idle_vs_active: None,
                pass_count: PassCountStats::default(),
                recommendations: Vec::new(),
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

        // GPU pass aggregates — group by label, compute stats
        let gpu_pass_aggregates = self.compute_gpu_pass_aggregates();

        let mean_frame = wall_times.iter().sum::<f64>() / n as f64;

        // Jitter analysis
        let jitter = self.compute_jitter(budget);

        // Thermal degradation
        let thermal = self.compute_thermal();

        // First-use spikes
        let first_use_spikes = self.detect_first_use_spikes(&gpu_pass_aggregates);

        // Idle vs active comparison
        let idle_vs_active = self.compute_idle_vs_active();

        // Pass count stats
        let pass_count = self.compute_pass_count_stats(budget);

        // Automated recommendations
        let recommendations = self.generate_recommendations(
            &gpu_pass_aggregates, &jitter, thermal.as_ref(),
            idle_vs_active.as_ref(), &pass_count, budget,
        );

        SessionSummary {
            frames_over_budget,
            worst_frame,
            mean_frame_ms: mean_frame,
            p95_frame_ms: percentile_value(&wall_times, 0.95),
            p99_frame_ms: percentile_value(&wall_times, 0.99),
            max_frame_ms: wall_times.last().copied().unwrap_or(0.0),
            phase_aggregates,
            gpu_pass_aggregates,
            hotspots,
            jitter,
            thermal,
            first_use_spikes,
            idle_vs_active,
            pass_count,
            recommendations,
        }
    }

    /// Compute per-GPU-pass aggregated statistics across all frames.
    fn compute_gpu_pass_aggregates(&self) -> Vec<GpuPassAggregate> {
        // Collect timings + first-seen frame grouped by label
        struct PassData {
            times: Vec<f64>,
            first_seen_frame: u64,
        }
        let mut by_label: std::collections::HashMap<String, PassData> =
            std::collections::HashMap::new();
        for frame in &self.frames {
            for pass in &frame.gpu_passes {
                let entry = by_label.entry(pass.name.clone()).or_insert_with(|| PassData {
                    times: Vec::new(),
                    first_seen_frame: frame.index,
                });
                entry.times.push(pass.ms);
            }
        }

        let mut aggregates: Vec<GpuPassAggregate> = by_label
            .into_iter()
            .map(|(name, mut data)| {
                data.times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let n = data.times.len();
                let steady_state_mean = if n > 1 {
                    // Exclude first occurrence for steady-state mean
                    let first_ms = data.times.first().copied().unwrap_or(0.0);
                    let total: f64 = data.times.iter().sum();
                    (total - first_ms) / (n - 1) as f64
                } else {
                    data.times.first().copied().unwrap_or(0.0)
                };
                GpuPassAggregate {
                    name,
                    mean_ms: data.times.iter().sum::<f64>() / n as f64,
                    p95_ms: percentile_value(&data.times, 0.95),
                    p99_ms: percentile_value(&data.times, 0.99),
                    max_ms: data.times.last().copied().unwrap_or(0.0),
                    frame_count: n as u64,
                    first_seen_frame: data.first_seen_frame,
                    steady_state_mean_ms: steady_state_mean,
                }
            })
            .collect();

        // Sort by mean_ms descending (most expensive first)
        aggregates
            .sort_by(|a, b| b.mean_ms.partial_cmp(&a.mean_ms).unwrap_or(std::cmp::Ordering::Equal));
        aggregates
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

    /// Frame pacing / jitter analysis.
    fn compute_jitter(&self, budget_ms: f64) -> JitterAnalysis {
        if self.frames.len() < 2 {
            return JitterAnalysis::default();
        }
        let times: Vec<f64> = self.frames.iter().map(|f| f.wall_time_ms).collect();
        let n = times.len() as f64;
        let mean = times.iter().sum::<f64>() / n;
        let variance = times.iter().map(|t| (t - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let stddev = variance.sqrt();
        let cv = if mean > 0.0 { stddev / mean } else { 0.0 };
        let jitter_threshold = budget_ms * 1.5;
        let jitter_count = times.iter().filter(|&&t| t > jitter_threshold).count() as u64;
        JitterAnalysis {
            mean_dt_ms: mean,
            stddev_dt_ms: stddev,
            coefficient_of_variation: cv,
            frames_with_significant_jitter: jitter_count,
        }
    }

    /// Thermal degradation: compare first 10% vs last 10% of session.
    fn compute_thermal(&self) -> Option<ThermalAnalysis> {
        if self.frames.len() < 20 {
            return None;
        }
        let n = self.frames.len();
        let chunk = (n / 10).max(1);
        let first_mean = self.frames[..chunk].iter()
            .map(|f| f.wall_time_ms).sum::<f64>() / chunk as f64;
        let last_mean = self.frames[n - chunk..].iter()
            .map(|f| f.wall_time_ms).sum::<f64>() / chunk as f64;
        let ratio = if first_mean > 0.0 { last_mean / first_mean } else { 1.0 };
        Some(ThermalAnalysis {
            first_10pct_mean_ms: first_mean,
            last_10pct_mean_ms: last_mean,
            degradation_ratio: ratio,
            likely_throttled: ratio > 1.15,
        })
    }

    /// Detect first-use spikes (shader compilation on first occurrence).
    fn detect_first_use_spikes(&self, aggregates: &[GpuPassAggregate]) -> Vec<FirstUseSpike> {
        let mut spikes = Vec::new();
        for agg in aggregates {
            if agg.frame_count < 2 { continue; }
            // Find the first occurrence timing
            let first_ms = self.frames.iter()
                .find_map(|f| f.gpu_passes.iter()
                    .find(|p| p.name == agg.name)
                    .map(|p| p.ms))
                .unwrap_or(0.0);
            let ratio = if agg.steady_state_mean_ms > 0.001 {
                first_ms / agg.steady_state_mean_ms
            } else {
                1.0
            };
            if ratio > 5.0 {
                spikes.push(FirstUseSpike {
                    pass_name: agg.name.clone(),
                    first_use_frame: agg.first_seen_frame,
                    first_use_ms: first_ms,
                    steady_state_mean_ms: agg.steady_state_mean_ms,
                    spike_ratio: ratio,
                });
            }
        }
        spikes.sort_by(|a, b| b.spike_ratio.partial_cmp(&a.spike_ratio)
            .unwrap_or(std::cmp::Ordering::Equal));
        spikes
    }

    /// Idle (0 active clips) vs active (1+ clips) comparison.
    fn compute_idle_vs_active(&self) -> Option<IdleActiveComparison> {
        let idle: Vec<f64> = self.frames.iter()
            .filter(|f| f.active_clips.is_empty())
            .map(|f| f.wall_time_ms)
            .collect();
        let active: Vec<f64> = self.frames.iter()
            .filter(|f| !f.active_clips.is_empty())
            .map(|f| f.wall_time_ms)
            .collect();
        if idle.is_empty() || active.is_empty() {
            return None;
        }
        let idle_mean = idle.iter().sum::<f64>() / idle.len() as f64;
        let active_mean = active.iter().sum::<f64>() / active.len() as f64;
        Some(IdleActiveComparison {
            idle_mean_ms: idle_mean,
            active_mean_ms: active_mean,
            overhead_ms: active_mean - idle_mean,
            idle_frame_count: idle.len() as u64,
            active_frame_count: active.len() as u64,
        })
    }

    /// GPU pass count statistics.
    fn compute_pass_count_stats(&self, budget_ms: f64) -> PassCountStats {
        if self.frames.is_empty() {
            return PassCountStats::default();
        }
        let counts: Vec<u32> = self.frames.iter()
            .map(|f| f.gpu_passes.len() as u32)
            .collect();
        let totals: Vec<f64> = self.frames.iter()
            .map(|f| f.gpu_passes.iter().map(|p| p.ms).sum::<f64>())
            .collect();
        let n = counts.len() as f64;
        let mean_count = counts.iter().map(|&c| c as f64).sum::<f64>() / n;
        let max_count = counts.iter().copied().max().unwrap_or(0);
        let mean_total = totals.iter().sum::<f64>() / n;
        PassCountStats {
            mean_pass_count: mean_count,
            max_pass_count: max_count,
            mean_gpu_total_ms: mean_total,
            gpu_budget_usage_pct: if budget_ms > 0.0 {
                mean_total / budget_ms * 100.0
            } else {
                0.0
            },
        }
    }

    /// Generate automated actionable recommendations.
    fn generate_recommendations(
        &self,
        gpu_passes: &[GpuPassAggregate],
        jitter: &JitterAnalysis,
        thermal: Option<&ThermalAnalysis>,
        idle_active: Option<&IdleActiveComparison>,
        pass_count: &PassCountStats,
        budget_ms: f64,
    ) -> Vec<String> {
        let mut recs = Vec::new();

        // Most expensive always-on effects (present in >80% of active frames)
        let active_frame_count = self.frames.iter()
            .filter(|f| !f.active_clips.is_empty()).count() as u64;
        if active_frame_count > 0 {
            for pass in gpu_passes.iter().take(5) {
                let usage_pct = pass.frame_count as f64 / active_frame_count as f64 * 100.0;
                if usage_pct > 80.0 && pass.mean_ms > 0.5 {
                    recs.push(format!(
                        "{} is the most expensive always-on pass ({:.2}ms mean, active {:.0}% of frames). \
                         Consider half-resolution intermediate buffers.",
                        pass.name, pass.mean_ms, usage_pct
                    ));
                }
            }
        }

        // High variance passes
        for pass in gpu_passes {
            if pass.max_ms > pass.mean_ms * 10.0 && pass.mean_ms > 0.1 {
                recs.push(format!(
                    "{} has {:.0}x variance (mean {:.2}ms, max {:.2}ms). \
                     Investigate spike at frame {}.",
                    pass.name, pass.max_ms / pass.mean_ms,
                    pass.mean_ms, pass.max_ms, pass.first_seen_frame
                ));
            }
        }

        // Jitter warning
        if jitter.coefficient_of_variation > 0.3 {
            recs.push(format!(
                "High frame time jitter (CV={:.2}). {} frames exceed 1.5x budget. \
                 Check for allocation spikes or GC pauses.",
                jitter.coefficient_of_variation,
                jitter.frames_with_significant_jitter
            ));
        }

        // Thermal
        if let Some(t) = thermal
            && t.likely_throttled
        {
            recs.push(format!(
                "Thermal throttling detected: {:.0}% slower by end of session \
                 (first 10%: {:.1}ms, last 10%: {:.1}ms).",
                (t.degradation_ratio - 1.0) * 100.0,
                t.first_10pct_mean_ms, t.last_10pct_mean_ms
            ));
        }

        // Pass count overhead
        if pass_count.mean_pass_count > 20.0 {
            recs.push(format!(
                "{:.0} GPU passes per frame on average. Each pass incurs barrier/scheduling \
                 overhead. Consider combining passes or reducing active effects.",
                pass_count.mean_pass_count
            ));
        }

        // Budget usage
        if pass_count.gpu_budget_usage_pct > 80.0 {
            recs.push(format!(
                "GPU pass time uses {:.0}% of frame budget ({:.2}ms / {:.2}ms). \
                 Little headroom for additional effects.",
                pass_count.gpu_budget_usage_pct,
                pass_count.mean_gpu_total_ms, budget_ms
            ));
        }

        // Idle vs active delta
        if let Some(ia) = idle_active
            && ia.overhead_ms > budget_ms * 0.8
        {
            recs.push(format!(
                "Rendering overhead is {:.1}ms (idle: {:.1}ms, active: {:.1}ms). \
                 Active content alone nearly exceeds the {:.1}ms budget.",
                ia.overhead_ms, ia.idle_mean_ms, ia.active_mean_ms, budget_ms
            ));
        }

        recs
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

        // timeline.json (if snapshot available)
        if let Some(ref snapshot) = self.timeline_snapshot {
            let timeline_json = serde_json::to_string_pretty(snapshot)
                .map_err(|e| format!("Failed to serialize timeline: {}", e))?;
            std::fs::write(output_dir.join("timeline.json"), timeline_json)
                .map_err(|e| format!("Failed to write timeline.json: {}", e))?;
        }

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
