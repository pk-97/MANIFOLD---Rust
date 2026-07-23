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

use manifold_gpu::raytrace::{
    GiMaterial, MetalShadowRayTracer, RtObjectGeometry, ShadowRayParams, ShadowRayTracer,
};
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
        // ao_spp/gi_spp are 0 in this proof's params — the only two
        // consumers of the fetched normal — so this fixture's position-only
        // `PackedVertex` (no normal field) never has it read.
        normal_offset: 0,
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
    // RAYTRACING_DESIGN.md §5.2 P2 widened `trace_shadow_rays` to also
    // write demodulated irradiance — this P1 proof only asserts on
    // `out_sv` (shadow visibility), so `out_irr` is an unread write
    // target, same ABI-stub discipline as every other unused-but-required
    // binding in this codebase.
    let out_irr = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-p1-out_irr-stub",
        mip_levels: 1,
    });
    // RT-T1-C: `trace_shadow_rays` now also writes the primary-hit vertex
    // normal — same unread-stub discipline as `out_irr` above (this P1
    // proof only asserts on `out_sv`).
    let out_n = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-p1-out_n-stub",
        mip_levels: 1,
    });

    // ao_spp: 0 (AO gather skipped — P1 fixture only proves hard shadows);
    // sun_color/ambient_color: unused by this test's assertions (out_irr
    // is never read here).
    let params = ShadowRayParams::new(
        [0.0, 0.0, 1.0],
        0.0,
        1,
        0,
        [2, 1],
        [2, 1],
        0.0,
        0,
        0, // RT-P3: gi_spp — 0, GI gather skipped, this proof only asserts on out_sv
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0], // RT-T1-B: camera_pos — unused, ao_spp/gi_spp both 0 above
        IDENTITY,
    );
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);
    // RT-P3: unread by this proof (gi_spp == 0 above), same ABI-stub
    // discipline as `out_irr` — one zeroed entry.
    let gi_materials_buffer =
        device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
    // RT-T1-B: unread by this proof (ao_spp == 0 && gi_spp == 0 above),
    // same ABI-stub discipline as `gi_materials_buffer`.
    let normal_sources_buffer =
        device.create_buffer_shared(std::mem::size_of::<manifold_gpu::raytrace::RtNormalSource>() as u64);

    let mut encoder = device.create_encoder("rt-p1-shadow-proof");
    tracer.dispatch_shadow_rays(
        &mut encoder,
        &accel,
        &params,
        &params_buffer,
        &gi_materials_buffer,
        &normal_sources_buffer,
        &depth_tex,
        &out_sv,
        &out_irr,
        &out_n,
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

/// BUG-309 follow-up (docs/BUG_BACKLOG.md) — the SAME fixture as
/// `shadow_rays_2tri_occluder_matches_cpu_oracle` above, but with a SECOND
/// BLAS added (a large ground quad at `z = 0`, instance index 0) ahead of
/// the occluder (now instance index 1) — mirroring `render_scene.rs`'s
/// real two-object TLAS (ground first, occluder second) exactly, which
/// the single-BLAS proof above never exercised. This is the permanent
/// gatekeeper for the whole "multi-BLAS instance/transform wiring" class:
/// BUG-309's remaining defect (the real scene's shadow only covering
/// roughly half its computed-correct footprint) was suspected to be a
/// multi-BLAS-specific bug, and this is the minimal rig to catch it if it
/// is one, kept green regardless of whether it reproduces the defect
/// (the isolated single-BLAS-only proof is HOW this bug class escaped).
///
/// The ground quad sits BEHIND both texels' rays (rays travel `+z` from
/// `z=0.3+bias`, away from `z=0`) — it must never register a hit itself,
/// and its mere presence in the TLAS must not change the occluder-hit
/// outcome for either texel.
#[test]
fn shadow_rays_2blas_ground_plus_occluder_matches_cpu_oracle() {
    let h = harness::shared();
    let device = &h.device;

    // ─── Object 0: ground quad at z=0, spanning x,y in [-10,10] ──
    let ground_verts = [
        PackedVertex { pos: [-10.0, -10.0, 0.0] },
        PackedVertex { pos: [10.0, -10.0, 0.0] },
        PackedVertex { pos: [10.0, 10.0, 0.0] },
        PackedVertex { pos: [-10.0, 10.0, 0.0] },
    ];
    let ground_indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
    let ground_vertex_buffer = write_shared_buffer(device, &ground_verts);
    let ground_index_buffer = write_shared_buffer(device, &ground_indices);

    // ─── Object 1: occluder quad at z=1, x in [-1, 0], y in [-1, 1] ──
    // (byte-identical to the single-BLAS proof's occluder above.)
    let occ_verts = [
        PackedVertex { pos: [-1.0, -1.0, 1.0] },
        PackedVertex { pos: [0.0, -1.0, 1.0] },
        PackedVertex { pos: [0.0, 1.0, 1.0] },
        PackedVertex { pos: [-1.0, 1.0, 1.0] },
    ];
    let occ_indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
    let occ_vertex_buffer = write_shared_buffer(device, &occ_verts);
    let occ_index_buffer = write_shared_buffer(device, &occ_indices);

    let tracer = MetalShadowRayTracer::new(device);
    let objects = [
        RtObjectGeometry {
            vertex_buffer: &ground_vertex_buffer,
            vertex_stride: std::mem::size_of::<PackedVertex>() as u32,
            vertex_offset: 0,
            index_buffer: Some(&ground_index_buffer),
            triangle_count: 2,
            transform: IDENTITY,
            // ao_spp/gi_spp are 0 in this proof's params — the only two
            // consumers of the fetched normal — so this fixture's
            // position-only `PackedVertex` (no normal field) never has it
            // read.
            normal_offset: 0,
        },
        RtObjectGeometry {
            vertex_buffer: &occ_vertex_buffer,
            vertex_stride: std::mem::size_of::<PackedVertex>() as u32,
            vertex_offset: 0,
            index_buffer: Some(&occ_index_buffer),
            triangle_count: 2,
            transform: IDENTITY,
            normal_offset: 0,
        },
    ];
    let accel = tracer.build_accel(device, &objects);

    // ─── Depth fixture: identical to the single-BLAS proof ──
    let depth_px: [f32; 2] = [0.3, 0.3];
    let depth_tex = upload_texture_f32(device, 2, 1, GpuTextureFormat::Depth32Float, &depth_px, "rt-p1-2blas-depth");

    let out_sv = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "rt-p1-2blas-out_sv",
        mip_levels: 1,
    });
    // See the single-BLAS proof above for why this stub exists.
    let out_irr = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-p1-2blas-out_irr-stub",
        mip_levels: 1,
    });
    // RT-T1-C: see the single-BLAS proof above for why this stub exists.
    let out_n = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-p1-2blas-out_n-stub",
        mip_levels: 1,
    });

    let params = ShadowRayParams::new(
        [0.0, 0.0, 1.0],
        0.0,
        1,
        0,
        [2, 1],
        [2, 1],
        0.0,
        0,
        0, // RT-P3: gi_spp — 0, GI gather skipped, this proof only asserts on out_sv
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0], // RT-T1-B: camera_pos — unused, ao_spp/gi_spp both 0 above
        IDENTITY,
    );
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);
    // RT-P3: unread by this proof (gi_spp == 0 above), same ABI-stub
    // discipline as `out_irr` — one zeroed entry.
    let gi_materials_buffer =
        device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
    // RT-T1-B: unread by this proof (ao_spp == 0 && gi_spp == 0 above),
    // same ABI-stub discipline as `gi_materials_buffer`.
    let normal_sources_buffer =
        device.create_buffer_shared(std::mem::size_of::<manifold_gpu::raytrace::RtNormalSource>() as u64);

    let mut encoder = device.create_encoder("rt-p1-2blas-shadow-proof");
    tracer.dispatch_shadow_rays(
        &mut encoder,
        &accel,
        &params,
        &params_buffer,
        &gi_materials_buffer,
        &normal_sources_buffer,
        &depth_tex,
        &out_sv,
        &out_irr,
        &out_n,
        "trace_shadow_rays-2blas-proof",
    );
    encoder.commit_and_wait_completed();

    let readback_buf = device.create_buffer_shared(2 * 4 * 4);
    let mut enc2 = device.create_encoder("rt-p1-2blas-readback");
    enc2.copy_texture_to_buffer(&out_sv, &readback_buf, 2, 1, 2 * 4 * 4);
    enc2.commit_and_wait_completed();
    let ptr = readback_buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let bytes: &[u8] = unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), 32) };
    let floats: &[f32] = unsafe { slice::from_raw_parts(bytes.as_ptr().cast::<f32>(), 8) };

    let vis_occluded = floats[0];
    let vis_lit = floats[4];

    assert_eq!(
        vis_occluded, 0.0,
        "two-BLAS fixture: texel 0 (world x=-0.5, inside the occluder's x in [-1,0]) must be \
         exactly shadowed — got {vis_occluded}. The ground BLAS at instance index 0 must not \
         change this outcome from the single-BLAS proof."
    );
    assert_eq!(
        vis_lit, 1.0,
        "two-BLAS fixture: texel 1 (world x=0.5, outside the occluder's x in [-1,0]) must be \
         exactly lit — got {vis_lit}. If this reads 0.0, the ray is self-intersecting the ground \
         BLAS (or the ground is wrongly shadowing) — the multi-BLAS wiring class BUG-309 suspected."
    );
}
