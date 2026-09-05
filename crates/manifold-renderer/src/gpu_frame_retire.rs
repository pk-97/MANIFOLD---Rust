//! Retire-before-reuse for GPU resources dropped on a mode/dimension switch
//! (BUG-rnnr).
//!
//! The RT/denoise targets in `node.render_scene` — `opaque_scene_color`,
//! `rt_temporal_color_scratch`, `rt_firefly_scratch`, and the MetalFX
//! denoiser/upscaler objects (whose internal temporal-history textures the
//! scaler object owns) — are direct `device.create_texture()` fields, NOT
//! `TexturePool`-backed, so the pool's fence-aware recycling
//! (`texture_pool.rs`) does not cover them. Dropping one while a prior
//! command buffer still references it page-faults the GPU, blacklists the
//! queue, and exits the content pipeline.
//!
//! Contract, same fence math as `TexturePool`:
//!   - `retire()` stamps the outgoing resource with the signal value the
//!     last committed frame reached (`ctx.gpu_signal_committed`). The
//!     outgoing resource's last GPU use is that frame's command buffer, so
//!     it is safe to free once the event's `signaled_value()` reaches the
//!     stamp. (`evaluate` runs before this frame's `signal_event`, so the
//!     previous commit's value is the correct stamp — same reason the pool
//!     stamps `current_value() + 1` from mid-frame release sites.)
//!   - `drain()` runs at the next natural per-frame point (the top of
//!     render_scene's resource-ensure block) with the event's current
//!     `signaled_value()` (`ctx.gpu_signaled`). No allocation: a
//!     fixed-capacity array compacted in place.
//!   - A `0` stamp (no fence plumbed — tests, thumbnails, export) drains on
//!     the next call: the previous instant-free behavior, correct on
//!     synchronous hosts.
//!
//! Overflow (more live switches than capacity while the GPU is that many
//! frames behind) is pathological; it logs loudly and frees immediately —
//! the pre-fix behavior — rather than growing or blocking the content
//! thread.
//!
//! `Drop` frees everything immediately: teardown has no in-flight frames by
//! construction (the host retires its queue before dropping the graph, and
//! only this node's own work references these resources).

/// Maximum resources held for GPU retirement at once. The realistic backlog
/// is the GPU's frames-in-flight depth (~3); 16 leaves room for resize churn
/// (a switch every frame while the GPU lags) before the overflow path.
const RETIRING_CAPACITY: usize = 16;

/// A GPU resource waiting for the frame that last used it to retire. The
/// payload is never read — staying alive until the entry drops IS the
/// mechanism (BUG-rnnr).
#[allow(dead_code)]
// Un-suppresses if a future caller needs to inspect, log, or hand back a
// retired resource instead of dropping it — until then, holding is the job.
pub(crate) enum RetiredResource {
    Texture(manifold_gpu::GpuTexture),
    /// The whole wrapper is retired (not just its output texture) because
    /// the MetalFX scaler object owns its temporal-history textures —
    /// keeping the object alive keeps them alive.
    Denoiser(Box<crate::denoiser::Denoiser>),
    TemporalUpscaler(Box<crate::metalfx_temporal_upscaler::MetalFxTemporalUpscaler>),
}

struct RetiredEntry {
    /// Held, never read — the payload's liveness until this entry drops IS
    /// the retire-before-reuse mechanism (same suppression rationale as
    /// `RetiredResource`).
    #[allow(dead_code)]
    resource: RetiredResource,
    /// Signal value the last-using frame's commit reached; free once
    /// `signaled_value() >= release_signal`.
    release_signal: u64,
}

/// Fixed-capacity set of resources stamped for fence-aware release.
pub struct GpuRetiring {
    entries: [Option<RetiredEntry>; RETIRING_CAPACITY],
}

impl GpuRetiring {
    pub fn new() -> Self {
        const EMPTY: Option<RetiredEntry> = None;
        Self {
            entries: [EMPTY; RETIRING_CAPACITY],
        }
    }

    /// Stash `resource` for release once the GPU passes `release_signal`.
    /// `release_signal` MUST be the committed signal value as of the start
    /// of the current frame (the last commit that could reference the
    /// resource). On overflow, logs and frees immediately (the pre-fix
    /// behavior) — never blocks the content thread.
    pub fn retire(&mut self, resource: impl Into<RetiredResource>, release_signal: u64) {
        let resource = resource.into();
        if let Some(slot) = self.entries.iter_mut().find(|e| e.is_none()) {
            *slot = Some(RetiredEntry {
                resource,
                release_signal,
            });
            return;
        }
        log::error!(
            "GpuRetiring: capacity {RETIRING_CAPACITY} exhausted with the GPU still \
             holding switched resources in flight — freeing immediately \
             (pre-BUG-rnnr behavior). Sustained churn at this depth indicates \
             a pacing problem.",
        );
        drop(resource);
    }

    /// Free every resource whose last-using frame the GPU has retired.
    /// `signaled` is the event's current `signaled_value()`. Drained in
    /// place — no allocation.
    pub fn drain(&mut self, signaled: u64) {
        for slot in self.entries.iter_mut() {
            let retire_now = slot.as_ref().is_some_and(|e| signaled >= e.release_signal);
            if retire_now {
                drop(slot.take());
            }
        }
    }

    /// Live backlog — the gpu-proofs value test pins the hold/release
    /// discipline on this.
    #[cfg(all(test, feature = "gpu-proofs"))]
    fn live_count(&self) -> usize {
        self.entries.iter().flatten().count()
    }
}

impl Default for GpuRetiring {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for GpuRetiring {
    fn drop(&mut self) {
        // Teardown: no in-flight frames remain that only this node's work
        // could reference (the host retires its queue before dropping the
        // graph). Free immediately rather than holding GPU memory.
        for slot in self.entries.iter_mut() {
            drop(slot.take());
        }
    }
}

impl From<manifold_gpu::GpuTexture> for RetiredResource {
    fn from(t: manifold_gpu::GpuTexture) -> Self {
        RetiredResource::Texture(t)
    }
}

impl From<Box<crate::denoiser::Denoiser>> for RetiredResource {
    fn from(d: Box<crate::denoiser::Denoiser>) -> Self {
        RetiredResource::Denoiser(d)
    }
}

impl From<Box<crate::metalfx_temporal_upscaler::MetalFxTemporalUpscaler>> for RetiredResource {
    fn from(u: Box<crate::metalfx_temporal_upscaler::MetalFxTemporalUpscaler>) -> Self {
        RetiredResource::TemporalUpscaler(u)
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Value-level proof of the release discipline: a retired texture
    //! survives `drain` calls whose signaled value hasn't reached its
    //! stamp, and is released exactly once it has (BUG-rnnr).
    use super::*;
    use manifold_gpu::{GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};

    fn make_texture(device: &manifold_gpu::GpuDevice, label: &str) -> manifold_gpu::GpuTexture {
        device.create_texture(&GpuTextureDesc {
            width: 8,
            height: 8,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
            label,
            mip_levels: 1,
        })
    }

    #[test]
    fn retired_texture_survives_until_signaled_then_released() {
        let device = crate::test_device();
        let mut retiring = GpuRetiring::new();

        // A switch at frame N stamps with the previous commit's value (10).
        // GPU has only retired up to 4: the texture MUST stay alive.
        retiring.retire(make_texture(&device, "rnnr-proof-a"), 10);
        assert_eq!(retiring.live_count(), 1);
        retiring.drain(4);
        assert_eq!(
            retiring.live_count(),
            1,
            "stamp 10 not yet signaled (4) — texture must be held"
        );
        retiring.drain(9);
        assert_eq!(
            retiring.live_count(),
            1,
            "stamp 10 not yet signaled (9) — texture must be held"
        );

        // GPU passes the stamp: released.
        retiring.drain(10);
        assert_eq!(retiring.live_count(), 0, "signaled >= stamp — released");

        // Stamps drain in order: a newer switch outlives an older drain.
        retiring.retire(make_texture(&device, "rnnr-proof-b"), 20);
        retiring.retire(make_texture(&device, "rnnr-proof-c"), 21);
        retiring.drain(20);
        assert_eq!(retiring.live_count(), 1, "only stamp 20 passed");
        retiring.drain(21);
        assert_eq!(retiring.live_count(), 0);
    }

    #[test]
    fn zero_stamp_drains_immediately() {
        // No fence plumbed (tests, thumbnails, export): a 0 stamp with a 0
        // signaled value releases on the next drain — the pre-fix behavior.
        let device = crate::test_device();
        let mut retiring = GpuRetiring::new();
        retiring.retire(make_texture(&device, "rnnr-proof-zero"), 0);
        retiring.drain(0);
        assert_eq!(retiring.live_count(), 0);
    }

    #[test]
    fn overflow_never_grows_past_capacity() {
        let device = crate::test_device();
        let mut retiring = GpuRetiring::new();
        for i in 0..(RETIRING_CAPACITY + 4) {
            retiring.retire(make_texture(&device, "rnnr-proof-overflow"), u64::MAX);
        }
        // The 17th+ retire with an unreachable stamp frees immediately —
        // the backlog stays at capacity instead of growing or blocking.
        assert_eq!(retiring.live_count(), RETIRING_CAPACITY);
        retiring.drain(u64::MAX);
        assert_eq!(retiring.live_count(), 0);
    }
}
