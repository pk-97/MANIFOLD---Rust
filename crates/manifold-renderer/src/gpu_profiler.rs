//! GPU pass-level profiler using wgpu timestamp queries.
//!
//! Records GPU execution time for individual render/compute passes by inserting
//! `write_timestamp` calls around generator, effect, and compositor operations.
//! No modification to individual effect/generator implementations required —
//! instrumentation happens at the orchestration level.
//!
//! Usage:
//!   profiler.begin_frame()
//!   profiler.begin_scope(encoder, "generator:fluid_sim")
//!   generator.render(...)   // creates internal passes
//!   profiler.end_scope(encoder)
//!   profiler.resolve(encoder)
//!   queue.submit(encoder.finish())
//!   device.poll(wait)
//!   let results = profiler.read_results()

/// Maximum number of timestamp pairs (begin + end) per frame.
/// 128 pairs = 256 queries. Covers ~40 generators + ~80 effects + overhead.
const MAX_TIMESTAMP_PAIRS: u32 = 128;
const MAX_QUERIES: u32 = MAX_TIMESTAMP_PAIRS * 2;

/// A single timed GPU scope with a label.
struct ProfileEntry {
    label: String,
    begin_query: u32,
    end_query: u32,
}

/// Result of a single GPU timing scope.
#[derive(Debug, Clone)]
pub struct GpuPassTiming {
    pub label: String,
    pub duration_ms: f64,
}

/// Manages GPU timestamp queries for per-pass profiling.
///
/// Owns a QuerySet + resolve/readback buffers. The content thread
/// creates one of these and uses it across frames. Each frame:
/// 1. begin_frame() — reset
/// 2. begin_scope()/end_scope() — bracket GPU work
/// 3. resolve(encoder) — resolve queries into buffer
/// 4. After poll: read_results() — map buffer and compute durations
pub struct GpuProfiler {
    query_set: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    timestamp_period: f32,
    entries: Vec<ProfileEntry>,
    next_query: u32,
    /// Whether this profiler is usable (device supports timestamp queries).
    available: bool,
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

        Some(Self {
            query_set,
            resolve_buffer,
            readback_buffer,
            timestamp_period,
            entries: Vec::with_capacity(MAX_TIMESTAMP_PAIRS as usize),
            next_query: 0,
            available: true,
        })
    }

    /// Whether this profiler is usable.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Reset for a new frame. Must call before any begin_scope().
    pub fn begin_frame(&mut self) {
        self.entries.clear();
        self.next_query = 0;
    }

    /// Write a begin timestamp and record the scope label.
    /// Call before the GPU work you want to measure.
    pub fn begin_scope(&mut self, encoder: &mut wgpu::CommandEncoder, label: &str) {
        if self.next_query + 2 > MAX_QUERIES {
            return; // Silently skip if we've exhausted query slots
        }
        let begin = self.next_query;
        encoder.write_timestamp(&self.query_set, begin);
        self.entries.push(ProfileEntry {
            label: label.to_string(),
            begin_query: begin,
            end_query: begin + 1, // Will be written by end_scope
        });
        self.next_query += 2;
    }

    /// Write an end timestamp for the most recent scope.
    /// Call after the GPU work you want to measure.
    pub fn end_scope(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(entry) = self.entries.last() {
            encoder.write_timestamp(&self.query_set, entry.end_query);
        }
    }

    /// Resolve all timestamps into the readback buffer.
    /// Call after all scopes are closed, before encoder.finish().
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        if self.next_query == 0 {
            return;
        }
        encoder.resolve_query_set(
            &self.query_set,
            0..self.next_query,
            &self.resolve_buffer,
            0,
        );
        encoder.copy_buffer_to_buffer(
            &self.resolve_buffer,
            0,
            &self.readback_buffer,
            0,
            (self.next_query as u64) * std::mem::size_of::<u64>() as u64,
        );
    }

    /// Map the readback buffer and compute durations.
    /// Call after device.poll() ensures GPU work is complete.
    /// Returns pass timings sorted by label.
    pub fn read_results(&self, device: &wgpu::Device) -> Vec<GpuPassTiming> {
        if self.entries.is_empty() {
            return Vec::new();
        }

        let buffer_slice = self.readback_buffer.slice(..);

        // Map synchronously (we've already polled the device)
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());

        match rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                log::warn!("[GpuProfiler] buffer map failed: {:?}", e);
                return Vec::new();
            }
            Err(_) => {
                log::warn!("[GpuProfiler] buffer map channel closed");
                return Vec::new();
            }
        }

        let data = buffer_slice.get_mapped_range();
        let timestamps: &[u64] =
            bytemuck::cast_slice(&data[..self.next_query as usize * std::mem::size_of::<u64>()]);

        let ns_per_tick = self.timestamp_period as f64;
        let mut results = Vec::with_capacity(self.entries.len());

        for entry in &self.entries {
            let begin_ts = timestamps[entry.begin_query as usize];
            let end_ts = timestamps[entry.end_query as usize];
            // GPU timestamps can wrap; treat as unsigned difference
            let delta_ticks = end_ts.wrapping_sub(begin_ts);
            let duration_ns = delta_ticks as f64 * ns_per_tick;
            let duration_ms = duration_ns / 1_000_000.0;

            results.push(GpuPassTiming {
                label: entry.label.clone(),
                duration_ms,
            });
        }

        drop(data);
        self.readback_buffer.unmap();

        results
    }
}
