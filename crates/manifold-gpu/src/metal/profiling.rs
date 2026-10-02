//! Per-dispatch GPU timestamp profiling via Metal counter sample buffers.
//!
//! Apple-silicon GPUs support counter sampling at **stage boundaries** only
//! (`MTLCounterSamplingPoint::AtStageBoundary`), never per-dispatch inside an
//! encoder. So profiled mode trades the encoder's batching for resolution:
//! every compute dispatch gets its own `MTLComputeCommandEncoder` with a
//! timestamp sample at the start and end of the encoder, and render/blit
//! passes (already one encoder per op) get boundary samples attached to their
//! pass descriptors. The absolute frame time under profiling is therefore a
//! little higher than production (encoder setup + lost cross-dispatch
//! overlap); the per-span *shares* are what the tool is for. The whole
//! mechanism is dormant unless [`GpuEncoder::enable_dispatch_profiling`] is
//! called on a frame's encoder — production frames pay one `Option` check
//! per dispatch.
//!
//! Timestamp domain: resolved counter samples are GPU-clock ticks. We
//! calibrate with two correlated CPU/GPU timestamp pairs
//! (`MTLDevice::sampleTimestamps`) — one when profiling is enabled, one
//! after `waitUntilCompleted` — and map GPU ticks linearly onto the CPU
//! `mach_absolute_time` axis, converted to nanoseconds via
//! `mach_timebase_info`.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSRange, NSString};
use objc2_metal::{
    MTLCounterResultTimestamp, MTLCounterSampleBuffer, MTLCounterSampleBufferDescriptor,
    MTLCounterSamplingPoint, MTLCounterSet, MTLDevice, MTLStorageMode,
};

/// Sentinel Metal writes for a sample that couldn't be taken
/// (`COUNTER_ERROR` in the headers — not exported by the bindings).
const COUNTER_ERROR: u64 = u64::MAX;

/// What kind of encoder a profiled span covered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuWorkKind {
    Compute,
    Render,
    Blit,
    AccelerationStructure,
}

impl GpuWorkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GpuWorkKind::Compute => "compute",
            GpuWorkKind::Render => "render",
            GpuWorkKind::Blit => "blit",
            GpuWorkKind::AccelerationStructure => "acceleration_structure",
        }
    }
}

/// A reusable chain of timestamp counter sample buffers. Cheap to clone
/// (retains the underlying Metal objects); one sampler can be re-attached to
/// a fresh encoder every profiled frame. Metal caps one buffer's sample
/// count, so capacity past the cap comes from more buffers, filled in order.
#[derive(Clone)]
pub struct GpuTimestampSampler {
    pub(crate) buffers: Vec<Retained<ProtocolObject<dyn MTLCounterSampleBuffer>>>,
    /// Samples per buffer. Even, so a span's pair never straddles two buffers.
    pub(crate) per_buffer: usize,
    /// Capacity in *samples* across every buffer (two per span).
    pub(crate) capacity: usize,
}

unsafe impl Send for GpuTimestampSampler {}

impl GpuTimestampSampler {
    /// Maximum number of spans (encoder start/end pairs) one frame can record.
    pub fn max_spans(&self) -> usize {
        self.capacity / 2
    }

    /// The buffer holding frame-wide sample `index`, and the index inside it.
    fn slot(&self, index: usize) -> (&Retained<ProtocolObject<dyn MTLCounterSampleBuffer>>, usize) {
        (&self.buffers[index / self.per_buffer], index % self.per_buffer)
    }
}

/// One resolved span: a single compute dispatch, render pass, blit pass, or
/// acceleration structure pass.
#[derive(Clone, Debug)]
pub struct GpuProfiledSpan {
    /// Attribution tag set by the host via [`GpuEncoder::set_profile_tag`]
    /// (e.g. the executor's step index). Empty if no tag was set.
    pub tag: String,
    /// The dispatch/pass debug label.
    pub label: String,
    pub kind: GpuWorkKind,
    /// The dispatched pipeline's static threadgroup (WGSL `var<workgroup>`)
    /// memory in bytes; 0 for every non-compute span.
    pub threadgroup_bytes: u32,
    /// GPU start time relative to the frame's first sample, milliseconds.
    pub start_ms: f64,
    /// GPU time spent in this span, milliseconds.
    pub millis: f64,
}

/// A whole profiled command buffer, resolved.
#[derive(Clone, Debug, Default)]
pub struct GpuFrameProfile {
    /// Whole-command-buffer GPU time (`GPUEndTime - GPUStartTime`), ms.
    pub total_ms: f64,
    pub spans: Vec<GpuProfiledSpan>,
    /// Dispatches that ran unprofiled because the sampler filled up. Above
    /// zero, span times are scaled wrong: the timed spans are stretched to
    /// the whole command buffer's time and the untimed tail reads as zero.
    pub overflow: usize,
    /// Spans whose samples resolved to `COUNTER_ERROR` (dropped).
    pub invalid: usize,
    pub failed_command_buffers: usize,
}

impl GpuFrameProfile {
    /// Sum of all resolved span times, ms. Work the spans don't cover
    /// (MPS/MetalFX internal encoders, overflow) shows up as
    /// `total_ms - attributed_ms`.
    pub fn attributed_ms(&self) -> f64 {
        self.spans.iter().map(|s| s.millis).sum()
    }
}

/// A span recorded during encoding, waiting for its two samples to resolve.
pub(crate) struct PendingSpan {
    pub(crate) tag: String,
    pub(crate) label: String,
    pub(crate) kind: GpuWorkKind,
    pub(crate) threadgroup_bytes: u32,
}

/// Encoder-side profiling state. Lives on [`GpuEncoder`] while a frame is
/// being encoded in profiled mode.
pub(crate) struct ProfileState {
    pub(crate) sampler: GpuTimestampSampler,
    pub(crate) spans: Vec<PendingSpan>,
    pub(crate) tag: String,
    pub(crate) overflow: usize,
    /// Correlated (cpu mach ticks, gpu ticks) pair taken at enable time.
    pub(crate) calib_start: (u64, u64),
    pub(crate) committed_buffers: Vec<Retained<ProtocolObject<dyn objc2_metal::MTLCommandBuffer>>>,
}

impl ProfileState {
    /// Reserve the next span's sample buffer and its start/end indices inside
    /// that buffer, or `None` when every buffer is full.
    pub(crate) fn reserve(
        &mut self,
        label: &str,
        kind: GpuWorkKind,
        threadgroup_bytes: u32,
    ) -> Option<SpanSlot> {
        let idx = self.spans.len() * 2;
        if idx + 1 >= self.sampler.capacity {
            self.overflow += 1;
            return None;
        }
        self.spans.push(PendingSpan {
            tag: self.tag.clone(),
            label: label.to_string(),
            kind,
            threadgroup_bytes,
        });
        let (buffer, start) = self.sampler.slot(idx);
        Some((buffer.clone(), start, start + 1))
    }
}

/// A reserved span's sample buffer and its start/end sample indices.
pub(crate) type SpanSlot = (Retained<ProtocolObject<dyn MTLCounterSampleBuffer>>, usize, usize);

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

/// Nanoseconds per `mach_absolute_time` tick.
fn mach_tick_nanos() -> f64 {
    let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
    unsafe { mach_timebase_info(&mut info) };
    if info.denom == 0 {
        return 1.0;
    }
    f64::from(info.numer) / f64::from(info.denom)
}

/// Sample a correlated (cpu, gpu) timestamp pair from the device.
pub(crate) fn sample_cpu_gpu(device: &ProtocolObject<dyn MTLDevice>) -> (u64, u64) {
    let mut cpu: u64 = 0;
    let mut gpu: u64 = 0;
    unsafe {
        device.sampleTimestamps_gpuTimestamp(
            std::ptr::NonNull::from(&mut cpu),
            std::ptr::NonNull::from(&mut gpu),
        );
    }
    (cpu, gpu)
}

/// Locate the device's timestamp counter set, if counter sampling at stage
/// boundaries is supported.
pub(crate) fn timestamp_counter_set(
    device: &ProtocolObject<dyn MTLDevice>,
) -> Option<Retained<ProtocolObject<dyn MTLCounterSet>>> {
    if !device.supportsCounterSampling(MTLCounterSamplingPoint::AtStageBoundary) {
        return None;
    }
    let sets = device.counterSets()?;
    let want: &NSString = unsafe { objc2_metal::MTLCommonCounterSetTimestamp };
    sets.iter()
        .find(|set| set.name().isEqualToString(want))
}

/// Create shared-storage timestamp sample buffers with capacity for
/// `max_spans` start/end pairs. One buffer's size halves on failure (device
/// caps vary) down to a floor of 64 spans; more buffers of that size make up
/// the rest. A later buffer failing leaves a smaller sampler, which shows as
/// `overflow` on frames that outgrow it. Measured on M4 Max: one buffer holds
/// 2,048 spans and a process holds 32 buffers, so ask for what a frame needs.
pub(crate) fn create_sampler(
    device: &ProtocolObject<dyn MTLDevice>,
    max_spans: usize,
) -> Option<GpuTimestampSampler> {
    create_sampler_capped(device, max_spans, usize::MAX)
}

/// [`create_sampler`] with one buffer held to at most `buffer_spans` spans,
/// so a test can force the chain on any device.
pub(crate) fn create_sampler_capped(
    device: &ProtocolObject<dyn MTLDevice>,
    max_spans: usize,
    buffer_spans: usize,
) -> Option<GpuTimestampSampler> {
    let counter_set = timestamp_counter_set(device)?;
    let make = |samples: usize| {
        let desc = MTLCounterSampleBufferDescriptor::new();
        desc.setCounterSet(Some(&counter_set));
        desc.setStorageMode(MTLStorageMode::Shared);
        unsafe { desc.setSampleCount(samples) };
        desc.setLabel(&NSString::from_str("manifold-dispatch-profiler"));
        device.newCounterSampleBufferWithDescriptor_error(&desc)
    };
    let wanted = max_spans.max(64);
    let mut spans = wanted.min(buffer_spans.max(64));
    let first = loop {
        match make(spans * 2) {
            Ok(buffer) => break buffer,
            Err(_) if spans > 64 => spans /= 2,
            Err(e) => {
                log::warn!("counter sample buffer creation failed: {e}");
                return None;
            }
        }
    };
    let mut buffers = vec![first];
    while buffers.len() * spans < wanted {
        match make(spans * 2) {
            Ok(buffer) => buffers.push(buffer),
            Err(e) => {
                log::warn!(
                    "counter sample buffer {} of {spans} spans failed ({e}); profiling holds {} spans",
                    buffers.len() + 1,
                    buffers.len() * spans
                );
                break;
            }
        }
    }
    Some(GpuTimestampSampler {
        capacity: buffers.len() * spans * 2,
        per_buffer: spans * 2,
        buffers,
    })
}

/// Resolve a frame's pending spans into wall-clock milliseconds.
///
/// `calib_start` is the (cpu, gpu) pair taken at enable; `calib_end` is taken
/// after `waitUntilCompleted`. GPU ticks map linearly between them.
pub(crate) fn resolve(
    state: &ProfileState,
    calib_end: (u64, u64),
    total_ms: f64,
) -> GpuFrameProfile {
    let span_count = state.spans.len();
    let mut profile = GpuFrameProfile {
        total_ms,
        spans: Vec::with_capacity(span_count),
        overflow: state.overflow,
        invalid: 0,
        failed_command_buffers: 0,
    };
    if span_count == 0 {
        return profile;
    }

    let used = span_count * 2;
    let mut stamps: Vec<u64> = Vec::with_capacity(used);
    for buffer in &state.sampler.buffers {
        let count = (used - stamps.len()).min(state.sampler.per_buffer);
        if count == 0 {
            break;
        }
        let Some(data) = (unsafe { buffer.resolveCounterRange(NSRange::new(0, count)) }) else {
            profile.invalid = span_count;
            return profile;
        };
        let bytes = unsafe { data.as_bytes_unchecked() };
        if bytes.len() < count * std::mem::size_of::<MTLCounterResultTimestamp>() {
            profile.invalid = span_count;
            return profile;
        }
        // Safety: MTLCounterResultTimestamp is repr(C) { u64 }; the resolved
        // blob is `count` consecutive entries.
        let resolved: &[MTLCounterResultTimestamp] = unsafe {
            std::slice::from_raw_parts(bytes.as_ptr().cast::<MTLCounterResultTimestamp>(), count)
        };
        stamps.extend(resolved.iter().map(|s| s.timestamp));
    }
    resolve_stamps(&state.spans, &stamps, state.calib_start, calib_end, profile)
}

/// The arithmetic half of [`resolve`]: `stamps` holds each span's start and
/// end tick in order. A stamp below the GPU tick sampled at enable predates
/// this frame: a dispatch that never ran (zero threadgroups) leaves its
/// sample slot holding an older frame's value, and such a stamp must not set
/// the frame origin or the stale span would collapse every other span's
/// scale. Those spans count as `invalid`.
fn resolve_stamps(
    spans: &[PendingSpan],
    stamps: &[u64],
    calib_start: (u64, u64),
    calib_end: (u64, u64),
    mut profile: GpuFrameProfile,
) -> GpuFrameProfile {
    let total_ms = profile.total_ms;
    let (cpu1, gpu1) = calib_start;
    let fresh = |t: u64| t != COUNTER_ERROR && t != 0 && t >= gpu1;

    // GPU-tick → ms conversion. The Apple-silicon GPU timestamp clock can
    // PAUSE while the GPU is idle, so calibrating against a CPU wall-clock
    // window (sampleTimestamps pairs around the workload) overstates
    // ns-per-tick by however long the GPU sat idle during encoding. Instead
    // we self-calibrate against the frame itself: the command buffer's
    // GPUStartTime/GPUEndTime total is authoritative, and with serial
    // per-dispatch encoders the earliest→latest sample ticks span that same
    // execution window. The sampleTimestamps calibration pair is kept only
    // as the fallback for a degenerate window (single span).
    let (cpu2, gpu2) = calib_end;
    let tick_ns = mach_tick_nanos();

    let valid = || stamps.iter().copied().filter(|&t| fresh(t));
    let origin = valid().min().unwrap_or(0);
    let last = valid().max().unwrap_or(0);
    let ns_per_gpu_tick = if last > origin && total_ms > 0.0 {
        total_ms * 1.0e6 / (last - origin) as f64
    } else if gpu2 > gpu1 {
        (cpu2.saturating_sub(cpu1)) as f64 * tick_ns / (gpu2 - gpu1) as f64
    } else {
        1.0
    };

    for (i, span) in spans.iter().enumerate() {
        let start = stamps[i * 2];
        let end = stamps[i * 2 + 1];
        if !fresh(start) || !fresh(end) || end < start {
            profile.invalid += 1;
            continue;
        }
        profile.spans.push(GpuProfiledSpan {
            tag: span.tag.clone(),
            label: span.label.clone(),
            kind: span.kind,
            threadgroup_bytes: span.threadgroup_bytes,
            start_ms: (start.saturating_sub(origin)) as f64 * ns_per_gpu_tick / 1.0e6,
            millis: (end - start) as f64 * ns_per_gpu_tick / 1.0e6,
        });
    }
    profile
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(label: &str) -> PendingSpan {
        PendingSpan { tag: String::new(), label: label.to_owned(), kind: GpuWorkKind::Compute, threadgroup_bytes: 0 }
    }

    fn profile(total_ms: f64) -> GpuFrameProfile {
        GpuFrameProfile { total_ms, ..Default::default() }
    }

    /// A slot left over from an older frame (a zero-group dispatch never
    /// rewrote it) must not become the frame origin: the fresh spans scale
    /// from the fresh origin and the stale span is dropped as invalid.
    #[test]
    fn stale_stamp_does_not_set_the_origin() {
        let enable_tick = 1_000_000;
        let spans = [span("fresh a"), span("stale"), span("fresh b")];
        // Fresh window 1_000_100..1_000_300 = 200 ticks for a 10 ms frame.
        let stamps = [1_000_100, 1_000_150, 400, 450, 1_000_200, 1_000_300];
        let out = resolve_stamps(&spans, &stamps, (0, enable_tick), (0, enable_tick + 400), profile(10.0));
        assert_eq!(out.invalid, 1);
        assert_eq!(out.spans.len(), 2);
        let a = &out.spans[0];
        let b = &out.spans[1];
        assert!((a.start_ms - 0.0).abs() < 1e-9, "fresh a starts the frame, got {}", a.start_ms);
        assert!((a.millis - 2.5).abs() < 1e-9, "50 of 200 ticks is 2.5 ms, got {}", a.millis);
        assert!((b.start_ms - 5.0).abs() < 1e-9, "got {}", b.start_ms);
        assert!((b.millis - 5.0).abs() < 1e-9, "got {}", b.millis);
    }

    #[test]
    fn counter_error_and_zero_are_invalid() {
        let spans = [span("a"), span("err"), span("zero")];
        let stamps = [100, 200, COUNTER_ERROR, 300, 0, 300];
        let out = resolve_stamps(&spans, &stamps, (0, 50), (0, 500), profile(1.0));
        assert_eq!(out.invalid, 2);
        assert_eq!(out.spans.len(), 1);
        // The fresh window is 100..300 (the invalid spans' fresh end stamps
        // still bound it): span a is 100 of 200 ticks.
        assert!((out.spans[0].millis - 0.5).abs() < 1e-9, "got {}", out.spans[0].millis);
    }
}
