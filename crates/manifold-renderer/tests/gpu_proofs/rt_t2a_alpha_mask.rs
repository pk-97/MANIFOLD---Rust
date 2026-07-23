//! `docs/RAYTRACING_DESIGN.md` §8.2 Tier-2 item 4 (D21, T2-A) — value-level
//! proof for alpha-aware shadow rays
//! (`manifold_gpu::raytrace::{ShadowRayTracer, MetalShadowRayTracer}`'s
//! `walk_with_alpha_test`/`sample_candidate_alpha` MSL path).
//!
//! Fixture: ONE occluder object — a flat (non-indexed) quad, 2 triangles,
//! `z = 1`, spanning `x,y in [-1, 1]` (covers BOTH texels' reconstructed
//! world-x below — unlike `rt_p1_shadow.rs`'s narrower occluder, this test
//! isolates the ALPHA term, not geometric coverage). UV mapped
//! `u = (x+1)/2` (vertex `x=-1 -> u=0`, `x=1 -> u=1`), `v` unused (constant
//! per this fixture's y-range).
//!
//! Same 2x1 depth fixture as `rt_p1_shadow.rs` (`inv_view_proj = IDENTITY`):
//! texel 0 (pixel (0,0)) reconstructs `world = (-0.5, 0, 0.3)`; texel 1
//! (pixel (1,0)) reconstructs `world = (0.5, 0, 0.3)`. `sun_dir = (0,0,1)`
//! (hard, no cone, `shadow_spp=1`): both rays travel `+z` and hit the quad
//! at `z=1`, texel 0 at `x=-0.5 -> u=0.25`, texel 1 at `x=0.5 -> u=0.75`.
//!
//! Base-color texture: 2x1 checkerboard, texel 0 alpha=0.0 (fully
//! transparent), texel 1 alpha=1.0 (fully opaque); `alpha_cutoff = 0.5`.
//! NEAREST sampling (`sample_candidate_alpha`'s discipline) maps `u=0.25`
//! to texel 0 (alpha 0.0, below cutoff — ray CONTINUES through, unblocked)
//! and `u=0.75` to texel 1 (alpha 1.0, at/above cutoff — ray blocked).
//!
//! Assert 1 (`AlphaMode::Mask`): texel 0 (behind the transparent texel)
//! `vis == 1.0` (lit); texel 1 (behind the opaque texel) `vis == 0.0`
//! (shadowed) — CPU oracle: alpha-below-cutoff must NOT register as a hit.
//! Assert 2 (`alpha_mask = false`, SAME geometry/texture): both texels
//! `vis == 0.0` — the quad geometrically covers both world positions, and
//! the opaque fast path (`encode_blas_build`'s `setOpaque(true)`) must
//! ignore the texture entirely, exactly as it did before this feature.

use std::ffi::c_void;
use std::slice;

use manifold_gpu::raytrace::{
    ensure_normal_sources, GiMaterial, MetalShadowRayTracer, RtObjectGeometry, ShadowRayParams,
    ShadowRayTracer,
};
use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

use crate::harness;

/// Flat (non-indexed) vertex layout: 12-byte position + 8-byte UV, no
/// padding — `packed_float3`/`packed_float2` mandatory (P0 §5.1 kernel
/// lesson), stride 20.
#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertexUV {
    pos: [f32; 3],
    uv: [f32; 2],
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

/// Runs the shared fixture (occluder + depth + textures) with `alpha_mask`
/// toggled, returning `[vis_texel0, vis_texel1]` (r channel of `out_sv`).
fn run_fixture(alpha_mask: bool) -> [f32; 2] {
    let h = harness::shared();
    let device = &h.device;

    // ─── Occluder: one quad at z=1, x,y in [-1,1], u=(x+1)/2 ──
    let verts = [
        PackedVertexUV { pos: [-1.0, -1.0, 1.0], uv: [0.0, 0.0] },
        PackedVertexUV { pos: [1.0, -1.0, 1.0], uv: [1.0, 0.0] },
        PackedVertexUV { pos: [1.0, 1.0, 1.0], uv: [1.0, 1.0] },
        PackedVertexUV { pos: [-1.0, -1.0, 1.0], uv: [0.0, 0.0] },
        PackedVertexUV { pos: [1.0, 1.0, 1.0], uv: [1.0, 1.0] },
        PackedVertexUV { pos: [-1.0, 1.0, 1.0], uv: [0.0, 1.0] },
    ];
    let vertex_buffer = write_shared_buffer(device, &verts);

    // ─── Base-color texture: 2x1 checkerboard, texel0 alpha=0 (transparent),
    // texel1 alpha=1 (opaque) ──
    let tex_px: [f32; 8] = [
        0.0, 0.0, 0.0, 0.0, // texel 0: rgba, alpha=0.0
        1.0, 1.0, 1.0, 1.0, // texel 1: rgba, alpha=1.0
    ];
    let base_color_tex = upload_texture_f32(device, 2, 1, GpuTextureFormat::Rgba32Float, &tex_px, "rt-t2a-basecolor");

    let objects = [RtObjectGeometry {
        vertex_buffer: &vertex_buffer,
        vertex_stride: std::mem::size_of::<PackedVertexUV>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: 2,
        transform: IDENTITY,
        // ao_spp/gi_spp are 0 below — normal never read.
        normal_offset: 0,
        uv_offset: std::mem::size_of::<[f32; 3]>() as u32, // 12: uv follows position
        alpha_mask,
        alpha_cutoff: 0.5,
        base_color_texture: Some(&base_color_tex),
    }];

    let tracer = MetalShadowRayTracer::new(device);
    let accel = tracer.build_accel(device, &objects);

    let mut normal_sources_slot = None;
    let mut normal_sources_capacity = 0usize;
    let alpha_textures = ensure_normal_sources(&mut normal_sources_slot, &mut normal_sources_capacity, device, &objects);
    let normal_sources_buffer = normal_sources_slot.expect("ensure_normal_sources must allocate");

    // ─── Depth fixture: 2x1, both texels valid (depth=0.3) — identical to
    // rt_p1_shadow.rs's fixture ──
    let depth_px: [f32; 2] = [0.3, 0.3];
    let depth_tex = upload_texture_f32(device, 2, 1, GpuTextureFormat::Depth32Float, &depth_px, "rt-t2a-depth");

    let out_sv = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "rt-t2a-out_sv",
        mip_levels: 1,
    });
    let out_irr = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-t2a-out_irr-stub",
        mip_levels: 1,
    });
    let out_n = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-t2a-out_n-stub",
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
        0,
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        IDENTITY,
    );
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);
    let gi_materials_buffer = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);

    let mut encoder = device.create_encoder("rt-t2a-shadow-proof");
    tracer.dispatch_shadow_rays(
        &mut encoder,
        &accel,
        &params,
        &params_buffer,
        &gi_materials_buffer,
        &normal_sources_buffer,
        &alpha_textures,
        &depth_tex,
        &out_sv,
        &out_irr,
        &out_n,
        "trace_shadow_rays-t2a-proof",
    );
    encoder.commit_and_wait_completed();

    let readback_buf = device.create_buffer_shared(2 * 4 * 4);
    let mut enc2 = device.create_encoder("rt-t2a-readback");
    enc2.copy_texture_to_buffer(&out_sv, &readback_buf, 2, 1, 2 * 4 * 4);
    enc2.commit_and_wait_completed();
    let ptr = readback_buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let bytes: &[u8] = unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), 32) };
    let floats: &[f32] = unsafe { slice::from_raw_parts(bytes.as_ptr().cast::<f32>(), 8) };

    [floats[0], floats[4]]
}

#[test]
fn alpha_mask_below_cutoff_texel_unblocks_shadow_ray() {
    let [vis_texel0, vis_texel1] = run_fixture(true);
    assert_eq!(
        vis_texel0, 1.0,
        "texel 0 (hit u=0.25, checkerboard alpha=0.0, below cutoff 0.5) must be LIT — the ray must \
         continue through a below-cutoff texel instead of registering a hit — got {vis_texel0}"
    );
    assert_eq!(
        vis_texel1, 0.0,
        "texel 1 (hit u=0.75, checkerboard alpha=1.0, at/above cutoff 0.5) must be SHADOWED — got \
         {vis_texel1}"
    );
}

#[test]
fn alpha_opaque_mode_ignores_texture_stays_fully_shadowed() {
    let [vis_texel0, vis_texel1] = run_fixture(false);
    assert_eq!(
        vis_texel0, 0.0,
        "opaque fast path (alpha_mask=false): texel 0 must stay SHADOWED — the same quad now \
         geometrically covers both texels, and the opaque path must ignore the texture entirely \
         (never sample it) — got {vis_texel0}"
    );
    assert_eq!(
        vis_texel1, 0.0,
        "opaque fast path (alpha_mask=false): texel 1 must stay SHADOWED — got {vis_texel1}"
    );
}
