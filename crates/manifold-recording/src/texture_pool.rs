//! Pre-allocated fixed-size texture ring pool for recording.
//!
//! The content thread acquires a slot (non-blocking), blits into its texture,
//! and submits the slot to the recording thread: the raw texture on the Mac,
//! the texture's readback buffer elsewhere. The recording
//! thread releases the slot after encoding. If the pool is exhausted, the
//! content thread drops the frame.

#[cfg(target_os = "macos")]
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(not(target_os = "macos"))]
use manifold_gpu::{BufferUse, GpuBuffer};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

/// Default number of textures in the pool.
pub const DEFAULT_POOL_SIZE: usize = 8;

/// A handle to an acquired pool slot. Carries the frame for the recorder and
/// a release flag. Must be released after encoding.
pub struct PoolSlot {
    /// Raw `id<MTLTexture>` pointer for the recording thread's FFI encoder.
    #[cfg(target_os = "macos")]
    pub raw_ptr: *mut c_void,
    /// The slot's mapped readback buffer: `width × height` RGBA texels.
    #[cfg(not(target_os = "macos"))]
    pub pixels: *const u8,
    /// Availability flag — set back to `true` on release.
    released: Arc<AtomicBool>,
}

// The pointer is to a texture or mapped buffer the pool owns, and the pool
// outlives the recording thread. The recording thread only reads it.
unsafe impl Send for PoolSlot {}

impl PoolSlot {
    /// Return this slot to the pool. Called by the recording thread after encoding.
    pub fn release(self) {
        self.released.store(true, Ordering::Release);
    }
}

/// Fixed-size pool of pre-allocated textures.
pub struct TextureRingPool {
    /// Owned textures. Content thread accesses by index for blitting.
    textures: Vec<GpuTexture>,
    /// One readback buffer per texture, which the recorder reads.
    #[cfg(not(target_os = "macos"))]
    readback: Vec<GpuBuffer>,
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
        #[cfg(not(target_os = "macos"))]
        let readback = (0..count)
            .map(|_| {
                device.create_buffer(
                    u64::from(width * height * format.bytes_per_pixel()),
                    BufferUse::Readback,
                )
            })
            .collect();

        for i in 0..count {
            let label = format!("RecordingPool[{i}]");
            let desc = GpuTextureDesc {
                width,
                height,
                depth: 1,
                format,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ
                    | GpuTextureUsage::SHADER_WRITE
                    | GpuTextureUsage::COPY_SRC,
                label: &label,
                mip_levels: 1,
            };
            textures.push(device.create_texture(&desc));
            available.push(Arc::new(AtomicBool::new(true)));
        }

        log::info!("[TextureRingPool] Allocated {count} textures: {width}x{height} {format:?}",);

        Self {
            textures,
            #[cfg(not(target_os = "macos"))]
            readback,
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
                    #[cfg(target_os = "macos")]
                    raw_ptr: self.textures[idx].raw_ptr(),
                    #[cfg(not(target_os = "macos"))]
                    pixels: self.readback[idx]
                        .mapped_ptr()
                        .expect("readback buffers are mapped")
                        .cast_const(),
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

    /// The readback buffer the recorder reads for the texture at `index`.
    #[cfg(not(target_os = "macos"))]
    pub fn readback(&self, index: usize) -> &GpuBuffer {
        &self.readback[index]
    }
}
