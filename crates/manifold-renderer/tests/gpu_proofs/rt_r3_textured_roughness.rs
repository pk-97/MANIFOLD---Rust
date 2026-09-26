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
//! MR texture: 2x1, texel 0 (`u=0.25`, exact texel-0 center) has `G=0.5`,
//! and texel 1 (`u=0.75`, exact texel-1 center) has `G=1.0`. The floor
//! roughness factor is `0.5`, so the product values are `0.25` and `0.5`;
//! both remain below `refl_max_roughness(0.6)+refl_rough_band(0.1)=0.7` and
//! therefore cast reflection rays. Replacement semantics would make texel 1
//! exactly `1.0` and take the no-ray-cast env branch.
//! `prefiltered_env` is a 1x1 all-zero dummy, so a cast-and-hit ray reads
//! EXACTLY the emitter's `GiMaterial::emissive` (env/sun-bounce terms all
//! multiply through zero — no caster, black env) and the env-branch reads
//! EXACTLY `(0,0,0)`.
//!
//! Assert 1 (MR texture bound): both texels stay bright, proving that the
//! texture roughness multiplies the non-neutral floor factor.
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
    assert!(matches!(format, GpuTextureFormat::Rgba32Float | GpuTextureFormat::Depth32Float),
        "f32 fixture uploads require a 32-bit channel format");
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

#[derive(Clone, Copy)]
struct FixtureConfig<'a> {
    mr_texture: Option<&'a manifold_gpu::GpuTexture>,
    floor_roughness: f32,
    floor_metallic: f32,
    floor_anisotropy: [f32; 2],
    floor_extra_material_textures: [Option<&'a manifold_gpu::GpuTexture>; 3],
    emitter_albedo: [f32; 3],
    emitter_emissive: [f32; 3],
    emitter_metallic: f32,
    emitter_roughness: f32,
    emitter_specular: [f32; 4],
    emitter_extra_material_textures: [Option<&'a manifold_gpu::GpuTexture>; 3],
    env_rgba: [f32; 4],
    emitter_bounds: [f32; 4], // x_min, x_max, z_min, z_max
}

impl<'a> FixtureConfig<'a> {
    fn base(mr_texture: Option<&'a manifold_gpu::GpuTexture>, floor_roughness: f32) -> Self {
        Self {
            mr_texture,
            floor_roughness,
            floor_metallic: 0.4,
            floor_anisotropy: [0.0, 0.0],
            floor_extra_material_textures: [None; 3],
            emitter_albedo: [0.5, 0.5, 0.5],
            emitter_emissive: EMITTER_EMISSIVE,
            emitter_metallic: 0.0,
            emitter_roughness: 0.5,
            emitter_specular: [0.04, 0.04, 0.04, 1.0],
            emitter_extra_material_textures: [None; 3],
            env_rgba: [0.0; 4],
            emitter_bounds: [-5.0, 5.0, -2.0, 2.0],
        }
    }
}

/// Runs the shared floor+emitter fixture with `mr_texture` and the floor's
/// flat `roughness` factor (multiplied by the map when present). Returns
/// `[refl_texel0_rgb, refl_texel1_rgb]` (`out_refl`'s rgb channels).
fn run_fixture_config(config: FixtureConfig<'_>, frame_index: u32) -> [[f32; 3]; 2] {
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
        PackedVertexNUV { pos: [config.emitter_bounds[0], 2.0, config.emitter_bounds[2]], normal: [0.0, -1.0, 0.0], uv: [0.0, 0.0] },
        PackedVertexNUV { pos: [config.emitter_bounds[1], 2.0, config.emitter_bounds[2]], normal: [0.0, -1.0, 0.0], uv: [1.0, 0.0] },
        PackedVertexNUV { pos: [config.emitter_bounds[1], 2.0, config.emitter_bounds[3]], normal: [0.0, -1.0, 0.0], uv: [1.0, 1.0] },
        PackedVertexNUV { pos: [config.emitter_bounds[0], 2.0, config.emitter_bounds[2]], normal: [0.0, -1.0, 0.0], uv: [0.0, 0.0] },
        PackedVertexNUV { pos: [config.emitter_bounds[1], 2.0, config.emitter_bounds[3]], normal: [0.0, -1.0, 0.0], uv: [1.0, 1.0] },
        PackedVertexNUV { pos: [config.emitter_bounds[0], 2.0, config.emitter_bounds[3]], normal: [0.0, -1.0, 0.0], uv: [0.0, 1.0] },
    ];
    let emitter_vertex_buffer = write_shared_buffer(device, &emitter_verts);

    let vsize = std::mem::size_of::<PackedVertexNUV>() as u32;
    let objects = [
        RtObjectGeometry { material_attributes: Default::default(),
            vertex_buffer: &floor_vertex_buffer,
            vertex_stride: vsize,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 2,
            transform: IDENTITY,
            normal_offset: 12,
            uv_offset: 24,
            alpha_mask: false,
            translucent: false,
            alpha_cutoff: 0.5,
            base_color_texture: None,
            mr_texture: config.mr_texture,
            normal_texture: None,
                        emissive_texture: None,
        extra_material_textures: config.floor_extra_material_textures,
                        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
                        emissive_uv_t: [0.0, 0.0],
            cast_shadows: true,
            instances_addr: 0,
            instances_buffer: None,
            instance_slots: 1,
            appearance_weights: None,
            appearance_gain: 1.0,
            base_color_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            mr_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            normal_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            normal_scale: 1.0,
            base_color_alpha: 1.0,
            tangent_offset: u32::MAX,
        },
        RtObjectGeometry { material_attributes: Default::default(),
            vertex_buffer: &emitter_vertex_buffer,
            vertex_stride: vsize,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 2,
            transform: IDENTITY,
            normal_offset: 12,
            uv_offset: 24,
            alpha_mask: false,
            translucent: false,
            alpha_cutoff: 0.5,
            base_color_texture: None,
            mr_texture: None,
            normal_texture: None,
                        emissive_texture: None,
        extra_material_textures: config.emitter_extra_material_textures,
                        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
                        emissive_uv_t: [0.0, 0.0],
            cast_shadows: true,
            instances_addr: 0,
            instances_buffer: None,
            instance_slots: 1,
            appearance_weights: None,
            appearance_gain: 1.0,
            base_color_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            mr_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            normal_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            normal_scale: 1.0,
            base_color_alpha: 1.0,
            tangent_offset: u32::MAX,
        },
    ];

    let tracer = MetalShadowRayTracer::new(device);
    // P3 seam: plan/prepare allocate, encode rides the dispatch encoder
    // below (built before the trace dispatch on the same command buffer).
    let plan = tracer.plan_accel(device, None, &objects).expect("plan accel");
    let mut accel_slot = None;
    tracer.prepare_accel(device, &mut accel_slot, plan).expect("prepare accel");
    let mut accel = accel_slot.unwrap();

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
        GpuTextureFormat::Rgba32Float,
        &config.env_rgba,
        "rt-r3-prefiltered-env-dummy",
    );

    let params = ShadowRayParams::new(
        &[],
        frame_index,
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
        manifold_gpu::raytrace::SVT_SLOT_NONE,
    );
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);

    // gi_materials[0] = floor (albedo/emissive unused on the primary-hit
    // path; only .y (roughness) is read as the flat-factor fallback).
    // gi_materials[1] = emitter (only .emissive is read on the reflection
    // HIT path — env/specular terms multiply through the zero dummy).
    let gi_materials = [
        GiMaterial::new([0.5, 0.5, 0.5], [0.0, 0.0, 0.0], [config.floor_metallic, config.floor_roughness, config.floor_anisotropy[0], config.floor_anisotropy[1]], [0.0, 0.0, 0.0, 0.0]),
        GiMaterial::new(config.emitter_albedo, config.emitter_emissive, [config.emitter_metallic, config.emitter_roughness, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]).with_surface(2.0, config.emitter_specular),
    ];
    let dummy_emissive = harness::dummy_emissive_buffer(device);
    let gi_materials_buffer = write_shared_buffer(device, &gi_materials);
    // P4a: the GPU emissive preparation indexes one material row per
    // object. The fixture's real `gi_materials` above feeds the
    // reflection-hit shading buffer only — the accel update historically
    // got `&[]` (no emissive table), so zeroed rows keep that behavior.
    let emissive_prep_materials = vec![
        GiMaterial::new([0.0; 3], [0.0; 3], [0.0; 4], [0.0; 4]);
        objects.len()
    ];

    let mut encoder = device.create_encoder("rt-r3-textured-roughness-proof");
    let changes = vec![manifold_gpu::raytrace::RtGeometryChange::Rebuild; objects.len()];
    tracer
        .encode_accel_update(device, &mut encoder, &mut accel, &objects, &changes, &emissive_prep_materials, true, true)
        .expect("encode accel update");
    let out_svt = device.create_texture(&GpuTextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "tl-c-out_svt",
        mip_levels: 1,
    });
    tracer.dispatch_shadow_rays(
        &mut encoder,
        device,
        &accel,
        accel
            .emissive_table
            .as_ref()
            .map(|t| &t.stats)
            .unwrap_or_else(|| tracer.zero_emissive_stats()),
        &params,
        &params_buffer,
        &gi_materials_buffer,
        &normal_sources_buffer,
        &objects,
        &material_textures,
        &depth_tex,
        &out_sv,
        &out_sv2,
        &out_svt,
        &out_irr,
        &out_n,
        &out_refl,
        &prefiltered_env,
        &dummy_emissive,
        &dummy_emissive,
        false,
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

fn run_fixture(mr_texture: Option<&manifold_gpu::GpuTexture>, floor_roughness: f32) -> [[f32; 3]; 2] {
    run_fixture_config(FixtureConfig::base(mr_texture, floor_roughness), 1)
}

fn mean_frames(config: FixtureConfig<'_>, frame_count: u32) -> [[f32; 3]; 2] {
    let mut sum = [[0.0; 3]; 2];
    for frame in 0..frame_count {
        let sample = run_fixture_config(config, frame);
        for pixel in 0..2 {
            for channel in 0..3 {
                sum[pixel][channel] += sample[pixel][channel];
            }
        }
    }
    for pixel in &mut sum {
        for channel in pixel {
            *channel /= frame_count as f32;
        }
    }
    sum
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// MR texture bound: both channels multiply non-neutral metallic/roughness
/// factors. The roughness products remain below the 0.7 ray-cast cutoff.
#[test]
fn mr_texture_multiplies_flat_factors_per_texel() {
    let mr_tex_px: [f32; 8] = [
        0.0, 0.5, 0.75, 1.0, // texel 0: products (metallic=.3, roughness=.25)
        0.0, 1.0, 0.25, 1.0, // texel 1: products (metallic=.1, roughness=.5)
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
    eprintln!("mr_texture_multiplies_flat_factors_per_texel: texel0={refl0:?} luma={luma0:.4} texel1={refl1:?} luma={luma1:.4}");

    const BRIGHT_THRESHOLD: f32 = 1.5; // emitter luma is exactly 2.0 (luma of gray [2,2,2])
    assert!(
        luma0 >= BRIGHT_THRESHOLD,
        "texel 0 (factor*map roughness=0.25) must show the emitter — luma {luma0:.4} < {BRIGHT_THRESHOLD}"
    );
    assert!(
        luma1 >= BRIGHT_THRESHOLD,
        "texel 1 (factor*map roughness=0.5) must show the emitter — luma {luma1:.4} < {BRIGHT_THRESHOLD}"
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

const EXTENSION_MEAN_FRAMES: u32 = 16;

#[test]
fn anisotropy_rotation_changes_elongated_emitter_coverage() {
    let mut x_strip = FixtureConfig::base(None, 0.55);
    x_strip.floor_anisotropy = [0.95, 0.0];
    x_strip.emitter_bounds = [-5.0, 5.0, 0.22, 0.38];
    let mut z_strip = x_strip;
    z_strip.floor_anisotropy[1] = std::f32::consts::FRAC_PI_2;

    let x_mean = mean_frames(x_strip, EXTENSION_MEAN_FRAMES);
    let z_mean = mean_frames(z_strip, EXTENSION_MEAN_FRAMES);
    let x_coverage = x_mean.iter().filter(|pixel| luma(**pixel) > 0.25).count();
    let z_coverage = z_mean.iter().filter(|pixel| luma(**pixel) > 0.25).count();
    eprintln!("anisotropy coverage: x={x_mean:?} ({x_coverage}) z={z_mean:?} ({z_coverage})");
    assert_ne!(
        x_coverage, z_coverage,
        "rotating anisotropy across an elongated emitter must change the measured reflection coverage"
    );
}

#[test]
fn zero_blue_anisotropy_map_equals_strength_zero_control() {
    let aniso_map = upload_texture_f32(
        &harness::shared().device,
        1,
        1,
        GpuTextureFormat::Rgba32Float,
        &[1.0, 0.0, 0.0, 1.0],
        "rt-r3-aniso-zero-blue",
    );
    let mut mapped = FixtureConfig::base(None, 0.45);
    mapped.floor_anisotropy = [1.0, 0.7];
    mapped.floor_extra_material_textures[0] = Some(&aniso_map);
    let control = FixtureConfig::base(None, 0.45);
    let mapped_mean = mean_frames(mapped, EXTENSION_MEAN_FRAMES);
    let control_mean = mean_frames(control, EXTENSION_MEAN_FRAMES);
    for pixel in 0..2 {
        for channel in 0..3 {
            assert!(
                (mapped_mean[pixel][channel] - control_mean[pixel][channel]).abs() < 1e-4,
                "zero-blue anisotropy map changed pixel {pixel} channel {channel}: mapped={} control={}",
                mapped_mean[pixel][channel],
                control_mean[pixel][channel]
            );
        }
    }
}

#[test]
fn anisotropy_rg_direction_matches_scalar_rotation() {
    let direction_map = upload_texture_f32(
        &harness::shared().device,
        1,
        1,
        GpuTextureFormat::Rgba32Float,
        &[0.5, 1.0, 1.0, 1.0],
        "rt-r3-aniso-direction",
    );
    let mut mapped = FixtureConfig::base(None, 0.5);
    mapped.floor_anisotropy = [1.0, 0.0];
    mapped.floor_extra_material_textures[0] = Some(&direction_map);
    let mut scalar = FixtureConfig::base(None, 0.5);
    scalar.floor_anisotropy = [1.0, std::f32::consts::FRAC_PI_2];
    let mapped_mean = mean_frames(mapped, EXTENSION_MEAN_FRAMES);
    let scalar_mean = mean_frames(scalar, EXTENSION_MEAN_FRAMES);
    for pixel in 0..2 {
        for channel in 0..3 {
            assert!(
                (mapped_mean[pixel][channel] - scalar_mean[pixel][channel]).abs() < 1e-3,
                "RG direction rotation disagrees with scalar rotation at pixel {pixel} channel {channel}: mapped={} scalar={}",
                mapped_mean[pixel][channel],
                scalar_mean[pixel][channel]
            );
        }
    }
}

#[test]
fn zero_specular_weight_removes_dielectric_but_not_metallic_reflection() {
    let weight_zero = upload_texture_f32(
        &harness::shared().device,
        1,
        1,
        GpuTextureFormat::Rgba32Float,
        &[1.0, 1.0, 1.0, 0.0],
        "rt-r3-specular-weight-zero",
    );
    let mut dielectric = FixtureConfig::base(None, 0.0);
    dielectric.emitter_albedo = [0.0; 3];
    dielectric.emitter_emissive = [0.0; 3];
    dielectric.env_rgba = [1.0; 4];
    let mut dielectric_zero = dielectric;
    dielectric_zero.emitter_extra_material_textures[1] = Some(&weight_zero);
    let dielectric_default = mean_frames(dielectric, EXTENSION_MEAN_FRAMES);
    let dielectric_zero_mean = mean_frames(dielectric_zero, EXTENSION_MEAN_FRAMES);
    assert!(
        luma(dielectric_default[0]) > 0.01,
        "unmasked dielectric should retain a visible white-environment reflection: {:?}",
        dielectric_default
    );
    assert!(
        luma(dielectric_zero_mean[0]) < 1e-3,
        "weight=0 must remove dielectric reflection: {:?}",
        dielectric_zero_mean
    );

    let mut metallic = dielectric;
    metallic.emitter_albedo = [1.0; 3];
    metallic.emitter_metallic = 1.0;
    let mut metallic_zero = metallic;
    metallic_zero.emitter_extra_material_textures[1] = Some(&weight_zero);
    let metallic_default = mean_frames(metallic, EXTENSION_MEAN_FRAMES);
    let metallic_zero_mean = mean_frames(metallic_zero, EXTENSION_MEAN_FRAMES);
    for channel in 0..3 {
        assert!(
            (metallic_default[0][channel] - metallic_zero_mean[0][channel]).abs() < 1e-3,
            "weight=0 changed metallic reflection channel {channel}: default={} zero={}",
            metallic_default[0][channel],
            metallic_zero_mean[0][channel]
        );
    }
}

#[test]
fn colored_specular_map_tints_f0_against_white_environment() {
    let color_map = upload_texture_f32(
        &harness::shared().device,
        1,
        1,
        GpuTextureFormat::Rgba32Float,
        &[1.0, 0.25, 0.1, 1.0],
        "rt-r3-specular-color",
    );
    let mut tinted = FixtureConfig::base(None, 0.0);
    tinted.emitter_albedo = [0.0; 3];
    tinted.emitter_emissive = [0.0; 3];
    tinted.env_rgba = [1.0; 4];
    tinted.emitter_extra_material_textures[2] = Some(&color_map);
    let mean = mean_frames(tinted, EXTENSION_MEAN_FRAMES);
    let rgb = mean[0];
    assert!(rgb[0] > 1e-3, "white environment should produce a nonzero F0 response: {rgb:?}");
    assert!((rgb[1] / rgb[0] - 0.25).abs() < 0.02, "green F0 tint ratio is wrong: {rgb:?}");
    assert!((rgb[2] / rgb[0] - 0.1).abs() < 0.02, "blue F0 tint ratio is wrong: {rgb:?}");
}
