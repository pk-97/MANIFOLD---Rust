use crate::render_target::RenderTarget;
/// Pool of reusable RenderTargets keyed by (width, height, format).
///
/// When a `TexturePool` (MTLHeap-backed) is available, delegates to it for
/// heap sub-allocation and recycling. Falls back to direct device allocation.
///
/// Usage:
///   let rt = pool.get(device, 1920, 1080, Rgba16Float, "MyTemp");
///   // ... use rt ...
///   pool.release(rt);  // returns to pool for reuse
use ahash::AHashMap;
use manifold_gpu::{GpuDevice, GpuTextureFormat, TexturePool};

/// `(width, height, format, mip_levels)` — mip-chained targets
/// (IMPORT_FIDELITY F-P6) recycle separately from flat ones so a mipped
/// request never receives a flat texture or vice versa.
type PoolKey = (u32, u32, GpuTextureFormat, u32);

pub struct RenderTargetPool {
    /// Local cache for render targets not backed by TexturePool.
    available: AHashMap<PoolKey, Vec<RenderTarget>>,
    /// Optional heap-backed texture pool (set during init).
    texture_pool: Option<*const TexturePool>,
}

// Safety: only used on the content thread.
unsafe impl Send for RenderTargetPool {}

impl RenderTargetPool {
    pub fn new() -> Self {
        Self {
            available: AHashMap::new(),
            texture_pool: None,
        }
    }

    /// Set the backing TexturePool for heap-backed allocation.
    pub fn set_texture_pool(&mut self, pool: &TexturePool) {
        self.texture_pool = Some(pool as *const TexturePool);
    }

    /// Get a RenderTarget from the pool, or create a new one if none available.
    /// `mipmapped` requests a full mip chain (IMPORT_FIDELITY F-P6) —
    /// those allocate directly on the device (the heap `TexturePool`
    /// recycles by `(w, h, format)` only and would alias flat textures)
    /// and recycle through the local cache under their own key.
    pub fn get(
        &mut self,
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        mipmapped: bool,
        label: &str,
    ) -> RenderTarget {
        let mip_levels = if mipmapped {
            crate::render_target::full_mip_chain_len(width, height)
        } else {
            1
        };
        // Check local cache first.
        let key = (width, height, format, mip_levels);
        if let Some(vec) = self.available.get_mut(&key)
            && let Some(rt) = vec.pop()
        {
            return rt;
        }
        if mipmapped {
            return RenderTarget::new_mipmapped(device, width, height, format, label);
        }
        // Allocate via TexturePool (heap) if available, else direct device.
        if let Some(pool_ptr) = self.texture_pool {
            let pool = unsafe { &*pool_ptr };
            RenderTarget::new_pooled(pool, width, height, format, label)
        } else {
            RenderTarget::new(device, width, height, format, label)
        }
    }

    /// Return a RenderTarget to the pool for future reuse.
    pub fn release(&mut self, rt: RenderTarget) {
        // Heap-pool release only for flat targets — mip-chained ones were
        // allocated directly (see `get`) and recycle via the local cache.
        if rt.mip_levels == 1
            && let Some(pool_ptr) = self.texture_pool
        {
            let pool = unsafe { &*pool_ptr };
            rt.release_to_pool(pool);
        } else {
            let key = (rt.width, rt.height, rt.format, rt.mip_levels);
            self.available.entry(key).or_default().push(rt);
        }
    }

    /// Release all pooled textures. Call on resize or cleanup.
    pub fn clear(&mut self) {
        self.available.clear();
    }
}

impl Default for RenderTargetPool {
    fn default() -> Self {
        Self::new()
    }
}
