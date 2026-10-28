//! The longest single command buffer in a span of encoding, read back on the
//! content thread. A span may cross command buffers (trace tiles, substep
//! syncs commit mid-span). The window server gets the GPU between buffers, so
//! one long buffer is what freezes the Mac; many short ones that add up to a
//! slow frame do not, and CPU work between buffers never counts. A buffer that
//! ends in error, such as a watchdog kill, counts as infinitely long. Polled
//! like the trace timer: no completion callbacks, no shared state, no
//! per-frame allocation.

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
    frame: u64,
    buffer: CommandBuffer,
    /// The span's last buffer: its completion completes the span.
    last: bool,
}

pub struct GpuSpanTimer {
    key: u64,
    frame: u64,
    /// Lent to the encoder while a span is open.
    scratch: Vec<CommandBuffer>,
    /// Ring of committed buffers in submission order.
    pending: [Option<Pending>; PENDING_CAPACITY],
    oldest: usize,
    len: usize,
    /// GPU seconds of the longest buffer of the span being read back.
    longest: f64,
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
            frame: 0,
            scratch: Vec::new(),
            pending: std::array::from_fn(|_| None),
            oldest: 0,
            len: 0,
            longest: 0.0,
            lost: false,
        }
    }
}

impl GpuSpanTimer {
    /// Start span `key` of `frame` in the encoder's current command buffer.
    pub fn begin(&mut self, encoder: &mut GpuEncoder, key: u64, frame: u64) {
        self.key = key;
        self.frame = frame;
        let mut buffers = std::mem::take(&mut self.scratch);
        buffers.clear();
        encoder.span_buffers = Some(buffers);
    }

    /// End the open span in the encoder's current command buffer, which the
    /// caller commits before encoding anything that is not the span's.
    pub fn end(&mut self, encoder: &mut GpuEncoder) {
        let Some(mut buffers) = encoder.span_buffers.take() else {
            return;
        };
        buffers.push(encoder.cmd_buf.clone());
        let count = buffers.len();
        for (index, buffer) in buffers.drain(..).enumerate() {
            self.push(Pending {
                key: self.key,
                frame: self.frame,
                buffer,
                last: index + 1 == count,
            });
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

    /// Hand `each` the key, frame and longest buffer's GPU seconds of every
    /// finished span, oldest first. One queue runs its buffers in order, so
    /// reading stops at the first unfinished one.
    pub fn poll(&mut self, mut each: impl FnMut(u64, u64, f64)) {
        while self.len > 0 {
            let Some(pending) = &self.pending[self.oldest] else {
                break;
            };
            let status = unsafe { pending.buffer.status() };
            if status != MTLCommandBufferStatus::Completed
                && status != MTLCommandBufferStatus::Error
            {
                break;
            }
            let pending = self.pending[self.oldest].take().expect("checked above");
            self.oldest = (self.oldest + 1) % PENDING_CAPACITY;
            self.len -= 1;
            if status == MTLCommandBufferStatus::Error {
                self.longest = f64::INFINITY;
            } else {
                // A buffer that never ran reports zero times.
                let (start, end) =
                    unsafe { (pending.buffer.GPUStartTime(), pending.buffer.GPUEndTime()) };
                if start > 0.0 && end >= start && (end - start).is_finite() {
                    self.longest = self.longest.max(end - start);
                }
            }
            if pending.last {
                if self.longest.is_infinite() || (!self.lost && self.longest > 0.0) {
                    each(pending.key, pending.frame, self.longest);
                }
                self.longest = 0.0;
                self.lost = false;
            }
        }
    }

    /// The frame of the oldest span still on the GPU. Every span of an
    /// earlier frame has been handed out by [`Self::poll`].
    pub fn in_flight_frame(&self) -> Option<u64> {
        self.pending[self.oldest]
            .as_ref()
            .filter(|_| self.len > 0)
            .map(|pending| pending.frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BufferUse;
    use crate::{GpuBinding, GpuDevice};

    /// A span reports its longest buffer, never the sum: a layer whose frame
    /// is spread over many short buffers (trace tiles) keeps the Mac
    /// responsive however long the frame. CPU pauses between buffers, longer
    /// than the runaway limit here, never count either.
    #[test]
    fn a_span_reports_its_longest_buffer_not_the_sum_nor_cpu_gaps() {
        let device = GpuDevice::new();
        let pipeline = device.create_compute_pipeline(
            "@group(0) @binding(0) var<storage, read_write> out: array<u32>;
             struct Work { loops: u32 };
             @group(0) @binding(1) var<uniform> work: Work;
             @compute @workgroup_size(64) fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
                 var acc = id.x;
                 for (var i = 0u; i < work.loops; i = i + 1u) { acc = acc * 1664525u + 1013904223u; }
                 if (acc == 0u) { out[0] = acc; }
             }",
            "cs_main",
            "span timer proof",
        );
        let buffer = device.create_buffer(4, BufferUse::Upload);
        let dispatch = |encoder: &mut GpuEncoder, loops: u32| {
            let work = loops.to_ne_bytes();
            encoder.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Buffer {
                        binding: 0,
                        buffer: &buffer,
                        offset: 0,
                    },
                    GpuBinding::Bytes {
                        binding: 1,
                        data: &work,
                    },
                ],
                [4096, 1, 1],
                "span",
            );
        };
        let mut timer = GpuSpanTimer::default();
        let mut encoder = device.create_encoder("span timer proof");
        let mut committed = Vec::new();
        timer.begin(&mut encoder, 7, 3);
        for loops in [200, 20_000, 200, 2_000] {
            dispatch(&mut encoder, loops);
            committed.push(encoder.cmd_buf.clone());
            encoder.commit_and_continue(&device);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        dispatch(&mut encoder, 200);
        committed.push(encoder.cmd_buf.clone());
        timer.end(&mut encoder);
        assert_eq!(
            timer.in_flight_frame(),
            Some(3),
            "the span's buffers are still pending"
        );
        encoder.commit_and_wait().expect("GPU work completes");

        let each: Vec<f64> = committed
            .iter()
            .map(|b| unsafe { b.GPUEndTime() - b.GPUStartTime() })
            .collect();
        let mut spans = Vec::new();
        timer.poll(|key, frame, seconds| spans.push((key, frame, seconds)));
        assert_eq!(spans.len(), 1, "one finished span: {spans:?}");
        assert_eq!((spans[0].0, spans[0].1), (7, 3));
        let longest = each.iter().copied().fold(0.0, f64::max);
        assert_eq!(
            spans[0].2, longest,
            "the span is its longest buffer: {each:?}"
        );
        assert!(
            spans[0].2 < each.iter().sum::<f64>(),
            "never the sum: {each:?}"
        );
        assert!(
            spans[0].2 < 0.25,
            "the 400 ms of CPU pauses never count: {spans:?}"
        );
        assert_eq!(timer.in_flight_frame(), None);
        let mut again = 0;
        timer.poll(|_, _, _| again += 1);
        assert_eq!(again, 0, "a span is read once");
    }
}
