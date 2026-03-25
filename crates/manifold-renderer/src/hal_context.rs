//! HAL context — extracts wgpu_hal Device + Queue from wgpu for direct encoding.
//!
//! Created once at startup. The wgpu Device/Queue must outlive this context
//! (enforced by construction: both are app-lifetime in GpuContext).

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
mod inner {
    use wgpu::hal::{self, Device as HalDevice};

    type MetalApi = hal::api::Metal;
    pub type MetalDevice = <MetalApi as hal::Api>::Device;
    pub type MetalQueue = <MetalApi as hal::Api>::Queue;
    pub type MetalCommandEncoder = <MetalApi as hal::Api>::CommandEncoder;
    pub type MetalCommandBuffer = <MetalApi as hal::Api>::CommandBuffer;
    pub type MetalFence = <MetalApi as hal::Api>::Fence;
    pub type MetalTexture = <MetalApi as hal::Api>::Texture;
    pub type MetalTextureView = <MetalApi as hal::Api>::TextureView;
    pub type MetalBuffer = <MetalApi as hal::Api>::Buffer;
    pub type MetalSampler = <MetalApi as hal::Api>::Sampler;
    pub type MetalBindGroup = <MetalApi as hal::Api>::BindGroup;

    /// Cached references to the underlying hal Metal device and queue.
    ///
    /// # Safety
    ///
    /// The raw pointers are valid for the lifetime of the wgpu Device/Queue they
    /// were extracted from. In MANIFOLD, both live in `GpuContext` for the entire
    /// app lifetime. The wgpu Device is never explicitly destroyed.
    pub struct HalContext {
        device_ptr: *const MetalDevice,
        queue_ptr: *const MetalQueue,
    }

    // Safety: the underlying Metal device/queue are Send+Sync.
    // The raw pointers point to wgpu-managed objects that are themselves Send+Sync.
    unsafe impl Send for HalContext {}
    unsafe impl Sync for HalContext {}

    impl HalContext {
        /// Extract hal Device + Queue from wgpu objects.
        ///
        /// # Safety
        ///
        /// The wgpu Device and Queue must outlive this HalContext. Both must be
        /// backed by the Metal backend.
        pub unsafe fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
            // as_hal returns an RAII guard with Deref<Target = MetalDevice>.
            // We extract the raw pointer and drop the guard. The underlying Metal
            // device is refcounted by wgpu and alive for the app lifetime.
            let device_guard = unsafe {
                device
                    .as_hal::<MetalApi>()
                    .expect("HalContext requires Metal backend")
            };
            let device_ptr: *const MetalDevice = &*device_guard;

            let queue_guard = unsafe {
                queue
                    .as_hal::<MetalApi>()
                    .expect("HalContext requires Metal backend")
            };
            let queue_ptr: *const MetalQueue = &*queue_guard;

            Self { device_ptr, queue_ptr }
        }

        pub fn device(&self) -> &MetalDevice {
            // Safety: pointer valid for app lifetime (see struct doc).
            unsafe { &*self.device_ptr }
        }

        pub fn queue(&self) -> &MetalQueue {
            // Safety: pointer valid for app lifetime (see struct doc).
            unsafe { &*self.queue_ptr }
        }

        /// Create a new hal command encoder for this frame.
        pub fn create_command_encoder(&self) -> MetalCommandEncoder {
            let desc = hal::CommandEncoderDescriptor {
                label: None,
                queue: self.queue(),
            };
            unsafe {
                self.device()
                    .create_command_encoder(&desc)
                    .expect("Failed to create hal command encoder")
            }
        }

        /// Create a hal fence for GPU-CPU synchronization.
        pub fn create_fence(&self) -> MetalFence {
            unsafe {
                self.device()
                    .create_fence()
                    .expect("Failed to create hal fence")
            }
        }
    }
}

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
pub use inner::*;

/// Stub HalContext when hal-encoding is not available.
/// Zero-sized type — `Option<&HalContext>` is always None and costs nothing.
/// This allows all constructors to have a stable signature regardless of feature.
#[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
pub struct HalContext {
    _private: (),
}

