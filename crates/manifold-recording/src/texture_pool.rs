//! Pre-allocated fixed-size texture ring pool for recording.
//!
//! The content thread acquires a slot (non-blocking), blits into its texture,
//! and submits the raw texture pointer to the recording thread. The recording
//! thread releases the slot after encoding. If the pool is exhausted, the
//! content thread drops the frame.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

/// Default number of textures in the pool.
pub const DEFAULT_POOL_SIZE: usize = 8;

/// A handle to an acquired pool slot. Carries the raw texture pointer (for FFI)
/// and a release flag. Must be released after encoding.
pub struct PoolSlot {
    /// Raw `id<MTLTexture>` pointer for the recording thread's FFI encoder.
    pub raw_ptr: *mut c_void,
    /// Availability flag — set back to `true` on release.
    released: Arc<AtomicBool>,
}

// Raw pointer is to a Metal texture that outlives the pool.
// The recording thread only reads via FFI (the encoder does its own copy).
unsafe impl Send for PoolSlot {}

impl PoolSlot {
    /// Return this slot to the pool. Called by the recording thread after encoding.
    pub fn release(self) {
        self.released.store(true, Ordering::Release);
    }
}

/// Fixed-size pool of pre-allocated Metal textures.
pub struct TextureRingPool {
    /// Owned textures. Content thread accesses by index for blitting.
    textures: Vec<GpuTexture>,
    /// Per-slot availability flag. `true` = available for the content thread.
    available: Vec<Arc<AtomicBool>>,
    /// Next slot to try acquiring.
    next_acquire: AtomicUsize,
}

impl TextureRingPool {
    /// Pre-allocate `count` textures at the given dimensions and format.
    pub fn new(
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        count: usize,
    ) -> Self {
        let mut textures = Vec::with_capacity(count);
        let mut available = Vec::with_capacity(count);

        for i in 0..count {
            let label = format!("RecordingPool[{i}]");
            let desc = GpuTextureDesc {
                width,
                height,
                depth: 1,
                format,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_DST
                    | GpuTextureUsage::STORAGE_BINDING,
                label: &label,
                mip_levels: 1,
            };
            textures.push(device.create_texture(&desc));
            available.push(Arc::new(AtomicBool::new(true)));
        }

        log::info!(
            "[TextureRingPool] Allocated {count} textures: {width}x{height} {format:?}",
        );

        Self {
            textures,
            available,
            next_acquire: AtomicUsize::new(0),
        }
    }

    /// Try to acquire a pool slot. Returns the slot index and a `PoolSlot`
    /// handle (carries the raw pointer + release flag). Returns `None` if all
    /// textures are in-flight (pool exhaustion — drop this frame).
    ///
    /// The caller uses the index to access the texture for blitting via
    /// [`texture()`], then sends the `PoolSlot` to the recording thread.
    pub fn try_acquire(&self) -> Option<(usize, PoolSlot)> {
        let count = self.textures.len();
        let start = self.next_acquire.load(Ordering::Relaxed);

        for offset in 0..count {
            let idx = (start + offset) % count;

            if self.available[idx]
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                self.next_acquire
                    .store((idx + 1) % count, Ordering::Relaxed);

                let slot = PoolSlot {
                    raw_ptr: self.textures[idx].raw_ptr(),
                    released: self.available[idx].clone(),
                };

                return Some((idx, slot));
            }
        }

        None
    }

    /// Get a reference to the texture at the given index (for content thread blitting).
    pub fn texture(&self, index: usize) -> &GpuTexture {
        &self.textures[index]
    }

    /// Number of textures in the pool.
    #[allow(dead_code)]
    pub fn capacity(&self) -> usize {
        self.textures.len()
    }
}
