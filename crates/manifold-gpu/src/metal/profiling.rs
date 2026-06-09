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
}

impl GpuWorkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GpuWorkKind::Compute => "compute",
            GpuWorkKind::Render => "render",
            GpuWorkKind::Blit => "blit",
        }
    }
}

/// A reusable timestamp counter sample buffer. Cheap to clone (retains the
/// underlying Metal object); one sampler can be re-attached to a fresh
/// encoder every profiled frame.
#[derive(Clone)]
pub struct GpuTimestampSampler {
    pub(crate) buffer: Retained<ProtocolObject<dyn MTLCounterSampleBuffer>>,
    /// Capacity in *samples* (two per span).
    pub(crate) capacity: usize,
}

unsafe impl Send for GpuTimestampSampler {}

impl GpuTimestampSampler {
    /// Maximum number of spans (encoder start/end pairs) one frame can record.
    pub fn max_spans(&self) -> usize {
        self.capacity / 2
    }
}

/// One resolved span: a single compute dispatch, render pass, or blit pass.
#[derive(Clone, Debug)]
pub struct GpuProfiledSpan {
    /// Attribution tag set by the host via [`GpuEncoder::set_profile_tag`]
    /// (e.g. the executor's step index). Empty if no tag was set.
    pub tag: String,
    /// The dispatch/pass debug label.
    pub label: String,
    pub kind: GpuWorkKind,
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
    /// Dispatches that ran unprofiled because the sample buffer filled up.
    pub overflow: usize,
    /// Spans whose samples resolved to `COUNTER_ERROR` (dropped).
    pub invalid: usize,
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
}

impl ProfileState {
    /// Reserve the next span's sample-index pair, or `None` when full.
    pub(crate) fn reserve(&mut self, label: &str, kind: GpuWorkKind) -> Option<(usize, usize)> {
        let idx = self.spans.len() * 2;
        if idx + 1 >= self.sampler.capacity {
            self.overflow += 1;
            return None;
        }
        self.spans.push(PendingSpan {
            tag: self.tag.clone(),
            label: label.to_string(),
            kind,
        });
        Some((idx, idx + 1))
    }
}

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

/// Create a shared-storage timestamp sample buffer with capacity for
/// `max_spans` start/end pairs. Halves the request on failure (device caps
/// vary) down to a floor of 64 spans.
pub(crate) fn create_sampler(
    device: &ProtocolObject<dyn MTLDevice>,
    max_spans: usize,
) -> Option<GpuTimestampSampler> {
    let counter_set = timestamp_counter_set(device)?;
    let mut spans = max_spans.max(64);
    loop {
        let desc = MTLCounterSampleBufferDescriptor::new();
        desc.setCounterSet(Some(&counter_set));
        desc.setStorageMode(MTLStorageMode::Shared);
        unsafe { desc.setSampleCount(spans * 2) };
        desc.setLabel(&NSString::from_str("manifold-dispatch-profiler"));
        match device.newCounterSampleBufferWithDescriptor_error(&desc) {
            Ok(buffer) => {
                return Some(GpuTimestampSampler {
                    buffer,
                    capacity: spans * 2,
                });
            }
            Err(_) if spans > 64 => spans /= 2,
            Err(e) => {
                log::warn!("counter sample buffer creation failed: {e}");
                return None;
            }
        }
    }
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
    };
    if span_count == 0 {
        return profile;
    }

    let Some(data) = (unsafe {
        state
            .sampler
            .buffer
            .resolveCounterRange(NSRange::new(0, span_count * 2))
    }) else {
        profile.invalid = span_count;
        return profile;
    };
    let bytes = unsafe { data.as_bytes_unchecked() };
    let expect = span_count * 2 * std::mem::size_of::<MTLCounterResultTimestamp>();
    if bytes.len() < expect {
        profile.invalid = span_count;
        return profile;
    }
    // Safety: MTLCounterResultTimestamp is repr(C) { u64 }; the resolved blob
    // is span_count*2 consecutive entries.
    let stamps: &[MTLCounterResultTimestamp] = unsafe {
        std::slice::from_raw_parts(
            bytes.as_ptr().cast::<MTLCounterResultTimestamp>(),
            span_count * 2,
        )
    };

    // GPU-tick → ms conversion. The Apple-silicon GPU timestamp clock can
    // PAUSE while the GPU is idle, so calibrating against a CPU wall-clock
    // window (sampleTimestamps pairs around the workload) overstates
    // ns-per-tick by however long the GPU sat idle during encoding. Instead
    // we self-calibrate against the frame itself: the command buffer's
    // GPUStartTime/GPUEndTime total is authoritative, and with serial
    // per-dispatch encoders the earliest→latest sample ticks span that same
    // execution window. The sampleTimestamps calibration pair is kept only
    // as the fallback for a degenerate window (single span).
    let (cpu1, gpu1) = state.calib_start;
    let (cpu2, gpu2) = calib_end;
    let tick_ns = mach_tick_nanos();

    let valid = || {
        stamps
            .iter()
            .map(|s| s.timestamp)
            .filter(|&t| t != COUNTER_ERROR && t != 0)
    };
    let origin = valid().min().unwrap_or(0);
    let last = valid().max().unwrap_or(0);
    let ns_per_gpu_tick = if last > origin && total_ms > 0.0 {
        total_ms * 1.0e6 / (last - origin) as f64
    } else if gpu2 > gpu1 {
        (cpu2.saturating_sub(cpu1)) as f64 * tick_ns / (gpu2 - gpu1) as f64
    } else {
        1.0
    };

    for (i, span) in state.spans.iter().enumerate() {
        let start = stamps[i * 2].timestamp;
        let end = stamps[i * 2 + 1].timestamp;
        if start == COUNTER_ERROR || end == COUNTER_ERROR || end < start {
            profile.invalid += 1;
            continue;
        }
        profile.spans.push(GpuProfiledSpan {
            tag: span.tag.clone(),
            label: span.label.clone(),
            kind: span.kind,
            start_ms: (start.saturating_sub(origin)) as f64 * ns_per_gpu_tick / 1.0e6,
            millis: (end - start) as f64 * ns_per_gpu_tick / 1.0e6,
        });
    }
    profile
}
