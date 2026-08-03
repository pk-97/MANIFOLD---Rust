//! `docs/RAYTRACING_DESIGN.md` section 9.6 Textured roughness (R3) gate — value-level
//! proof for per-texel metallic-roughness in the reflection lobe
//! (`manifold_gpu::raytrace`'s `RtNormalSource::mr_tex_index` +
//! `ensure_normal_sources`' dedupe, consumed at the primary hit in
//! `trace_shadow_rays`). Follows the `rt_t2a_alpha_mask.rs` low-level
//! fixture pattern (`RtObjectGeometry` built by hand, `dispatch_shadow_rays`
//! called directly, `out_refl` read back) — no scene-graph JSON.
//!
//! Fixture: a flat floor quad at `y=0`, `x in [-1,1]`, `z in [0,1]`, vertex
//! normal `(0,1,0)`, UV `u=(x+1)/2, v=z` (so UV is an exact affine function
//! of position — barycentric interpolation reconstructs it exactly
//! regardless of triangulation). A 2x1 depth fixture (IDENTITY
//! `inv_view_proj`, `depth=0.3` both texels — same convention as
//! `rt_p1_shadow.rs`/`rt_t2a_alpha_mask.rs`) reconstructs texel 0 at world
//! `(-0.5, 0, 0.3)` (UV `u=0.25`) and texel 1 at world `(0.5, 0, 0.3)`
//! (UV `u=0.75`). `camera_pos = (0, 1.0, 0.3)` (directly "above" the row,
//! same z) — CPU-computed mirror math: at texel 0 the reflection direction
//! is `(-0.4472, 0.8944, 0)`, at texel 1 it is `(0.4472, 0.8944, 0)`; both
//! reach `y=2.0` at `x = -1.5` / `x = +1.5` respectively (`t = 2/0.8944`).
//! A wide emissive quad (`y=2.0`, `x in [-5,5]`, `z in [-2,2]`) catches
//! both, so ANY texel that casts a reflection ray hits it.
//!
//! MR texture: 2x1, texel 0 (`u=0.25`, exact texel-0 center) `G=0.0` (sharp,
//! `max(0.0,0.01)=0.01` after the kernel's floor — below
//! `refl_max_roughness(0.6)+refl_rough_band(0.1)=0.7`, so a reflection ray
//! IS cast), texel 1 (`u=0.75`, exact texel-1 center) `G=1.0` (rough, above
//! the cutoff — the kernel takes the no-ray-cast env branch entirely).
//! `prefiltered_env` is a 1x1 all-zero dummy, so a cast-and-hit ray reads
//! EXACTLY the emitter's `GiMaterial::emissive` (env/sun-bounce terms all
//! multiply through zero — no caster, black env) and the env-branch reads
//! EXACTLY `(0,0,0)`.
//!
//! Assert 1 (MR texture bound): texel 0 (sharp) >= a bright threshold;
//! texel 1 (rough) stays near-zero (env-band path, no ray cast).
//! Assert 2 (no MR texture — flat `GiMaterial::metallic_roughness.y`
//! fallback, both directions): factor `0.0` (exact mirror, NO GGX
//! perturbation since `roughness > 0.0` is false) makes BOTH texels hit the
//! emitter (their reflection directions land at x=-1.5/+1.5, both inside
//! the emitter's `[-5,5]` span) — asserted near-exact against the
//! CPU-computed emissive value; factor `1.0` makes BOTH texels read exactly
//! `(0,0,0)` (env-band path, no ray cast, exact — no perturbation math to
//! introduce noise).

use std::ffi::c_void;
use std::slice;

use manifold_gpu::raytrace::{
    ensure_normal_sources, GiMaterial, MetalShadowRayTracer, RtObjectGeometry, ShadowRayParams,
    ShadowRayTracer,
};
use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

use crate::harness;

/// Flat (non-indexed) vertex layout: 12-byte position + 12-byte normal +
/// 8-byte UV, no padding — `packed_float3`/`packed_float2` mandatory (P0
/// section 5.1 kernel lesson), stride 32.
#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertexNUV {
    pos: [f32; 3],
    normal: [f32; 3],
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

const EMITTER_EMISSIVE: [f32; 3] = [2.0, 2.0, 2.0];

/// Runs the shared floor+emitter fixture with `mr_texture` and the floor's
/// flat `roughness` factor (read only when `mr_texture` is `None`, or when
/// the texture is bound but this texel's roughness path is meant to fall
/// through — here always overridden by the texture when bound). Returns
/// `[refl_texel0_rgb, refl_texel1_rgb]` (`out_refl`'s rgb channels).
fn run_fixture(mr_texture: Option<&manifold_gpu::GpuTexture>, floor_roughness: f32) -> [[f32; 3]; 2] {
    let h = harness::shared();
    let device = &h.device;

    // ─── Floor: y=0, x in [-1,1], z in [0,1], normal (0,1,0), uv=((x+1)/2, z) ──
    let floor_verts = [
        PackedVertexNUV { pos: [-1.0, 0.0, 0.0], normal: [0.0, 1.0, 0.0], uv: [0.0, 0.0] },
        PackedVertexNUV { pos: [1.0, 0.0, 0.0], normal: [0.0, 1.0, 0.0], uv: [1.0, 0.0] },
        PackedVertexNUV { pos: [1.0, 0.0, 1.0], normal: [0.0, 1.0, 0.0], uv: [1.0, 1.0] },
        PackedVertexNUV { pos: [-1.0, 0.0, 0.0], normal: [0.0, 1.0, 0.0], uv: [0.0, 0.0] },
        PackedVertexNUV { pos: [1.0, 0.0, 1.0], normal: [0.0, 1.0, 0.0], uv: [1.0, 1.0] },
        PackedVertexNUV { pos: [-1.0, 0.0, 1.0], normal: [0.0, 1.0, 0.0], uv: [0.0, 1.0] },
    ];
    let floor_vertex_buffer = write_shared_buffer(device, &floor_verts);

    // ─── Emitter: wide quad at y=2.0, x in [-5,5], z in [-2,2] — catches
    // both texels' reflection directions (CPU math: x=-1.5 / x=+1.5) ──
    let emitter_verts = [
        PackedVertexNUV { pos: [-5.0, 2.0, -2.0], normal: [0.0, 0.0, 1.0], uv: [0.0, 0.0] },
        PackedVertexNUV { pos: [5.0, 2.0, -2.0], normal: [0.0, 0.0, 1.0], uv: [1.0, 0.0] },
        PackedVertexNUV { pos: [5.0, 2.0, 2.0], normal: [0.0, 0.0, 1.0], uv: [1.0, 1.0] },
        PackedVertexNUV { pos: [-5.0, 2.0, -2.0], normal: [0.0, 0.0, 1.0], uv: [0.0, 0.0] },
        PackedVertexNUV { pos: [5.0, 2.0, 2.0], normal: [0.0, 0.0, 1.0], uv: [1.0, 1.0] },
        PackedVertexNUV { pos: [-5.0, 2.0, 2.0], normal: [0.0, 0.0, 1.0], uv: [0.0, 1.0] },
    ];
    let emitter_vertex_buffer = write_shared_buffer(device, &emitter_verts);

    let vsize = std::mem::size_of::<PackedVertexNUV>() as u32;
    let objects = [
        RtObjectGeometry {
            vertex_buffer: &floor_vertex_buffer,
            vertex_stride: vsize,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 2,
            transform: IDENTITY,
            normal_offset: 12,
            uv_offset: 24,
            alpha_mask: false,
            alpha_cutoff: 0.5,
            base_color_texture: None,
            mr_texture,
            normal_texture: None,
                        emissive_texture: None,
                        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
                        emissive_uv_t: [0.0, 0.0],
            cast_shadows: true,
        },
        RtObjectGeometry {
            vertex_buffer: &emitter_vertex_buffer,
            vertex_stride: vsize,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 2,
            transform: IDENTITY,
            normal_offset: 12,
            uv_offset: 24,
            alpha_mask: false,
            alpha_cutoff: 0.5,
            base_color_texture: None,
            mr_texture: None,
            normal_texture: None,
                        emissive_texture: None,
                        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
                        emissive_uv_t: [0.0, 0.0],
            cast_shadows: true,
        },
    ];

    let tracer = MetalShadowRayTracer::new(device);
    let accel = tracer.build_accel(device, &objects, &[]);

    let mut normal_sources_slot = None;
    let mut normal_sources_capacity = 0usize;
    let material_textures =
        ensure_normal_sources(&mut normal_sources_slot, &mut normal_sources_capacity, device, &objects);
    let normal_sources_buffer = normal_sources_slot.expect("ensure_normal_sources must allocate");

    // ─── Depth fixture: 2x1, both texels valid (depth=0.3) — identical to
    // rt_p1_shadow.rs / rt_t2a_alpha_mask.rs's fixture ──
    let depth_px: [f32; 2] = [0.3, 0.3];
    let depth_tex = upload_texture_f32(device, 2, 1, GpuTextureFormat::Depth32Float, &depth_px, "rt-r3-depth");

    let out_sv = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-r3-out_sv-stub",
        mip_levels: 1,
    });
    let out_sv2 = device.create_texture(&GpuTextureDesc {
        width: 2, height: 1, depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-r3-out_sv2-stub",
        mip_levels: 1,
    });
    let out_irr = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-r3-out_irr-stub",
        mip_levels: 1,
    });
    let out_n = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-r3-out_n-stub",
        mip_levels: 1,
    });
    let out_refl = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "rt-r3-out_refl",
        mip_levels: 1,
    });
    // 1x1 all-zero dummy — miss/env-branch reads exactly (0,0,0), and the
    // hit-point's env/specular terms (`hit_diffuse_env`/`hit_specular_env`)
    // contribute exactly zero regardless of direction/roughness/normal, so
    // a cast-and-hit ray's traced value is EXACTLY the emitter's emissive.
    let prefiltered_env = upload_texture_f32(
        device,
        1,
        1,
        GpuTextureFormat::Rgba16Float,
        &[0.0f32, 0.0, 0.0, 0.0],
        "rt-r3-prefiltered-env-dummy",
    );

    let params = ShadowRayParams::new(
        &[],
        1,
        0,
        [2, 1],
        [2, 1],
        0.0,
        0, // ao_spp
        0, // gi_spp
        [0.0, 1.0, 0.3], // camera_pos — see module doc's mirror math
        IDENTITY,
        1,   // refl_spp
        0.6, // refl_max_roughness
        0.1, // refl_rough_band
        0.0, // RS-B: emissive_table_mean_power — no emissive in fixture
        0,   // RS-C: emissive_table_count — no emissive in fixture
        0.0, // RS-C: emissive_table_total_area — no emissive in fixture
    );
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);

    // gi_materials[0] = floor (albedo/emissive unused on the primary-hit
    // path; only .y (roughness) is read as the flat-factor fallback).
    // gi_materials[1] = emitter (only .emissive is read on the reflection
    // HIT path — env/specular terms multiply through the zero dummy).
    let gi_materials = [
        GiMaterial::new([0.5, 0.5, 0.5], [0.0, 0.0, 0.0], [0.0, floor_roughness, 0.0, 0.0]),
        GiMaterial::new([0.5, 0.5, 0.5], EMITTER_EMISSIVE, [0.0, 0.5, 0.0, 0.0]),
    ];
    let dummy_emissive = device.create_buffer_shared(1);
    let gi_materials_buffer = write_shared_buffer(device, &gi_materials);

    let mut encoder = device.create_encoder("rt-r3-textured-roughness-proof");
    tracer.dispatch_shadow_rays(
        &mut encoder,
        &accel,
        &params,
        &params_buffer,
        &gi_materials_buffer,
        &normal_sources_buffer,
        &material_textures,
        &depth_tex,
        &out_sv,
        &out_sv2,
        &out_irr,
        &out_n,
        &out_refl,
        &prefiltered_env,
        &dummy_emissive,
        &dummy_emissive,
        "trace_shadow_rays-r3-proof",
    );
    encoder.commit_and_wait_completed();

    let readback_buf = device.create_buffer_shared(2 * 4 * 4);
    let mut enc2 = device.create_encoder("rt-r3-readback");
    enc2.copy_texture_to_buffer(&out_refl, &readback_buf, 2, 1, 2 * 4 * 4);
    enc2.commit_and_wait_completed();
    let ptr = readback_buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let bytes: &[u8] = unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), 32) };
    let floats: &[f32] = unsafe { slice::from_raw_parts(bytes.as_ptr().cast::<f32>(), 8) };

    [[floats[0], floats[1], floats[2]], [floats[4], floats[5], floats[6]]]
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// MR texture bound: 2x1, texel 0 G=0.0 (sharp), texel 1 G=1.0 (rough).
/// `floor_roughness` factor is deliberately set to 0.5 (< the 0.7 cutoff) —
/// if the texture were silently ignored, BOTH texels would cast a ray and
/// read bright, failing the "rough stays dark" assertion below.
#[test]
fn mr_texture_replaces_flat_factor_per_texel() {
    let mr_tex_px: [f32; 8] = [
        0.0, 0.0, 0.0, 1.0, // texel 0: sharp (G=0.0)
        0.0, 1.0, 0.0, 1.0, // texel 1: rough (G=1.0)
    ];
    let mr_tex = upload_texture_f32(
        &harness::shared().device,
        2,
        1,
        GpuTextureFormat::Rgba32Float,
        &mr_tex_px,
        "rt-r3-mr-texture",
    );
    let [refl0, refl1] = run_fixture(Some(&mr_tex), 0.5);
    let (luma0, luma1) = (luma(refl0), luma(refl1));
    eprintln!("mr_texture_replaces_flat_factor_per_texel: sharp(texel0)={refl0:?} luma={luma0:.4} rough(texel1)={refl1:?} luma={luma1:.4}");

    const BRIGHT_THRESHOLD: f32 = 1.5; // emitter luma is exactly 2.0 (luma of gray [2,2,2])
    const DARK_CEILING: f32 = 0.05;
    assert!(
        luma0 >= BRIGHT_THRESHOLD,
        "sharp texel (G=0.0 -> roughness 0.01) must show the emitter — luma {luma0:.4} < {BRIGHT_THRESHOLD}"
    );
    assert!(
        luma1 <= DARK_CEILING,
        "rough texel (G=1.0 -> roughness 1.0) must stay dark (env-band path, no ray cast) — luma {luma1:.4} > {DARK_CEILING}"
    );
}

/// No MR texture bound: falls back to `GiMaterial::metallic_roughness.y`,
/// same value for BOTH texels (one floor object). Factor 0.0 is an EXACT
/// mirror (no GGX perturbation — `roughness > 0.0` is false), so both
/// texels' reflected radiance is byte-exact against the CPU-computed
/// emitter emissive; factor 1.0 is exactly the env-band's (0,0,0), same
/// exactness (no ray cast at all).
#[test]
fn no_mr_texture_falls_back_to_flat_factor_both_directions() {
    let [sharp0, sharp1] = run_fixture(None, 0.0);
    eprintln!("no_mr_texture (factor=0.0, exact mirror): texel0={sharp0:?} texel1={sharp1:?}");
    for (i, c) in [sharp0, sharp1].iter().enumerate() {
        for (ch, &v) in c.iter().enumerate() {
            assert!(
                (v - EMITTER_EMISSIVE[ch]).abs() < 1e-3,
                "factor=0.0 texel{i} channel{ch}: expected {} (exact emitter emissive), got {v}",
                EMITTER_EMISSIVE[ch]
            );
        }
    }

    let [rough0, rough1] = run_fixture(None, 1.0);
    eprintln!("no_mr_texture (factor=1.0, env-band): texel0={rough0:?} texel1={rough1:?}");
    for (i, c) in [rough0, rough1].iter().enumerate() {
        for (ch, &v) in c.iter().enumerate() {
            assert!(
                v.abs() < 1e-3,
                "factor=1.0 texel{i} channel{ch}: expected 0.0 (env-band, no ray cast), got {v}"
            );
        }
    }
}
