//! Metal 4 FX scaler bridge — MTL4FXTemporalDenoisedScaler and
//! MTL4FXTemporalScaler with GPU-side synchronization only
//! (RAYTRACING_DESIGN.md section 17.7 DN-K).
//!
//! Uses the typed objc2-metal MTL4 API (additive features — does NOT
//! affect classic MTLCommandBuffer creation). GPU-side synchronization
//! via MTLSharedEvent — no CPU stalls, fully pipelined overlap.
//!
//! One MTL4 command queue + allocator is created lazily per `GpuDevice`
//! and shared by all MTL4FX scalers on that device.

use objc2::{AnyThread, Message, rc::Retained};
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTL4CommandAllocator, MTL4CommandBuffer, MTL4CommandQueue, MTLAllocation, MTLCommandBuffer,
    MTLDevice, MTLEvent, MTLPixelFormat, MTLResidencySet, MTLResidencySetDescriptor,
    MTLSharedEvent, MTLStages, MTLTexture,
};
use objc2_metal_fx::{
    MTL4FXTemporalDenoisedScaler, MTL4FXTemporalScaler, MTLFXTemporalDenoisedScalerBase,
    MTLFXTemporalDenoisedScalerDescriptor, MTLFXTemporalScalerBase,
    MTLFXTemporalScalerDescriptor,
};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::GpuTexture;
use crate::{GpuEvent, GpuTextureFormat};

// ─── Availability ─────────────────────────────────────────────────────

/// Check if MTL4FX Temporal Denoised Scaler is available on this system.
///
/// STOPGAP (BUG-woji): hard-off. On macOS 26.6.1 (25G76), MetalFX 31.8, the MTL4
/// denoiser creation aborts the process (uncatchable SIGABRT) inside Apple's
/// own MPSGraph framework:
///
///   MPSGraphExecutable.mm:3467 — "Incompatible shape for parameter at index 0"
///   call chain: _M4FXTemporalDenoisingScalingEffect -> mlKernelMetal4
///             → MPSGraphExecutable
///
/// Bisected 2026-08-11 on Peter's Tahoe M4 machine (same rig that authored
/// the original hard-off):
///
///   - MTL4Compiler + MTLFXTemporalScaler: CREATES AND ENCODES FINE
///     (m4_temporal_scaler_encodes_one_frame green; barrier stages + residency
///     proven working).
///   - MTL4Compiler + MTLFXTemporalDenoisedScaler: CRASHES for EVERY
///     at creation, independent of: reactive mask (on/off), G-buffer aux
///     format settings, input/output dimensions (each format valid/invalid x each enable on/off), sync init on/off, varied dims, both color formats
///     (Rgba32Float).
///   - MTLFXTemporalDenoisedScaler WITHOUT compiler (legacy creation path):
///     CREATES FINE.
///   - supportsMetal4FX: answers YES on this device throughout.
///
/// The crash is at framework graph-compile time, not our encode time — no
/// descriptor property or caller-side change can avoid it. Re-test with
/// MANIFOLD_MTL4FX_DENOISER=1 after each macOS update. The temporal scaler
/// (non-denoised) remains live and is not gated by this flag.
pub fn metalfx_m4_denoiser_available() -> bool {
    if std::env::var_os("MANIFOLD_MTL4FX_DENOISER").is_none() {
        return false;
    }
    mtl4fx_creation_supported(
        c"MTLFXTemporalDenoisedScalerDescriptor",
        objc2::sel!(newTemporalDenoisedScalerWithDevice:compiler:),
    )
}

/// Check if MTL4FX Temporal Scaler is available on this system.
pub fn metalfx_m4_temporal_available() -> bool {
    mtl4fx_creation_supported(
        c"MTLFXTemporalScalerDescriptor",
        objc2::sel!(newTemporalScalerWithDevice:compiler:),
    )
}

/// MTL4FX scaler names are protocols, not classes — a class lookup on them
/// always fails. The real gate: the public descriptor must answer the MTL4
/// compiler-based creation selector.
fn mtl4fx_creation_supported(
    descriptor_class: &'static std::ffi::CStr,
    creation_sel: objc2::runtime::Sel,
) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    let Some(cls) = AnyClass::get(descriptor_class) else {
        return false;
    };
    unsafe { msg_send![cls, instancesRespondToSelector: creation_sel] }
}

/// Check if the MTL4FX Temporal Denoised Scaler is supported for the
/// given device. Requires both class availability AND device support.
pub fn supports_m4_denoiser(device: &ProtocolObject<dyn MTLDevice>) -> bool {
    if !metalfx_m4_denoiser_available() {
        return false;
    }
    unsafe { MTLFXTemporalDenoisedScalerDescriptor::supportsDevice(device) }
}

/// Check if the MTL4FX Temporal Scaler is supported for the given device.
pub fn supports_m4_temporal(device: &ProtocolObject<dyn MTLDevice>) -> bool {
    if !metalfx_m4_temporal_available() {
        return false;
    }
    unsafe { MTLFXTemporalScalerDescriptor::supportsDevice(device) }
}

/// Cast a texture to its MTLAllocation conformance. MTLTexture conforms to
/// MTLAllocation at the ObjC level; the objc2-metal bindings don't express
/// it in the trait hierarchy, so this goes through the raw pointer. Same
/// object, same lifetime — the cast is layout-transparent.
fn as_allocation(tex: &ProtocolObject<dyn MTLTexture>) -> &ProtocolObject<dyn MTLAllocation> {
    unsafe {
        &*(tex as *const ProtocolObject<dyn MTLTexture> as *const ProtocolObject<dyn MTLAllocation>)
    }
}

/// Metal 4 requires declaring which pipeline stages border the effect's
/// textures: Metal validation asserts "_outputTextureBarrierStages not set"
/// and WITHOUT validation the encode silently writes nothing (probed on
/// Tahoe 26.6.1). The color input defaults to MTLStageDispatch; the output
/// defaults to unset. There is no public setter for the output stages —
/// KVC writes the ivar directly. Tripwire: the gpu-proofs smoke tests in
/// this module (an Apple rename turns this into a KVC exception there).
unsafe fn set_mtl4fx_barrier_stages<T: ?Sized>(scaler: &ProtocolObject<T>) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;
    let all: Option<Retained<AnyObject>> =
        msg_send![class!(NSNumber), numberWithUnsignedLongLong: MTLStages::All.0 as u64];
    let Some(all) = all else {
        log::error!("[MetalFX MTL4] NSNumber creation failed — barrier stages unset, MTL4FX encode may produce black output");
        return;
    };
    for key in ["colorTextureBarrierStages", "outputTextureBarrierStages"] {
        let key = NSString::from_str(key);
        let _: () = msg_send![scaler, setValue: &*all, forKey: &*key];
    }
}

// ─── Format mapping ───────────────────────────────────────────────────

fn to_mtl_pixel_format(fmt: GpuTextureFormat) -> MTLPixelFormat {
    match fmt {
        GpuTextureFormat::Rgba16Float => MTLPixelFormat::RGBA16Float,
        GpuTextureFormat::Rgba32Float => MTLPixelFormat::RGBA32Float,
        GpuTextureFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        GpuTextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        GpuTextureFormat::Bgra8UnormSrgb => MTLPixelFormat::BGRA8Unorm_sRGB,
        GpuTextureFormat::R32Float => MTLPixelFormat::R32Float,
        GpuTextureFormat::Rg32Float => MTLPixelFormat::RG32Float,
        GpuTextureFormat::R16Float => MTLPixelFormat::R16Float,
        GpuTextureFormat::Rg16Float => MTLPixelFormat::RG16Float,
        GpuTextureFormat::R32Uint => MTLPixelFormat::R32Uint,
        GpuTextureFormat::Rgba8UnormSrgb => MTLPixelFormat::RGBA8Unorm_sRGB,
        GpuTextureFormat::R8Unorm => MTLPixelFormat::R8Unorm,
        GpuTextureFormat::Depth32Float => MTLPixelFormat::Depth32Float,
    }
}

// ─── MTL4 command bridge (per-device, shared) ──────────────────────────

/// Minimal MTL4 command infrastructure for encoding MTL4FX scaler work.
///
/// Uses the typed objc2-metal MTL4 API (MTL4CommandQueue, MTL4CommandAllocator,
/// MTL4CommandBuffer). GPU-side synchronization via MTLSharedEvent.
/// One bridge per device is created lazily and shared by all MTL4FX scalers
/// on that device (RAYTRACING_DESIGN.md section 17.7 DN-K).
pub struct MTL4Bridge {
    queue: Retained<ProtocolObject<dyn MTL4CommandQueue>>,
    allocators: [MTL4AllocatorSlot; 3],
    event: GpuEvent,
    event_value: AtomicU64,
    saturated_logged: std::sync::atomic::AtomicBool,
    residency_set: Retained<ProtocolObject<dyn MTLResidencySet>>,
}

struct MTL4AllocatorSlot {
    allocator: Retained<ProtocolObject<dyn MTL4CommandAllocator>>,
    last_signal: AtomicU64,
}

// Safety: MTL4CommandQueue and MTL4CommandAllocator are thread-safe
// (Apple docs). GpuEvent is already Send+Sync. AtomicU64 is thread-safe.
unsafe impl Send for MTL4Bridge {}
unsafe impl Sync for MTL4Bridge {}
unsafe impl Send for MTL4AllocatorSlot {}
unsafe impl Sync for MTL4AllocatorSlot {}

impl MTL4Bridge {
    /// Create the MTL4 command bridge for a given device.
    /// Returns `None` if MTL4 command infrastructure is not available.
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Option<Self> {
        let queue = device.newMTL4CommandQueue()?;
        let event = unsafe {
            let raw = device
                .newSharedEvent()
                .expect("[MetalFX MTL4] Failed to create shared event");
            GpuEvent::new(raw)
        };

        // Ring of three command allocators. MTL4 allocators must be reset
        // between uses; we cycle slots, reusing one only after the GPU has
        // passed the signal value of the frame that last used it. This avoids
        // any CPU-side wait while keeping at most ~3 frames in flight.
        let allocators = std::array::from_fn(|_| {
            let allocator = device
                .newCommandAllocator()
                .expect("[MetalFX MTL4] Failed to create command allocator");
            MTL4AllocatorSlot {
                allocator,
                last_signal: AtomicU64::new(0),
            }
        });

        // Metal 4 residency is explicit: classic-created textures are
        // invisible to MTL4-committed work unless registered in a residency
        // set attached to the MTL4 queue — without it the MTL4FX kernels
        // silently read/write nothing (probe-proven, 2026-08-11). One set
        // per bridge, attached once; ensure_resident() maintains contents.
        let residency_set = {
            let desc = MTLResidencySetDescriptor::new();
            let set = device
                .newResidencySetWithDescriptor_error(&desc)
                .expect("[MetalFX MTL4] Failed to create residency set");
            queue.addResidencySet(&set);
            set
        };

        log::info!("[MetalFX MTL4] Bridge created — typed MTL4 queue + 3 allocator ring + shared event + residency set");

        Some(Self {
            queue,
            allocators,
            event,
            event_value: AtomicU64::new(0),
            saturated_logged: std::sync::atomic::AtomicBool::new(false),
            residency_set,
        })
    }

    fn next_event_value(&self) -> u64 {
        self.event_value.fetch_add(1, Ordering::Relaxed)
    }

    /// Returns a reusable allocator slot and marks it in-flight. Returns `None`
    /// when all three slots are still in flight (GPU has not yet signaled the
    /// completion values of the frames that used them). The caller must skip
    /// encoding this frame rather than reset a live allocator.
    fn acquire_allocator_slot(&self) -> Option<&MTL4AllocatorSlot> {
        const IN_FLIGHT: u64 = u64::MAX;

        let signaled = unsafe { self.event.raw().signaledValue() };

        for slot in &self.allocators {
            let last = slot.last_signal.load(Ordering::Relaxed);
            if last <= signaled {
                // Already signaled: claim it immediately.
                slot.last_signal.store(IN_FLIGHT, Ordering::Relaxed);
                return Some(slot);
            }
        }

        // No slot has completed. With a 3-frame ring this means at least 3
        // frames are still in flight, which is the saturation point. Do NOT
        // reset a live allocator; the caller will skip this frame.
        None
    }

    /// Register this frame's textures in the queue-attached residency set.
    /// Add-if-missing, commit only on change. Prunes only when the set
    /// exceeds the cap AND no MTL4 work is in flight — removing a texture
    /// an in-flight frame still reads is a page fault (BUG-84fv class).
    fn ensure_resident(&self, textures: &[&ProtocolObject<dyn MTLTexture>]) {
        const PRUNE_CAP: usize = 64;

        let mut changed = false;
        for tex in textures {
            let allocation = as_allocation(tex);
            if !self.residency_set.containsAllocation(allocation) {
                self.residency_set.addAllocation(allocation);
                changed = true;
            }
        }

        if self.residency_set.allocationCount() > PRUNE_CAP {
            let signaled = unsafe { self.event.raw().signaledValue() };
            let all_idle = self
                .allocators
                .iter()
                .all(|s| s.last_signal.load(Ordering::Relaxed) <= signaled);
            if all_idle {
                self.residency_set.removeAllAllocations();
                for tex in textures {
                    self.residency_set.addAllocation(as_allocation(tex));
                }
                changed = true;
                log::info!("[MetalFX MTL4] Residency set pruned (exceeded {PRUNE_CAP} allocations)");
            }
        }

        if changed {
            self.residency_set.commit();
        }
    }

    /// Encode MTL4FX scaler work with GPU-side synchronization only.
    ///
    /// Timeline per encode (returns true), or returns false when the allocator
    /// ring is saturated and the frame must be skipped (no signal/wait pair is
    /// emitted, so the caller can safely present fallback output).
    /// 1. Try to acquire an allocator slot that has completed. If saturated,
    ///    log once and return false.
    /// 2. Signal event on classic cmd_buf (after all prior classic work).
    /// 3. Enqueue wait on MTL4 queue (blocks MTL4 until classic signal).
    /// 4. Reset the acquired allocator, begin a fresh command buffer, encode
    ///    scaler work, end the command buffer.
    /// 5. Commit MTL4 command buffer to MTL4 queue.
    /// 6. Enqueue signal on MTL4 queue after all previously committed work.
    /// 7. Tag the allocator slot with the completion signal value.
    /// 8. Enqueue wait on classic queue (for MTL4 completion signal).
    fn encode_gpu_only<F>(
        &self,
        classic_cmd_buf: &ProtocolObject<dyn objc2_metal::MTLCommandBuffer>,
        device: &ProtocolObject<dyn MTLDevice>,
        textures: &[&ProtocolObject<dyn MTLTexture>],
        encode_fn: F,
    ) -> bool
    where
        F: FnOnce(&ProtocolObject<dyn MTL4CommandBuffer>),
    {
        // 1. Acquire a slot BEFORE consuming event values. If the ring is
        // saturated, skip this frame without creating a signal/wait pair.
        let Some(slot) = self.acquire_allocator_slot() else {
            if !self.saturated_logged.swap(true, Ordering::Relaxed) {
                log::warn!("[MetalFX MTL4] Allocator ring saturated (3 frames in flight); skipping denoiser this frame");
            }
            return false;
        };

        self.ensure_resident(textures);

        let wait_val = self.next_event_value();
        let signal_val = self.next_event_value();

        // 2. Signal on classic cmd_buf.
        unsafe {
            classic_cmd_buf.encodeSignalEvent_value(
                ProtocolObject::from_ref(self.event.raw()),
                wait_val,
            );
        }

        // 3. Enqueue wait on MTL4 queue.
        unsafe {
            self.queue.waitForEvent_value(
                ProtocolObject::from_ref(self.event.raw()) as &ProtocolObject<dyn MTLEvent>,
                wait_val,
            );
        }

        // 4. Reset the acquired allocator, begin a fresh command buffer, encode.
        unsafe {
            slot.allocator.reset();
        }
        let Some(cmd_buf) = device.newCommandBuffer() else {
            log::error!("[MetalFX MTL4] Failed to create MTL4 command buffer");
            return false;
        };
        unsafe {
            cmd_buf.beginCommandBufferWithAllocator(&slot.allocator);
        }

        encode_fn(&cmd_buf);

        unsafe {
            cmd_buf.endCommandBuffer();
        }

        // 5. Commit MTL4 command buffer to MTL4 queue.
        unsafe {
            let buf_ptr = &*cmd_buf as *const ProtocolObject<dyn MTL4CommandBuffer>;
            let ptrs = [buf_ptr];
            self.queue.commit_count(
                NonNull::new(ptrs.as_ptr() as *mut _).expect("non-null"),
                1,
            );
        }

        // 6. Enqueue signal on MTL4 queue.
        unsafe {
            self.queue.signalEvent_value(
                ProtocolObject::from_ref(self.event.raw()) as &ProtocolObject<dyn MTLEvent>,
                signal_val,
            );
        }

        // 7. Tag the allocator slot so we only reuse it once the GPU passes
        // this signal value.
        slot.last_signal.store(signal_val, Ordering::Relaxed);

        // 8. Enqueue wait on classic queue for MTL4 completion signal.
        unsafe {
            classic_cmd_buf.encodeWaitForEvent_value(
                ProtocolObject::from_ref(self.event.raw()),
                signal_val,
            );
        }

        true
    }
}

// ─── MTL4 Metal4FxDenoisedScaler ──────────────────────────────────────

/// Metal 4 FX Temporal Denoised Scaler — wraps `MTL4FXTemporalDenoisedScaler`
/// with a shared, per-device MTL4 command bridge.
///
/// When `MTL4FXTemporalDenoisedScaler` is available on this system, this
/// is the PREFERRED denoiser implementation (DN-K). Falls back to
/// [`super::denoiser::MetalFxDenoisedScaler`] (classic MTLFX) when
/// unavailable.
///
/// All synchronization is GPU-side via MTLSharedEvent — no CPU stalls.
/// Pipelined overlap allows the classic queue to continue work in parallel
/// with the MTL4 scaler encode.
pub struct Metal4FxDenoisedScaler {
    scaler: Retained<ProtocolObject<dyn MTL4FXTemporalDenoisedScaler>>,
    bridge: Arc<MTL4Bridge>,
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub input_width: u32,
    pub input_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}

// Safety: MTL4FX scalers are thread-safe for encoding (Apple docs).
unsafe impl Send for Metal4FxDenoisedScaler {}
unsafe impl Sync for Metal4FxDenoisedScaler {}

impl Metal4FxDenoisedScaler {
    /// Create a Metal 4 denoised scaler. Returns `None` if the MTL4FX
    /// scaler is unavailable or construction fails.
    ///
    /// `bridge` is the lazily-created, per-device MTL4 command bridge
    /// from `GpuDevice::mtl4_bridge()`.
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        bridge: Arc<MTL4Bridge>,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
        color_format: GpuTextureFormat,
        depth_reversed: bool,
    ) -> Option<Self> {
        if !supports_m4_denoiser(device) {
            log::info!(
                "[MetalFX MTL4 Denoiser] Not available — falling back to classic MTLFX"
            );
            return None;
        }

        let color_pixel_format = to_mtl_pixel_format(color_format);

        let desc = unsafe {
            let desc = MTLFXTemporalDenoisedScalerDescriptor::init(
                MTLFXTemporalDenoisedScalerDescriptor::alloc(),
            );
            desc.setInputWidth(input_width as usize);
            desc.setInputHeight(input_height as usize);
            desc.setOutputWidth(output_width as usize);
            desc.setOutputHeight(output_height as usize);
            desc.setColorTextureFormat(color_pixel_format);
            desc.setOutputTextureFormat(color_pixel_format);

            desc.setDepthTextureFormat(MTLPixelFormat::R32Float);
            desc.setMotionTextureFormat(MTLPixelFormat::RG16Float);
            desc.setNormalTextureFormat(MTLPixelFormat::RGBA16Float);
            desc.setRoughnessTextureFormat(MTLPixelFormat::R16Float);
            desc.setDiffuseAlbedoTextureFormat(MTLPixelFormat::RGBA16Float);
            desc.setSpecularAlbedoTextureFormat(MTLPixelFormat::RGBA16Float);
            desc.setSpecularHitDistanceTextureFormat(MTLPixelFormat::R16Float);

            desc.setReactiveMaskTextureEnabled(true);
            desc.setReactiveMaskTextureFormat(MTLPixelFormat::R16Float);

            desc.setAutoExposureEnabled(false);

            // KVC: Xcode 26.0+ properties not in objc2-metal-fx 0.3.2.
            // Un-suppressed when the bindings catch up (crate update or
            // upstream patch that generates these typed setters).
            {
                use objc2::msg_send;
                use objc2_foundation::NSString;

                let true_ns: &objc2::runtime::AnyObject =
                    msg_send![objc2::class!(NSNumber), numberWithBool: true];

                let k_spec_hit = NSString::from_str(
                    "specularHitDistanceTextureEnabled",
                );
                let _: () = msg_send![&desc, setValue: true_ns, forKey: &*k_spec_hit];

                let k_dns_str = NSString::from_str(
                    "denoiseStrengthMaskTextureEnabled",
                );
                let _: () = msg_send![&desc, setValue: true_ns, forKey: &*k_dns_str];

                let k_transp = NSString::from_str(
                    "transparencyOverlayTextureEnabled",
                );
                let _: () = msg_send![&desc, setValue: true_ns, forKey: &*k_transp];

                let denoise_fmt: &objc2::runtime::AnyObject = msg_send![
                    objc2::class!(NSNumber),
                    numberWithUnsignedInteger: MTLPixelFormat::R16Float.0
                ];
                let k_dns_fmt = NSString::from_str(
                    "denoiseStrengthMaskTextureFormat",
                );
                let _: () = msg_send![&desc, setValue: denoise_fmt, forKey: &*k_dns_fmt];

                let transp_fmt: &objc2::runtime::AnyObject = msg_send![
                    objc2::class!(NSNumber),
                    numberWithUnsignedInteger: MTLPixelFormat::RGBA16Float.0
                ];
                let k_transp_fmt = NSString::from_str(
                    "transparencyOverlayTextureFormat",
                );
                let _: () = msg_send![&desc, setValue: transp_fmt, forKey: &*k_transp_fmt];

                let k_sync = NSString::from_str(
                    "requiresSynchronousInitialization",
                );
                let _: () = msg_send![&desc, setValue: true_ns, forKey: &*k_sync];
            }

            desc
        };

        let compiler = unsafe {
            let compiler_desc = objc2_metal::MTL4CompilerDescriptor::new();
            device.newCompilerWithDescriptor_error(&compiler_desc)
        }
        .map_err(|e| {
            log::error!(
                "[MetalFX MTL4 Denoiser] Failed to create MTL4Compiler: {:?}",
                e
            );
        })
        .ok()?;

        let scaler = unsafe {
            desc.newTemporalDenoisedScalerWithDevice_compiler(device, &compiler)
        };

        let Some(scaler) = scaler else {
            log::error!(
                "[MetalFX MTL4 Denoiser] Failed to create MTL4 scaler ({}x{} -> {}x{})",
                input_width,
                input_height,
                output_width,
                output_height
            );
            return None;
        };

        // Motion vectors arrive in NDC-space; MetalFX expects pixel units.
        unsafe {
            let base: &ProtocolObject<dyn MTLFXTemporalDenoisedScalerBase> =
                ProtocolObject::from_ref(&*scaler);
            base.setMotionVectorScaleX(input_width as f32 * 0.5);
            base.setMotionVectorScaleY(input_height as f32 * 0.5);
            base.setDepthReversed(depth_reversed);
            set_mtl4fx_barrier_stages(&scaler);
        }

        log::info!(
            "[MetalFX MTL4 Denoiser] Created MTL4 denoised scaler: {}x{} -> {}x{}",
            input_width,
            input_height,
            output_width,
            output_height
        );

        Some(Self {
            scaler,
            bridge,
            device: device.retain(),
            input_width,
            input_height,
            output_width,
            output_height,
        })
    }

    /// Set the per-frame jitter offset (in pixels).
    pub fn set_jitter(&self, x: f32, y: f32) {
        unsafe {
            let base: &ProtocolObject<dyn MTLFXTemporalDenoisedScalerBase> =
                ProtocolObject::from_ref(&*self.scaler);
            base.setJitterOffsetX(x);
            base.setJitterOffsetY(y);
        }
    }

    /// Set a pre-exposure value.
    pub fn set_pre_exposure(&self, value: f32) {
        unsafe {
            let base: &ProtocolObject<dyn MTLFXTemporalDenoisedScalerBase> =
                ProtocolObject::from_ref(&*self.scaler);
            base.setPreExposure(value);
        }
    }

    /// Check if this scaler matches the given dimensions (for caching).
    pub fn matches(&self, in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> bool {
        self.input_width == in_w
            && self.input_height == in_h
            && self.output_width == out_w
            && self.output_height == out_h
    }

    /// Encode the denoise (and optional upscale) operation.
    ///
    /// Returns `true` if the scaler wrote to `output`, or `false` if the
    /// per-device MTL4 allocator ring was saturated (too many frames in
    /// flight) and no work was encoded. On `false` the caller must present
    /// fallback output — the `output` texture is untouched.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        classic_cmd_buf: &ProtocolObject<dyn objc2_metal::MTLCommandBuffer>,
        color: &GpuTexture,
        depth: &GpuTexture,
        depth_reversed: bool,
        motion: &GpuTexture,
        normal: &GpuTexture,
        roughness: &GpuTexture,
        diffuse_albedo: &GpuTexture,
        specular_albedo: &GpuTexture,
        specular_hit_distance: &GpuTexture,
        exposure: Option<&GpuTexture>,
        output: &GpuTexture,
        reset: bool,
        reactive_mask: Option<&GpuTexture>,
    ) -> bool {
        let scaler: &ProtocolObject<dyn MTL4FXTemporalDenoisedScaler> = &self.scaler;
        let device: &ProtocolObject<dyn MTLDevice> = &self.device;

        let mut textures: [&ProtocolObject<dyn MTLTexture>; 11] = [
            &color.raw, &depth.raw, &motion.raw, &normal.raw, &roughness.raw,
            &diffuse_albedo.raw, &specular_albedo.raw, &specular_hit_distance.raw,
            &output.raw,
            &color.raw, // placeholder for exposure (index 9)
            &color.raw, // placeholder for reactive_mask (index 10)
        ];
        let mut texture_count = 9;
        if let Some(e) = exposure {
            textures[9] = &e.raw;
            texture_count += 1;
        }
        if let Some(rm) = reactive_mask {
            textures[10] = &rm.raw;
            texture_count += 1;
        }

        self.bridge.encode_gpu_only(classic_cmd_buf, device, &textures[..texture_count], |mtl4_cmd_buf| {
            unsafe {
                let base: &ProtocolObject<dyn MTLFXTemporalDenoisedScalerBase> =
                    ProtocolObject::from_ref(scaler);
                base.setColorTexture(Some(&color.raw));
                base.setDepthTexture(Some(&depth.raw));
                base.setDepthReversed(depth_reversed);
                base.setMotionTexture(Some(&motion.raw));
                base.setNormalTexture(Some(&normal.raw));
                base.setRoughnessTexture(Some(&roughness.raw));
                base.setDiffuseAlbedoTexture(Some(&diffuse_albedo.raw));
                base.setSpecularAlbedoTexture(Some(&specular_albedo.raw));
                base.setSpecularHitDistanceTexture(Some(&specular_hit_distance.raw));
                base.setExposureTexture(
                    exposure.map(|t| &*t.raw as &ProtocolObject<dyn MTLTexture>),
                );
                base.setOutputTexture(Some(&output.raw));
                base.setShouldResetHistory(reset);
                base.setReactiveMaskTexture(
                    reactive_mask.map(|t| &*t.raw as &ProtocolObject<dyn MTLTexture>),
                );

                scaler.encodeToCommandBuffer(mtl4_cmd_buf);
            }
        })
    }
}

// ─── MTL4 Metal4FxTemporalScaler ──────────────────────────────────────

/// Metal 4 FX Temporal Scaler — wraps `MTL4FXTemporalScaler` with a shared,
/// per-device MTL4 command bridge.
///
/// Motion-vector-fed upscaling with history accumulation, the Metal 4
/// equivalent of [`super::metalfx::MetalFxTemporalScaler`]. Falls back
/// to the classic MTLFX path when unavailable.
///
/// All synchronization is GPU-side via MTLSharedEvent — no CPU stalls.
pub struct Metal4FxTemporalScaler {
    scaler: Retained<ProtocolObject<dyn MTL4FXTemporalScaler>>,
    bridge: Arc<MTL4Bridge>,
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub input_width: u32,
    pub input_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}

// Safety: MTL4FX scalers are thread-safe for encoding (Apple docs).
unsafe impl Send for Metal4FxTemporalScaler {}
unsafe impl Sync for Metal4FxTemporalScaler {}

impl Metal4FxTemporalScaler {
    /// Create a Metal 4 temporal scaler. Returns `None` if unavailable.
    ///
    /// `bridge` is the lazily-created, per-device MTL4 command bridge
    /// from `GpuDevice::mtl4_bridge()`.
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        bridge: Arc<MTL4Bridge>,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
        color_format: GpuTextureFormat,
    ) -> Option<Self> {
        if !supports_m4_temporal(device) {
            log::info!(
                "[MetalFX MTL4 Temporal] Not available — falling back to classic MTLFX"
            );
            return None;
        }

        let color_pixel_format = to_mtl_pixel_format(color_format);

        let desc = unsafe {
            let desc = MTLFXTemporalScalerDescriptor::init(
                MTLFXTemporalScalerDescriptor::alloc(),
            );
            desc.setInputWidth(input_width as usize);
            desc.setInputHeight(input_height as usize);
            desc.setOutputWidth(output_width as usize);
            desc.setOutputHeight(output_height as usize);
            desc.setColorTextureFormat(color_pixel_format);
            desc.setDepthTextureFormat(MTLPixelFormat::R32Float);
            desc.setMotionTextureFormat(MTLPixelFormat::RG16Float);
            desc.setOutputTextureFormat(color_pixel_format);
            desc
        };

        let compiler = unsafe {
            let compiler_desc = objc2_metal::MTL4CompilerDescriptor::new();
            device.newCompilerWithDescriptor_error(&compiler_desc)
        }
        .map_err(|e| {
            log::error!(
                "[MetalFX MTL4 Temporal] Failed to create MTL4Compiler: {:?}",
                e
            );
        })
        .ok()?;

        let scaler = unsafe {
            desc.newTemporalScalerWithDevice_compiler(device, &compiler)
        };

        let Some(scaler) = scaler else {
            log::error!(
                "[MetalFX MTL4 Temporal] Failed to create MTL4 scaler ({}x{} -> {}x{})",
                input_width,
                input_height,
                output_width,
                output_height
            );
            return None;
        };

        unsafe {
            let base: &ProtocolObject<dyn MTLFXTemporalScalerBase> =
                ProtocolObject::from_ref(&*scaler);
            base.setInputContentWidth(input_width as usize);
            base.setInputContentHeight(input_height as usize);
            base.setMotionVectorScaleX(input_width as f32 * 0.5);
            base.setMotionVectorScaleY(input_height as f32 * 0.5);
            set_mtl4fx_barrier_stages(&scaler);
        }

        log::info!(
            "[MetalFX MTL4 Temporal] Created MTL4 temporal scaler: {}x{} -> {}x{}",
            input_width,
            input_height,
            output_width,
            output_height
        );

        Some(Self {
            scaler,
            bridge,
            device: device.retain(),
            input_width,
            input_height,
            output_width,
            output_height,
        })
    }

    /// Check if this scaler matches the given dimensions (for caching).
    pub fn matches(&self, in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> bool {
        self.input_width == in_w
            && self.input_height == in_h
            && self.output_width == out_w
            && self.output_height == out_h
    }

    /// Encode the temporal upscale operation.
    ///
    /// Returns `true` if the scaler wrote to `dst`, or `false` if the
    /// per-device MTL4 allocator ring was saturated and the frame must be
    /// skipped.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        classic_cmd_buf: &ProtocolObject<dyn objc2_metal::MTLCommandBuffer>,
        color: &GpuTexture,
        depth: &GpuTexture,
        motion: &GpuTexture,
        dst: &GpuTexture,
        jitter_offset_x: f32,
        jitter_offset_y: f32,
        reset: bool,
    ) -> bool {
        let scaler: &ProtocolObject<dyn MTL4FXTemporalScaler> = &self.scaler;
        let device: &ProtocolObject<dyn MTLDevice> = &self.device;

        let textures: [&ProtocolObject<dyn MTLTexture>; 4] =
            [&color.raw, &depth.raw, &motion.raw, &dst.raw];

        self.bridge.encode_gpu_only(classic_cmd_buf, device, &textures, |mtl4_cmd_buf| {
            unsafe {
                let base: &ProtocolObject<dyn MTLFXTemporalScalerBase> =
                    ProtocolObject::from_ref(scaler);
                base.setColorTexture(Some(&color.raw));
                base.setDepthTexture(Some(&depth.raw));
                base.setMotionTexture(Some(&motion.raw));
                base.setOutputTexture(Some(&dst.raw));
                base.setJitterOffsetX(jitter_offset_x);
                base.setJitterOffsetY(jitter_offset_y);
                base.setReset(reset);

                scaler.encodeToCommandBuffer(mtl4_cmd_buf);
            }
        })
    }
}

// ─── Value proof ──────────────────────────────────────────────────────

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;
    use crate::metal::GpuDevice;
    use crate::{GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};

    /// Truncate f32 to IEEE 754 half-precision. Returns `[le_low, le_high]`.
    fn f32_to_f16(f: f32) -> [u8; 2] {
        let bits = f32::to_bits(f);
        let sign = (bits >> 16) & 0x8000u32;
        let exp = ((bits >> 23) & 0xFF) as i32 - 127;
        let frac = (bits & 0x7FFFFF) >> 13;
        let h: u16 = if exp >= 16 {
            (sign | 0x7C00) as u16
        } else if exp >= -14 {
            (sign | (((exp + 15) as u32) << 10) | frac) as u16
        } else if exp >= -24 {
            let sub = (frac | 0x800000) >> (-(exp + 14) as u32);
            (sign | sub) as u16
        } else {
            sign as u16
        };
        h.to_le_bytes()
    }

    /// Upload f32 data into a texture, encoding into the target format.
    fn upload_texture(
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        data: &[f32],
        label: &str,
    ) -> GpuTexture {
        let tex = device.create_texture(&GpuTextureDesc {
            width,
            height,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label,
            mip_levels: 1,
        });
        let bpp = format.bytes_per_pixel();
        let row_bytes = width as u64 * bpp as u64;
        let total = (row_bytes * height as u64) as usize;
        let mut bytes: Vec<u8> = vec![0; total];

        let ch = match format {
            GpuTextureFormat::Rgba32Float | GpuTextureFormat::Rgba16Float
            | GpuTextureFormat::Rgba8Unorm | GpuTextureFormat::Rgba8UnormSrgb => 4,
            GpuTextureFormat::Rg32Float | GpuTextureFormat::Rg16Float => 2,
            _ => 1,
        };

        let npixels = (width * height) as usize;
        for pixel in 0..npixels {
            let src = pixel * ch;
            let dst = pixel * bpp as usize;
            match format {
                GpuTextureFormat::Rgba32Float
                | GpuTextureFormat::R32Float
                | GpuTextureFormat::Rg32Float => {
                    for c in 0..(bpp as usize / 4) {
                        let v: f32 = data.get(src + c).copied().unwrap_or(0.0);
                        bytes[dst + c * 4..dst + c * 4 + 4]
                            .copy_from_slice(&f32::to_ne_bytes(v));
                    }
                }
                GpuTextureFormat::Rgba16Float
                | GpuTextureFormat::R16Float
                | GpuTextureFormat::Rg16Float => {
                    for c in 0..(bpp as usize / 2) {
                        let v: f32 = data.get(src + c).copied().unwrap_or(0.0);
                        bytes[dst + c * 2..dst + c * 2 + 2]
                            .copy_from_slice(&f32_to_f16(v));
                    }
                }
                _ => {
                    let src_bytes: &[u8] = unsafe {
                        std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4)
                    };
                    let copy = src_bytes.len().min(bytes.len() - dst);
                    bytes[dst..dst + copy].copy_from_slice(&src_bytes[..copy]);
                }
            }
        }

        let mut enc = device.create_encoder("upload");
        enc.upload_texture(&tex, width, height, 1, &bytes);
        enc.commit_and_wait_completed();
        tex
    }

    /// Read back a texture into a f32 slice via a shared buffer blit.
    fn readback_texture_via_buffer(device: &GpuDevice, tex: &GpuTexture, buf: &mut [f32]) {
        let bpp = tex.format.bytes_per_pixel();
        let row_bytes = bpp * tex.width;
        let total = (row_bytes * tex.height) as u64;
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("readback");
        enc.copy_texture_to_buffer(tex, &readback, tex.width, tex.height, row_bytes);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared buffer mapped pointer");
        let f32_len = (total / 4) as usize;
        assert!(buf.len() >= f32_len);
        unsafe {
            std::ptr::copy_nonoverlapping(ptr as *const f32, buf.as_mut_ptr(), f32_len);
        }
    }

    /// Mirror of `denoiser::tests::denoise_reduces_error_vs_clean_ramp`, but
    /// exercises the full MTL4 bridge: classic command buffer signals, MTL4
    /// queue waits/encodes/scales, classic queue waits on MTL4 completion.
    /// If Metal 4 requires explicit residency sets for our classic-created
    /// input textures, this test will crash or return garbage.
    #[test]
    fn m4_denoise_reduces_error_vs_clean_ramp() {
        let device = GpuDevice::new();

        if !metalfx_m4_denoiser_available() {
            eprintln!(
                "[metalfx_m4 test] MTL4FXTemporalDenoisedScaler not available -- skipping"
            );
            return;
        }

        const W: u32 = 64;
        const H: u32 = 64;

        let bridge = device
            .mtl4_bridge()
            .expect("MTL4 bridge must be available when MTL4FX scaler class exists");
        let scaler = Metal4FxDenoisedScaler::new(
            device.raw_device(),
            bridge,
            W,
            H,
            W,
            H,
            GpuTextureFormat::Rgba32Float,
            false, // depth not reversed
        );

        let Some(scaler) = scaler else {
            panic!(
                "Metal4FxDenoisedScaler::new returned None on a device that \
                 reported metalfx_m4_denoiser_available()"
            );
        };

        let npixels = (W * H) as usize;
        let ncomponents = npixels * 4;
        let mut clean = vec![0.0f32; ncomponents];
        let mut noisy = vec![0.0f32; ncomponents];
        let scale = 1.0 / (W + H - 2) as f32;
        let noise_amplitude: f32 = 0.15;
        let mut rng = {
            let mut state: u64 = (W as u64) << 32 | (H as u64);
            move || -> f32 {
                state = state.wrapping_add(0x9E3779B97F4A7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                z = z ^ (z >> 31);
                (z as f32 / u64::MAX as f32) * 2.0 - 1.0
            }
        };

        for y in 0..H {
            for x in 0..W {
                let idx = ((y * W + x) * 4) as usize;
                let v = (x + y) as f32 * scale;
                clean[idx] = v;
                clean[idx + 3] = 1.0;
                let noise = rng() * noise_amplitude;
                noisy[idx] = (v + noise).clamp(0.0, 1.0);
                noisy[idx + 3] = 1.0;
            }
        }

        let mut albedo_data = vec![0.0f32; ncomponents];
        let mut normal_data = vec![0.0f32; ncomponents];
        for i in 0..npixels {
            let a = i * 4;
            albedo_data[a] = clean[a];
            albedo_data[a + 3] = 1.0;
            normal_data[a + 2] = 1.0;
        }

        let color_noisy = upload_texture(
            &device, W, H, GpuTextureFormat::Rgba32Float, &noisy, "m4_color_noisy",
        );
        let depth_tex = upload_texture(
            &device, W, H, GpuTextureFormat::R32Float, &vec![0.5f32; npixels], "m4_depth",
        );
        let motion_tex = upload_texture(
            &device, W, H, GpuTextureFormat::Rg16Float, &vec![0.0f32; npixels * 2], "m4_motion",
        );
        let normal_tex = upload_texture(
            &device, W, H, GpuTextureFormat::Rgba16Float, &normal_data, "m4_normal",
        );
        let roughness_tex = upload_texture(
            &device, W, H, GpuTextureFormat::R16Float, &vec![0.5f32; npixels], "m4_roughness",
        );
        let diffuse_tex = upload_texture(
            &device, W, H, GpuTextureFormat::Rgba16Float, &albedo_data, "m4_diffuse_albedo",
        );
        let specular_tex = upload_texture(
            &device,
            W,
            H,
            GpuTextureFormat::Rgba16Float,
            &vec![0.0f32; ncomponents],
            "m4_specular_albedo",
        );
        let hit_distance_tex = upload_texture(
            &device,
            W,
            H,
            GpuTextureFormat::R16Float,
            &vec![0.0f32; npixels],
            "m4_specular_hit_distance",
        );

        let output_tex = device.create_texture(&GpuTextureDesc {
            width: W,
            height: H,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "m4_denoiser_output",
            mip_levels: 1,
        });

        let mut enc = device.create_encoder("m4-denoise-proof");
        let encoded = scaler.encode(
            enc.raw_cmd_buf(),
            &color_noisy,
            &depth_tex,
            false,
            &motion_tex,
            &normal_tex,
            &roughness_tex,
            &diffuse_tex,
            &specular_tex,
            &hit_distance_tex,
            None,
            &output_tex,
            true,  // reset
            None,  // no reactive mask
        );
        assert!(
            encoded,
            "MTL4 denoiser encode skipped on a single-frame test (allocator ring should not be saturated)"
        );
        enc.commit_and_wait_completed();

        let mut result = vec![0.0f32; ncomponents];
        readback_texture_via_buffer(&device, &output_tex, &mut result);

        let mut noisy_error_sum: f64 = 0.0;
        let mut result_error_sum: f64 = 0.0;
        for i in 0..npixels {
            let idx = i * 4;
            let clean_r = clean[idx];
            let noisy_r = noisy[idx];
            let result_r = result[idx];
            noisy_error_sum += (noisy_r - clean_r).abs() as f64;
            result_error_sum += (result_r - clean_r).abs() as f64;
        }
        let nf = npixels as f64;
        let noisy_mae = noisy_error_sum / nf;
        let result_mae = result_error_sum / nf;

        eprintln!(
            "[m4 denoiser proof] Noisy MAE: {:.6}, Denoised MAE: {:.6}, Reduction: {:.1}%",
            noisy_mae,
            result_mae,
            (1.0 - result_mae / noisy_mae.max(f32::EPSILON as f64)) * 100.0
        );

        assert!(
            result_mae < noisy_mae,
            "MTL4 denoised MAE ({:.6}) not below noisy MAE ({:.6}) -- denoiser did not reduce error",
            result_mae,
            noisy_mae
        );

        assert!(
            result_mae < noisy_mae * 0.5,
            "MTL4 denoised MAE ({:.6}) not below 50% of noisy MAE ({:.6}) -- denoiser reduction too weak",
            result_mae,
            noisy_mae,
        );
    }

    /// Smoke proof for the MTL4 temporal scaler: on a rig reporting
    /// available, creation must succeed, one encode must complete without
    /// GPU error, and the output must carry content (any nonzero byte).
    /// This is the tripwire for the barrier-stages requirement documented
    /// on `set_mtl4fx_barrier_stages`.
    #[test]
    fn m4_temporal_scaler_encodes_one_frame() {
        let device = GpuDevice::new();

        if !metalfx_m4_temporal_available() {
            eprintln!("[metalfx_m4 test] MTL4FXTemporalScaler not available -- skipping");
            return;
        }

        const IN_W: u32 = 64;
        const IN_H: u32 = 64;
        const OUT_W: u32 = 128;
        const OUT_H: u32 = 128;

        let bridge = device
            .mtl4_bridge()
            .expect("MTL4 bridge must be available when MTL4FX temporal is available");
        let scaler = Metal4FxTemporalScaler::new(
            device.raw_device(),
            bridge,
            IN_W,
            IN_H,
            OUT_W,
            OUT_H,
            GpuTextureFormat::Rgba16Float,
        )
        .expect("Metal4FxTemporalScaler::new returned None on a rig reporting available");

        let npixels = (IN_W * IN_H) as usize;
        let mut color_data = vec![0.0f32; npixels * 4];
        for y in 0..IN_H {
            for x in 0..IN_W {
                let idx = ((y * IN_W + x) * 4) as usize;
                color_data[idx] = (x + y) as f32 / (IN_W + IN_H - 2) as f32;
                color_data[idx + 3] = 1.0;
            }
        }
        let color = upload_texture(
            &device, IN_W, IN_H, GpuTextureFormat::Rgba16Float, &color_data, "m4t_color",
        );
        let depth = upload_texture(
            &device, IN_W, IN_H, GpuTextureFormat::R32Float, &vec![0.5f32; npixels], "m4t_depth",
        );
        let motion = upload_texture(
            &device, IN_W, IN_H, GpuTextureFormat::Rg16Float, &vec![0.0f32; npixels * 2], "m4t_motion",
        );
        let output = device.create_texture(&GpuTextureDesc {
            width: OUT_W,
            height: OUT_H,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "m4t_output",
            mip_levels: 1,
        });

        let mut enc = device.create_encoder("m4-temporal-smoke");
        let encoded = scaler.encode(
            enc.raw_cmd_buf(),
            &color,
            &depth,
            &motion,
            &output,
            0.0,
            0.0,
            true, // reset
        );
        assert!(
            encoded,
            "MTL4 temporal encode skipped on a single-frame test (allocator ring should not be saturated)"
        );
        enc.commit_and_wait_completed();

        let bpp = output.format.bytes_per_pixel();
        let row_bytes = bpp * output.width;
        let total = (row_bytes * output.height) as u64;
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("readback");
        enc.copy_texture_to_buffer(&output, &readback, output.width, output.height, row_bytes);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared buffer mapped pointer");
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, total as usize) };
        let nonzero = bytes.iter().filter(|&&b| b != 0).count();
        assert!(
            nonzero > 0,
            "MTL4 temporal scaler output is all zeros — encode completed but wrote nothing"
        );
    }
}
