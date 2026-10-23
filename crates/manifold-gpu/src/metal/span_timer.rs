//! GPU time of a span of encoding, read back on the content thread. A span
//! may cross command buffers (trace tiles, substep syncs commit mid-span);
//! its time is the sum of each buffer's own GPU time, so CPU work between
//! two of its buffers never counts. Polled like the trace timer: no
//! completion callbacks, no shared state, no per-frame allocation.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLCommandBufferStatus};

use super::encoder::GpuEncoder;

/// Buffers awaiting readback. Past this the oldest is dropped and its span
/// goes unmeasured.
const PENDING_CAPACITY: usize = 256;

type CommandBuffer = Retained<ProtocolObject<dyn MTLCommandBuffer>>;

struct Pending {
    key: u64,
    buffer: CommandBuffer,
    /// The span's last buffer: its completion completes the span.
    last: bool,
}

pub struct GpuSpanTimer {
    key: u64,
    /// Lent to the encoder while a span is open.
    scratch: Vec<CommandBuffer>,
    /// Ring of committed buffers in submission order.
    pending: [Option<Pending>; PENDING_CAPACITY],
    oldest: usize,
    len: usize,
    /// GPU seconds of the span being read back so far.
    sum: f64,
    /// The span being read back lost a buffer to a full ring.
    lost: bool,
}

// SAFETY: Metal command buffers are thread-safe for status and timestamp
// reads. The timer has one owner on one thread at a time.
unsafe impl Send for GpuSpanTimer {}

impl Default for GpuSpanTimer {
    fn default() -> Self {
        Self {
            key: 0,
            scratch: Vec::new(),
            pending: std::array::from_fn(|_| None),
            oldest: 0,
            len: 0,
            sum: 0.0,
            lost: false,
        }
    }
}

impl GpuSpanTimer {
    /// Start span `key` in the encoder's current command buffer.
    pub fn begin(&mut self, encoder: &mut GpuEncoder, key: u64) {
        self.key = key;
        let mut buffers = std::mem::take(&mut self.scratch);
        buffers.clear();
        encoder.span_buffers = Some(buffers);
    }

    /// End the open span in the encoder's current command buffer, which the
    /// caller commits before encoding anything that is not the span's.
    pub fn end(&mut self, encoder: &mut GpuEncoder) {
        let Some(mut buffers) = encoder.span_buffers.take() else { return };
        buffers.push(encoder.cmd_buf.clone());
        let count = buffers.len();
        for (index, buffer) in buffers.drain(..).enumerate() {
            self.push(Pending { key: self.key, buffer, last: index + 1 == count });
        }
        self.scratch = buffers;
    }

    fn push(&mut self, pending: Pending) {
        if self.len == PENDING_CAPACITY {
            self.pending[self.oldest] = None;
            self.oldest = (self.oldest + 1) % PENDING_CAPACITY;
            self.len -= 1;
            self.lost = true;
        }
        self.pending[(self.oldest + self.len) % PENDING_CAPACITY] = Some(pending);
        self.len += 1;
    }

    /// Hand `each` the key and GPU seconds of every finished span, oldest
    /// first. One queue runs its buffers in order, so reading stops at the
    /// first unfinished one.
    pub fn poll(&mut self, mut each: impl FnMut(u64, f64)) {
        while self.len > 0 {
            let Some(pending) = &self.pending[self.oldest] else { break };
            let status = unsafe { pending.buffer.status() };
            if status != MTLCommandBufferStatus::Completed && status != MTLCommandBufferStatus::Error {
                break;
            }
            let pending = self.pending[self.oldest].take().expect("checked above");
            self.oldest = (self.oldest + 1) % PENDING_CAPACITY;
            self.len -= 1;
            // A buffer that never ran reports zero times.
            let (start, end) = unsafe { (pending.buffer.GPUStartTime(), pending.buffer.GPUEndTime()) };
            if start > 0.0 && end >= start && (end - start).is_finite() {
                self.sum += end - start;
            }
            if pending.last {
                if !self.lost && self.sum > 0.0 {
                    each(pending.key, self.sum);
                }
                self.sum = 0.0;
                self.lost = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GpuBinding, GpuDevice};

    /// A span over two command buffers sums their GPU time. The CPU pause
    /// between them, longer than the runaway limit, never counts: a layer
    /// whose physics waits on the CPU mid-frame is not a runaway GPU frame.
    #[test]
    fn a_span_sums_its_buffers_and_never_counts_the_cpu_gap_between_them() {
        let device = GpuDevice::new();
        let pipeline = device.create_compute_pipeline(
            "@group(0) @binding(0) var<storage, read_write> out: array<u32>;
             @compute @workgroup_size(1) fn cs_main() { out[0] = out[0] + 1u; }",
            "cs_main",
            "span timer proof",
        );
        let buffer = device.create_buffer_shared(4);
        let dispatch = |encoder: &mut GpuEncoder| {
            encoder.dispatch_compute(&pipeline, &[GpuBinding::Buffer { binding: 0, buffer: &buffer, offset: 0 }], [1, 1, 1], "span");
        };
        let mut timer = GpuSpanTimer::default();
        let mut encoder = device.create_encoder("span timer proof");
        timer.begin(&mut encoder, 7);
        dispatch(&mut encoder);
        encoder.commit_and_continue(&device);
        std::thread::sleep(std::time::Duration::from_millis(400));
        dispatch(&mut encoder);
        timer.end(&mut encoder);
        encoder.commit_and_wait_completed();

        let mut spans = Vec::new();
        timer.poll(|key, seconds| spans.push((key, seconds)));
        assert_eq!(spans.len(), 1, "one finished span: {spans:?}");
        assert_eq!(spans[0].0, 7);
        assert!(spans[0].1 > 0.0 && spans[0].1 < 0.1, "GPU seconds of two tiny buffers, no CPU gap: {spans:?}");
        let mut again = 0;
        timer.poll(|_, _| again += 1);
        assert_eq!(again, 0, "a span is read once");
    }
}
