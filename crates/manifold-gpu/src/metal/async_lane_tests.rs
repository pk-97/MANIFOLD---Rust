//! GPU proofs for the encoder's async lane: the lane waits for the main
//! lane's work before it, the main lane waits for a finished lane, every
//! return to the lane hands off again, a continuation stays on the lane's
//! queue, a profiled encoder refuses the lane, and the lane's buffers join
//! the open runaway span. A `GpuEvent` gate the CPU holds back makes the
//! order observable: while the gate is closed, nothing behind it may finish.

use crate::BufferUse;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLSharedEvent};

use super::*;

const WORDS: usize = 256;
const BYTES: u64 = (WORDS * 4) as u64;
/// How long a closed gate is watched for leaks.
const SETTLE_MS: u64 = 50;
/// How long an open gate may take to drain the queues.
const LONG_MS: u64 = 5000;

/// An event the CPU signals to let gated GPU work run. Dropping it opens the
/// gate, so a failed assertion never leaves a buffer waiting on it forever.
struct Gate(GpuEvent);

impl Gate {
    fn new(device: &GpuDevice) -> Self {
        Self(device.create_event())
    }

    /// Value the GPU work waits for.
    const OPEN: u64 = 1;

    fn event(&self) -> &GpuEvent {
        &self.0
    }

    fn open(&self) {
        unsafe { self.0.raw().setSignaledValue(Self::OPEN) };
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.open();
    }
}

fn pattern(seed: u32) -> Vec<u32> {
    (0..WORDS as u32)
        .map(|i| i.wrapping_mul(2_654_435_761).wrapping_add(seed))
        .collect()
}

fn shared(device: &GpuDevice, values: &[u32]) -> GpuBuffer {
    let buffer = device.create_buffer(BYTES, BufferUse::Upload);
    let ptr = buffer.mapped_ptr().expect("shared buffer").cast::<u32>();
    // SAFETY: the buffer is shared, `BYTES` long and not yet in use.
    unsafe { std::ptr::copy_nonoverlapping(values.as_ptr(), ptr, values.len()) };
    buffer
}

/// A buffer holding a value no test pattern produces, so an unwritten word
/// never matches.
fn blank(device: &GpuDevice) -> GpuBuffer {
    shared(device, &[0xDEAD_BEEF; WORDS])
}

fn words(buffer: &GpuBuffer) -> Vec<u32> {
    let ptr = buffer.mapped_ptr().expect("shared buffer").cast::<u32>();
    // SAFETY: the buffer is shared, `BYTES` long, and every caller has seen
    // the GPU finish the writes.
    unsafe { std::slice::from_raw_parts(ptr, WORDS) }.to_vec()
}

fn counters() -> (u64, u64) {
    (
        GpuEncoder::async_lanes_on_this_thread(),
        GpuEncoder::handoffs_on_this_thread(),
    )
}

fn queue_of(enc: &GpuEncoder) -> *const ProtocolObject<dyn MTLCommandQueue> {
    Retained::as_ptr(&enc.cmd_buf.commandQueue())
}

/// Everything the main lane wrote before the handoff is there when the async
/// lane copies it: with the main lane held behind a gate, the lane's finish
/// cannot arrive, and once the gate opens the data reaches the far end.
#[test]
fn async_lane_waits_for_the_main_work_before_it() {
    let device = GpuDevice::new();
    let source = shared(&device, &pattern(11));
    let (a, b, c) = (blank(&device), blank(&device), blank(&device));
    let gate = Gate::new(&device);
    let handoff = device.create_event();
    let (lanes, handoffs) = counters();

    let mut enc = device.create_encoder("async-lane order");
    enc.wait_event(gate.event(), Gate::OPEN);
    enc.copy_buffer_to_buffer(&source, &a, BYTES);
    assert!(enc.use_async_lane(&device, &handoff));
    enc.copy_buffer_to_buffer(&a, &b, BYTES);
    enc.finish_async_lane(&handoff);
    let finished = handoff.encoded_value();
    assert_eq!(finished, 2, "one handoff signal and one finish signal");
    assert!(
        !handoff.wait_until_done_timeout(finished, SETTLE_MS),
        "the async lane ran ahead of the main work before the handoff"
    );

    gate.open();
    assert!(
        handoff.wait_until_done_timeout(finished, LONG_MS),
        "the async lane never finished"
    );
    enc.wait_async_lane(&handoff);
    enc.copy_buffer_to_buffer(&b, &c, BYTES);
    enc.commit_and_wait_completed();

    assert_eq!(words(&c), pattern(11));
    assert_eq!(counters(), (lanes + 1, handoffs + 1));
}

/// The main lane's wait for a finished async lane holds back the main work
/// after it: with the async lane held behind a gate, a probe signalled after
/// the wait cannot arrive, and once the gate opens it does, behind the
/// lane's write.
#[test]
fn main_lane_waits_for_a_finished_async_lane() {
    let device = GpuDevice::new();
    let source = shared(&device, &pattern(7));
    let b = blank(&device);
    let gate = Gate::new(&device);
    let handoff = device.create_event();
    let probe = device.create_event();
    let (lanes, handoffs) = counters();

    let mut enc = device.create_encoder("async-lane wait");
    assert!(enc.use_async_lane(&device, &handoff));
    enc.wait_event(gate.event(), Gate::OPEN);
    enc.copy_buffer_to_buffer(&source, &b, BYTES);
    enc.finish_async_lane(&handoff);
    enc.wait_async_lane(&handoff);
    enc.signal_event(&probe);
    let probed = probe.encoded_value();
    enc.commit();
    assert!(
        !probe.wait_until_done_timeout(probed, SETTLE_MS),
        "the main lane ran past a wait for the async lane"
    );

    gate.open();
    assert!(
        probe.wait_until_done_timeout(probed, LONG_MS),
        "the main lane never got past its wait"
    );
    assert_eq!(words(&b), pattern(7));
    assert_eq!(counters(), (lanes + 1, handoffs + 1));
}

/// Going back to an open async lane hands off again: its next copy reads
/// what the main lane wrote after the first handoff, so it waits for the
/// main lane's gate. A lane that kept its first handoff would copy early.
#[test]
fn returning_to_the_async_lane_hands_off_again() {
    let device = GpuDevice::new();
    let first = shared(&device, &pattern(3));
    let second = shared(&device, &pattern(5));
    let (b1, a2, b2) = (blank(&device), blank(&device), blank(&device));
    let gate = Gate::new(&device);
    let handoff = device.create_event();
    let (lanes, handoffs) = counters();

    let mut enc = device.create_encoder("async-lane return");
    assert!(enc.use_async_lane(&device, &handoff));
    enc.copy_buffer_to_buffer(&first, &b1, BYTES);
    enc.use_main_lane();
    enc.wait_event(gate.event(), Gate::OPEN);
    enc.copy_buffer_to_buffer(&second, &a2, BYTES);
    assert!(enc.use_async_lane(&device, &handoff));
    enc.copy_buffer_to_buffer(&a2, &b2, BYTES);
    enc.finish_async_lane(&handoff);
    let finished = handoff.encoded_value();
    assert_eq!(finished, 3, "two handoff signals and one finish signal");
    assert!(
        !handoff.wait_until_done_timeout(finished, SETTLE_MS),
        "the async copy ran before the main lane's gated write"
    );

    gate.open();
    assert!(
        handoff.wait_until_done_timeout(finished, LONG_MS),
        "the async lane never finished"
    );
    enc.wait_async_lane(&handoff);
    enc.commit_and_wait_completed();

    assert_eq!(words(&b1), pattern(3));
    assert_eq!(words(&b2), pattern(5));
    assert_eq!(counters(), (lanes + 1, handoffs + 2));
}

/// A host sync on the async lane commits and waits that lane's buffer and
/// goes on in a new buffer from the same queue.
#[test]
fn async_lane_continues_on_its_own_queue() {
    let device = GpuDevice::new();
    let source = shared(&device, &pattern(21));
    let out = blank(&device);
    let handoff = device.create_event();
    let main_queue = Retained::as_ptr(&device.clone_queue());
    let async_queue = Retained::as_ptr(device.async_queue());
    assert_ne!(main_queue, async_queue, "two queues");

    let mut enc = device.create_encoder("async-lane queue");
    assert_eq!(queue_of(&enc), main_queue);
    assert!(enc.use_async_lane(&device, &handoff));
    assert_eq!(queue_of(&enc), async_queue);
    enc.copy_buffer_to_buffer(&source, &out, BYTES);
    enc.commit_wait_and_continue(&device);
    assert_eq!(
        queue_of(&enc),
        async_queue,
        "the continuation stays on the async queue"
    );
    assert_eq!(
        words(&out),
        pattern(21),
        "the sync waited for the async buffer"
    );

    enc.use_main_lane();
    assert_eq!(queue_of(&enc), main_queue);
    enc.finish_async_lane(&handoff);
    enc.wait_async_lane(&handoff);
    enc.commit_and_wait_completed();
}

/// A profiled frame keeps one timeline: the lane is refused, nothing is
/// signalled, and the lane calls that follow have nothing to do.
#[test]
fn profiled_encoder_refuses_the_async_lane() {
    let device = GpuDevice::new();
    let Some(sampler) = device.create_timestamp_sampler(16) else {
        return;
    };
    let handoff = device.create_event();
    let before = counters();

    let mut enc = device.create_encoder("async-lane profiled");
    enc.enable_dispatch_profiling(sampler, &device);
    assert!(!enc.use_async_lane(&device, &handoff));
    enc.use_main_lane();
    enc.finish_async_lane(&handoff);
    enc.wait_async_lane(&handoff);
    assert_eq!(handoff.encoded_value(), 0, "no signal was encoded");
    enc.commit_and_wait_profiled(&device);

    assert_eq!(counters(), before);
}

/// Both buffers of a lane visit join the layer's open span: the main
/// buffer the handoff committed and the async buffer the finish committed,
/// before the span's own last buffer.
#[test]
fn async_lane_buffers_join_the_open_span() {
    let device = GpuDevice::new();
    let handoff = device.create_event();
    let mut timer = GpuSpanTimer::default();

    let mut enc = device.create_encoder("async-lane span");
    timer.begin(&mut enc, 1, 1);
    assert!(enc.use_async_lane(&device, &handoff));
    assert_eq!(
        enc.span_buffers.as_ref().map(Vec::len),
        Some(1),
        "the main split"
    );
    enc.finish_async_lane(&handoff);
    assert_eq!(
        enc.span_buffers.as_ref().map(Vec::len),
        Some(2),
        "the main split and the async buffer"
    );
    enc.wait_async_lane(&handoff);
    timer.end(&mut enc);
    assert!(enc.span_buffers.is_none(), "the span closed");
    enc.commit_and_wait_completed();
}
