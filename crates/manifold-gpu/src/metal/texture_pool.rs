//! Frame-stamped texture recycling pool.
//!
//! Matches Unity's `RenderTexture.GetTemporary()` / `ReleaseTemporary()` pattern.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLDevice, MTLTexture};

use super::*;
use crate::types::*;

/// Frame-stamped texture recycling pool.
pub struct TexturePool {
    inner: std::cell::UnsafeCell<TexturePoolInner>,
}

/// Maximum number of textures cached in the pool.
const MAX_POOL_TEXTURES: usize = 128;

type PoolKey = (u32, u32, GpuTextureFormat);

struct PoolEntry {
    texture: GpuTexture,
    release_frame: u64,
}

struct TexturePoolInner {
    available: std::collections::HashMap<PoolKey, Vec<PoolEntry>>,
    /// Retained Metal device handle for allocation.
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    /// Current frame number, incremented by begin_frame().
    current_frame: u64,
    /// Number of frames that can execute concurrently on the GPU.
    frames_in_flight: u64,
    /// New allocations via device.create_texture().
    stats_allocated: u64,
    /// Textures recycled from pool (avoided allocation).
    stats_recycled: u64,
}

// Safety: TexturePool is only used on the content thread (single-threaded).
unsafe impl Send for TexturePool {}

impl TexturePool {
    /// Create a new texture pool with frame-stamped recycling.
    pub fn new(device: &GpuDevice, frames_in_flight: u64) -> Self {
        let dev_ptr = device.raw_device() as *const _ as *mut ProtocolObject<dyn MTLDevice>;
        let mtl_device = unsafe { Retained::retain(dev_ptr) }
            .expect("MTLDevice retain returned nil");
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

    /// Mark the start of a new frame.
    pub fn begin_frame(&self) {
        let inner = unsafe { &mut *self.inner.get() };
        inner.current_frame += 1;
    }

    /// Acquire a texture, recycling one if a safe match is available.
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
                inner.current_frame.saturating_sub(entry.release_frame) >= inner.frames_in_flight
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
            mip_levels: 1,
        };
        let mtl_desc = GpuDevice::build_mtl_texture_desc(&desc);
        let raw = inner
            .device
            .newTextureWithDescriptor(&mtl_desc)
            .expect("Metal: TexturePool allocation failed — GPU memory exhausted");
        GpuTexture {
            raw,
            width,
            height,
            depth: 1,
            format,
        }
    }

    /// Return a texture to the pool for future reuse.
    pub fn release(&self, texture: GpuTexture) {
        let inner = unsafe { &mut *self.inner.get() };
        let total: usize = inner.available.values().map(|v| v.len()).sum();
        if total >= MAX_POOL_TEXTURES {
            return;
        }
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
    /// `stale_frames` frames.
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
