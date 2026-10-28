//! Content-to-UI texture handoff.
//!
//! The content and UI `GpuDevice`s are two wrappers on one native device,
//! each with its own queue (`GpuDevice::sibling`), so a texture the content
//! thread renders into is the same texture the UI thread samples — no copy.
//!
//!   Content queue ──render──▶ GpuTexture ◀──sample── UI queue

use parking_lot::RwLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use std::sync::atomic::AtomicBool;

/// Number of bridge surfaces (triple-buffered).
/// 3 surfaces allow 2 content frames in flight while the UI reads a third.
pub const SURFACE_COUNT: usize = 3;

/// Shared state between content and UI threads for one texture handoff.
/// Created once during init, Arc-shared between both threads.
/// Uses interior mutability (RwLock) so resize works through Arc.
///
/// Triple-buffered: three surfaces allow the content thread to have 2 frames
/// in flight while the UI thread reads a completed frame. An atomic front_index
/// tracks which surface is safe to read. Combined with separate command
/// queues (content + UI), this eliminates GPU starvation and
/// synchronous poll stalls.
pub struct SharedTextureBridge {
    /// The three surfaces. Behind RwLock for resize (rare write, frequent
    /// read via `texture`).
    textures: RwLock<[manifold_gpu::GpuTexture; SURFACE_COUNT]>,
    /// Pixel format of every surface: `Rgba16Float`, or `R8Unorm` for a
    /// selection mask.
    format: manifold_gpu::GpuTextureFormat,
    /// Texture dimensions (atomic for lock-free dimension checks).
    width: AtomicU32,
    height: AtomicU32,
    /// Which surface the UI thread should read (0, 1, or 2).
    /// Content thread calls `publish_front()` after confirming GPU completion.
    front_index: AtomicU32,
    /// Per-slot "has ever been published". `front_index` starts at 0, which
    /// is indistinguishable from "slot 0 was published" — without this flag
    /// the read fence's front-moved condition deadlocks at startup: the
    /// first frame targets slot 0, whose front condition can never clear
    /// until a frame completes, which requires the first frame to be written
    /// (bridge-probe measured the hang; production would hit it on launch).
    published: [AtomicBool; SURFACE_COUNT],
    published_frames: [AtomicU64; SURFACE_COUNT],
    /// Generation counter — incremented on resize so both sides detect stale textures.
    generation: AtomicU64,
    // ── Read fence (BUG-xaw4) ─────────────────────────────────────────
    // The UI's reads and the content thread's writes go through different
    // GpuDevice wrappers and queues, so nothing on the GPU orders them — the
    // fence is CPU-side atomics advanced by GPU completion handlers.
    // `reads_in_flight[slot]` counts UI reads of the slot that have been
    // encoded but not yet GPU-retired. Content's reuse wait
    // (`wait_reusable`) requires front to have moved off the slot AND the
    // count at zero before overwriting. A COUNT, not a max-stamp: reads
    // retire out of order, and a stamp design measured 17.7% torn reads in
    // the bridge-probe because a later read's retirement covered an earlier
    // read of the same slot.
    reads_in_flight: [AtomicU64; SURFACE_COUNT],
    /// Content-thread frame of the last `publish_front`. Bridges whose
    /// publisher can pause (graph-editor previews) use this to stop gating
    /// once every surface they hold is stale — see `is_reusable_ephemeral`.
    last_publish_frame: AtomicU64,
}

/// A UI-side claim on a bridge slot: `slot` was the published front when the
/// lease was taken. Retire via [`SharedTextureBridge::retire_read`] from the
/// completion handler of the encoder that sampled the slot — or immediately
/// if no sample was encoded (a lease with no GPU read behind it may retire
/// at once).
#[derive(Clone, Copy)]
pub struct BridgeReadLease {
    slot: usize,
}

impl BridgeReadLease {
    pub fn slot(&self) -> usize {
        self.slot
    }
}

/// A batch of bridge read leases held by one UI frame.
pub(crate) type BridgeLeaseBatch = Vec<(std::sync::Arc<SharedTextureBridge>, BridgeReadLease)>;

/// Retire a batch of bridge read leases when `enc`'s GPU work completes,
/// then wake the content thread with `SurfaceReady` (its surface wait sleeps
/// on that command). `enc` must be ordered AFTER the encoder that sampled
/// the bridges on the same queue — any later command buffer qualifies,
/// because per-queue completion is in commit order.
pub(crate) fn attach_lease_retire(
    enc: &manifold_gpu::GpuEncoder,
    content_tx: Option<crossbeam_channel::Sender<crate::content_command::ContentCommand>>,
    leases: BridgeLeaseBatch,
) {
    if leases.is_empty() {
        return;
    }
    enc.add_completed_handler(move || {
        for (bridge, lease) in &leases {
            bridge.retire_read(*lease);
        }
        if let Some(ref tx) = content_tx {
            crate::content_command::ContentCommand::send(
                tx,
                crate::content_command::ContentCommand::SurfaceReady,
            );
        }
    });
}

/// Marker-encoder variant for paths that sampled a bridge but commit no
/// further work this frame (resize / drawable-starved early returns). The
/// empty buffer completes only after the sampling encoder ahead of it in the
/// queue, so the retirement ordering still holds.
pub(crate) fn retire_leases_via_marker(
    device: &manifold_gpu::GpuDevice,
    content_tx: Option<crossbeam_channel::Sender<crate::content_command::ContentCommand>>,
    leases: BridgeLeaseBatch,
) {
    if leases.is_empty() {
        return;
    }
    let enc = device.create_encoder("Bridge-retire");
    attach_lease_retire(&enc, content_tx, leases);
    enc.commit();
}

/// The slot-reuse condition, pure so the gate logic is testable without a
/// GPU. A slot may be overwritten when no new read can start on it
/// (never published, front moved off, or `quiet_bypass` — the bridge stopped
/// publishing and every surface it holds is stale) AND no claimed read is
/// still in flight on the GPU. Reads always gate: only the front-pin is
/// bypassable.
fn slot_reusable(
    published: bool,
    front_is_slot: bool,
    reads_in_flight: u64,
    quiet_bypass: bool,
) -> bool {
    (quiet_bypass || !published || !front_is_slot) && reads_in_flight == 0
}

/// One bridge surface: a 2D render target both queues use.
fn create_surface(
    device: &manifold_gpu::GpuDevice,
    width: u32,
    height: u32,
    format: manifold_gpu::GpuTextureFormat,
) -> manifold_gpu::GpuTexture {
    device.create_texture(&manifold_gpu::GpuTextureDesc {
        width,
        height,
        depth: 1,
        format,
        dimension: manifold_gpu::GpuTextureDimension::D2,
        usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
        label: "Bridge surface",
        mip_levels: 1,
    })
}

impl SharedTextureBridge {
    /// A triple-buffered `Rgba16Float` bridge on `device`.
    pub fn new(device: &manifold_gpu::GpuDevice, width: u32, height: u32) -> Self {
        Self::with_format(
            device,
            width,
            height,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
        )
    }

    /// A bridge of `format` surfaces (`Rgba16Float` or `R8Unorm`).
    pub fn with_format(
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
        format: manifold_gpu::GpuTextureFormat,
    ) -> Self {
        let textures = std::array::from_fn(|_| create_surface(device, width, height, format));
        log::info!(
            "[SharedTextureBridge] created {SURFACE_COUNT}x {width}x{height} {format:?} surfaces"
        );

        Self {
            format,
            textures: RwLock::new(textures),
            width: AtomicU32::new(width),
            height: AtomicU32::new(height),
            front_index: AtomicU32::new(0),
            published: [
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
            ],
            published_frames: std::array::from_fn(|_| AtomicU64::new(0)),
            generation: AtomicU64::new(0),
            reads_in_flight: [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)],
            last_publish_frame: AtomicU64::new(0),
        }
    }

    /// Surface `slot` (0, 1 or 2). Callers keep the clone until the next
    /// generation bump, then take fresh ones.
    pub fn texture(&self, slot: usize) -> manifold_gpu::GpuTexture {
        self.textures.read()[slot].clone()
    }

    /// Resize the bridge to three new surfaces on `device`. Both sides take
    /// fresh textures after this call (detected via the generation counter).
    pub fn resize(&self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        let textures = std::array::from_fn(|_| create_surface(device, width, height, self.format));
        {
            let mut guard = self.textures.write();
            // Dimensions change under the write lock so a reader that holds
            // the read lock never pairs new surfaces with stale dimensions.
            self.width.store(width, Ordering::Release);
            self.height.store(height, Ordering::Release);
            *guard = textures;
        }
        self.front_index.store(0, Ordering::Release);
        self.generation.fetch_add(1, Ordering::Release);
        // New surfaces, new contract: no read of a stale surface may gate a
        // write to a fresh one (both sides take fresh textures on the bump).
        for c in &self.reads_in_flight {
            c.store(0, Ordering::Release);
        }
        for p in &self.published {
            p.store(false, Ordering::Release);
        }
        log::info!("[SharedTextureBridge] resized {SURFACE_COUNT}x surfaces to {width}x{height}");
    }

    /// Which surface index the UI thread should read from.
    /// Updated by the content thread after confirming GPU completion.
    pub fn front_index(&self) -> u32 {
        self.front_index.load(Ordering::Acquire)
    }

    /// Publish a completed surface as the new front buffer.
    /// Called by the content thread after confirming the GPU finished
    /// writing to this surface (fence ready). `frame` is the content-thread
    /// frame counter — it stamps `last_publish_frame` for the quiet-bridge
    /// bypass in `is_reusable_ephemeral`.
    pub fn publish_front(&self, index: u32, frame: u64) {
        self.published_frames[index as usize].store(frame, Ordering::Release);
        self.published[index as usize].store(true, Ordering::Release);
        self.last_publish_frame.store(frame, Ordering::Release);
        self.front_index.store(index, Ordering::Release);
    }

    /// Take a read lease on the current front slot (BUG-xaw4 read fence).
    /// The in-flight claim is recorded BEFORE the caller encodes its sample,
    /// then re-validated against front: if front moved mid-acquire the claim
    /// is released and retried, so a sample is only ever encoded for a slot
    /// the content thread's `wait_reusable` is already blocked on (the claim
    /// precedes any possible pass of the count check).
    pub fn acquire_read(&self) -> BridgeReadLease {
        loop {
            let slot = self.front_index() as usize;
            self.reads_in_flight[slot].fetch_add(1, Ordering::AcqRel);
            if self.front_index() as usize == slot {
                return BridgeReadLease { slot };
            }
            self.reads_in_flight[slot].fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Read only while holding the lease. The slot cannot be reused until
    /// that lease retires, so its image and frame stamp stay paired.
    pub fn leased_frame(&self, lease: BridgeReadLease) -> Option<u64> {
        self.published[lease.slot]
            .load(Ordering::Acquire)
            .then(|| self.published_frames[lease.slot].load(Ordering::Acquire))
    }

    /// Mark a read lease's GPU work retired — call from the completion
    /// handler of the encoder that sampled the slot, or immediately when no
    /// sample was encoded. Saturating: a lease retired across a resize
    /// (counters reset) must not underflow.
    pub fn retire_read(&self, lease: BridgeReadLease) {
        let _ = self.reads_in_flight[lease.slot].fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |x| Some(x.saturating_sub(1)),
        );
    }

    /// One pass of the reuse condition, non-blocking — the content thread's
    /// surface-readiness poll shape (`ContentPipeline::is_surface_ready`).
    /// True when the slot may be overwritten: front has moved off it (so no
    /// NEW read can start) and every claimed read has retired on the GPU.
    /// When false the content thread stalls instead of tearing — under
    /// saturation it paces down rather than sampling-and-writing the same
    /// surface on two devices at once.
    pub fn is_reusable(&self, slot: usize) -> bool {
        slot_reusable(
            self.published[slot].load(Ordering::Acquire),
            self.front_index() as usize == slot,
            self.reads_in_flight[slot].load(Ordering::Acquire),
            false,
        )
    }

    /// Reuse check for a bridge whose publisher can pause or stop — the
    /// graph-editor previews (node preview, thumbnail atlas). Unlike the
    /// main preview they don't publish every frame: the atlas skips frames
    /// whose dump is empty, and both go silent when the editor closes or the
    /// watched chain stops running. A pinned front must not gate forever in
    /// that state — once a full rotation passes with no publish, every
    /// surface the bridge holds is stale and blocking the show output
    /// protects nothing (wedged the whole pipeline when the editor closed
    /// with front on the upcoming write slot). In-flight UI reads still
    /// gate: a quiet bridge can have a live reader, never a live writer.
    pub fn is_reusable_ephemeral(&self, slot: usize, frame: u64) -> bool {
        let quiet = frame.saturating_sub(self.last_publish_frame.load(Ordering::Acquire))
            >= SURFACE_COUNT as u64;
        slot_reusable(
            self.published[slot].load(Ordering::Acquire),
            self.front_index() as usize == slot,
            self.reads_in_flight[slot].load(Ordering::Acquire),
            quiet,
        )
    }

    /// Slot-state dump for the surface-timeout diagnostic (BUG-j8gy): when the
    /// content thread's surface wait times out, the blocker is named by these
    /// three numbers — front still ON the slot means the UI never consumed a
    /// newer frame; reads_in_flight > 0 means a UI read lease hasn't retired.
    pub fn debug_slot_state(&self, slot: usize) -> (u32, bool, u64) {
        (
            self.front_index(),
            self.published[slot].load(Ordering::Acquire),
            self.reads_in_flight[slot].load(Ordering::Acquire),
        )
    }

    /// Probe-side diagnostics: the in-flight read count for a slot
    /// (bridge-probe's stuck-state dump; perf-soak is the probe's feature).
    #[cfg(feature = "perf-soak")]
    pub fn debug_reads_in_flight(&self, slot: usize) -> u64 {
        self.reads_in_flight[slot].load(Ordering::Acquire)
    }

    /// Current dimensions.
    pub fn width(&self) -> u32 {
        self.width.load(Ordering::Acquire)
    }

    pub fn height(&self) -> u32 {
        self.height.load(Ordering::Acquire)
    }

    /// Generation counter — changes on resize. Both sides compare against
    /// their last-seen generation to know when to re-import.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::slot_reusable;

    #[test]
    fn unpublished_slot_is_reusable_with_front_pinned() {
        // Startup: front_index starts at 0 before any publish — slot 0 must
        // not deadlock the first frame.
        assert!(slot_reusable(false, true, 0, false));
    }

    #[test]
    fn published_front_slot_gates_until_front_moves() {
        assert!(!slot_reusable(true, true, 0, false));
        assert!(slot_reusable(true, false, 0, false));
    }

    #[test]
    fn in_flight_reads_always_gate() {
        // Even with front moved off — and even on a quiet bridge — a claimed
        // read not yet retired on the GPU blocks the overwrite.
        assert!(!slot_reusable(true, false, 1, false));
        assert!(!slot_reusable(true, true, 1, true));
    }

    #[test]
    fn quiet_bridge_front_pin_is_bypassed() {
        // The editor-closed wedge: published front pinned on the write slot,
        // no reads in flight, publisher gone — the slot must free up.
        assert!(slot_reusable(true, true, 0, true));
    }
}
