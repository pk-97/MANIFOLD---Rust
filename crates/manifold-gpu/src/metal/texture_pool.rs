//! Frame-stamped texture recycling pool.
//!
//! Matches Unity's `RenderTexture.GetTemporary()` / `ReleaseTemporary()` pattern.

use crate::types::*;
use super::*;

/// Frame-stamped texture recycling pool.
///
/// Matches Unity's `RenderTexture.GetTemporary()` / `ReleaseTemporary()` pattern.
/// Textures are recycled by (width, height, format) key, but only after enough
/// frames have passed to guarantee the GPU is done reading them.
///
/// **Frame-stamped lifetime:** each released texture is tagged with the frame
/// it was released on. `acquire()` only recycles textures released at least
/// `frames_in_flight` frames ago. This prevents inter-frame GPU aliasing —
/// the same protection Unity/Unreal use internally.
///
/// After a warmup period (= frames_in_flight), allocation count drops to zero
/// at steady state. All textures are recycled, no kernel calls.
///
/// Uses interior mutability (UnsafeCell) — safe because TexturePool is only
/// used on the content thread (single-threaded).
pub struct TexturePool {
    inner: std::cell::UnsafeCell<TexturePoolInner>,
}

type PoolKey = (u32, u32, GpuTextureFormat);

/// A released texture waiting to be recycled, tagged with the frame it was
/// released on. Only eligible for reuse after `frames_in_flight` frames.
struct PoolEntry {
    texture: GpuTexture,
    release_frame: u64,
}

struct TexturePoolInner {
    available: std::collections::HashMap<PoolKey, Vec<PoolEntry>>,
    /// Owned clone of the Metal device for allocation.
    /// metal::Device is a refcounted ObjC object — clone is just a retain.
    device: metal::Device,
    /// Current frame number, incremented by begin_frame().
    current_frame: u64,
    /// Number of frames that can execute concurrently on the GPU.
    /// Textures are only recycled after this many frames have passed.
    frames_in_flight: u64,
    /// New allocations via device.create_texture().
    stats_allocated: u64,
    /// Textures recycled from pool (avoided allocation).
    stats_recycled: u64,
}

// Safety: TexturePool is only used on the content thread (single-threaded).
unsafe impl Send for TexturePool {}
unsafe impl Sync for TexturePool {}

impl TexturePool {
    /// Create a new texture pool with frame-stamped recycling.
    /// `frames_in_flight` = max concurrent GPU frames (typically 3).
    pub fn new(device: &GpuDevice, frames_in_flight: u64) -> Self {
        let mtl_device = device.raw_device().to_owned();
        log::info!(
            "TexturePool: frame-stamped recycling, {} frames in flight",
            frames_in_flight,
        );
        Self {
            inner: std::cell::UnsafeCell::new(TexturePoolInner {
                available: std::collections::HashMap::new(),
                device: mtl_device,
                current_frame: 0,
                frames_in_flight,
                stats_allocated: 0,
                stats_recycled: 0,
            }),
        }
    }

    /// Mark the start of a new frame. Must be called once per frame before
    /// any acquire/release calls. Drives the frame-stamp recycling clock.
    pub fn begin_frame(&self) {
        let inner = unsafe { &mut *self.inner.get() };
        inner.current_frame += 1;
    }

    /// Acquire a texture, recycling one if a safe match is available.
    /// Only recycles textures released >= `frames_in_flight` frames ago,
    /// guaranteeing the GPU has finished reading them.
    /// Falls back to `device.create_texture()` if no safe match exists.
    pub fn acquire(
        &self,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        usage: GpuTextureUsage,
        _label: &str,
    ) -> GpuTexture {
        let inner = unsafe { &mut *self.inner.get() };
        let key = (width, height, format);

        // Try to recycle a texture that's old enough to be safe.
        if let Some(vec) = inner.available.get_mut(&key)
            && let Some(idx) = vec.iter().position(|entry| {
                inner.current_frame.saturating_sub(entry.release_frame)
                    >= inner.frames_in_flight
            })
        {
            inner.stats_recycled += 1;
            return vec.swap_remove(idx).texture;
        }

        // No safe recycled texture — allocate fresh via device.
        inner.stats_allocated += 1;
        let desc = GpuTextureDesc {
            width,
            height,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage,
            label: _label,
        };
        let mtl_desc = GpuDevice::build_mtl_texture_desc(&desc);
        let raw = inner.device.new_texture(&mtl_desc);
        GpuTexture {
            raw,
            width,
            height,
            depth: 1,
            format,
        }
    }

    /// Return a texture to the pool for future reuse.
    /// Tagged with the current frame — won't be recycled until
    /// `frames_in_flight` frames have passed.
    pub fn release(&self, texture: GpuTexture) {
        let inner = unsafe { &mut *self.inner.get() };
        let key = (texture.width, texture.height, texture.format);
        inner.available.entry(key).or_default().push(PoolEntry {
            texture,
            release_frame: inner.current_frame,
        });
    }

    /// Release all cached textures. Call on resolution change or shutdown.
    pub fn clear(&self) {
        let inner = unsafe { &mut *self.inner.get() };
        inner.available.clear();
    }

    /// Pool statistics: (total_allocated, total_recycled).
    pub fn stats(&self) -> (u64, u64) {
        let inner = unsafe { &*self.inner.get() };
        (inner.stats_allocated, inner.stats_recycled)
    }

    /// Number of textures currently cached in the pool.
    pub fn cached_count(&self) -> usize {
        let inner = unsafe { &*self.inner.get() };
        inner.available.values().map(|v| v.len()).sum()
    }

    /// Current frame number.
    pub fn current_frame(&self) -> u64 {
        let inner = unsafe { &*self.inner.get() };
        inner.current_frame
    }

    /// Remove textures that have been sitting in the pool unreused for
    /// `stale_frames` frames. Prevents GPU memory from growing monotonically
    /// after resolution changes or project switches.
    pub fn prune_stale(&self, stale_frames: u64) {
        let inner = unsafe { &mut *self.inner.get() };
        let threshold = inner.current_frame.saturating_sub(stale_frames);
        let mut pruned = 0u64;
        inner.available.retain(|_key, entries| {
            let before = entries.len();
            entries.retain(|entry| entry.release_frame >= threshold);
            pruned += (before - entries.len()) as u64;
            !entries.is_empty()
        });
        if pruned > 0 {
            log::debug!(
                "TexturePool: pruned {} stale textures (threshold={})",
                pruned,
                stale_frames,
            );
        }
    }
}
