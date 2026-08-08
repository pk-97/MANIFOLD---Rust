//! `docs/RAYTRACING_DESIGN.md` section 16 (TL-C) — value-level proofs for
//! `out_svt` (rgb sun-transmission tint), the TL-B consistency leg, and the
//! `rt_svt_slot` CPU discriminator.
//!
//! Fixture: 2x1 depth buffer at depth 0.3, `inv_view_proj = IDENTITY`:
//! texel 0 reconstructs `world = (-0.5, 0, 0.3)`, texel 1 reconstructs
//! `world = (0.5, 0, 0.3)`. Sun caster at `(0,0,1)`, shadow_spp=1.
//!
//! A single quad at z=1 between the light and the surface occludes both
//! texels. With translucency > 0, out_sv carries luma(tint) and out_svt
//! carries the full rgb tint for the designated sun slot.
//!
//! Test 1 — red_petal_tints_transmitted_pool:
//!   Sun caster (slot 0 designated via svt_slot=0), one occluder albedo
//!   (1.0, 0.1, 0.1), factor 0.6. out_svt texels behind occluder ≈
//!   (0.6, 0.06, 0.06). r/g ≈ 10, r/b ≈ 10. out_sv luma channel equals
//!   luma of the rgb tint (TL-B consistency). Unoccluded texels read
//!   (1,1,1) in out_svt.
//!
//! Test 2 — point_caster_control_svt_stays_white:
//!   Point caster → svt_slot = SVT_SLOT_NONE → out_svt reads (1,1,1)
//!   everywhere. out_sv carries the attenuated luma (luma discipline
//!   unchanged — the point caster still occludes).
//!
//! Test 3 — factor_zero_occluder_svt_reads_zero:
//!   Sun caster, factor 0 occluder → fully opaque → out_svt reads (0,0,0)
//!   behind occluder (fully shadowed sun). Control: svt_slot=0 should NOT
//!   force white through an opaque occluder.
//!
//! Test 4 — unoccluded_texels_read_white_in_svt:
//!   Occluder behind the depth surface → out_svt reads (1,1,1), out_sv
//!   fully lit.
//!
//! The `rt_svt_slot` CPU discriminator (sun-first → Some(0); point-only →
//! None; sun at index >= MAX_RT_CASTERS → None) is tested against the
//! production fn in render_scene.rs's cfg(test) mod — a duplicated copy
//! here would drift silently.

use std::ffi::c_void;
use std::slice;

use manifold_gpu::raytrace::{
    ensure_normal_sources, GiMaterial, MetalShadowRayTracer, RtCasterParams, RtObjectGeometry,
    ShadowRayParams, ShadowRayTracer, SVT_SLOT_NONE,
};
use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

use crate::harness;

// Reuse the TL-B geometry helpers.

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

/// Run a TL-C fixture: dispatch with the given casters + gi_materials,
/// read back out_svt (rgb from svt) and out_sv (luma from sv).
/// Returns (svt_texel0_rgb, sv_texel0, svt_texel1_rgb, sv_texel1).
fn run_tlc_fixture(
    objects: &[RtObjectGeometry],
    gi_materials: &[GiMaterial],
    casters: &[RtCasterParams],
    svt_slot: u32,
    base_color_tex: Option<&manifold_gpu::GpuTexture>,
) -> ([f32; 3], f32, [f32; 3], f32) {
    let h = harness::shared();
    let device = &h.device;

    let tracer = MetalShadowRayTracer::new(device);
    let accel = tracer.build_accel(device, objects, &[]);

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

    let depth_px: [f32; 2] = [0.3, 0.3];
    let depth_tex = upload_texture_f32(device, 2, 1, GpuTextureFormat::Depth32Float, &depth_px, "tlc-depth");

    // out_svt at 2x1 so we can read both texels. Rgba32Float for
    // straightforward readback (kernel writes float4; Metal converts to f16
    // for Rgba16Float targets, but readback is simpler with f32).
    let out_svt = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "tlc-out_svt",
        mip_levels: 1,
    });
    let out_sv = device.create_texture(&GpuTextureDesc {
        width: 2,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "tlc-out_sv",
        mip_levels: 1,
    });
    let out_irr = device.create_texture(&GpuTextureDesc {
        width: 2, height: 1, depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "tlc-out_irr-stub",
        mip_levels: 1,
    });
    let out_n = device.create_texture(&GpuTextureDesc {
        width: 2, height: 1, depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE,
        label: "tlc-out_n-stub",
        mip_levels: 1,
    });
    let out_refl = device.create_texture(&GpuTextureDesc {
        width: 2, height: 1, depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ,
        label: "tlc-out_refl-stub",
        mip_levels: 1,
    });
    let prefiltered_env = device.create_texture(&GpuTextureDesc {
        width: 1, height: 1, depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ,
        label: "tlc-prefiltered-env-dummy",
        mip_levels: 1,
    });
    let out_sv2_dummy = device.create_texture(&GpuTextureDesc {
        width: 1, height: 1, depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "tlc-out_sv2-dummy",
        mip_levels: 1,
    });

    let tex_list: Vec<&manifold_gpu::GpuTexture> = objects
        .iter()
        .filter_map(|o| o.base_color_texture)
        .collect();
    let owned_dummy;
    let final_alpha_textures: Vec<&manifold_gpu::GpuTexture> = if !tex_list.is_empty() {
        tex_list
    } else {
        if let Some(tex) = base_color_tex {
            vec![tex]
        } else {
            owned_dummy = upload_texture_f32(device, 1, 1, GpuTextureFormat::Rgba32Float, &[1.0, 1.0, 1.0, 1.0], "tlc-tex-dummy");
            vec![&owned_dummy]
        }
    };

    let params = ShadowRayParams::new(
        casters,
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
        svt_slot,
    );
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);
    let dummy_emissive = device.create_buffer_shared(1);

    let mut encoder = device.create_encoder("tlc-proof");
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
        "trace_shadow_rays-tlc-proof",
    );
    encoder.commit_and_wait_completed();

    // Read back out_svt (Rgba32Float, at 2x1) — same approach as out_sv.
    let svt_readback_buf = device.create_buffer_shared(2 * 4 * 4);
    let mut enc_svt = device.create_encoder("tlc-svt-readback");
    enc_svt.copy_texture_to_buffer(&out_svt, &svt_readback_buf, 2, 1, 2 * 4 * 4);
    enc_svt.commit_and_wait_completed();
    let svt_ptr = svt_readback_buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let svt_bytes: &[u8] = unsafe { slice::from_raw_parts(svt_ptr.cast::<c_void>().cast::<u8>(), 32) };
    let svt_floats: &[f32] = unsafe { slice::from_raw_parts(svt_bytes.as_ptr().cast::<f32>(), 8) };
    let svt0: [f32; 3] = [svt_floats[0], svt_floats[1], svt_floats[2]];
    let svt1: [f32; 3] = [svt_floats[4], svt_floats[5], svt_floats[6]];

    // Read back out_sv (Rgba32Float, at 2x1).
    let sv_readback_buf = device.create_buffer_shared(2 * 4 * 4);
    let mut enc2 = device.create_encoder("tlc-sv-readback");
    enc2.copy_texture_to_buffer(&out_sv, &sv_readback_buf, 2, 1, 2 * 4 * 4);
    enc2.commit_and_wait_completed();
    let ptr = sv_readback_buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let bytes: &[u8] = unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), 32) };
    let floats: &[f32] = unsafe { slice::from_raw_parts(bytes.as_ptr().cast::<f32>(), 8) };
    let sv0 = floats[0]; // texel 0, r channel
    let sv1 = floats[4]; // texel 1, r channel

    // out_svt texel 0 and 1 should be identical in this fixture (both occluded by one quad).
    (svt0, sv0, svt1, sv1)
}

/// Rec.709 luma weights.
fn luma(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

// ─── Test 1: red petal tints transmitted pool ───────────────────────

#[test]
fn red_petal_tints_transmitted_pool() {
    let h = harness::shared();
    let device = &h.device;

    let verts = quad_verts_z(1.0);
    let vertex_buffer = write_shared_buffer(device, &verts);

    // Red albedo, factor 0.6
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
        [1.0, 0.1, 0.1], // red albedo
        [0.0, 0.0, 0.0], // no emissive
        [0.0, 0.0, 0.0, 0.0], // metallic_roughness
        [0.6, 0.0, 0.0, 0.0], // translucency factor 0.6
    )];

    // Sun caster (kind=0), svt_slot=0 → this caster's rgb goes to out_svt.
    let casters = [RtCasterParams::new([0.0, 0.0, 1.0], 0.0, [0.0, 0.0, 0.0], 0)];

    let (svt0, sv0, svt1, sv1) = run_tlc_fixture(&objects, &gi_materials, &casters, 0, None);

    // Expected tint: factor * albedo = (0.6, 0.06, 0.06)
    let expected: [f32; 3] = [0.6, 0.06, 0.06];

    // out_svt behind occluder ≈ expected tint.
    let eps = 0.01;
    for texel in [svt0, svt1] {
        assert!((texel[0] - expected[0]).abs() < eps,
            "svt r: expected {}, got {}", expected[0], texel[0]);
        assert!((texel[1] - expected[1]).abs() < eps,
            "svt g: expected {}, got {}", expected[1], texel[1]);
        assert!((texel[2] - expected[2]).abs() < eps,
            "svt b: expected {}, got {}", expected[2], texel[2]);
    }

    // RGB ratios: r/g ≈ 10, r/b ≈ 10.
    for texel in [svt0, svt1] {
        let rg = texel[0] / texel[1];
        let rb = texel[0] / texel[2];
        assert!((rg - 10.0).abs() < 0.5, "r/g ratio {:.3} not ~10", rg);
        assert!((rb - 10.0).abs() < 0.5, "r/b ratio {:.3} not ~10", rb);
    }

    // out_sv luma channel equals luma(tint) — TL-B consistency.
    let expected_luma = luma(expected);
    for sv in [sv0, sv1] {
        assert!((sv - expected_luma).abs() < eps,
            "sv luma: expected {}, got {}", expected_luma, sv);
    }
}

// ─── Test 2: point caster control — svt stays white ─────────────────

#[test]
fn point_caster_control_svt_stays_white() {
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
        [1.0, 0.1, 0.1],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0, 0.0],
        [0.6, 0.0, 0.0, 0.0], // factor 0.6
    )];

    // Point caster (kind=1), svt_slot = SVT_SLOT_NONE since no Sun caster.
    // Position at (0, 0, 5) — far enough that its occluder at z=1 still blocks.
    let casters = [RtCasterParams::new([0.0, 0.0, 5.0], 0.0, [0.0, 0.0, 0.0], 1)];

    let (svt0, sv0, svt1, sv1) = run_tlc_fixture(
        &objects, &gi_materials, &casters, SVT_SLOT_NONE, None,
    );

    // out_svt should be white everywhere — svt_slot NONE means no designated sun.
    let eps = 0.01;
    for texel in [svt0, svt1] {
        assert!((texel[0] - 1.0).abs() < eps, "svt r: expected 1.0, got {}", texel[0]);
        assert!((texel[1] - 1.0).abs() < eps, "svt g: expected 1.0, got {}", texel[1]);
        assert!((texel[2] - 1.0).abs() < eps, "svt b: expected 1.0, got {}", texel[2]);
    }

    // out_sv still carries the attenuated luma — point caster occludes.
    let expected_luma = luma([0.6, 0.06, 0.06]);
    for sv in [sv0, sv1] {
        assert!((sv - expected_luma).abs() < eps,
            "sv luma should still attenuate: expected {}, got {}", expected_luma, sv);
    }
}

// ─── Test 3: factor-0 control — svt reads black behind opaque ───────

#[test]
fn factor_zero_occluder_svt_reads_zero() {
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
        translucent: false, // factor 0 → opaque, BLAS stays on fast path
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
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0, 0.0], // factor 0 = opaque
    )];

    let casters = [RtCasterParams::new([0.0, 0.0, 1.0], 0.0, [0.0, 0.0, 0.0], 0)];

    let (svt0, sv0, svt1, sv1) = run_tlc_fixture(&objects, &gi_materials, &casters, 0, None);

    // Fully opaque occluder → svt reads (0,0,0) behind it (fully shadowed).
    let eps = 0.01;
    for texel in [svt0, svt1] {
        assert!(texel[0].abs() < eps, "svt r: expected 0, got {}", texel[0]);
        assert!(texel[1].abs() < eps, "svt g: expected 0, got {}", texel[1]);
        assert!(texel[2].abs() < eps, "svt b: expected 0, got {}", texel[2]);
    }

    // out_sv reads zero too (fully shadowed).
    for sv in [sv0, sv1] {
        assert!(sv.abs() < eps, "sv should be 0 (fully shadowed), got {}", sv);
    }
}

// ─── Test 4: unoccluded texels read white in out_svt ─────────────────

#[test]
fn unoccluded_texels_read_white_in_svt() {
    let h = harness::shared();
    let device = &h.device;

    // Quad at z=-1 (BEHIND the depth surface) — doesn't occlude.
    let verts = quad_verts_z(-1.0);
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
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0, 0.0],
        [0.5, 0.0, 0.0, 0.0],
    )];

    let casters = [RtCasterParams::new([0.0, 0.0, 1.0], 0.0, [0.0, 0.0, 0.0], 0)];

    let (svt0, sv0, svt1, sv1) = run_tlc_fixture(&objects, &gi_materials, &casters, 0, None);

    // Unoccluded → white (1,1,1) in out_svt.
    let eps = 0.01;
    for texel in [svt0, svt1] {
        assert!((texel[0] - 1.0).abs() < eps, "unoccluded svt r: expected 1, got {}", texel[0]);
        assert!((texel[1] - 1.0).abs() < eps, "unoccluded svt g: expected 1, got {}", texel[1]);
        assert!((texel[2] - 1.0).abs() < eps, "unoccluded svt b: expected 1, got {}", texel[2]);
    }

    // out_sv reads fully lit (1.0).
    for sv in [sv0, sv1] {
        assert!((sv - 1.0).abs() < eps, "unoccluded sv: expected 1, got {}", sv);
    }
}
