//! `docs/RAYTRACING_DESIGN.md` section 16 (TL-B) — value-level proofs for
//! `walk_with_transmission` (`manifold_gpu::raytrace::MetalShadowRayTracer`'s
//! translucent shadow-ray walk — `tint *= factor * albedo_at_hit`, declined
//! candidate continuation, single/stack/cutout legs).
//!
//! Fixture: 2x1 depth buffer at depth 0.3 with `inv_view_proj = IDENTITY`:
//! texel 0 (pixel (0,0)) reconstructs `world = (-0.5, 0, 0.3)`, texel 1
//! (pixel (1,0)) reconstructs `world = (0.5, 0, 0.3)`. Sun caster at
//! `(0,0,1)`, cone 0, `shadow_spp=1` — both rays travel +z and hit the
//! quad(s) at their respective z-depths.
//!
//! Test 1 — single_translucent_occluder_attenuates_half:
//!   One quad at z=1, translucent=true, factor=0.5, white albedo, no texture.
//!   Both texels vis = 0.5 (a tl=0.5 surface halves the illumination).
//!
//! Test 2 — factor_zero_control_stays_fully_shadowed:
//!   SAME geometry, factor 0.0, translucent=false → opaque fast path.
//!   Both texels vis = 0.0.
//!
//! Test 3 — stacked_petals_compound_to_quarter:
//!   Two quads at z=1 and z=2, both translucent factor 0.5, white albedo.
//!   First accepted hit: tint = 0.5 (declined — no commit, walk continues).
//!   Second accepted hit: tint = 0.5*0.5 = 0.25.
//!   Both texels vis = 0.25. THIS IS THE MECHANISM PROOF — declined candidate
//!   continuation through multiple translucent layers.
//!
//! Test 4 — cutout_texel_passes_unattenuated_accepted_texel_attenuates:
//!   2x1 checkerboard texture (texel0 rgba=(0,0,0,0), texel1=(1,1,1,1)),
//!   alpha_mask=true, alpha_cutoff=0.5, translucent=true, factor=0.5.
//!   Texel 0: below-cutoff passes UNATTENUATED — vis=1.0.
//!   Texel 1: accepted, albedo from texture = (1,1,1), tint = 0.5 — vis=0.5.
//!
//! Test 5 — albedo_tint_folds_to_luma:
//!   One quad, factor 0.6, albedo (1.0, 0.1, 0.1), no texture.
//!   tint = (0.6, 0.06, 0.06). Expected luma = 0.2126*0.6 + 0.7152*0.06 + 0.0722*0.06 = 0.174804.

use std::ffi::c_void;
use std::slice;

use manifold_gpu::raytrace::{
    ensure_normal_sources, GiMaterial, MetalShadowRayTracer, RtCasterParams, RtObjectGeometry,
    ShadowRayParams, ShadowRayTracer,
};
use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

use crate::harness;

/// Flat (non-indexed) vertex layout: 12-byte position + 8-byte UV, no
/// padding — `packed_float3`/`packed_float2` mandatory (P0 section 5.1 kernel
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

/// Quad at z, spanning x,y in [-1,1], u=(x+1)/2 — same geometry as the
/// alpha-mask template.
fn quad_verts_z(z: f32) -> [PackedVertexUV; 6] {
    [
        PackedVertexUV { pos: [-1.0, -1.0, z], uv: [0.0, 0.0] },
        PackedVertexUV { pos: [1.0, -1.0, z], uv: [1.0, 0.0] },
        PackedVertexUV { pos: [1.0, 1.0, z], uv: [1.0, 1.0] },
        PackedVertexUV { pos: [-1.0, -1.0, z], uv: [0.0, 0.0] },
        PackedVertexUV { pos: [1.0, 1.0, z], uv: [1.0, 1.0] },
        PackedVertexUV { pos: [-1.0, 1.0, z], uv: [0.0, 1.0] },
    ]
}

/// Core fixture: set up a scene with `objects`, `gi_materials`, optional
/// `base_color_tex`, and return `[vis_texel0, vis_texel1]` from `out_sv` r.
fn run_tl_fixture(
    objects: &[RtObjectGeometry],
    gi_materials: &[GiMaterial],
    base_color_tex: Option<&manifold_gpu::GpuTexture>,
) -> [f32; 2] {
    let h = harness::shared();
    let device = &h.device;

    let tracer = MetalShadowRayTracer::new(device);
    let accel = tracer.build_accel(device, objects, &[], None);

    // Upload gi_materials — real entries, unlike the template's zeroed dummy.
    // Positioned AFTER the accel build like the template to preserve Metal
    // command-buffer ordering against the async accel-build commit.
    let gi_materials_buffer = write_shared_buffer(device, gi_materials);

    let mut normal_sources_slot = None;
    let mut normal_sources_capacity = 0usize;
    let _alpha_textures = ensure_normal_sources(
        &mut normal_sources_slot,
        &mut normal_sources_capacity,
        device,
        objects,
    );
    let normal_sources_buffer = normal_sources_slot.expect("ensure_normal_sources must allocate");

    // Depth fixture: 2x1, both texels at depth 0.3.
    let depth_px: [f32; 2] = [0.3, 0.3];
    let depth_tex = upload_texture_f32(device, 2, 1, GpuTextureFormat::Depth32Float, &depth_px, "rt-tlb-depth");

    let out_sv = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "rt-tlb-out_sv",
        mip_levels: 1,
    });
    let out_irr = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-tlb-out_irr-stub",
        mip_levels: 1,
    });
    let out_n = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "rt-tlb-out_n-stub",
        mip_levels: 1,
    });
    let out_refl = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ,
        label: "rt-tlb-out_refl-stub",
        mip_levels: 1,
    });
    let prefiltered_env = device.create_texture(&GpuTextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ,
        label: "rt-tlb-prefiltered-env-dummy",
        mip_levels: 1,
    });

    // Build the alpha_textures slice: collect per-object base-color textures.
    let tex_list: Vec<&manifold_gpu::GpuTexture> = objects
        .iter()
        .filter_map(|o| o.base_color_texture)
        .collect();
    // If there are no textures in objects but the caller provided one, include
    // it so the allocation is alive during dispatch.
    let owned_dummy;
    let final_alpha_textures: Vec<&manifold_gpu::GpuTexture> = if !tex_list.is_empty() {
        tex_list
    } else {
        // Use the caller's base_color_tex if provided, otherwise a 1x1 dummy.
        if let Some(tex) = base_color_tex {
            vec![tex]
        } else {
            owned_dummy = upload_texture_f32(device, 1, 1, GpuTextureFormat::Rgba32Float, &[1.0, 1.0, 1.0, 1.0], "rt-tlb-tex-dummy");
            vec![&owned_dummy]
        }
    };

    // Single sun caster, same as the template.
    let casters = [RtCasterParams::new([0.0, 0.0, 1.0], 0.0, [0.0, 0.0, 0.0], 0)];
    let params = ShadowRayParams::new(
        &casters,
        1,       // shadow_spp
        0,       // frame_index
        [2, 1],  // trace_size
        [2, 1],  // gbuffer_size
        0.0,     // ao_radius
        0,       // ao_spp
        0,       // gi_spp
        [0.0, 0.0, 0.0], // camera_pos
        IDENTITY,
        0,       // refl_spp
        0.6,     // refl_max_roughness
        0.1,     // refl_rough_band
        0.0,     // emissive_table_mean_power
        0,       // emissive_table_count
        0.0,     // emissive_table_total_area
        manifold_gpu::raytrace::SVT_SLOT_NONE,
    );
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);
    let dummy_emissive = device.create_buffer_shared(1);

    let mut encoder = device.create_encoder("rt-tlb-transmission-proof");
    let out_sv2_dummy = device.create_texture(&GpuTextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "rt-tlb-out_sv2-dummy",
        mip_levels: 1,
    });
    let out_svt = device.create_texture(&GpuTextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "rt-tlb-out_svt",
        mip_levels: 1,
    });
    tracer.dispatch_shadow_rays(
        &mut encoder,
        &accel,
        &params,
        &params_buffer,
        &gi_materials_buffer,
        &normal_sources_buffer,
        &final_alpha_textures,
        &depth_tex,
        &out_sv,
        &out_sv2_dummy,
        &out_svt,
        &out_irr,
        &out_n,
        &out_refl,
        &prefiltered_env,
        &dummy_emissive,
        &dummy_emissive,
        true,
        "trace_shadow_rays-tlb-proof",
    );
    encoder.commit_and_wait_completed();

    let readback_buf = device.create_buffer_shared(2 * 4 * 4);
    let mut enc2 = device.create_encoder("rt-tlb-readback");
    enc2.copy_texture_to_buffer(&out_sv, &readback_buf, 2, 1, 2 * 4 * 4);
    enc2.commit_and_wait_completed();
    let ptr = readback_buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let bytes: &[u8] = unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), 32) };
    let floats: &[f32] = unsafe { slice::from_raw_parts(bytes.as_ptr().cast::<f32>(), 8) };

    [floats[0], floats[4]]
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[test]
fn single_translucent_occluder_attenuates_half() {
    let h = harness::shared();
    let device = &h.device;

    let verts = quad_verts_z(1.0);
    let vertex_buffer = write_shared_buffer(device, &verts);

    let objects = [RtObjectGeometry {
        vertex_buffer: &vertex_buffer,
        vertex_stride: std::mem::size_of::<PackedVertexUV>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: 2,
        transform: IDENTITY,
        normal_offset: 0,
        uv_offset: std::mem::size_of::<[f32; 3]>() as u32,
        alpha_mask: false,
        translucent: true,
        alpha_cutoff: 0.5,
        base_color_texture: None,
        mr_texture: None,
        normal_texture: None,
        emissive_texture: None,
        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
        emissive_uv_t: [0.0, 0.0],
        cast_shadows: true,
    }];
    let gi_materials = [GiMaterial::new(
        [1.0, 1.0, 1.0],
        [0.0; 3],
        [0.0; 4],
        [0.5, 0.0, 0.0, 0.0],
    )];

    let [v0, v1] = run_tl_fixture(&objects, &gi_materials, None);
    assert!(
        (v0 - 0.5).abs() < 1e-6,
        "texel 0: single translucent occluder factor 0.5 — expected vis 0.5, got {v0}"
    );
    assert!(
        (v1 - 0.5).abs() < 1e-6,
        "texel 1: single translucent occluder factor 0.5 — expected vis 0.5, got {v1}"
    );
}

#[test]
fn factor_zero_control_stays_fully_shadowed() {
    let h = harness::shared();
    let device = &h.device;

    let verts = quad_verts_z(1.0);
    let vertex_buffer = write_shared_buffer(device, &verts);

    let objects = [RtObjectGeometry {
        vertex_buffer: &vertex_buffer,
        vertex_stride: std::mem::size_of::<PackedVertexUV>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: 2,
        transform: IDENTITY,
        normal_offset: 0,
        uv_offset: std::mem::size_of::<[f32; 3]>() as u32,
        alpha_mask: false,
        translucent: false,
        alpha_cutoff: 0.5,
        base_color_texture: None,
        mr_texture: None,
        normal_texture: None,
        emissive_texture: None,
        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
        emissive_uv_t: [0.0, 0.0],
        cast_shadows: true,
    }];
    let gi_materials = [GiMaterial::new(
        [1.0, 1.0, 1.0],
        [0.0; 3],
        [0.0; 4],
        [0.0, 0.0, 0.0, 0.0],
    )];

    let [v0, v1] = run_tl_fixture(&objects, &gi_materials, None);
    assert_eq!(
        v0, 0.0,
        "texel 0: factor-zero control (translucent=false, tl=0) must be fully shadowed — got {v0}"
    );
    assert_eq!(
        v1, 0.0,
        "texel 1: factor-zero control (translucent=false, tl=0) must be fully shadowed — got {v1}"
    );
}

#[test]
fn stacked_petals_compound_to_quarter() {
    let h = harness::shared();
    let device = &h.device;

    let verts1 = quad_verts_z(1.0);
    let verts2 = quad_verts_z(2.0);
    let vertex_buffer1 = write_shared_buffer(device, &verts1);
    let vertex_buffer2 = write_shared_buffer(device, &verts2);

    let objects = [
        RtObjectGeometry {
            vertex_buffer: &vertex_buffer1,
            vertex_stride: std::mem::size_of::<PackedVertexUV>() as u32,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 2,
            transform: IDENTITY,
            normal_offset: 0,
            uv_offset: std::mem::size_of::<[f32; 3]>() as u32,
            alpha_mask: false,
            translucent: true,
            alpha_cutoff: 0.5,
            base_color_texture: None,
            mr_texture: None,
            normal_texture: None,
            emissive_texture: None,
            emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
            emissive_uv_t: [0.0, 0.0],
            cast_shadows: true,
        },
        RtObjectGeometry {
            vertex_buffer: &vertex_buffer2,
            vertex_stride: std::mem::size_of::<PackedVertexUV>() as u32,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 2,
            transform: IDENTITY,
            normal_offset: 0,
            uv_offset: std::mem::size_of::<[f32; 3]>() as u32,
            alpha_mask: false,
            translucent: true,
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
    let gi_materials = [
        GiMaterial::new([1.0, 1.0, 1.0], [0.0; 3], [0.0; 4], [0.5, 0.0, 0.0, 0.0]),
        GiMaterial::new([1.0, 1.0, 1.0], [0.0; 3], [0.0; 4], [0.5, 0.0, 0.0, 0.0]),
    ];

    let [v0, v1] = run_tl_fixture(&objects, &gi_materials, None);
    assert!(
        (v0 - 0.25).abs() < 1e-6,
        "texel 0: stacked petals (0.5*0.5) — expected vis 0.25, got {v0}"
    );
    assert!(
        (v1 - 0.25).abs() < 1e-6,
        "texel 1: stacked petals (0.5*0.5) — expected vis 0.25, got {v1}"
    );
}

#[test]
fn cutout_texel_passes_unattenuated_accepted_texel_attenuates() {
    let h = harness::shared();
    let device = &h.device;

    let verts = quad_verts_z(1.0);
    let vertex_buffer = write_shared_buffer(device, &verts);

    // 2x1 checkerboard: texel 0 alpha=0 (below cutoff), texel 1 alpha=1 (opaque).
    let tex_px: [f32; 8] = [
        0.0, 0.0, 0.0, 0.0, // texel 0: transparent
        1.0, 1.0, 1.0, 1.0, // texel 1: opaque white
    ];
    let base_color_tex = upload_texture_f32(device, 2, 1, GpuTextureFormat::Rgba32Float, &tex_px, "rt-tlb-cutout-tex");

    let objects = [RtObjectGeometry {
        vertex_buffer: &vertex_buffer,
        vertex_stride: std::mem::size_of::<PackedVertexUV>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: 2,
        transform: IDENTITY,
        normal_offset: 0,
        uv_offset: std::mem::size_of::<[f32; 3]>() as u32,
        alpha_mask: true,
        translucent: true,
        alpha_cutoff: 0.5,
        base_color_texture: Some(&base_color_tex),
        mr_texture: None,
        normal_texture: None,
        emissive_texture: None,
        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
        emissive_uv_t: [0.0, 0.0],
        cast_shadows: true,
    }];
    // Albedo ignored when texture supplies albedo at hit — still passed through
    // so the flat-albedo fallback (no-texture branch in walk_with_transmission) is
    // exercised in other tests. Here texture supplies (0,0,0) for texel 0 and
    // (1,1,1) for texel 1.
    let gi_materials = [GiMaterial::new(
        [1.0, 1.0, 1.0],
        [0.0; 3],
        [0.0; 4],
        [0.5, 0.0, 0.0, 0.0],
    )];

    let [v0, v1] = run_tl_fixture(&objects, &gi_materials, Some(&base_color_tex));
    assert_eq!(
        v0, 1.0,
        "texel 0 (checkerboard alpha=0.0, below cutoff): must pass UNATTENUATED — vis=1.0, got {v0}"
    );
    assert!(
        (v1 - 0.5).abs() < 1e-6,
        "texel 1 (checkerboard alpha=1.0, accepted): translucent factor 0.5, albedo from texture (1,1,1) — expected vis 0.5, got {v1}"
    );
}

#[test]
fn albedo_tint_folds_to_luma() {
    let h = harness::shared();
    let device = &h.device;

    let verts = quad_verts_z(1.0);
    let vertex_buffer = write_shared_buffer(device, &verts);

    let objects = [RtObjectGeometry {
        vertex_buffer: &vertex_buffer,
        vertex_stride: std::mem::size_of::<PackedVertexUV>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: 2,
        transform: IDENTITY,
        normal_offset: 0,
        uv_offset: std::mem::size_of::<[f32; 3]>() as u32,
        alpha_mask: false,
        translucent: true,
        alpha_cutoff: 0.5,
        base_color_texture: None,
        mr_texture: None,
        normal_texture: None,
        emissive_texture: None,
        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
        emissive_uv_t: [0.0, 0.0],
        cast_shadows: true,
    }];
    // factor 0.6, albedo (1.0, 0.1, 0.1) → tint = (0.6, 0.06, 0.06)
    // luma = 0.2126*0.6 + 0.7152*0.06 + 0.0722*0.06 = 0.174804
    let gi_materials = [GiMaterial::new(
        [1.0, 0.1, 0.1],
        [0.0; 3],
        [0.0; 4],
        [0.6, 0.0, 0.0, 0.0],
    )];

    let [v0, v1] = run_tl_fixture(&objects, &gi_materials, None);
    let expected: f32 = 0.2126 * 0.6 + 0.7152 * 0.06 + 0.0722 * 0.06;
    assert!(
        (v0 - expected).abs() < 1e-5,
        "texel 0: albedo-tinted translucent — expected luma {expected}, got {v0}"
    );
    assert!(
        (v1 - expected).abs() < 1e-5,
        "texel 1: albedo-tinted translucent — expected luma {expected}, got {v1}"
    );
}
