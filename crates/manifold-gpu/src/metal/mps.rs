//! Metal Performance Shaders (MPS) integration for manifold-gpu.
//!
//! MPS kernels are Apple's hand-tuned GPU image processing primitives.
//! They encode directly into MTLCommandBuffer and operate on MTLTexture —
//! no intermediate copies.
//!
//! MPS kernels are created once per device (or per parameter set) and cached.
//! They are NOT per-dispatch objects — reuse them across frames.
//!
//! Uses `msg_send!` because the `metal` crate v0.33 only exposes
//! MPS ray tracing types, not image processing kernels.
//!
//! ## Why not objc2-metal-performance-shaders?
//!
//! The `objc2-metal-performance-shaders` crate (v0.3.2) provides typed bindings for
//! all MPS image kernels we use (MPSImageGaussianBlur, MPSImageSobel, MPSImageBox,
//! MPSImageTent, MPSImageLanczosScale). However, it uses `objc2-metal`'s type system
//! (`ProtocolObject<dyn MTLDevice>`, `ProtocolObject<dyn MTLCommandBuffer>`, etc.)
//! which is incompatible with our `metal` crate types. Using it would require either
//! unsafe pointer casting between the two type systems, or a full migration from the
//! `metal` crate to `objc2-metal` across all of manifold-gpu. The full migration is
//! a future task (see mod.rs header). Once that migration happens, MPS and MetalFX
//! bindings should be replaced with the typed objc2 crates.

use objc::runtime::{Class, Object, BOOL, YES};
use std::ffi::c_void;
use std::ptr;

use super::GpuTexture;

// ─── Link MPS framework ──────────────────────────────────────────────

#[link(name = "MetalPerformanceShaders", kind = "framework")]
unsafe extern "C" {
    fn MPSSupportsMTLDevice(device: *const c_void) -> BOOL;
}

/// Check if the device supports MPS.
pub fn mps_supports_device(device: &metal::DeviceRef) -> bool {
    let b: BOOL = unsafe {
        let ptr: *const metal::DeviceRef = device;
        MPSSupportsMTLDevice(ptr as *const c_void)
    };
    b == YES
}

// ─── Raw ObjC helpers ─────────────────────────────────────────────────

unsafe extern "C" {
    fn objc_retain(obj: *mut c_void) -> *mut c_void;
    fn objc_release(obj: *mut c_void);
}

/// Wrapper around an ObjC MPS kernel object. Retains on creation, releases on drop.
struct MpsObject {
    ptr: *mut Object,
}

impl MpsObject {
    /// Wrap a newly created (autoreleased) ObjC object. Retains it.
    unsafe fn from_raw(ptr: *mut Object) -> Self {
        assert!(!ptr.is_null(), "MPS kernel creation returned null");
        unsafe { objc_retain(ptr as *mut c_void); }
        Self { ptr }
    }

    fn as_ptr(&self) -> *mut Object {
        self.ptr
    }
}

impl Drop for MpsObject {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { objc_release(self.ptr as *mut c_void); }
        }
    }
}

// Safety: MPS kernels are thread-safe for encoding (Apple docs).
unsafe impl Send for MpsObject {}
unsafe impl Sync for MpsObject {}

// ─── Encode helpers ───────────────────────────────────────────────────

/// Encode a unary image kernel (src → dst) into a command buffer.
///
/// # Safety
/// `kernel` must be a valid MPSUnaryImageKernel subclass.
/// `cmd_buf`, `src`, `dst` must be valid Metal objects.
unsafe fn encode_unary(
    kernel: *mut Object,
    cmd_buf: &metal::CommandBufferRef,
    src: &metal::TextureRef,
    dst: &metal::TextureRef,
) {
    let _: () = unsafe {
        msg_send![kernel,
            encodeToCommandBuffer: cmd_buf as *const _ as *mut Object
            sourceTexture: src as *const _ as *mut Object
            destinationTexture: dst as *const _ as *mut Object
        ]
    };
}

/// Encode a binary image kernel (a + b → dst) into a command buffer.
///
/// # Safety
/// `kernel` must be a valid MPSBinaryImageKernel subclass.
unsafe fn encode_binary(
    kernel: *mut Object,
    cmd_buf: &metal::CommandBufferRef,
    primary: &metal::TextureRef,
    secondary: &metal::TextureRef,
    dst: &metal::TextureRef,
) {
    let _: () = unsafe {
        msg_send![kernel,
            encodeToCommandBuffer: cmd_buf as *const _ as *mut Object
            primaryTexture: primary as *const _ as *mut Object
            secondaryTexture: secondary as *const _ as *mut Object
            destinationTexture: dst as *const _ as *mut Object
        ]
    };
}

// ─── Scale transform for MPSImageScale ────────────────────────────────

/// Scale transform struct matching MPSScaleTransform.
#[repr(C)]
pub struct MpsScaleTransform {
    pub scale_x: f64,
    pub scale_y: f64,
    pub translate_x: f64,
    pub translate_y: f64,
}

// ─── MPS Kernel Wrappers ──────────────────────────────────────────────

// -- Blur --

/// MPSImageGaussianBlur — separable Gaussian blur optimized for Apple Silicon.
pub struct MpsGaussianBlur {
    inner: MpsObject,
}

impl MpsGaussianBlur {
    /// Create a Gaussian blur kernel with the given sigma.
    /// Sigma must be > 0. The kernel size is computed automatically.
    pub fn new(device: &metal::DeviceRef, sigma: f32) -> Self {
        let cls = Class::get("MPSImageGaussianBlur").expect("MPSImageGaussianBlur class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                sigma: sigma as f64
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    /// Encode blur: src → dst. Textures must be compatible formats.
    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }

    /// Get the current sigma value.
    pub fn sigma(&self) -> f32 {
        let s: f64 = unsafe { msg_send![self.inner.as_ptr(), sigma] };
        s as f32
    }
}

/// MPSImageBox — fast box blur.
pub struct MpsBoxBlur {
    inner: MpsObject,
}

impl MpsBoxBlur {
    pub fn new(device: &metal::DeviceRef, kernel_width: u32, kernel_height: u32) -> Self {
        let cls = Class::get("MPSImageBox").expect("MPSImageBox class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                kernelWidth: kernel_width as u64
                kernelHeight: kernel_height as u64
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageTent — tent (triangle) blur.
pub struct MpsTentBlur {
    inner: MpsObject,
}

impl MpsTentBlur {
    pub fn new(device: &metal::DeviceRef, kernel_width: u32, kernel_height: u32) -> Self {
        let cls = Class::get("MPSImageTent").expect("MPSImageTent class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                kernelWidth: kernel_width as u64
                kernelHeight: kernel_height as u64
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageMedian — median filter.
pub struct MpsMedian {
    inner: MpsObject,
}

impl MpsMedian {
    /// Kernel size must be odd and >= 3.
    pub fn new(device: &metal::DeviceRef, kernel_diameter: u32) -> Self {
        let cls = Class::get("MPSImageMedian").expect("MPSImageMedian class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                kernelDiameter: kernel_diameter as u64
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

// -- Scale --

/// MPSImageBilinearScale — bilinear texture scaling.
pub struct MpsBilinearScale {
    inner: MpsObject,
}

impl MpsBilinearScale {
    pub fn new(device: &metal::DeviceRef) -> Self {
        let cls = Class::get("MPSImageBilinearScale")
            .expect("MPSImageBilinearScale class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc, initWithDevice: device as *const _ as *mut Object]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    /// Set the scale transform before encoding.
    pub fn set_transform(&self, transform: &MpsScaleTransform) {
        let _: () = unsafe {
            msg_send![self.inner.as_ptr(),
                setScaleTransform: transform as *const MpsScaleTransform
            ]
        };
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageLanczosScale — high-quality Lanczos texture scaling.
pub struct MpsLanczosScale {
    inner: MpsObject,
}

impl MpsLanczosScale {
    pub fn new(device: &metal::DeviceRef) -> Self {
        let cls = Class::get("MPSImageLanczosScale")
            .expect("MPSImageLanczosScale class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc, initWithDevice: device as *const _ as *mut Object]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn set_transform(&self, transform: &MpsScaleTransform) {
        let _: () = unsafe {
            msg_send![self.inner.as_ptr(),
                setScaleTransform: transform as *const MpsScaleTransform
            ]
        };
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

// -- Edge / Feature Detection --

/// MPSImageSobel — Sobel edge detection.
pub struct MpsSobel {
    inner: MpsObject,
}

impl MpsSobel {
    pub fn new(device: &metal::DeviceRef) -> Self {
        let cls = Class::get("MPSImageSobel").expect("MPSImageSobel class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc, initWithDevice: device as *const _ as *mut Object]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageLaplacian — Laplacian edge detection.
pub struct MpsLaplacian {
    inner: MpsObject,
}

impl MpsLaplacian {
    pub fn new(device: &metal::DeviceRef) -> Self {
        let cls = Class::get("MPSImageLaplacian").expect("MPSImageLaplacian class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc, initWithDevice: device as *const _ as *mut Object]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageConvolution — arbitrary convolution kernel.
pub struct MpsConvolution {
    inner: MpsObject,
}

impl MpsConvolution {
    /// Create with a custom kernel. `weights` length must be `width * height`.
    pub fn new(
        device: &metal::DeviceRef,
        kernel_width: u32,
        kernel_height: u32,
        weights: &[f32],
    ) -> Self {
        assert_eq!(
            weights.len(),
            (kernel_width * kernel_height) as usize,
            "Convolution weights length must match kernel dimensions"
        );
        let cls = Class::get("MPSImageConvolution")
            .expect("MPSImageConvolution class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                kernelWidth: kernel_width as u64
                kernelHeight: kernel_height as u64
                weights: weights.as_ptr()
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

// -- Morphology --

/// MPSImageDilate — morphological dilation.
pub struct MpsDilate {
    inner: MpsObject,
}

impl MpsDilate {
    pub fn new(
        device: &metal::DeviceRef,
        kernel_width: u32,
        kernel_height: u32,
        values: &[f32],
    ) -> Self {
        assert_eq!(values.len(), (kernel_width * kernel_height) as usize);
        let cls = Class::get("MPSImageDilate").expect("MPSImageDilate class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                kernelWidth: kernel_width as u64
                kernelHeight: kernel_height as u64
                values: values.as_ptr()
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageErode — morphological erosion.
pub struct MpsErode {
    inner: MpsObject,
}

impl MpsErode {
    pub fn new(
        device: &metal::DeviceRef,
        kernel_width: u32,
        kernel_height: u32,
        values: &[f32],
    ) -> Self {
        assert_eq!(values.len(), (kernel_width * kernel_height) as usize);
        let cls = Class::get("MPSImageErode").expect("MPSImageErode class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                kernelWidth: kernel_width as u64
                kernelHeight: kernel_height as u64
                values: values.as_ptr()
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

// -- Threshold --

/// MPSImageThresholdBinary — binary threshold.
pub struct MpsThresholdBinary {
    inner: MpsObject,
}

impl MpsThresholdBinary {
    /// Pixels above `threshold` become `max_value`, below become 0.
    pub fn new(
        device: &metal::DeviceRef,
        threshold: f32,
        max_value: f32,
        linear_gray_color_transform: Option<&[f32; 3]>,
    ) -> Self {
        let transform = linear_gray_color_transform
            .map(|t| t.as_ptr())
            .unwrap_or(ptr::null());
        let cls = Class::get("MPSImageThresholdBinary")
            .expect("MPSImageThresholdBinary class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                thresholdValue: threshold
                maximumValue: max_value
                linearGrayColorTransform: transform
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageThresholdTruncate — truncate values above threshold.
pub struct MpsThresholdTruncate {
    inner: MpsObject,
}

impl MpsThresholdTruncate {
    pub fn new(
        device: &metal::DeviceRef,
        threshold: f32,
        linear_gray_color_transform: Option<&[f32; 3]>,
    ) -> Self {
        let transform = linear_gray_color_transform
            .map(|t| t.as_ptr())
            .unwrap_or(ptr::null());
        let cls = Class::get("MPSImageThresholdTruncate")
            .expect("MPSImageThresholdTruncate class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                thresholdValue: threshold
                linearGrayColorTransform: transform
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageThresholdToZero — zero values below threshold.
pub struct MpsThresholdToZero {
    inner: MpsObject,
}

impl MpsThresholdToZero {
    pub fn new(
        device: &metal::DeviceRef,
        threshold: f32,
        linear_gray_color_transform: Option<&[f32; 3]>,
    ) -> Self {
        let transform = linear_gray_color_transform
            .map(|t| t.as_ptr())
            .unwrap_or(ptr::null());
        let cls = Class::get("MPSImageThresholdToZero")
            .expect("MPSImageThresholdToZero class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                thresholdValue: threshold
                linearGrayColorTransform: transform
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

// -- Arithmetic --

/// Helper to create MPSImageArithmetic subclass kernels.
unsafe fn create_arithmetic(
    class_name: &str,
    device: &metal::DeviceRef,
) -> MpsObject {
    let cls = Class::get(class_name)
        .unwrap_or_else(|| panic!("{class_name} class not found"));
    let obj: *mut Object = unsafe {
        let alloc: *mut Object = msg_send![cls, alloc];
        msg_send![alloc, initWithDevice: device as *const _ as *mut Object]
    };
    unsafe { MpsObject::from_raw(obj) }
}

/// MPSImageAdd — element-wise addition of two textures.
pub struct MpsAdd {
    inner: MpsObject,
}

impl MpsAdd {
    pub fn new(device: &metal::DeviceRef) -> Self {
        Self { inner: unsafe { create_arithmetic("MPSImageAdd", device) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        primary: &metal::TextureRef,
        secondary: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_binary(self.inner.as_ptr(), cmd_buf, primary, secondary, dst); }
    }
}

/// MPSImageSubtract — element-wise subtraction (primary - secondary).
pub struct MpsSubtract {
    inner: MpsObject,
}

impl MpsSubtract {
    pub fn new(device: &metal::DeviceRef) -> Self {
        Self { inner: unsafe { create_arithmetic("MPSImageSubtract", device) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        primary: &metal::TextureRef,
        secondary: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_binary(self.inner.as_ptr(), cmd_buf, primary, secondary, dst); }
    }
}

/// MPSImageMultiply — element-wise multiplication.
pub struct MpsMultiply {
    inner: MpsObject,
}

impl MpsMultiply {
    pub fn new(device: &metal::DeviceRef) -> Self {
        Self { inner: unsafe { create_arithmetic("MPSImageMultiply", device) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        primary: &metal::TextureRef,
        secondary: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_binary(self.inner.as_ptr(), cmd_buf, primary, secondary, dst); }
    }
}

/// MPSImageDivide — element-wise division (primary / secondary).
pub struct MpsDivide {
    inner: MpsObject,
}

impl MpsDivide {
    pub fn new(device: &metal::DeviceRef) -> Self {
        Self { inner: unsafe { create_arithmetic("MPSImageDivide", device) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        primary: &metal::TextureRef,
        secondary: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_binary(self.inner.as_ptr(), cmd_buf, primary, secondary, dst); }
    }
}

// -- Statistics --

/// MPSImageStatisticsMinAndMax — compute min/max of an image.
pub struct MpsMinMax {
    inner: MpsObject,
}

impl MpsMinMax {
    pub fn new(device: &metal::DeviceRef) -> Self {
        let cls = Class::get("MPSImageStatisticsMinAndMax")
            .expect("MPSImageStatisticsMinAndMax class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc, initWithDevice: device as *const _ as *mut Object]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    /// Encode: result is written to a 2-pixel texture (min, max).
    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageAreaMax — maximum value in a rectangular region.
pub struct MpsAreaMax {
    inner: MpsObject,
}

impl MpsAreaMax {
    pub fn new(device: &metal::DeviceRef, kernel_width: u32, kernel_height: u32) -> Self {
        let cls = Class::get("MPSImageAreaMax")
            .expect("MPSImageAreaMax class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                kernelWidth: kernel_width as u64
                kernelHeight: kernel_height as u64
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageAreaMin — minimum value in a rectangular region.
pub struct MpsAreaMin {
    inner: MpsObject,
}

impl MpsAreaMin {
    pub fn new(device: &metal::DeviceRef, kernel_width: u32, kernel_height: u32) -> Self {
        let cls = Class::get("MPSImageAreaMin")
            .expect("MPSImageAreaMin class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                kernelWidth: kernel_width as u64
                kernelHeight: kernel_height as u64
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageIntegral — summed area table (integral image).
pub struct MpsIntegral {
    inner: MpsObject,
}

impl MpsIntegral {
    pub fn new(device: &metal::DeviceRef) -> Self {
        let cls = Class::get("MPSImageIntegral")
            .expect("MPSImageIntegral class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc, initWithDevice: device as *const _ as *mut Object]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

/// MPSImageIntegralOfSquares — summed area table of squared values.
pub struct MpsIntegralOfSquares {
    inner: MpsObject,
}

impl MpsIntegralOfSquares {
    pub fn new(device: &metal::DeviceRef) -> Self {
        let cls = Class::get("MPSImageIntegralOfSquares")
            .expect("MPSImageIntegralOfSquares class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc, initWithDevice: device as *const _ as *mut Object]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

// -- Histogram --

/// MPSImageHistogram — compute histogram of an image into a buffer.
pub struct MpsHistogram {
    inner: MpsObject,
}

/// Histogram info matching MPSImageHistogramInfo.
#[repr(C)]
pub struct MpsHistogramInfo {
    pub number_of_histogram_entries: u64,
    pub histogram_for_alpha: BOOL,
    pub min_pixel_value: [f32; 4],
    pub max_pixel_value: [f32; 4],
}

impl MpsHistogram {
    pub fn new(device: &metal::DeviceRef, info: &MpsHistogramInfo) -> Self {
        let cls = Class::get("MPSImageHistogram")
            .expect("MPSImageHistogram class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                histogramInfo: info as *const MpsHistogramInfo
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    /// Encode histogram computation. Result goes into `histogram_buffer` at offset 0.
    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        histogram_buffer: &metal::BufferRef,
        histogram_offset: u64,
    ) {
        let _: () = unsafe {
            msg_send![self.inner.as_ptr(),
                encodeToCommandBuffer: cmd_buf as *const _ as *mut Object
                sourceTexture: src as *const _ as *mut Object
                histogram: histogram_buffer as *const _ as *mut Object
                histogramOffset: histogram_offset
            ]
        };
    }

    /// Returns the size in bytes needed for the histogram buffer.
    pub fn histogram_size(&self) -> u64 {
        unsafe { msg_send![self.inner.as_ptr(), histogramSize] }
    }
}

/// MPSImageHistogramEqualization — equalize image histogram.
pub struct MpsHistogramEqualization {
    inner: MpsObject,
}

impl MpsHistogramEqualization {
    pub fn new(device: &metal::DeviceRef, info: &MpsHistogramInfo) -> Self {
        let cls = Class::get("MPSImageHistogramEqualization")
            .expect("MPSImageHistogramEqualization class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                histogramInfo: info as *const MpsHistogramInfo
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    /// Must call this before encode() to provide the histogram data.
    pub fn encode_transform(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        histogram_buffer: &metal::BufferRef,
        histogram_offset: u64,
    ) {
        let _: () = unsafe {
            msg_send![self.inner.as_ptr(),
                encodeTransformToCommandBuffer: cmd_buf as *const _ as *mut Object
                sourceTexture: src as *const _ as *mut Object
                histogram: histogram_buffer as *const _ as *mut Object
                histogramOffset: histogram_offset
            ]
        };
    }

    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        dst: &metal::TextureRef,
    ) {
        unsafe { encode_unary(self.inner.as_ptr(), cmd_buf, src, dst); }
    }
}

// -- Utility --
// MPSImageCopyToTexture is not a real MPS class — use blit encoder for copies.
// Actual copies use GpuEncoder::copy_texture_to_texture().

// -- Keypoints --

/// MPSImageFindKeypoints — feature point detection.
pub struct MpsFindKeypoints {
    inner: MpsObject,
}

/// Keypoint info matching MPSImageKeypointRangeInfo.
#[repr(C)]
pub struct MpsKeypointRangeInfo {
    pub maximum_keypoint_count: u64,
    pub minimum_threshold_value: f32,
}

impl MpsFindKeypoints {
    pub fn new(device: &metal::DeviceRef, info: &MpsKeypointRangeInfo) -> Self {
        let cls = Class::get("MPSImageFindKeypoints")
            .expect("MPSImageFindKeypoints class not found");
        let obj: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![cls, alloc];
            msg_send![alloc,
                initWithDevice: device as *const _ as *mut Object
                info: info as *const MpsKeypointRangeInfo
            ]
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    /// Encode keypoint detection. Results go into `keypoint_data_buffer`.
    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &metal::TextureRef,
        keypoint_count_buffer: &metal::BufferRef,
        keypoint_count_offset: u64,
        keypoint_data_buffer: &metal::BufferRef,
        keypoint_data_offset: u64,
    ) {
        let _: () = unsafe {
            msg_send![self.inner.as_ptr(),
                encodeToCommandBuffer: cmd_buf as *const _ as *mut Object
                sourceTexture: src as *const _ as *mut Object
                regions: ptr::null::<c_void>()
                numberOfRegions: 1u64
                keypointCountBuffer: keypoint_count_buffer as *const _ as *mut Object
                keypointCountBufferOffset: keypoint_count_offset
                keypointDataBuffer: keypoint_data_buffer as *const _ as *mut Object
                keypointDataBufferOffset: keypoint_data_offset
            ]
        };
    }
}

// -- Random --

/// MPS random distribution type.
#[derive(Clone, Copy, Debug)]
pub enum MpsRandomDistribution {
    Uniform,
    Normal { mean: f32, std_dev: f32 },
}

/// MPSMatrixRandomMTGP32 — GPU-accelerated Mersenne Twister random number generation.
pub struct MpsMatrixRandom {
    inner: MpsObject,
}

/// Helper: create uniform distribution descriptor. Extracted to avoid clippy ICE
/// from deeply nested msg_send! in match arms.
unsafe fn create_uniform_dist_desc() -> *mut Object {
    let desc_cls = Class::get("MPSMatrixRandomDistributionDescriptor")
        .expect("MPSMatrixRandomDistributionDescriptor class not found");
    msg_send![desc_cls,
        uniformDistributionDescriptorWithMinimum: 0.0f32
        maximum: 1.0f32
    ]
}

/// Helper: create normal distribution descriptor.
unsafe fn create_normal_dist_desc(mean: f32, std_dev: f32) -> *mut Object {
    let desc_cls = Class::get("MPSMatrixRandomDistributionDescriptor")
        .expect("MPSMatrixRandomDistributionDescriptor class not found");
    msg_send![desc_cls,
        normalDistributionDescriptorWithMean: mean
        standardDeviation: std_dev
    ]
}

/// Helper: create MTGP32 random kernel with a distribution descriptor.
unsafe fn create_mtgp32(device: *mut Object, desc: *mut Object) -> *mut Object {
    let cls = Class::get("MPSMatrixRandomMTGP32")
        .expect("MPSMatrixRandomMTGP32 class not found");
    let alloc: *mut Object = msg_send![cls, alloc];
    // MPSDataTypeFloat32 = 0x10000020
    msg_send![alloc,
        initWithDevice: device
        destinationDataType: 0x10000020u64
        seed: 0u64
        distributionDescriptor: desc
    ]
}

impl MpsMatrixRandom {
    /// Create a random number generator.
    pub fn new(
        device: &metal::DeviceRef,
        distribution: MpsRandomDistribution,
    ) -> Self {
        let dev_ptr = device as *const _ as *mut Object;
        let obj: *mut Object = unsafe {
            let desc = match distribution {
                MpsRandomDistribution::Uniform => create_uniform_dist_desc(),
                MpsRandomDistribution::Normal { mean, std_dev } => {
                    create_normal_dist_desc(mean, std_dev)
                }
            };
            create_mtgp32(dev_ptr, desc)
        };
        Self { inner: unsafe { MpsObject::from_raw(obj) } }
    }

    /// Encode random fill into a buffer (as an MPS vector).
    pub fn encode_to_buffer(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        buffer: &metal::BufferRef,
        length: u64,
    ) {
        let vector = unsafe { create_mps_vector(buffer, length) };
        let _: () = unsafe {
            msg_send![self.inner.as_ptr(),
                encodeToCommandBuffer: cmd_buf as *const _ as *mut Object
                destinationVector: vector
            ]
        };
    }
}

/// Helper: create MPSVector wrapping an existing buffer.
unsafe fn create_mps_vector(buffer: &metal::BufferRef, length: u64) -> *mut Object {
    let vec_cls = Class::get("MPSVector").expect("MPSVector class not found");
    let desc_cls = Class::get("MPSVectorDescriptor")
        .expect("MPSVectorDescriptor class not found");
    // MPSDataTypeFloat32 = 0x10000020
    let desc: *mut Object = msg_send![desc_cls,
        vectorDescriptorWithLength: length
        dataType: 0x10000020u64
    ];
    let alloc: *mut Object = msg_send![vec_cls, alloc];
    msg_send![alloc,
        initWithBuffer: buffer as *const _ as *mut Object
        descriptor: desc
    ]
}

// ─── High-level convenience API on GpuEncoder ─────────────────────────

impl super::GpuEncoder {
    /// End any active encoder and return the raw command buffer.
    /// MPS kernels encode directly into the command buffer.
    pub(crate) fn raw_cmd_buf_for_mps(&mut self) -> &metal::CommandBufferRef {
        self.end_current();
        self.cmd_buf()
    }

    /// Encode an MPS Gaussian blur. Creates a temporary kernel per-call.
    /// For repeated use with the same sigma, cache `MpsGaussianBlur` yourself.
    pub fn mps_gaussian_blur(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        sigma: f32,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsGaussianBlur::new(device, sigma);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode an MPS box blur.
    pub fn mps_box_blur(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        kernel_width: u32,
        kernel_height: u32,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsBoxBlur::new(device, kernel_width, kernel_height);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode an MPS tent blur.
    pub fn mps_tent_blur(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        kernel_width: u32,
        kernel_height: u32,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsTentBlur::new(device, kernel_width, kernel_height);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode an MPS median filter.
    pub fn mps_median(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        kernel_diameter: u32,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsMedian::new(device, kernel_diameter);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode an MPS bilinear scale.
    pub fn mps_bilinear_scale(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let scale_x = dst.width as f64 / src.width as f64;
        let scale_y = dst.height as f64 / src.height as f64;
        let kernel = MpsBilinearScale::new(device);
        kernel.set_transform(&MpsScaleTransform {
            scale_x,
            scale_y,
            translate_x: 0.0,
            translate_y: 0.0,
        });
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode an MPS Lanczos scale.
    pub fn mps_lanczos_scale(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let scale_x = dst.width as f64 / src.width as f64;
        let scale_y = dst.height as f64 / src.height as f64;
        let kernel = MpsLanczosScale::new(device);
        kernel.set_transform(&MpsScaleTransform {
            scale_x,
            scale_y,
            translate_x: 0.0,
            translate_y: 0.0,
        });
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode an MPS Sobel edge detection.
    pub fn mps_sobel(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsSobel::new(device);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode an MPS Laplacian edge detection.
    pub fn mps_laplacian(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsLaplacian::new(device);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode an MPS convolution with custom kernel weights.
    pub fn mps_convolution(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        kernel_width: u32,
        kernel_height: u32,
        weights: &[f32],
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsConvolution::new(device, kernel_width, kernel_height, weights);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode MPS morphological dilation.
    pub fn mps_dilate(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        kernel_width: u32,
        kernel_height: u32,
        values: &[f32],
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsDilate::new(device, kernel_width, kernel_height, values);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode MPS morphological erosion.
    pub fn mps_erode(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        kernel_width: u32,
        kernel_height: u32,
        values: &[f32],
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsErode::new(device, kernel_width, kernel_height, values);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode MPS binary threshold.
    pub fn mps_threshold_binary(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        threshold: f32,
        max_value: f32,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsThresholdBinary::new(device, threshold, max_value, None);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode MPS truncate threshold.
    pub fn mps_threshold_truncate(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        threshold: f32,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsThresholdTruncate::new(device, threshold, None);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode MPS threshold-to-zero.
    pub fn mps_threshold_to_zero(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        threshold: f32,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsThresholdToZero::new(device, threshold, None);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode MPS element-wise addition.
    pub fn mps_add(
        &mut self,
        a: &GpuTexture,
        b: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsAdd::new(device);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &a.raw, &b.raw, &dst.raw);
    }

    /// Encode MPS element-wise subtraction.
    pub fn mps_subtract(
        &mut self,
        a: &GpuTexture,
        b: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsSubtract::new(device);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &a.raw, &b.raw, &dst.raw);
    }

    /// Encode MPS element-wise multiplication.
    pub fn mps_multiply(
        &mut self,
        a: &GpuTexture,
        b: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsMultiply::new(device);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &a.raw, &b.raw, &dst.raw);
    }

    /// Encode MPS element-wise division.
    pub fn mps_divide(
        &mut self,
        a: &GpuTexture,
        b: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsDivide::new(device);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &a.raw, &b.raw, &dst.raw);
    }

    /// Encode MPS area maximum.
    pub fn mps_area_max(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        kernel_width: u32,
        kernel_height: u32,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsAreaMax::new(device, kernel_width, kernel_height);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode MPS area minimum.
    pub fn mps_area_min(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        kernel_width: u32,
        kernel_height: u32,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsAreaMin::new(device, kernel_width, kernel_height);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode MPS min/max statistics.
    pub fn mps_min_max(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsMinMax::new(device);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode MPS integral image (summed area table).
    pub fn mps_integral(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsIntegral::new(device);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }

    /// Encode MPS integral of squares.
    pub fn mps_integral_of_squares(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        device: &metal::DeviceRef,
    ) {
        let kernel = MpsIntegralOfSquares::new(device);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }
}
