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
        /// Fence used for hal queue submissions. Monotonically incrementing value.
        submit_fence: std::cell::UnsafeCell<MetalFence>,
        submit_fence_value: std::cell::Cell<u64>,
        /// MTLSharedEvent for frame completion signaling.
        /// CPU reads signaled_value() to check if a frame's GPU work is done —
        /// near-zero overhead vs device.poll() which goes through wgpu machinery.
        frame_event: metal::SharedEvent,
        /// Monotonically increasing value. Incremented each time we signal.
        frame_event_value: std::cell::Cell<u64>,
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

            let fence = unsafe {
                (&*device_ptr)
                    .create_fence()
                    .expect("Failed to create hal fence")
            };

            let frame_event = unsafe { &*device_ptr }.raw_device().new_shared_event();

            Self {
                device_ptr,
                queue_ptr,
                submit_fence: std::cell::UnsafeCell::new(fence),
                submit_fence_value: std::cell::Cell::new(0),
                frame_event,
                frame_event_value: std::cell::Cell::new(0),
            }
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

        /// Submit hal command buffers to the GPU queue.
        ///
        /// Uses an internal fence with monotonically incrementing values.
        /// Metal command queue guarantees in-order execution.
        ///
        /// # Safety
        ///
        /// Command buffers must be valid and fully encoded.
        pub unsafe fn submit(
            &self,
            command_buffers: &[&MetalCommandBuffer],
        ) {
            use wgpu::hal::Queue as HalQueue;
            let value = self.submit_fence_value.get() + 1;
            self.submit_fence_value.set(value);
            let fence = unsafe { &mut *self.submit_fence.get() };
            unsafe {
                self.queue()
                    .submit(command_buffers, &[], (fence, value))
                    .expect("hal submit failed");
            }
        }

        /// Signal frame completion via MTLSharedEvent.
        ///
        /// Creates a lightweight command buffer that encodes a signal event and
        /// commits it. Metal in-order queue execution guarantees the signal fires
        /// after all preceding work (compositor, IOSurface copy, readbacks).
        ///
        /// # Safety
        ///
        /// Must be called AFTER all encoder submissions for the frame.
        pub unsafe fn signal_frame_completion(&self) {
            let value = self.frame_event_value.get() + 1;
            self.frame_event_value.set(value);
            let raw_queue = self.queue().as_raw().lock();
            let cmd_buf = raw_queue.new_command_buffer_with_unretained_references();
            cmd_buf.set_label("frame signal");
            cmd_buf.encode_signal_event(&self.frame_event, value);
            cmd_buf.commit();
        }

        /// Check if the GPU has completed the frame at the given signal value.
        /// Direct GPU counter read — near-zero overhead.
        pub fn is_frame_done(&self, value: u64) -> bool {
            self.frame_event.signaled_value() >= value
        }

        /// Current signal value (store after signal_frame_completion).
        pub fn current_signal_value(&self) -> u64 {
            self.frame_event_value.get()
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

