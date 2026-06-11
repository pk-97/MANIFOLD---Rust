//! Frame-stamped texture recycling pool.
//!
//! Matches Unity's `RenderTexture.GetTemporary()` / `ReleaseTemporary()` pattern.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::MTLDevice;

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
        let mtl_device =
            unsafe { Retained::retain(dev_ptr) }.expect("MTLDevice retain returned nil");
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

    /// Total bytes currently cached in the free pool (each entry counted at
    /// `w * h * bytes_per_pixel`; mip-free 2D, so this is the real backing size
    /// modulo Metal's internal alignment). The live, in-use set is NOT counted —
    /// held textures aren't in the pool — so this is purely reclaimable slack.
    pub fn cached_bytes(&self) -> u64 {
        let inner = unsafe { &*self.inner.get() };
        inner
            .available
            .iter()
            .map(|((w, h, fmt), entries)| {
                (*w as u64) * (*h as u64) * (fmt.bytes_per_pixel() as u64) * entries.len() as u64
            })
            .sum()
    }

    /// Evict every FREE entry whose resolution differs from `(keep_w, keep_h)`,
    /// returning `(textures_freed, bytes_freed)`. The conservative fix for the
    /// canvas-change leak: when the render resolution moves, old-resolution
    /// entries can never be recycled again (acquire keys on the new dims) yet
    /// linger until the slow `prune_stale` ages them out — dead 4K allocations.
    /// This reclaims them immediately while KEEPING any entry that already
    /// matches the new resolution warm. Safe by construction: the pool only ever
    /// holds RELEASED textures, so this can't touch a persistent (feedback-state)
    /// or sticky (memo-held) resource — those are held by the executor and never
    /// returned here. A dropped texture still referenced by an in-flight command
    /// buffer stays alive until GPU completion (Metal retains command-buffer
    /// resources), so dropping mid-flight is safe.
    pub fn evict_resolution_mismatch(&self, keep_w: u32, keep_h: u32) -> (usize, u64) {
        let inner = unsafe { &mut *self.inner.get() };
        let mut freed_count = 0usize;
        let mut freed_bytes = 0u64;
        inner.available.retain(|(w, h, fmt), entries| {
            if *w == keep_w && *h == keep_h {
                return true;
            }
            freed_count += entries.len();
            freed_bytes +=
                (*w as u64) * (*h as u64) * (fmt.bytes_per_pixel() as u64) * entries.len() as u64;
            false
        });
        if freed_count > 0 {
            log::info!(
                "TexturePool: evicted {} old-resolution textures ({:.1} MiB) on change to {}x{}",
                freed_count,
                freed_bytes as f64 / (1024.0 * 1024.0),
                keep_w,
                keep_h,
            );
        }
        (freed_count, freed_bytes)
    }

    /// Human-readable breakdown of the free pool: one line per `(w, h, format)`
    /// key with the cached count, bytes, and oldest/newest release age in frames,
    /// then totals + the lifetime allocate/recycle counters. A lasting diagnostic
    /// — call it behind an env-var or a profiling mode, never per-frame. Identifies
    /// what's dead (large old-resolution keys with high age) and why (low recycle
    /// ratio ⇒ churn, high age ⇒ never reused since a resolution/project change).
    pub fn report(&self) -> String {
        use std::fmt::Write;
        let inner = unsafe { &*self.inner.get() };
        let mut keys: Vec<&PoolKey> = inner.available.keys().collect();
        keys.sort_by_key(|(w, h, _)| std::cmp::Reverse((*w as u64) * (*h as u64)));
        let mut out = String::new();
        let _ = writeln!(
            out,
            "--- TexturePool report (frame {}, {} frames in flight) ---",
            inner.current_frame, inner.frames_in_flight
        );
        let mut total_bytes = 0u64;
        let mut total_count = 0usize;
        for key in &keys {
            let entries = &inner.available[*key];
            if entries.is_empty() {
                continue;
            }
            let (w, h, fmt) = **key;
            let bytes = (w as u64) * (h as u64) * (fmt.bytes_per_pixel() as u64) * entries.len() as u64;
            total_bytes += bytes;
            total_count += entries.len();
            let oldest = entries
                .iter()
                .map(|e| inner.current_frame.saturating_sub(e.release_frame))
                .max()
                .unwrap_or(0);
            let newest = entries
                .iter()
                .map(|e| inner.current_frame.saturating_sub(e.release_frame))
                .min()
                .unwrap_or(0);
            let _ = writeln!(
                out,
                "  {:>5}x{:<5} {:<14?} x{:<3} {:>8.2} MiB   age {}..{} frames",
                w,
                h,
                fmt,
                entries.len(),
                bytes as f64 / (1024.0 * 1024.0),
                newest,
                oldest,
            );
        }
        let _ = writeln!(
            out,
            "  TOTAL cached: {} textures, {:.1} MiB   (cap {})   lifetime: {} allocated / {} recycled",
            total_count,
            total_bytes as f64 / (1024.0 * 1024.0),
            MAX_POOL_TEXTURES,
            inner.stats_allocated,
            inner.stats_recycled,
        );
        out
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
