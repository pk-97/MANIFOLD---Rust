/// Pool of reusable RenderTargets keyed by (width, height, format).
///
/// Effects like WireframeDepth and BlobTracking create temporary RenderTargets
/// per frame. Without pooling, this creates GPU allocation churn (measured:
/// 1.55 GiB over 5 seconds). The pool caches released textures and returns
/// them on the next `get()` call with matching dimensions and format.
///
/// Usage:
///   let rt = pool.get(device, 1920, 1080, Rgba16Float, "MyTemp");
///   // ... use rt ...
///   pool.release(rt);  // returns to pool for reuse
use ahash::AHashMap;
use crate::render_target::RenderTarget;

type PoolKey = (u32, u32, wgpu::TextureFormat);

pub struct RenderTargetPool {
    available: AHashMap<PoolKey, Vec<RenderTarget>>,
}

impl RenderTargetPool {
    pub fn new() -> Self {
        Self {
            available: AHashMap::new(),
        }
    }

    /// Get a RenderTarget from the pool, or create a new one if none available.
    pub fn get(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        label: &str,
    ) -> RenderTarget {
        let key = (width, height, format);
        if let Some(vec) = self.available.get_mut(&key)
            && let Some(rt) = vec.pop()
        {
            return rt;
        }
        RenderTarget::new(device, width, height, format, label)
    }

    /// Return a RenderTarget to the pool for future reuse.
    pub fn release(&mut self, rt: RenderTarget) {
        let key = (rt.width, rt.height, rt.format);
        self.available.entry(key).or_default().push(rt);
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
