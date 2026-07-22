//! `docs/RAYTRACING_DESIGN.md` §5.2 P1 — value-level proof for the
//! `manifold-gpu` hard-shadow-ray trait/kernel
//! (`manifold_gpu::raytrace::{ShadowRayTracer, MetalShadowRayTracer}`),
//! ported from `tools/rt_prototype/src/accel.rs` +
//! `tools/rt_prototype/shaders/rt_trace.metal`'s shadow-only slice.
//!
//! Fixture (the P1 gate's literal "2-triangle occluder" fixture): one
//! quad (2 triangles, `y = 1`, spanning `x,z in [-1, 1]`) as the sole
//! occluder. Two shading points, `trace_size == gbuffer_size` (2x1, no
//! upsample in this fixture — the gate asks for the shadow TERM, not the
//! upsample pass):
//!
//! - texel 0: world pos `(0, 0, 0)`, normal `(0, 1, 0)` — directly under
//!   the quad. A hard (`sun_cone = 0`, `shadow_spp = 1`, so the single ray
//!   is exactly `sun_dir`, no RNG jitter) shadow ray straight up (`sun_dir
//!   = (0, 1, 0)`) MUST hit the quad. CPU oracle: occluded, `vis == 0.0`
//!   exactly.
//! - texel 1: world pos `(5, 0, 0)`, same normal/sun_dir. The quad's `x`
//!   extent is `[-1, 1]`, so the same straight-up ray passes outside the
//!   quad entirely. CPU oracle: unoccluded, `vis == 1.0` exactly.
//!
//! Both are exact (not merely "> threshold") because `shadow_spp = 1` and
//! `sun_cone = 0` remove all sampling — the kernel evaluates one
//! deterministic ray-triangle intersection per texel, matching a hand
//! solved intersection exactly.

use std::ffi::c_void;
use std::slice;

use manifold_gpu::raytrace::{MetalShadowRayTracer, ShadowRayParams, ShadowRayTracer};
use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

use crate::harness;

/// `packed_float3` stride-12 vertex layout — must match `raytrace.rs`'s
/// `make_descriptor` (`setVertexFormat(Float3); setVertexStride(12)`).
#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertex {
    pos: [f32; 3],
}

fn write_shared_buffer<T: Copy>(device: &GpuDevice, data: &[T]) -> manifold_gpu::GpuBuffer {
    let bytes = std::mem::size_of_val(data) as u64;
    let buf = device.create_buffer_shared(bytes.max(16));
    let ptr = buf
        .mapped_ptr()
        .expect("shared buffer must expose a mapped pointer");
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), ptr, bytes as usize);
    }
    buf
}

fn upload_texture_f32(
    device: &GpuDevice,
    width: u32,
    height: u32,
    format: GpuTextureFormat,
    pixels: &[f32],
    label: &str,
) -> manifold_gpu::GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width,
        height,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    });
    let bytes =
        unsafe { slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(pixels)) };
    device.upload_texture(&texture, bytes);
    texture
}

#[test]
fn shadow_rays_2tri_occluder_matches_cpu_oracle() {
    let h = harness::shared();
    let device = &h.device;

    // ─── Occluder: one quad (2 triangles), y = 1, x,z in [-1, 1] ──────
    let verts = [
        PackedVertex { pos: [-1.0, 1.0, -1.0] },
        PackedVertex { pos: [1.0, 1.0, -1.0] },
        PackedVertex { pos: [1.0, 1.0, 1.0] },
        PackedVertex { pos: [-1.0, 1.0, 1.0] },
    ];
    let indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
    let vertex_buffer = write_shared_buffer(device, &verts);
    let index_buffer = write_shared_buffer(device, &indices);

    let tracer = MetalShadowRayTracer::new(device);
    let accel = tracer.build_accel(device, &vertex_buffer, &index_buffer, 2);

    // ─── G-buffer fixture: 2x1, texel 0 occluded, texel 1 lit ─────────
    // rgba32f: xyz = world pos, w = view dist (>0 = valid surface).
    let g_wpos_px: [f32; 8] = [
        0.0, 0.0, 0.0, 1.0, // texel 0: under the quad
        5.0, 0.0, 0.0, 1.0, // texel 1: outside the quad's x extent
    ];
    let g_wpos = upload_texture_f32(device, 2, 1, GpuTextureFormat::Rgba32Float, &g_wpos_px, "rt-p1-g_wpos");
    // rgba16f would halve precision unnecessarily for this fixture;
    // Rgba32Float is a valid `texture2d<float>` read source for the
    // kernel same as Rgba16Float — only the format tag differs.
    let g_nrm_px: [f32; 8] = [0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let g_nrm = upload_texture_f32(device, 2, 1, GpuTextureFormat::Rgba32Float, &g_nrm_px, "rt-p1-g_nrm");

    let out_sv = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "rt-p1-out_sv",
        mip_levels: 1,
    });

    let params = ShadowRayParams {
        sun_dir: [0.0, 1.0, 0.0],
        sun_cone: 0.0,
        shadow_spp: 1,
        frame_index: 0,
        trace_size: [2, 1],
        gbuffer_size: [2, 1],
    };
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);

    let mut encoder = device.create_encoder("rt-p1-shadow-proof");
    tracer.dispatch_shadow_rays(
        &mut encoder,
        &accel,
        &params,
        &params_buffer,
        &g_wpos,
        &g_nrm,
        &out_sv,
        "trace_shadow_rays-proof",
    );
    encoder.commit_and_wait_completed();

    // ─── Readback + CPU-oracle comparison ──────────────────────────────
    let readback_buf = device.create_buffer_shared(2 * 4 * 4); // 2 texels * rgba * f32
    let mut enc2 = device.create_encoder("rt-p1-readback");
    enc2.copy_texture_to_buffer(&out_sv, &readback_buf, 2, 1, 2 * 4 * 4);
    enc2.commit_and_wait_completed();
    let ptr = readback_buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let bytes: &[u8] = unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), 32) };
    let floats: &[f32] = unsafe { slice::from_raw_parts(bytes.as_ptr().cast::<f32>(), 8) };

    let vis_occluded = floats[0]; // texel 0's r channel
    let vis_lit = floats[4]; // texel 1's r channel

    assert_eq!(
        vis_occluded, 0.0,
        "texel 0 (under the 2-tri occluder) must be exactly shadowed (CPU oracle: ray straight \
         up from (0,0,0) hits the quad at y=1, x,z in [-1,1]) — got {vis_occluded}"
    );
    assert_eq!(
        vis_lit, 1.0,
        "texel 1 (x=5, outside the quad's x in [-1,1] extent) must be exactly lit (CPU oracle: \
         ray straight up misses the quad entirely) — got {vis_lit}"
    );
}
