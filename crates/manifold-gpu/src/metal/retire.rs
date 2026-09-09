//! Fence-stamped retirement for manifold-gpu-allocated texture AND buffer
//! drops — the production fix for the BUG-l7t4 class (bare drop of a GPU
//! resource while an in-flight command buffer still references it).
//!
//! The diagnosis (lane/fault-mechanism, on this branch's base) established
//! by experiment that draining the GPU queue per frame removes the fault,
//! and that deferring drops removes it. On current code the class bisect
//! flipped: deferring BUFFERS alone (2 frames) is sufficient, and holding
//! every texture drop is not — so buffers retire here too, alongside every
//! texture manifold-gpu allocates or imports. Both classes share one queue
//! and one completion-fence clock.
//!
//! How it works:
//! - Every texture manifold-gpu allocates, and every IOSurface import on a
//!   retirement-wired device, carries an `Option<Arc<RetireMark>>`
//!   (`GpuTexture::retire`). Textures whose owner manages lifetime (CAMetalLayer
//!   drawable wraps, mip views of a marked parent) carry `None` and keep
//!   releasing immediately.
//! - On drop, a marked texture is NOT released. It is stamped with
//!   `event.current_value() + 1` — the signal value the dropping frame's
//!   commit will reach on the GPU timeline (same clock as the fence-aware
//!   TexturePool recycling) — and pushed onto a lock-free queue.
//! - The content thread drains the queue once per frame in its render path:
//!   entries whose event has signaled past their stamp are released; the
//!   rest wait (in a reused scratch Vec — no per-frame allocation).
//! - At teardown the queue's `Drop` flushes: it waits for every event's
//!   final committed value, then releases all. Drops racing a dead receiver
//!   fall back to immediate release (sound: the flush already waited out
//!   every commit the device will ever make).
//!
//! Threading: drops can happen on any thread (content thread during renders,
//! the shutdown thread during teardown, test threads in headless harnesses).
//! The queue is an `std::sync::mpsc` channel — `Send` is lock-free — so no
//! `Arc<Mutex>` is needed; the receiving half is only ever touched by the
//! content thread's drain and by the final flush.

use std::sync::mpsc::{Receiver, Sender, channel};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLResource, MTLTexture};

use super::defer_drop::{self, SendBuffer, SendTexture};
use super::types::GpuEvent;

/// A dropped GPU resource waiting for its stamp to signal.
enum RetireResource {
    Tex(SendTexture),
    Buf(SendBuffer),
}

/// One resource drop waiting for its stamp to signal on the GPU timeline.
struct RetireItem {
    /// The signal value the dropping frame's commit will reach.
    stamp: u64,
    /// The clock the stamp is expressed in (second handle; shared per device).
    event: GpuEvent,
    resource: RetireResource,
    /// Drains survived since enqueue. Diagnostic only (MANIFOLD_RETIRE_MIN_DRAINS):
    /// a fixed lag on top of the fence gate — discriminates "completion-gated
    /// is enough" from "the driver needs N frames of quiescence" (BUG-jddy
    /// residency class). Default 0 = fence gate only.
    age: u32,
}

fn min_drains() -> u32 {
    static MIN: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MIN.get_or_init(|| {
        std::env::var("MANIFOLD_RETIRE_MIN_DRAINS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    })
}

/// Sender half of the retirement queue. Cloneable, `Send + Sync`, lock-free.
/// Stored inside [`RetireMark`]; drops use it to enqueue.
#[derive(Clone)]
pub struct RetireSender {
    tx: Sender<RetireItem>,
}

/// Shared per-device retirement configuration. One `Arc<RetireMark>` is
/// stored in every marked texture; `GpuDevice` and `TexturePool` hold the
/// same `Arc` to stamp new allocations.
pub struct RetireMark {
    event: GpuEvent,
    sender: RetireSender,
}

impl RetireMark {
    /// Build a mark: the frame-completion event whose `current_value()` is
    /// the retirement clock, plus the queue to enqueue drops onto.
    pub fn new(event: GpuEvent, sender: RetireSender) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self { event, sender })
    }

    fn enqueue(&self, resource: RetireResource) {
        let item = RetireItem {
            stamp: self.event.current_value().saturating_add(1),
            event: self.event.second_handle(),
            resource,
            age: 0,
        };
        // On Err the receiver is gone: the queue's Drop already flushed
        // (waiting out every commit the device will ever make), so the
        // returned item can release immediately — `SendError` drops it.
        let _ = self.sender.tx.send(item);
    }

    /// Consume a texture drop: stamp and enqueue for fence retirement.
    /// Called from `GpuTexture::drop` — takes the raw handle (clone) and
    /// never blocks.
    pub(crate) fn retire_texture(&self, raw: &Retained<ProtocolObject<dyn MTLTexture>>) {
        if defer_drop::backtrace_enabled() {
            let label = unsafe { raw.label() }
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(unlabeled)".into());
            eprintln!("[retire] enqueue texture {label} (fence-stamped retirement)");
        }
        self.enqueue(RetireResource::Tex(SendTexture(raw.clone())));
    }

    /// Consume a buffer drop. Same contract as [`Self::retire_texture`];
    /// called from `GpuBuffer::drop`.
    pub(crate) fn retire_buffer(
        &self,
        raw: &Retained<ProtocolObject<dyn objc2_metal::MTLBuffer>>,
    ) {
        if defer_drop::backtrace_enabled() {
            let label = unsafe { raw.label() }
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(unlabeled)".into());
            eprintln!("[retire] enqueue buffer {label} (fence-stamped retirement)");
        }
        self.enqueue(RetireResource::Buf(SendBuffer(raw.clone())));
    }
}

/// Receiver half. Lives on the content thread (inside `ContentPipeline`);
/// drained once per frame, flushed at teardown.
pub struct RetireQueue {
    rx: Receiver<RetireItem>,
    /// Entries whose stamp has not signaled yet.
    pending: Vec<RetireItem>,
    /// Scratch for the retain partition in `drain` — reused every frame so
    /// the drain path never allocates.
    keep: Vec<RetireItem>,
}

impl RetireQueue {
    /// Create a (sender, queue) pair. The sender goes into `RetireMark`;
    /// the queue is drained per frame.
    pub fn new() -> (RetireSender, Self) {
        let (tx, rx) = channel();
        (
            RetireSender { tx },
            Self {
                rx,
                pending: Vec::new(),
                keep: Vec::new(),
            },
        )
    }

    /// Release every entry whose stamp has signaled. Call once per frame
    /// from the content thread's render path.
    pub fn drain(&mut self) {
        while let Ok(item) = self.rx.try_recv() {
            self.pending.push(item);
        }
        if self.pending.is_empty() {
            return;
        }
        for item in self.pending.drain(..) {
            if item.event.signaled_value() >= item.stamp && item.age >= min_drains() {
                // Actual release — funnels through the defer-drop probe so
                // the env-gated harness stays composable with retirement on.
                release(item.resource);
            } else {
                self.keep.push(RetireItem {
                    age: item.age.saturating_add(1),
                    ..item
                });
            }
        }
        std::mem::swap(&mut self.pending, &mut self.keep);
        self.keep.clear();
    }

    /// Wait for every pending entry's stamp (clamped to the event's final
    /// committed value — a predicted stamp one past the last commit never
    /// signals) and release all. Called at teardown; also runs from
    /// `Drop`, so late enqueues from sibling fields' teardown are covered.
    pub fn flush(&mut self) {
        while let Ok(item) = self.rx.try_recv() {
            self.pending.push(item);
        }
        // A blacklisted queue never signals again (BUG-665r — the driver
        // ignores submissions after a GPU error): no wait can complete, and
        // the process is already on its way out. Release without waiting.
        if !super::gpu_fault::submissions_ignored() {
            for item in &self.pending {
                let target = item.stamp.min(item.event.current_value());
                item.event.wait_until_done(target);
            }
        }
        for item in self.pending.drain(..) {
            release(item.resource);
        }
    }

    /// Entries still waiting for their stamp (post-drain). Diagnostic/tests.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

/// Release a retired resource — funnels through the defer-drop probe so the
/// env-gated harness stays composable with retirement on.
fn release(resource: RetireResource) {
    match resource {
        RetireResource::Tex(t) => defer_drop::drop_texture(t.0),
        RetireResource::Buf(b) => defer_drop::drop_buffer(b.0),
    }
}

impl Drop for RetireQueue {
    fn drop(&mut self) {
        // Flush, don't leak: this runs after the owner's own teardown (the
        // owner drops first and its fields may enqueue on the way out), so
        // the late entries get their wait-then-release here.
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::GpuTexture;
    use crate::GpuDevice;

    const FORMAT: crate::GpuTextureFormat = crate::GpuTextureFormat::Rgba8Unorm;

    /// Drive one real commit that signals the event, then wait for GPU
    /// completion so `signaled_value()` is authoritative.
    fn commit_one_signal(device: &GpuDevice, event: &GpuEvent) {
        let mut enc = device.create_encoder("retire fence test");
        enc.signal_event(event);
        enc.commit_and_wait_completed();
    }

    fn marked_texture(device: &GpuDevice, mark: &std::sync::Arc<RetireMark>) -> GpuTexture {
        let mut tex = device.create_texture(&crate::GpuTextureDesc {
            width: 64,
            height: 64,
            depth: 1,
            format: FORMAT,
            dimension: crate::GpuTextureDimension::D2,
            usage: crate::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "retire-test",
            mip_levels: 1,
        });
        tex.retire = Some(mark.clone());
        tex
    }

    #[test]
    fn marked_drop_defers_until_signal() {
        let device = GpuDevice::new();
        let event = device.create_event();
        let (sender, mut queue) = RetireQueue::new();
        let mark = RetireMark::new(event.second_handle(), sender);

        let tex = marked_texture(&device, &mark);
        drop(tex);
        queue.drain();
        assert_eq!(
            queue.pending_count(),
            1,
            "marked drop must defer: entry waits for its stamp to signal"
        );

        commit_one_signal(&device, &event);
        queue.drain();
        assert_eq!(
            queue.pending_count(),
            0,
            "drain releases once signaled_value >= stamp"
        );
    }

    #[test]
    fn unmarked_drop_releases_immediately() {
        let device = GpuDevice::new();
        let event = device.create_event();
        let (sender, mut queue) = RetireQueue::new();
        let mark = RetireMark::new(event.second_handle(), sender);

        // Acquired before the mark existed — the pre-wiring discipline.
        let tex = marked_texture(&device, &mark);
        let mut tex = tex;
        tex.retire = None;
        drop(tex);
        queue.drain();
        assert_eq!(
            queue.pending_count(),
            0,
            "unmarked (external/pre-wiring) texture releases immediately"
        );
    }

    #[test]
    fn drain_keeps_only_unsignaled_entries() {
        let device = GpuDevice::new();
        let event = device.create_event();
        let (sender, mut queue) = RetireQueue::new();
        let mark = RetireMark::new(event.second_handle(), sender);

        queue.pending.reserve(4);
        queue.keep.reserve(4);
        let warm_capacity = queue.pending.capacity() + queue.keep.capacity();

        // Entry A: stamp 1, no commit yet.
        drop(marked_texture(&device, &mark));
        queue.drain();
        assert_eq!(queue.pending_count(), 1, "unsignaled entry stays pending");
        // Repeated drains of an unsignaled resource must retain both scratch
        // allocations rather than freeing a consumed vector every frame.
        for _ in 0..3 {
            queue.drain();
            assert_eq!(queue.pending.capacity() + queue.keep.capacity(), warm_capacity);
        }

        // Commit (signaled = 1), then entry B drops → stamp 2.
        commit_one_signal(&device, &event);
        drop(marked_texture(&device, &mark));
        queue.drain();
        assert_eq!(
            queue.pending_count(),
            1,
            "A released (signaled 1 >= stamp 1), B kept (signaled 1 < stamp 2)"
        );

        commit_one_signal(&device, &event); // signaled = 2
        queue.drain();
        assert_eq!(
            queue.pending_count(),
            0,
            "stamp 2 signaled — the remaining entry releases"
        );
    }

    #[test]
    fn marked_buffer_drop_defers_until_signal() {
        let device = GpuDevice::new();
        let event = device.create_event();
        let (sender, mut queue) = RetireQueue::new();
        let mark = RetireMark::new(event.second_handle(), sender);
        device.set_retirement(mark.clone());

        // Allocated AFTER set_retirement — carries the mark by itself.
        let buf = device.create_buffer(4096);
        drop(buf);
        queue.drain();
        assert_eq!(
            queue.pending_count(),
            1,
            "marked buffer drop defers like a texture drop"
        );

        commit_one_signal(&device, &event);
        queue.drain();
        assert_eq!(queue.pending_count(), 0, "buffer releases once signaled");
    }

    #[test]
    fn flush_releases_everything_without_further_commits() {
        let device = GpuDevice::new();
        let event = device.create_event();
        let (sender, mut queue) = RetireQueue::new();
        let mark = RetireMark::new(event.second_handle(), sender);

        // Teardown shape: drops after the last commit, stamp points one past
        // the final signal value — flush must still release (clamped wait).
        commit_one_signal(&device, &event);
        drop(marked_texture(&device, &mark));
        drop(marked_texture(&device, &mark));
        queue.flush();
        assert_eq!(
            queue.pending_count(),
            0,
            "flush waits out the final commit and releases all"
        );
    }
}
