//! `docs/RAYTRACING_DESIGN.md` §5.2 P1/RT-D3 — value-level proof for the
//! `manifold-gpu` hard-shadow-ray trait/kernel
//! (`manifold_gpu::raytrace::{ShadowRayTracer, MetalShadowRayTracer}`),
//! ported from `tools/rt_prototype/src/accel.rs` +
//! `tools/rt_prototype/shaders/rt_trace.metal`'s shadow-only slice, RT-D3
//! integration: ray origins reconstructed in-kernel from a depth texture +
//! `inv_view_proj` (no g_wpos/g_nrm G-buffer).
//!
//! Fixture (the P1 gate's literal "2-triangle occluder" fixture), built
//! with `inv_view_proj = IDENTITY` so pixel -> NDC -> world is exactly
//! `world = (ndc_x, ndc_y, depth)` — lets this test control the exact
//! reconstructed world position per texel via pixel coordinate + depth
//! value alone, without needing a full camera:
//!
//! - `gbuffer_size = (2, 1)`. Texel 0 (pixel (0,0)): `uv = (0.25, 0.5)` ->
//!   `ndc = (-0.5, 0.0)`. Texel 1 (pixel (1,0)): `uv = (0.75, 0.5)` ->
//!   `ndc = (0.5, 0.0)`. Both texels' depth = 0.3 (same, non-void: `< 1.0
//!   - 1e-6`) -> world0 = (-0.5, 0, 0.3), world1 = (0.5, 0, 0.3).
//! - Occluder: one quad (2 triangles) in the world XY-plane at `z = 1`,
//!   spanning `x in [-1, 0]`, `y in [-1, 1]` — covers world0's `x = -0.5`,
//!   excludes world1's `x = 0.5`.
//! - `sun_dir = (0, 0, 1)` (hard, `sun_cone = 0`, `shadow_spp = 1` — no
//!   RNG, exactly one deterministic ray-triangle intersection per texel).
//!   world0's ray travels world.z: 0.3 -> +inf along constant x = -0.5 ->
//!   crosses the quad's z = 1 plane at x = -0.5, inside `[-1, 0]` -> HIT
//!   -> CPU oracle: `vis == 0.0` exactly. world1's ray at constant x =
//!   0.5 crosses z = 1 at x = 0.5, outside `[-1, 0]` -> MISS -> CPU
//!   oracle: `vis == 1.0` exactly.
//!
//! `MTLAccelerationStructureTriangleGeometryDescriptor`'s vertex format
//! needs Shared/CPU-writable storage for a depth-format texture upload —
//! confirmed working on Apple Silicon's unified memory (this crate's only
//! target); a discrete-GPU Vulkan backend would need a render pass here
//! instead (not this test's concern — Metal-only proof).

use std::ffi::c_void;
use std::slice;

use manifold_gpu::raytrace::{MetalShadowRayTracer, RtObjectGeometry, ShadowRayParams, ShadowRayTracer};
use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

use crate::harness;

/// `packed_float3` stride-12 vertex layout for this fixture's occluder —
/// `RtObjectGeometry::vertex_stride` need not match `MeshVertex`'s 48-byte
/// production stride; the trait reads whatever stride/offset it's told.
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

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

#[test]
fn shadow_rays_2tri_occluder_matches_cpu_oracle() {
    let h = harness::shared();
    let device = &h.device;

    // ─── Occluder: one quad (2 triangles) at z=1, x in [-1, 0], y in [-1, 1] ──
    let verts = [
        PackedVertex { pos: [-1.0, -1.0, 1.0] },
        PackedVertex { pos: [0.0, -1.0, 1.0] },
        PackedVertex { pos: [0.0, 1.0, 1.0] },
        PackedVertex { pos: [-1.0, 1.0, 1.0] },
    ];
    let indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
    let vertex_buffer = write_shared_buffer(device, &verts);
    let index_buffer = write_shared_buffer(device, &indices);

    let tracer = MetalShadowRayTracer::new(device);
    let objects = [RtObjectGeometry {
        vertex_buffer: &vertex_buffer,
        vertex_stride: std::mem::size_of::<PackedVertex>() as u32,
        vertex_offset: 0,
        index_buffer: Some(&index_buffer),
        triangle_count: 2,
        // Vertices are already world-space — identity transform.
        transform: IDENTITY,
    }];
    let accel = tracer.build_accel(device, &objects);

    // ─── Depth fixture: 2x1, both texels valid (depth=0.3, < 1.0 clear) ──
    let depth_px: [f32; 2] = [0.3, 0.3];
    let depth_tex = upload_texture_f32(device, 2, 1, GpuTextureFormat::Depth32Float, &depth_px, "rt-p1-depth");

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

    let params = ShadowRayParams::new([0.0, 0.0, 1.0], 0.0, 1, 0, [2, 1], [2, 1], IDENTITY);
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);

    let mut encoder = device.create_encoder("rt-p1-shadow-proof");
    tracer.dispatch_shadow_rays(
        &mut encoder,
        &accel,
        &params,
        &params_buffer,
        &depth_tex,
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

    let vis_occluded = floats[0]; // texel 0's r channel (world x=-0.5, inside occluder)
    let vis_lit = floats[4]; // texel 1's r channel (world x=0.5, outside occluder)

    assert_eq!(
        vis_occluded, 0.0,
        "texel 0 (reconstructed world x=-0.5, inside the occluder's x in [-1,0]) must be exactly \
         shadowed — got {vis_occluded}"
    );
    assert_eq!(
        vis_lit, 1.0,
        "texel 1 (reconstructed world x=0.5, outside the occluder's x in [-1,0]) must be exactly \
         lit — got {vis_lit}"
    );
}
