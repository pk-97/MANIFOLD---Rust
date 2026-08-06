//! `ShadowSoftness::Hard` must make the RT sun shadow mask a deterministic
//! function of the geometry: cone half-angle exactly 0.0, `cone_sample`
//! short-circuits, every pixel resolves to exactly 0.0 or 1.0 and is
//! bit-identical across frame indices — no jitter for the light the user
//! asked to be hard.
//!
//! (A fixed jitter seed for SOFT cones was tried here too and dropped:
//! the shadow mask gained real temporal accumulation on main — SV-ACCUM,
//! RAYTRACING_DESIGN.md section 15 (many-light) — which needs the
//! per-frame reseed to average, and a frozen seed broke the BUG-322
//! moving-object gate. Soft-cone temporal stability is SV-ACCUM's job,
//! enforced by `scripts/rt_noise_gate.py`.)
//!
//! Fixture — a deliberately worst-case model of the real symptom (blossom
//! petals under a canopy of other petals):
//!
//! - 64x1 depth image, `inv_view_proj = IDENTITY`, depth 0.3 everywhere,
//!   so pixel `i` reconstructs `world = ((i+0.5)/32 - 1, 0, 0.3)` — 64
//!   independent shading points spread across `x in (-1, 1)`.
//! - ONE alpha-masked occluder quad at `z = 50.3`, spanning `x,y in
//!   [-4, 4]`, `u = (x+4)/8`. Sun direction `+z`, so every shadow ray
//!   travels 50 world units to reach it and always lands inside the quad —
//!   geometric coverage is constant, which isolates the alpha term.
//! - Alpha texture: 8x1, alternating `alpha = 0.0 / 1.0`, `alpha_cutoff =
//!   0.5`. Each texel is 1.0 world unit wide at the occluder, and the real
//!   `Soft` sun cone (0.02 rad half-angle) spreads the ray up to
//!   `tan(0.02) * 50 ~= 1.0` unit — so a jittered ray can cross into the
//!   neighbouring cutout cell and flip between blocked and unblocked. That
//!   is exactly the coupling that made petals hatch and crawl.
//!
//! `shadow_spp = 1` (the production value): one sample decides each pixel,
//! so there is no averaging to hide a stray jitter.
//!
//! Assert: with `cone = 0.0` the 64 visibility values for `frame_index = 0`
//! are EXACTLY equal to those for `frame_index = 7`, every pixel exactly
//! 0.0 or 1.0. Exact, not tolerant — Hard means deterministic.
//!
//! Guard against a vacuous pass: the hard mask must hold a real mix of lit
//! and shadowed pixels, and the soft (0.02 rad) run must produce at least
//! one pixel that disagrees with the hard run — proof the fixture sits in
//! jitter-sensitive penumbra, so an accidentally nonzero Hard cone would
//! be caught.

use std::ffi::c_void;
use std::slice;

use manifold_gpu::raytrace::{
    ensure_normal_sources, GiMaterial, MetalShadowRayTracer, RtCasterParams, RtObjectGeometry,
    ShadowRayParams, ShadowRayTracer,
};
use manifold_gpu::{
    GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

use crate::harness;

/// Flat (non-indexed) 12-byte position + 8-byte UV, stride 20 — the
/// `packed_float3`/`packed_float2` layout `fetch_uv` reads.
#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertexUV {
    pos: [f32; 3],
    uv: [f32; 2],
}

const WIDTH: u32 = 64;
/// World-space z of the occluder quad. The shadow ray starts at z = 0.3, so
/// it travels 50 units — far enough that the real 0.02 rad cone spreads it
/// a full cutout cell sideways.
const OCCLUDER_Z: f32 = 50.3;
/// `SUN_CONE_SOFT_RADIANS` in `render_scene.rs` — the production `Soft` sun
/// cone. Kept in sync by hand (no cross-crate constant); the vacuity guard
/// below fails loudly if this stops spreading the ray across a cell.
const SOFT_CONE_RADIANS: f32 = 0.02;

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

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
    let bytes = unsafe {
        slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(pixels))
    };
    device.upload_texture(&texture, bytes);
    texture
}

fn write_only_texture(
    device: &GpuDevice,
    format: GpuTextureFormat,
    label: &str,
) -> manifold_gpu::GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: WIDTH,
        height: 1,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    })
}

/// Traces the fixture once and returns the 64 per-pixel sun visibility
/// values (`out_sv.r`).
fn run_fixture(cone_half_angle: f32, frame_index: u32) -> Vec<f32> {
    let h = harness::shared();
    let device = &h.device;

    // Occluder quad, x,y in [-4,4] at z = OCCLUDER_Z, u = (x+4)/8.
    let z = OCCLUDER_Z;
    let verts = [
        PackedVertexUV { pos: [-4.0, -4.0, z], uv: [0.0, 0.0] },
        PackedVertexUV { pos: [4.0, -4.0, z], uv: [1.0, 0.0] },
        PackedVertexUV { pos: [4.0, 4.0, z], uv: [1.0, 1.0] },
        PackedVertexUV { pos: [-4.0, -4.0, z], uv: [0.0, 0.0] },
        PackedVertexUV { pos: [4.0, 4.0, z], uv: [1.0, 1.0] },
        PackedVertexUV { pos: [-4.0, 4.0, z], uv: [0.0, 1.0] },
    ];
    let vertex_buffer = write_shared_buffer(device, &verts);

    // 8x1 cutout stripe: alpha 0,1,0,1,... Each cell is 1.0 world unit wide.
    let mut tex_px = Vec::with_capacity(8 * 4);
    for i in 0..8u32 {
        let a = if i % 2 == 0 { 0.0 } else { 1.0 };
        tex_px.extend_from_slice(&[1.0, 1.0, 1.0, a]);
    }
    let base_color_tex = upload_texture_f32(
        device,
        8,
        1,
        GpuTextureFormat::Rgba32Float,
        &tex_px,
        "rt-t2c-cutout-stripe",
    );

    let objects = [RtObjectGeometry {
        vertex_buffer: &vertex_buffer,
        vertex_stride: std::mem::size_of::<PackedVertexUV>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: 2,
        transform: IDENTITY,
        // ao_spp/gi_spp are 0 below — the normal is never read.
        normal_offset: 0,
        uv_offset: std::mem::size_of::<[f32; 3]>() as u32,
        alpha_mask: true,
        alpha_cutoff: 0.5,
        base_color_texture: Some(&base_color_tex),
        mr_texture: None,
        normal_texture: None,
        emissive_texture: None,
        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
        emissive_uv_t: [0.0, 0.0],
        cast_shadows: true,
    }];

    let tracer = MetalShadowRayTracer::new(device);
    let accel = tracer.build_accel(device, &objects, &[]);

    let mut normal_sources_slot = None;
    let mut normal_sources_capacity = 0usize;
    let alpha_textures = ensure_normal_sources(
        &mut normal_sources_slot,
        &mut normal_sources_capacity,
        device,
        &objects,
    );
    let normal_sources_buffer =
        normal_sources_slot.expect("ensure_normal_sources must allocate");

    let depth_px = vec![0.3f32; WIDTH as usize];
    let depth_tex = upload_texture_f32(
        device,
        WIDTH,
        1,
        GpuTextureFormat::Depth32Float,
        &depth_px,
        "rt-t2c-depth",
    );

    let out_sv = device.create_texture(&GpuTextureDesc {
        width: WIDTH,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "rt-t2c-out_sv",
        mip_levels: 1,
    });
    // RS-A (caster cap 8): second shadow-visibility output — unread, this
    // proof asserts on out_sv only (one caster, slot 0).
    let out_sv2 = write_only_texture(device, GpuTextureFormat::Rgba16Float, "rt-t2c-out_sv2-stub");
    let out_irr = write_only_texture(device, GpuTextureFormat::Rgba16Float, "rt-t2c-out_irr-stub");
    let out_n = write_only_texture(device, GpuTextureFormat::Rgba16Float, "rt-t2c-out_n-stub");
    let out_refl = write_only_texture(device, GpuTextureFormat::Rgba16Float, "rt-t2c-out_refl-stub");
    let prefiltered_env = device.create_texture(&GpuTextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ,
        label: "rt-t2c-prefiltered-env-dummy",
        mip_levels: 1,
    });

    // One sun caster, toward +z.
    let casters = [RtCasterParams::new(
        [0.0, 0.0, 1.0],
        cone_half_angle,
        [0.0, 0.0, 0.0],
        0,
    )];
    let params = ShadowRayParams::new(
        &casters,
        1, // shadow_spp — the production value
        frame_index,
        [WIDTH, 1],
        [WIDTH, 1],
        0.0,
        0, // ao_spp — AO off, this proof is the shadow term only
        0, // gi_spp
        [0.0, 0.0, 0.0], // camera_pos — unused, ao_spp/gi_spp both 0
        IDENTITY,
        0, // refl_spp
        0.6,
        0.1,
        0.0, // RS-B: emissive_table_mean_power — no emissive in fixture
        0,   // RS-C: emissive_table_count — no emissive in fixture
        0.0, // RS-C: emissive_table_total_area — no emissive in fixture
    );
    let dummy_emissive = device.create_buffer_shared(1);
    let params_buffer =
        device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);
    let gi_materials_buffer = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);

    let mut encoder = device.create_encoder("rt-t2c-shadow-temporal-stability");
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
        &out_sv2,
        &out_irr,
        &out_n,
        &out_refl,
        &prefiltered_env,
        &dummy_emissive,
        &dummy_emissive,
        "trace_shadow_rays-t2c-proof",
    );
    encoder.commit_and_wait_completed();

    let row_bytes = WIDTH * 4 * 4;
    let readback_buf = device.create_buffer_shared(u64::from(row_bytes));
    let mut enc2 = device.create_encoder("rt-t2c-readback");
    enc2.copy_texture_to_buffer(&out_sv, &readback_buf, WIDTH, 1, row_bytes);
    enc2.commit_and_wait_completed();
    let ptr = readback_buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let floats: &[f32] = unsafe {
        slice::from_raw_parts(
            ptr.cast::<c_void>().cast::<f32>(),
            (WIDTH * 4) as usize,
        )
    };
    (0..WIDTH as usize).map(|i| floats[i * 4]).collect()
}

#[test]
fn hard_sun_shadow_mask_is_identical_across_frames() {
    let f0 = run_fixture(0.0, 0);
    let f7 = run_fixture(0.0, 7);

    let disagreements: Vec<(usize, f32, f32)> = f0
        .iter()
        .zip(f7.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| (i, *a, *b))
        .collect();
    assert!(
        disagreements.is_empty(),
        "a Hard (cone 0.0) sun shadow mask must be a deterministic function of the scene — \
         nothing moved between frame_index 0 and 7, so every one of the {} pixels must read back \
         EXACTLY the same. {} disagree (pixel, frame0, frame7): {:?}. Hard shadows carry no \
         jitter to reseed.",
        WIDTH,
        disagreements.len(),
        &disagreements[..disagreements.len().min(8)]
    );
}

#[test]
fn fixture_actually_exercises_the_cone_jitter() {
    // Vacuity guard. "Identical across frames" is worthless if the mask is
    // uniform, or if the cone never bends a ray into a different cutout
    // cell — either would let a reseeded jitter pass unnoticed.
    let hard = run_fixture(0.0, 0);
    let soft = run_fixture(SOFT_CONE_RADIANS, 0);

    let lit = hard.iter().filter(|v| **v == 1.0).count();
    let shadowed = hard.iter().filter(|v| **v == 0.0).count();
    assert_eq!(
        lit + shadowed,
        WIDTH as usize,
        "hard (cone 0.0) shadow_spp=1 must resolve every pixel to exactly 0.0 or 1.0 — got \
         {hard:?}"
    );
    assert!(
        lit > 0 && shadowed > 0,
        "the cutout stripe must put some pixels in shadow and leave others lit, or the mask is \
         uniform and proves nothing — {lit} lit, {shadowed} shadowed"
    );

    let bent = hard.iter().zip(soft.iter()).filter(|(a, b)| a != b).count();
    assert!(
        bent > 0,
        "the {SOFT_CONE_RADIANS} rad cone must bend at least one ray into a different cutout cell \
         than the unjittered ray hits, or this fixture never exercises the jitter the stability \
         test is guarding. hard={hard:?} soft={soft:?}"
    );
}
