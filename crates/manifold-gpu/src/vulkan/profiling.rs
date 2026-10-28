//! Per-dispatch timestamps on a query pool (D17). Profiled work writes a
//! timestamp where it starts; the next profiled work, or the end of the
//! command buffer, writes the end of the span before it. At the tag
//! granularity a run of dispatches under one tag shares one span.
//! Unprofiled frames pay one `Option` check per command.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use ash::vk;
use ash::vk::PipelineStageFlags2 as S;

use super::core::Core;
use crate::{GpuFrameProfile, GpuProfiledSpan, GpuWorkKind, ProfileGranularity};

pub(crate) struct QueryPool {
    core: Arc<Core>,
    pub(crate) raw: vk::QueryPool,
}

impl Drop for QueryPool {
    fn drop(&mut self) {
        unsafe { self.core.device.destroy_query_pool(self.raw, None) };
    }
}

/// A timestamp query pool with two queries per span. Cheap to clone; one
/// sampler is reset and reused by every profiled frame.
#[derive(Clone)]
pub struct GpuTimestampSampler {
    pub(crate) pool: Arc<QueryPool>,
    /// Queries in the pool: two per span.
    pub(crate) capacity: u32,
}

impl GpuTimestampSampler {
    /// Maximum number of spans (encoder start/end pairs) one frame can record.
    pub fn max_spans(&self) -> usize {
        self.capacity as usize / 2
    }
}

/// A sampler for `max_spans` spans, or `None` when the queue writes no
/// timestamps.
pub(crate) fn create_sampler(core: &Arc<Core>, max_spans: usize) -> Option<GpuTimestampSampler> {
    core.timestamps?;
    let capacity = u32::try_from(max_spans.max(64) * 2).ok()?;
    let raw = unsafe {
        core.device.create_query_pool(
            &vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::TIMESTAMP)
                .query_count(capacity),
            None,
        )
    }
    .map_err(|e| log::warn!("[Vulkan] timestamp query pool of {capacity}: {e}"))
    .ok()?;
    Some(GpuTimestampSampler {
        pool: Arc::new(QueryPool {
            core: core.clone(),
            raw,
        }),
        capacity,
    })
}

/// A recorded span, waiting for its two timestamps.
struct PendingSpan {
    tag: String,
    label: String,
    kind: GpuWorkKind,
}

/// The encoder's profiling state while a frame is profiled.
pub(crate) struct ProfileState {
    sampler: GpuTimestampSampler,
    spans: Vec<PendingSpan>,
    pub(crate) tag: String,
    granularity: ProfileGranularity,
    /// Proof-only name for the next span.
    #[cfg(feature = "gpu-proofs")]
    pub(crate) next_label: Option<&'static str>,
    /// Whether the last span still waits for its end timestamp.
    open: bool,
    /// The tag of the dispatch run the open span collects, if it is one.
    open_tag: Option<String>,
    overflow: usize,
    /// Every submit's GPU seconds while profiled, summed (`f64` bits).
    pub(crate) gpu_seconds: Arc<AtomicU64>,
}

impl ProfileState {
    /// Profiling from here on in `cmd`: the sampler's queries are reset
    /// first, so one sampler serves every profiled frame.
    pub(crate) fn new(
        device: &ash::Device,
        cmd: vk::CommandBuffer,
        sampler: GpuTimestampSampler,
        granularity: ProfileGranularity,
    ) -> Self {
        unsafe { device.cmd_reset_query_pool(cmd, sampler.pool.raw, 0, sampler.capacity) };
        Self {
            sampler,
            spans: Vec::new(),
            tag: String::new(),
            granularity,
            #[cfg(feature = "gpu-proofs")]
            next_label: None,
            open: false,
            open_tag: None,
            overflow: 0,
            gpu_seconds: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Timestamps for work about to be recorded in `cmd`: the open span's
    /// end, unless a dispatch continues its tag's run, then this work's
    /// start.
    pub(crate) fn begin(
        &mut self,
        device: &ash::Device,
        cmd: vk::CommandBuffer,
        label: &str,
        kind: GpuWorkKind,
    ) {
        if kind == GpuWorkKind::Compute
            && self
                .granularity
                .reuses_open_encoder(self.open_tag.as_deref(), &self.tag)
        {
            return;
        }
        self.end(device, cmd);
        #[cfg(feature = "gpu-proofs")]
        let label = self.next_label.take().unwrap_or(label);
        let query = self.spans.len() as u32 * 2;
        if query + 1 >= self.sampler.capacity {
            self.overflow += 1;
            return;
        }
        self.spans.push(PendingSpan {
            tag: self.tag.clone(),
            label: label.to_string(),
            kind,
        });
        unsafe { device.cmd_write_timestamp2(cmd, S::ALL_COMMANDS, self.sampler.pool.raw, query) };
        self.open = true;
        self.open_tag = (kind == GpuWorkKind::Compute).then(|| self.tag.clone());
    }

    /// The open span's end timestamp, before other work or the end of the
    /// command buffer.
    pub(crate) fn end(&mut self, device: &ash::Device, cmd: vk::CommandBuffer) {
        if std::mem::take(&mut self.open) {
            let query = self.spans.len() as u32 * 2 - 1;
            unsafe {
                device.cmd_write_timestamp2(cmd, S::ALL_COMMANDS, self.sampler.pool.raw, query)
            };
        }
        self.open_tag = None;
    }

    /// The spans from the pool once every submit completed. `total_ms` is
    /// the submits' summed GPU time.
    pub(crate) fn resolve(&self, core: &Core, total_ms: f64) -> GpuFrameProfile {
        let mut profile = GpuFrameProfile {
            total_ms,
            spans: Vec::with_capacity(self.spans.len()),
            overflow: self.overflow,
            invalid: 0,
            failed_command_buffers: 0,
        };
        let Some((tick_ns, mask)) = core.timestamps else {
            profile.invalid = self.spans.len();
            return profile;
        };
        if self.spans.is_empty() {
            return profile;
        }
        // Each query as (value, availability).
        let mut stamps = vec![[0u64; 2]; self.spans.len() * 2];
        let read = unsafe {
            core.device.get_query_pool_results(
                self.sampler.pool.raw,
                0,
                &mut stamps,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WITH_AVAILABILITY,
            )
        };
        // NOT_READY still fills every available query.
        if matches!(read, Err(e) if e != vk::Result::NOT_READY) {
            profile.invalid = self.spans.len();
            return profile;
        }
        let ticks = |q: [u64; 2]| (q[1] != 0).then_some(q[0] & mask);
        let origin = stamps.iter().filter_map(|&q| ticks(q)).min().unwrap_or(0);
        let ms = |t: u64| t as f64 * tick_ns / 1.0e6;
        for (span, pair) in self.spans.iter().zip(stamps.chunks(2)) {
            match (ticks(pair[0]), ticks(pair[1])) {
                (Some(start), Some(end)) if end >= start => profile.spans.push(GpuProfiledSpan {
                    tag: span.tag.clone(),
                    label: span.label.clone(),
                    kind: span.kind,
                    // Vulkan reports no static workgroup memory per pipeline.
                    threadgroup_bytes: 0,
                    start_ms: ms(start - origin),
                    millis: ms(end - start),
                }),
                _ => profile.invalid += 1,
            }
        }
        profile
    }
}
