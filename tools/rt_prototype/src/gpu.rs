//! Thin wrapper around `manifold_gpu::GpuDevice` for device/queue setup and
//! buffer/texture allocation (amendment: reuse manifold-gpu rather than
//! reimplementing it — see BRIEF.md). Raw MSL library compile and the
//! raytracing compute-encoder timing helpers stay hand-rolled via
//! objc2-metal directly against `gpu.raw_device()`/`raw_queue()`: manifold-gpu's
//! pipeline path is WGSL-only (naga → MSL) and has no acceleration-structure
//! API, so `rt_trace.metal`'s `metal_raytracing` kernels and the AS build in
//! `accel.rs` cannot go through it.

use std::ffi::c_void;
use std::ptr::NonNull;

use manifold_gpu::{GpuBuffer, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBuffer, MTLCommandBuffer, MTLCommandQueue, MTLCompileOptions, MTLComputePipelineState,
    MTLDevice, MTLLanguageVersion, MTLLibrary, MTLTexture,
};

pub struct Gpu {
    pub device: GpuDevice,
}

impl Gpu {
    pub fn new() -> Self {
        let device = GpuDevice::new();
        println!("[gpu] device = {}", device.device_name());
        Self { device }
    }

    /// Compile an MSL source string into a `MTLLibrary`. Raytracing needs
    /// `metal_raytracing`, which requires the default (latest) language
    /// version — unlike manifold-gpu's WGSL-derived MSL pipelines we do NOT
    /// pin Version2_4 here, since that predates raytracing support.
    pub fn compile_library(&self, source: &str, label: &str) -> Retained<ProtocolObject<dyn MTLLibrary>> {
        let opts = MTLCompileOptions::init(MTLCompileOptions::alloc());
        opts.setLanguageVersion(MTLLanguageVersion::Version3_1);
        let src_ns = NSString::from_str(source);
        self.device
            .raw_device()
            .newLibraryWithSource_options_error(&src_ns, Some(&opts))
            .unwrap_or_else(|e| panic!("{label}: MTL library compile error: {}", e.localizedDescription()))
    }

    pub fn compute_pipeline(
        &self,
        library: &ProtocolObject<dyn MTLLibrary>,
        entry: &str,
    ) -> Retained<ProtocolObject<dyn MTLComputePipelineState>> {
        let name = NSString::from_str(entry);
        let func = library
            .newFunctionWithName(&name)
            .unwrap_or_else(|| panic!("entry point '{entry}' not found in compiled library"));
        self.device
            .raw_device()
            .newComputePipelineStateWithFunction_error(&func)
            .unwrap_or_else(|e| panic!("{entry}: compute PSO error: {}", e.localizedDescription()))
    }

    pub fn buffer_with_data<T: Copy>(&self, data: &[T]) -> GpuBuffer {
        let bytes = std::mem::size_of_val(data);
        let buf = self.device.create_buffer_shared(bytes.max(16) as u64);
        if bytes > 0 {
            let ptr = buf.raw().contents().as_ptr() as *mut u8;
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, ptr, bytes) };
        }
        buf
    }

    pub fn buffer_zeroed(&self, bytes: usize) -> GpuBuffer {
        self.device.create_buffer_shared(bytes.max(16) as u64)
    }

    /// `cpu_readable` requests `GpuTextureUsage::CPU_UPLOAD`, which is what
    /// steers manifold-gpu's `create_texture` to `StorageModeShared` instead
    /// of the default `StorageModePrivate` — needed for the PNG readback
    /// path (`read_rgba_f32`), not for intermediate compute/render targets.
    pub fn texture(
        &self,
        format: manifold_gpu::GpuTextureFormat,
        width: u32,
        height: u32,
        usage: manifold_gpu::GpuTextureUsage,
        cpu_readable: bool,
        label: &str,
    ) -> GpuTexture {
        let usage = if cpu_readable {
            usage | manifold_gpu::GpuTextureUsage::CPU_UPLOAD
        } else {
            usage
        };
        self.device.create_texture(&GpuTextureDesc {
            width,
            height,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage,
            label,
            mip_levels: 1,
        })
    }

    /// Read a CPU-readable (`cpu_readable: true` at creation) RGBA texture
    /// back to CPU as `[f32; 4]` per texel.
    pub fn read_rgba_f32(tex: &ProtocolObject<dyn MTLTexture>, width: u32, height: u32, bytes_per_channel: usize) -> Vec<[f32; 4]> {
        use objc2_metal::{MTLOrigin, MTLRegion, MTLSize};
        let bpp = 4 * bytes_per_channel;
        let bytes_per_row = width as usize * bpp;
        let mut raw = vec![0u8; bytes_per_row * height as usize];
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize { width: width as usize, height: height as usize, depth: 1 },
        };
        unsafe {
            tex.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                NonNull::new(raw.as_mut_ptr() as *mut c_void).unwrap(),
                bytes_per_row,
                region,
                0,
            );
        }
        let mut out = vec![[0f32; 4]; (width * height) as usize];
        if bytes_per_channel == 4 {
            for (i, px) in out.iter_mut().enumerate() {
                let off = i * 16;
                for c in 0..4 {
                    let b = [raw[off + c * 4], raw[off + c * 4 + 1], raw[off + c * 4 + 2], raw[off + c * 4 + 3]];
                    px[c] = f32::from_le_bytes(b);
                }
            }
        } else if bytes_per_channel == 2 {
            for (i, px) in out.iter_mut().enumerate() {
                let off = i * 8;
                for c in 0..4 {
                    let b = [raw[off + c * 2], raw[off + c * 2 + 1]];
                    px[c] = half_to_f32(u16::from_le_bytes(b));
                }
            }
        } else {
            panic!("unsupported bytes_per_channel {bytes_per_channel}");
        }
        out
    }

    /// New command buffer with GPU timing enabled by default (every command
    /// buffer's GPUStartTime/GPUEndTime is always available after
    /// waitUntilCompleted — no separate opt-in needed on macOS).
    pub fn command_buffer(&self, label: &str) -> Retained<ProtocolObject<dyn MTLCommandBuffer>> {
        let cb = self.device.raw_queue().commandBuffer().expect("commandBuffer failed");
        cb.setLabel(Some(&NSString::from_str(label)));
        cb
    }

    /// Commit, wait, and return (GPUEndTime - GPUStartTime) in milliseconds.
    pub fn commit_and_time(cb: &ProtocolObject<dyn MTLCommandBuffer>) -> f64 {
        cb.commit();
        cb.waitUntilCompleted();
        (cb.GPUEndTime() - cb.GPUStartTime()) * 1000.0
    }
}

fn half_to_f32(h: u16) -> f32 {
    let sign = (h >> 15) & 1;
    let exp = (h >> 10) & 0x1f;
    let frac = h & 0x3ff;
    let f: f32 = if exp == 0 {
        if frac == 0 {
            0.0
        } else {
            (frac as f32 / 1024.0) * 2f32.powi(-14)
        }
    } else if exp == 0x1f {
        if frac == 0 { f32::INFINITY } else { f32::NAN }
    } else {
        (1.0 + frac as f32 / 1024.0) * 2f32.powi(exp as i32 - 15)
    };
    if sign == 1 { -f } else { f }
}
