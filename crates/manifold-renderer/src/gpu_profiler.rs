//! GPU pass-level profiler using wgpu TimestampWrites.
//!
//! Provides hardware-accurate per-pass GPU timing by injecting `TimestampWrites`
//! directly into `RenderPassDescriptor` and `ComputePassDescriptor`. This measures
//! actual GPU execution time (shader work + memory bandwidth + barriers), not
//! command stream position.
//!
//! Uses `Cell<u32>` + `RefCell<Vec>` for interior mutability so only `&self`
//! (shared reference) is needed throughout the rendering pipeline.
//!
//! Usage:
//!   profiler.begin_frame();
//!   let ts = profiler.render_timestamps("Bloom Blur H", 1920, 1080);
//!   encoder.begin_render_pass(&RenderPassDescriptor {
//!       timestamp_writes: ts.as_ref(),
//!       ...
//!   });
//!   profiler.resolve(&mut encoder);
//!   queue.submit(encoder.finish());
//!   device.poll(wait);
//!   let results = profiler.read_results(&device);

use std::cell::{Cell, RefCell};

/// Maximum number of timestamp pairs (begin + end) per frame.
/// 256 pairs = 512 queries. Covers complex scenes: ~20 generators (some with
/// 10+ passes each) + ~30 effects + compositor blend/tonemap + overhead.
const MAX_TIMESTAMP_PAIRS: u32 = 256;
const MAX_QUERIES: u32 = MAX_TIMESTAMP_PAIRS * 2;

/// A single timed GPU pass with metadata.
struct ProfileEntry {
    label: String,
    begin_query: u32,
    end_query: u32,
    width: u32,
    height: u32,
    is_compute: bool,
}

/// Result of a single GPU pass timing measurement.
#[derive(Debug, Clone)]
pub struct GpuPassTiming {
    pub label: String,
    pub duration_ms: f64,
    /// Absolute begin timestamp in nanoseconds (GPU clock, frame-relative).
    pub begin_ns: f64,
    /// Absolute end timestamp in nanoseconds (GPU clock, frame-relative).
    pub end_ns: f64,
    pub width: u32,
    pub height: u32,
    pub is_compute: bool,
}

/// Manages GPU timestamp queries for per-pass profiling.
///
/// Uses interior mutability (`Cell`/`RefCell`) so only `&self` is needed.
/// The content thread creates one of these and reuses it across frames.
///
/// Per-frame flow:
/// 1. `begin_frame()` — reset counters (requires `&mut self`, called outside render)
/// 2. `render_timestamps()` / `compute_timestamps()` — allocate query pairs (`&self`)
/// 3. `resolve()` — resolve queries into readback buffer (`&self`)
/// 4. After `device.poll()`: `read_results()` — map buffer and compute durations (`&mut self`)
pub struct GpuProfiler {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    timestamp_period: f32,
    entries: RefCell<Vec<ProfileEntry>>,
    next_query: Cell<u32>,
    /// GPU adapter name (e.g. "Apple M2 Max").
    adapter_name: String,
    /// Duration of the last read_results() call in ms (profiler self-overhead).
    last_readback_ms: Cell<f64>,
    /// Label prefix prepended to all subsequent timestamp labels (e.g. "master:" or "clip:abc123:").
    /// Set by the orchestration layer (effect_chain, compositor) to scope passes.
    scope_prefix: RefCell<String>,
}

impl GpuProfiler {
    /// Create a new GPU profiler. Returns None if the adapter doesn't support
    /// timestamp queries.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, adapter: &wgpu::Adapter) -> Option<Self> {
        if !adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            log::warn!("[GpuProfiler] adapter does not support TIMESTAMP_QUERY");
            return None;
        }

        let timestamp_period = queue.get_timestamp_period();
        if timestamp_period == 0.0 {
            log::warn!("[GpuProfiler] timestamp_period is 0, timestamps unavailable");
            return None;
        }

        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("GpuProfiler QuerySet"),
            ty: wgpu::QueryType::Timestamp,
            count: MAX_QUERIES,
        });

        let buffer_size = (MAX_QUERIES as u64) * std::mem::size_of::<u64>() as u64;

        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GpuProfiler Resolve"),
            size: buffer_size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GpuProfiler Readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        log::info!(
            "[GpuProfiler] initialized (timestamp_period={:.2}ns, max_pairs={})",
            timestamp_period, MAX_TIMESTAMP_PAIRS
        );

        let adapter_name = adapter.get_info().name.clone();

        Some(Self {
            query_set,
            resolve_buffer,
            readback_buffer,
            timestamp_period,
            entries: RefCell::new(Vec::with_capacity(MAX_TIMESTAMP_PAIRS as usize)),
            next_query: Cell::new(0),
            adapter_name,
            last_readback_ms: Cell::new(0.0),
            scope_prefix: RefCell::new(String::new()),
        })
    }

    /// GPU adapter name (e.g. "Apple M2 Max").
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Duration of the last `read_results()` call in ms (profiler self-overhead).
    pub fn last_readback_overhead_ms(&self) -> f64 {
        self.last_readback_ms.get()
    }

    /// Set a label prefix for all subsequent timestamp allocations.
    /// E.g. "master:" or "clip:abc123:" — prepended to pass labels.
    pub fn set_scope(&self, prefix: &str) {
        *self.scope_prefix.borrow_mut() = prefix.to_string();
    }

    /// Clear the scope prefix.
    pub fn clear_scope(&self) {
        self.scope_prefix.borrow_mut().clear();
    }

    /// Reset for a new frame. Call before rendering begins.
    pub fn begin_frame(&mut self) {
        self.entries.borrow_mut().clear();
        self.next_query.set(0);
    }

    /// Allocate a timestamp pair for a render pass.
    /// Returns `TimestampWrites` to plug into `RenderPassDescriptor::timestamp_writes`.
    /// Returns `None` if query slots are exhausted.
    ///
    /// `width`/`height` are the pass output dimensions (for resolution analysis).
    pub fn render_timestamps(
        &self,
        label: &str,
        width: u32,
        height: u32,
    ) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let begin = self.allocate_pair(label, width, height, false)?;
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(begin),
            end_of_pass_write_index: Some(begin + 1),
        })
    }

    /// Allocate a timestamp pair for a compute pass.
    /// Returns `TimestampWrites` to plug into `ComputePassDescriptor::timestamp_writes`.
    /// Returns `None` if query slots are exhausted.
    ///
    /// `width`/`height` are the dispatch dimensions or texture size (for analysis).
    pub fn compute_timestamps(
        &self,
        label: &str,
        width: u32,
        height: u32,
    ) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        let begin = self.allocate_pair(label, width, height, true)?;
        Some(wgpu::ComputePassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(begin),
            end_of_pass_write_index: Some(begin + 1),
        })
    }

    /// Allocate a query pair and record the entry. Returns the begin index.
    /// If a scope prefix is set, it's prepended to the label.
    fn allocate_pair(
        &self,
        label: &str,
        width: u32,
        height: u32,
        is_compute: bool,
    ) -> Option<u32> {
        let current = self.next_query.get();
        if current + 2 > MAX_QUERIES {
            return None; // Exhausted query slots
        }
        self.next_query.set(current + 2);
        let prefix = self.scope_prefix.borrow();
        let full_label = if prefix.is_empty() {
            label.to_string()
        } else {
            format!("{}{}", *prefix, label)
        };
        self.entries.borrow_mut().push(ProfileEntry {
            label: full_label,
            begin_query: current,
            end_query: current + 1,
            width,
            height,
            is_compute,
        });
        Some(current)
    }

    /// Resolve all timestamps into the readback buffer.
    /// Call after all passes are recorded, before `encoder.finish()`.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        let count = self.next_query.get();
        if count == 0 {
            return;
        }
        encoder.resolve_query_set(
            &self.query_set,
            0..count,
            &self.resolve_buffer,
            0,
        );
        encoder.copy_buffer_to_buffer(
            &self.resolve_buffer,
            0,
            &self.readback_buffer,
            0,
            (count as u64) * std::mem::size_of::<u64>() as u64,
        );
    }

    /// Map the readback buffer and compute durations.
    /// Call after `device.poll()` ensures GPU work is complete.
    /// Also measures its own overhead (buffer map + read time).
    pub fn read_results(&self, device: &wgpu::Device) -> Vec<GpuPassTiming> {
        let readback_start = std::time::Instant::now();

        let entries = self.entries.borrow();
        if entries.is_empty() {
            self.last_readback_ms.set(0.0);
            return Vec::new();
        }

        let buffer_slice = self.readback_buffer.slice(..);

        // Map synchronously (we've already polled the device in render_content)
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());

        match rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                log::warn!("[GpuProfiler] buffer map failed: {:?}", e);
                self.last_readback_ms.set(readback_start.elapsed().as_secs_f64() * 1000.0);
                return Vec::new();
            }
            Err(_) => {
                log::warn!("[GpuProfiler] buffer map channel closed");
                self.last_readback_ms.set(readback_start.elapsed().as_secs_f64() * 1000.0);
                return Vec::new();
            }
        }

        let count = self.next_query.get();
        let data = buffer_slice.get_mapped_range();
        let timestamps: &[u64] =
            bytemuck::cast_slice(&data[..count as usize * std::mem::size_of::<u64>()]);

        let ns_per_tick = self.timestamp_period as f64;

        // Use the first timestamp as the frame-relative origin for absolute times
        let origin_ticks = if !entries.is_empty() {
            timestamps[entries[0].begin_query as usize]
        } else {
            0
        };

        let mut results = Vec::with_capacity(entries.len());

        for entry in entries.iter() {
            let begin_ts = timestamps[entry.begin_query as usize];
            let end_ts = timestamps[entry.end_query as usize];
            let delta_ticks = end_ts.wrapping_sub(begin_ts);
            let duration_ns = delta_ticks as f64 * ns_per_tick;
            let duration_ms = duration_ns / 1_000_000.0;

            // Absolute timestamps relative to frame start
            let begin_ns = begin_ts.wrapping_sub(origin_ticks) as f64 * ns_per_tick;
            let end_ns = end_ts.wrapping_sub(origin_ticks) as f64 * ns_per_tick;

            results.push(GpuPassTiming {
                label: entry.label.clone(),
                duration_ms,
                begin_ns,
                end_ns,
                width: entry.width,
                height: entry.height,
                is_compute: entry.is_compute,
            });
        }

        drop(data);
        self.readback_buffer.unmap();

        self.last_readback_ms.set(readback_start.elapsed().as_secs_f64() * 1000.0);
        results
    }
}
