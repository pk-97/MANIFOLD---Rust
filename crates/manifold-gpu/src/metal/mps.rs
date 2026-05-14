//! Metal Performance Shaders (MPS) integration for manifold-gpu.
//!
//! MPS kernels are Apple's hand-tuned GPU image processing primitives.
//! They encode directly into MTLCommandBuffer and operate on MTLTexture —
//! no intermediate copies.
//!
//! MPS kernels are created once per device (or per parameter set) and cached.
//! They are NOT per-dispatch objects — reuse them across frames.
//!
//! Uses typed `objc2-metal-performance-shaders` bindings end-to-end.

use std::ffi::c_float;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, Encode, Encoding, RefEncode, msg_send};
use objc2_metal::{MTLBuffer, MTLCommandBuffer, MTLDevice, MTLTexture};
use objc2_metal_performance_shaders::{
    MPSBinaryImageKernel, MPSImageAdd, MPSImageAreaMax, MPSImageAreaMin, MPSImageBilinearScale,
    MPSImageBox, MPSImageConvolution, MPSImageDilate, MPSImageDivide, MPSImageErode,
    MPSImageFindKeypoints, MPSImageGaussianBlur, MPSImageHistogram, MPSImageHistogramEqualization,
    MPSImageIntegral, MPSImageIntegralOfSquares, MPSImageKeypointRangeInfo, MPSImageLanczosScale,
    MPSImageLaplacian, MPSImageMedian, MPSImageMultiply, MPSImageSobel,
    MPSImageStatisticsMinAndMax, MPSImageSubtract, MPSImageTent, MPSImageThresholdBinary,
    MPSImageThresholdToZero, MPSImageThresholdTruncate, MPSScaleTransform, MPSUnaryImageKernel,
};

use super::GpuTexture;

// ─── Link MPS framework ──────────────────────────────────────────────

unsafe extern "C" {
    fn MPSSupportsMTLDevice(device: *const std::ffi::c_void) -> bool;
}

/// Check if the device supports MPS.
pub fn mps_supports_device(device: &ProtocolObject<dyn MTLDevice>) -> bool {
    unsafe { MPSSupportsMTLDevice(device as *const _ as *const std::ffi::c_void) }
}

// ─── Scale transform ──────────────────────────────────────────────────

/// Scale transform struct matching MPSScaleTransform.
///
/// Kept as the public ABI; converts to/from `objc2_metal_performance_shaders::MPSScaleTransform`
/// (identical `#[repr(C)]` layout) at call sites.
#[repr(C)]
pub struct MpsScaleTransform {
    pub scale_x: f64,
    pub scale_y: f64,
    pub translate_x: f64,
    pub translate_y: f64,
}

impl MpsScaleTransform {
    #[inline]
    fn as_mps(&self) -> MPSScaleTransform {
        MPSScaleTransform {
            scaleX: self.scale_x,
            scaleY: self.scale_y,
            translateX: self.translate_x,
            translateY: self.translate_y,
        }
    }
}

// ─── Small helper macros ──────────────────────────────────────────────

/// Encode a unary image kernel (src → dst) via the typed MPSUnaryImageKernel protocol.
#[inline]
unsafe fn encode_unary_typed<K>(
    kernel: &K,
    cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
    src: &ProtocolObject<dyn MTLTexture>,
    dst: &ProtocolObject<dyn MTLTexture>,
) where
    K: AsRef<MPSUnaryImageKernel>,
{
    unsafe {
        kernel
            .as_ref()
            .encodeToCommandBuffer_sourceTexture_destinationTexture(cmd_buf, src, dst);
    }
}

/// Encode a binary image kernel (a + b → dst) via the typed MPSBinaryImageKernel protocol.
#[inline]
unsafe fn encode_binary_typed<K>(
    kernel: &K,
    cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
    primary: &ProtocolObject<dyn MTLTexture>,
    secondary: &ProtocolObject<dyn MTLTexture>,
    dst: &ProtocolObject<dyn MTLTexture>,
) where
    K: AsRef<MPSBinaryImageKernel>,
{
    unsafe {
        kernel
            .as_ref()
            .encodeToCommandBuffer_primaryTexture_secondaryTexture_destinationTexture(
                cmd_buf, primary, secondary, dst,
            );
    }
}

// ─── MPS Kernel Wrappers ──────────────────────────────────────────────

// -- Blur --

/// MPSImageGaussianBlur — separable Gaussian blur optimized for Apple Silicon.
pub struct MpsGaussianBlur {
    inner: Retained<MPSImageGaussianBlur>,
}

unsafe impl Send for MpsGaussianBlur {}
unsafe impl Sync for MpsGaussianBlur {}

impl MpsGaussianBlur {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>, sigma: f32) -> Self {
        let inner = unsafe {
            MPSImageGaussianBlur::initWithDevice_sigma(
                MPSImageGaussianBlur::alloc(),
                device,
                sigma as c_float,
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }

    pub fn sigma(&self) -> f32 {
        unsafe { self.inner.sigma() }
    }
}

/// MPSImageBox — fast box blur.
pub struct MpsBoxBlur {
    inner: Retained<MPSImageBox>,
}

unsafe impl Send for MpsBoxBlur {}
unsafe impl Sync for MpsBoxBlur {}

impl MpsBoxBlur {
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        kernel_width: u32,
        kernel_height: u32,
    ) -> Self {
        let inner = unsafe {
            MPSImageBox::initWithDevice_kernelWidth_kernelHeight(
                MPSImageBox::alloc(),
                device,
                kernel_width as usize,
                kernel_height as usize,
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageTent — tent (triangle) blur.
pub struct MpsTentBlur {
    inner: Retained<MPSImageTent>,
}

unsafe impl Send for MpsTentBlur {}
unsafe impl Sync for MpsTentBlur {}

impl MpsTentBlur {
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        kernel_width: u32,
        kernel_height: u32,
    ) -> Self {
        let inner = unsafe {
            MPSImageTent::initWithDevice_kernelWidth_kernelHeight(
                MPSImageTent::alloc(),
                device,
                kernel_width as usize,
                kernel_height as usize,
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageMedian — median filter.
pub struct MpsMedian {
    inner: Retained<MPSImageMedian>,
}

unsafe impl Send for MpsMedian {}
unsafe impl Sync for MpsMedian {}

impl MpsMedian {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>, kernel_diameter: u32) -> Self {
        let inner = unsafe {
            MPSImageMedian::initWithDevice_kernelDiameter(
                MPSImageMedian::alloc(),
                device,
                kernel_diameter as usize,
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

// -- Scale --

/// MPSImageBilinearScale — bilinear texture scaling.
pub struct MpsBilinearScale {
    inner: Retained<MPSImageBilinearScale>,
}

unsafe impl Send for MpsBilinearScale {}
unsafe impl Sync for MpsBilinearScale {}

impl MpsBilinearScale {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner = unsafe {
            MPSImageBilinearScale::initWithDevice(MPSImageBilinearScale::alloc(), device)
        };
        Self { inner }
    }

    pub fn set_transform(&self, transform: &MpsScaleTransform) {
        let xform = transform.as_mps();
        unsafe {
            // setScaleTransform takes a nullable pointer to MPSScaleTransform.
            let ptr: *const MPSScaleTransform = &xform;
            let _: () = msg_send![&*self.inner, setScaleTransform: ptr];
        }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageLanczosScale — high-quality Lanczos texture scaling.
pub struct MpsLanczosScale {
    inner: Retained<MPSImageLanczosScale>,
}

unsafe impl Send for MpsLanczosScale {}
unsafe impl Sync for MpsLanczosScale {}

impl MpsLanczosScale {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner =
            unsafe { MPSImageLanczosScale::initWithDevice(MPSImageLanczosScale::alloc(), device) };
        Self { inner }
    }

    pub fn set_transform(&self, transform: &MpsScaleTransform) {
        let xform = transform.as_mps();
        unsafe {
            let ptr: *const MPSScaleTransform = &xform;
            let _: () = msg_send![&*self.inner, setScaleTransform: ptr];
        }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

// -- Edge / Feature Detection --

/// MPSImageSobel — Sobel edge detection.
pub struct MpsSobel {
    inner: Retained<MPSImageSobel>,
}

unsafe impl Send for MpsSobel {}
unsafe impl Sync for MpsSobel {}

impl MpsSobel {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner = unsafe { MPSImageSobel::initWithDevice(MPSImageSobel::alloc(), device) };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageLaplacian — Laplacian edge detection.
pub struct MpsLaplacian {
    inner: Retained<MPSImageLaplacian>,
}

unsafe impl Send for MpsLaplacian {}
unsafe impl Sync for MpsLaplacian {}

impl MpsLaplacian {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner =
            unsafe { MPSImageLaplacian::initWithDevice(MPSImageLaplacian::alloc(), device) };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageConvolution — arbitrary convolution kernel.
pub struct MpsConvolution {
    inner: Retained<MPSImageConvolution>,
}

unsafe impl Send for MpsConvolution {}
unsafe impl Sync for MpsConvolution {}

impl MpsConvolution {
    /// Create with a custom kernel. `weights` length must be `width * height`.
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        kernel_width: u32,
        kernel_height: u32,
        weights: &[f32],
    ) -> Self {
        assert_eq!(
            weights.len(),
            (kernel_width * kernel_height) as usize,
            "Convolution weights length must match kernel dimensions"
        );
        let inner = unsafe {
            MPSImageConvolution::initWithDevice_kernelWidth_kernelHeight_weights(
                MPSImageConvolution::alloc(),
                device,
                kernel_width as usize,
                kernel_height as usize,
                NonNull::new(weights.as_ptr() as *mut c_float).unwrap(),
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

// -- Morphology --

/// MPSImageDilate — morphological dilation.
pub struct MpsDilate {
    inner: Retained<MPSImageDilate>,
}

unsafe impl Send for MpsDilate {}
unsafe impl Sync for MpsDilate {}

impl MpsDilate {
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        kernel_width: u32,
        kernel_height: u32,
        values: &[f32],
    ) -> Self {
        assert_eq!(values.len(), (kernel_width * kernel_height) as usize);
        let inner = unsafe {
            MPSImageDilate::initWithDevice_kernelWidth_kernelHeight_values(
                MPSImageDilate::alloc(),
                device,
                kernel_width as usize,
                kernel_height as usize,
                NonNull::new(values.as_ptr() as *mut c_float).unwrap(),
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageErode — morphological erosion.
pub struct MpsErode {
    inner: Retained<MPSImageErode>,
}

unsafe impl Send for MpsErode {}
unsafe impl Sync for MpsErode {}

impl MpsErode {
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        kernel_width: u32,
        kernel_height: u32,
        values: &[f32],
    ) -> Self {
        assert_eq!(values.len(), (kernel_width * kernel_height) as usize);
        let inner = unsafe {
            MPSImageErode::initWithDevice_kernelWidth_kernelHeight_values(
                MPSImageErode::alloc(),
                device,
                kernel_width as usize,
                kernel_height as usize,
                NonNull::new(values.as_ptr() as *mut c_float).unwrap(),
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

// -- Threshold --

/// MPSImageThresholdBinary — binary threshold.
pub struct MpsThresholdBinary {
    inner: Retained<MPSImageThresholdBinary>,
}

unsafe impl Send for MpsThresholdBinary {}
unsafe impl Sync for MpsThresholdBinary {}

impl MpsThresholdBinary {
    /// Pixels above `threshold` become `max_value`, below become 0.
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        threshold: f32,
        max_value: f32,
        linear_gray_color_transform: Option<&[f32; 3]>,
    ) -> Self {
        let transform_ptr = linear_gray_color_transform
            .map(|t| t.as_ptr() as *const c_float)
            .unwrap_or(std::ptr::null());
        let inner = unsafe {
            MPSImageThresholdBinary::initWithDevice_thresholdValue_maximumValue_linearGrayColorTransform(
                MPSImageThresholdBinary::alloc(),
                device,
                threshold,
                max_value,
                transform_ptr,
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageThresholdTruncate — truncate values above threshold.
pub struct MpsThresholdTruncate {
    inner: Retained<MPSImageThresholdTruncate>,
}

unsafe impl Send for MpsThresholdTruncate {}
unsafe impl Sync for MpsThresholdTruncate {}

impl MpsThresholdTruncate {
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        threshold: f32,
        linear_gray_color_transform: Option<&[f32; 3]>,
    ) -> Self {
        let transform_ptr = linear_gray_color_transform
            .map(|t| t.as_ptr() as *const c_float)
            .unwrap_or(std::ptr::null());
        let inner = unsafe {
            MPSImageThresholdTruncate::initWithDevice_thresholdValue_linearGrayColorTransform(
                MPSImageThresholdTruncate::alloc(),
                device,
                threshold,
                transform_ptr,
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageThresholdToZero — zero values below threshold.
pub struct MpsThresholdToZero {
    inner: Retained<MPSImageThresholdToZero>,
}

unsafe impl Send for MpsThresholdToZero {}
unsafe impl Sync for MpsThresholdToZero {}

impl MpsThresholdToZero {
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        threshold: f32,
        linear_gray_color_transform: Option<&[f32; 3]>,
    ) -> Self {
        let transform_ptr = linear_gray_color_transform
            .map(|t| t.as_ptr() as *const c_float)
            .unwrap_or(std::ptr::null());
        let inner = unsafe {
            MPSImageThresholdToZero::initWithDevice_thresholdValue_linearGrayColorTransform(
                MPSImageThresholdToZero::alloc(),
                device,
                threshold,
                transform_ptr,
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

// -- Arithmetic --

/// MPSImageAdd — element-wise addition of two textures.
pub struct MpsAdd {
    inner: Retained<MPSImageAdd>,
}

unsafe impl Send for MpsAdd {}
unsafe impl Sync for MpsAdd {}

impl MpsAdd {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner = unsafe { MPSImageAdd::initWithDevice(MPSImageAdd::alloc(), device) };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        primary: &ProtocolObject<dyn MTLTexture>,
        secondary: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_binary_typed(&*self.inner, cmd_buf, primary, secondary, dst) }
    }
}

/// MPSImageSubtract — element-wise subtraction (primary - secondary).
pub struct MpsSubtract {
    inner: Retained<MPSImageSubtract>,
}

unsafe impl Send for MpsSubtract {}
unsafe impl Sync for MpsSubtract {}

impl MpsSubtract {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner = unsafe { MPSImageSubtract::initWithDevice(MPSImageSubtract::alloc(), device) };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        primary: &ProtocolObject<dyn MTLTexture>,
        secondary: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_binary_typed(&*self.inner, cmd_buf, primary, secondary, dst) }
    }
}

/// MPSImageMultiply — element-wise multiplication.
pub struct MpsMultiply {
    inner: Retained<MPSImageMultiply>,
}

unsafe impl Send for MpsMultiply {}
unsafe impl Sync for MpsMultiply {}

impl MpsMultiply {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner = unsafe { MPSImageMultiply::initWithDevice(MPSImageMultiply::alloc(), device) };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        primary: &ProtocolObject<dyn MTLTexture>,
        secondary: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_binary_typed(&*self.inner, cmd_buf, primary, secondary, dst) }
    }
}

/// MPSImageDivide — element-wise division (primary / secondary).
pub struct MpsDivide {
    inner: Retained<MPSImageDivide>,
}

unsafe impl Send for MpsDivide {}
unsafe impl Sync for MpsDivide {}

impl MpsDivide {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner = unsafe { MPSImageDivide::initWithDevice(MPSImageDivide::alloc(), device) };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        primary: &ProtocolObject<dyn MTLTexture>,
        secondary: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_binary_typed(&*self.inner, cmd_buf, primary, secondary, dst) }
    }
}

// -- Statistics --

/// MPSImageStatisticsMinAndMax — compute min/max of an image.
pub struct MpsMinMax {
    inner: Retained<MPSImageStatisticsMinAndMax>,
}

unsafe impl Send for MpsMinMax {}
unsafe impl Sync for MpsMinMax {}

impl MpsMinMax {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner = unsafe {
            MPSImageStatisticsMinAndMax::initWithDevice(
                MPSImageStatisticsMinAndMax::alloc(),
                device,
            )
        };
        Self { inner }
    }

    /// Encode: result is written to a 2-pixel texture (min, max).
    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageAreaMax — maximum value in a rectangular region.
pub struct MpsAreaMax {
    inner: Retained<MPSImageAreaMax>,
}

unsafe impl Send for MpsAreaMax {}
unsafe impl Sync for MpsAreaMax {}

impl MpsAreaMax {
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        kernel_width: u32,
        kernel_height: u32,
    ) -> Self {
        let inner = unsafe {
            MPSImageAreaMax::initWithDevice_kernelWidth_kernelHeight(
                MPSImageAreaMax::alloc(),
                device,
                kernel_width as usize,
                kernel_height as usize,
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageAreaMin — minimum value in a rectangular region.
pub struct MpsAreaMin {
    inner: Retained<MPSImageAreaMin>,
}

unsafe impl Send for MpsAreaMin {}
unsafe impl Sync for MpsAreaMin {}

impl MpsAreaMin {
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        kernel_width: u32,
        kernel_height: u32,
    ) -> Self {
        let inner = unsafe {
            MPSImageAreaMin::initWithDevice_kernelWidth_kernelHeight(
                MPSImageAreaMin::alloc(),
                device,
                kernel_width as usize,
                kernel_height as usize,
            )
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageIntegral — summed area table (integral image).
pub struct MpsIntegral {
    inner: Retained<MPSImageIntegral>,
}

unsafe impl Send for MpsIntegral {}
unsafe impl Sync for MpsIntegral {}

impl MpsIntegral {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner = unsafe { MPSImageIntegral::initWithDevice(MPSImageIntegral::alloc(), device) };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

/// MPSImageIntegralOfSquares — summed area table of squared values.
pub struct MpsIntegralOfSquares {
    inner: Retained<MPSImageIntegralOfSquares>,
}

unsafe impl Send for MpsIntegralOfSquares {}
unsafe impl Sync for MpsIntegralOfSquares {}

impl MpsIntegralOfSquares {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let inner = unsafe {
            MPSImageIntegralOfSquares::initWithDevice(MPSImageIntegralOfSquares::alloc(), device)
        };
        Self { inner }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

// -- Histogram --

/// MPSImageHistogram — compute histogram of an image into a buffer.
pub struct MpsHistogram {
    inner: Retained<MPSImageHistogram>,
}

unsafe impl Send for MpsHistogram {}
unsafe impl Sync for MpsHistogram {}

/// Histogram info — matches MPSImageHistogramInfo layout. Kept as a local
/// repr(C) struct because the typed crate doesn't export it publicly.
///
/// `histogram_for_alpha` uses `Bool` (ObjC BOOL) rather than Rust `bool` to
/// match the underlying C struct layout exactly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MpsHistogramInfo {
    pub number_of_histogram_entries: usize,
    pub histogram_for_alpha: Bool,
    pub min_pixel_value: [f32; 4],
    pub max_pixel_value: [f32; 4],
}

// Safety: `MpsHistogramInfo` is `#[repr(C)]` and its layout matches
// MPSImageHistogramInfo from Metal.framework.
unsafe impl Encode for MpsHistogramInfo {
    const ENCODING: Encoding = Encoding::Struct(
        "?",
        &[
            <usize as Encode>::ENCODING,
            <Bool as Encode>::ENCODING,
            <[f32; 4] as Encode>::ENCODING,
            <[f32; 4] as Encode>::ENCODING,
        ],
    );
}

unsafe impl RefEncode for MpsHistogramInfo {
    const ENCODING_REF: Encoding = Encoding::Pointer(&<Self as Encode>::ENCODING);
}

impl MpsHistogram {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>, info: &MpsHistogramInfo) -> Self {
        let inner = unsafe {
            let cls =
                AnyClass::get(c"MPSImageHistogram").expect("MPSImageHistogram class not found");
            let alloc: *mut AnyObject = msg_send![cls, alloc];
            let info_ptr: *const MpsHistogramInfo = info;
            let obj: *mut MPSImageHistogram = msg_send![
                alloc,
                initWithDevice: device,
                histogramInfo: info_ptr,
            ];
            Retained::from_raw(obj).expect("MPSImageHistogram init returned nil")
        };
        Self { inner }
    }

    /// Encode histogram computation. Result goes into `histogram_buffer` at offset 0.
    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        histogram_buffer: &ProtocolObject<dyn MTLBuffer>,
        histogram_offset: u64,
    ) {
        unsafe {
            let _: () = msg_send![
                &*self.inner,
                encodeToCommandBuffer: cmd_buf,
                sourceTexture: src,
                histogram: histogram_buffer as *const _ as *const AnyObject,
                histogramOffset: histogram_offset as usize,
            ];
        }
    }

    /// Returns the size in bytes needed for the histogram buffer.
    pub fn histogram_size(&self) -> u64 {
        let size: usize = unsafe { msg_send![&*self.inner, histogramSize] };
        size as u64
    }
}

/// MPSImageHistogramEqualization — equalize image histogram.
pub struct MpsHistogramEqualization {
    inner: Retained<MPSImageHistogramEqualization>,
}

unsafe impl Send for MpsHistogramEqualization {}
unsafe impl Sync for MpsHistogramEqualization {}

impl MpsHistogramEqualization {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>, info: &MpsHistogramInfo) -> Self {
        let inner = unsafe {
            let cls = AnyClass::get(c"MPSImageHistogramEqualization")
                .expect("MPSImageHistogramEqualization class not found");
            let alloc: *mut AnyObject = msg_send![cls, alloc];
            let info_ptr: *const MpsHistogramInfo = info;
            let obj: *mut MPSImageHistogramEqualization = msg_send![
                alloc,
                initWithDevice: device,
                histogramInfo: info_ptr,
            ];
            Retained::from_raw(obj).expect("MPSImageHistogramEqualization init returned nil")
        };
        Self { inner }
    }

    /// Must call this before encode() to provide the histogram data.
    pub fn encode_transform(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        histogram_buffer: &ProtocolObject<dyn MTLBuffer>,
        histogram_offset: u64,
    ) {
        unsafe {
            let _: () = msg_send![
                &*self.inner,
                encodeTransformToCommandBuffer: cmd_buf,
                sourceTexture: src,
                histogram: histogram_buffer as *const _ as *const AnyObject,
                histogramOffset: histogram_offset as usize,
            ];
        }
    }

    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
    ) {
        unsafe { encode_unary_typed(&*self.inner, cmd_buf, src, dst) }
    }
}

// -- Keypoints --

/// MPSImageFindKeypoints — feature point detection.
pub struct MpsFindKeypoints {
    inner: Retained<MPSImageFindKeypoints>,
}

unsafe impl Send for MpsFindKeypoints {}
unsafe impl Sync for MpsFindKeypoints {}

/// Keypoint info — public ABI alias for the typed struct.
pub type MpsKeypointRangeInfo = MPSImageKeypointRangeInfo;

impl MpsFindKeypoints {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>, info: &MpsKeypointRangeInfo) -> Self {
        let inner = unsafe {
            MPSImageFindKeypoints::initWithDevice_info(
                MPSImageFindKeypoints::alloc(),
                device,
                NonNull::new(info as *const _ as *mut _).unwrap(),
            )
        };
        Self { inner }
    }

    /// Encode keypoint detection. Results go into `keypoint_data_buffer`.
    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &ProtocolObject<dyn MTLTexture>,
        keypoint_count_buffer: &ProtocolObject<dyn MTLBuffer>,
        keypoint_count_offset: u64,
        keypoint_data_buffer: &ProtocolObject<dyn MTLBuffer>,
        keypoint_data_offset: u64,
    ) {
        unsafe {
            let _: () = msg_send![
                &*self.inner,
                encodeToCommandBuffer: cmd_buf,
                sourceTexture: src,
                regions: std::ptr::null::<std::ffi::c_void>(),
                numberOfRegions: 1usize,
                keypointCountBuffer: keypoint_count_buffer as *const _ as *const AnyObject,
                keypointCountBufferOffset: keypoint_count_offset as usize,
                keypointDataBuffer: keypoint_data_buffer as *const _ as *const AnyObject,
                keypointDataBufferOffset: keypoint_data_offset as usize,
            ];
        }
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
///
/// The typed `objc2-metal-performance-shaders` crate exposes MPSMatrixRandom types,
/// but the distribution descriptor factory methods + the MPSVector constructor used
/// here aren't in the generated surface, so this wrapper uses untyped `msg_send!`
/// (still via objc2's safer runtime).
pub struct MpsMatrixRandom {
    inner: Retained<AnyObject>,
}

unsafe impl Send for MpsMatrixRandom {}
unsafe impl Sync for MpsMatrixRandom {}

/// Helper: create uniform distribution descriptor (+1 retained? No — class method returns autoreleased).
unsafe fn create_uniform_dist_desc() -> Retained<AnyObject> {
    let desc_cls = AnyClass::get(c"MPSMatrixRandomDistributionDescriptor")
        .expect("MPSMatrixRandomDistributionDescriptor class not found");
    unsafe {
        let desc: *mut AnyObject = msg_send![
            desc_cls,
            uniformDistributionDescriptorWithMinimum: 0.0f32,
            maximum: 1.0f32,
        ];
        Retained::retain(desc).expect("uniformDistributionDescriptor returned nil")
    }
}

/// Helper: create normal distribution descriptor.
unsafe fn create_normal_dist_desc(mean: f32, std_dev: f32) -> Retained<AnyObject> {
    let desc_cls = AnyClass::get(c"MPSMatrixRandomDistributionDescriptor")
        .expect("MPSMatrixRandomDistributionDescriptor class not found");
    unsafe {
        let desc: *mut AnyObject = msg_send![
            desc_cls,
            normalDistributionDescriptorWithMean: mean,
            standardDeviation: std_dev,
        ];
        Retained::retain(desc).expect("normalDistributionDescriptor returned nil")
    }
}

/// Helper: create MTGP32 random kernel with a distribution descriptor.
unsafe fn create_mtgp32(
    device: &ProtocolObject<dyn MTLDevice>,
    desc: &AnyObject,
) -> Retained<AnyObject> {
    let cls =
        AnyClass::get(c"MPSMatrixRandomMTGP32").expect("MPSMatrixRandomMTGP32 class not found");
    unsafe {
        let alloc: *mut AnyObject = msg_send![cls, alloc];
        // MPSDataTypeFloat32 = 0x10000020
        let obj: *mut AnyObject = msg_send![
            alloc,
            initWithDevice: device,
            destinationDataType: 0x10000020u64,
            seed: 0usize,
            distributionDescriptor: desc,
        ];
        Retained::from_raw(obj).expect("MPSMatrixRandomMTGP32 init returned nil")
    }
}

impl MpsMatrixRandom {
    /// Create a random number generator.
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        distribution: MpsRandomDistribution,
    ) -> Self {
        let dev = unsafe { device };
        let desc = unsafe {
            match distribution {
                MpsRandomDistribution::Uniform => create_uniform_dist_desc(),
                MpsRandomDistribution::Normal { mean, std_dev } => {
                    create_normal_dist_desc(mean, std_dev)
                }
            }
        };
        let inner = unsafe { create_mtgp32(dev, &desc) };
        Self { inner }
    }

    /// Encode random fill into a buffer (as an MPS vector).
    pub fn encode_to_buffer(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        buffer: &ProtocolObject<dyn MTLBuffer>,
        length: u64,
    ) {
        unsafe {
            let vector = create_mps_vector(buffer, length);
            let _: () = msg_send![
                &*self.inner,
                encodeToCommandBuffer: cmd_buf,
                destinationVector: &*vector,
            ];
        }
    }
}

/// Helper: create MPSVector wrapping an existing buffer.
unsafe fn create_mps_vector(
    buffer: &ProtocolObject<dyn MTLBuffer>,
    length: u64,
) -> Retained<AnyObject> {
    let vec_cls = AnyClass::get(c"MPSVector").expect("MPSVector class not found");
    let desc_cls =
        AnyClass::get(c"MPSVectorDescriptor").expect("MPSVectorDescriptor class not found");
    unsafe {
        // MPSDataTypeFloat32 = 0x10000020
        let desc: *mut AnyObject = msg_send![
            desc_cls,
            vectorDescriptorWithLength: length as usize,
            dataType: 0x10000020u64,
        ];
        let alloc: *mut AnyObject = msg_send![vec_cls, alloc];
        let obj: *mut AnyObject = msg_send![
            alloc,
            initWithBuffer: buffer as *const _ as *const AnyObject,
            descriptor: desc,
        ];
        Retained::from_raw(obj).expect("MPSVector init returned nil")
    }
}

// ─── High-level convenience API on GpuEncoder ─────────────────────────

impl super::GpuEncoder {
    /// End any active encoder and return the raw command buffer.
    /// MPS kernels encode directly into the command buffer.
    pub(crate) fn raw_cmd_buf_for_mps(&mut self) -> &ProtocolObject<dyn MTLCommandBuffer> {
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
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
        device: &ProtocolObject<dyn MTLDevice>,
    ) {
        let kernel = MpsIntegralOfSquares::new(device);
        let cmd_buf = self.raw_cmd_buf_for_mps();
        kernel.encode(cmd_buf, &src.raw, &dst.raw);
    }
}
