//! RT_INSTANCING_DESIGN.md P3/D8 — the emissive light table spans (object,
//! slot) pairs: an instanced emissive object (loop count = 2, one quad
//! duplicated per slot) lights a diffuse receiver card, and the traced
//! irradiance matches a CPU oracle that accounts for BOTH copies emitting.
//! This proof is the arbiter of the D8 weighting derivation: the alias
//! proposal is per-slot-duplicated LOCAL power and the kernel weights by
//! the sampled entry's TRUE world area composed from the TLAS descriptor
//! buffer — if the normalization were off by the copy count or a scale
//! factor, this oracle says so.
//!
//! Fixture (synthetic, BUG-twa6): object 0 = the emissive quad, wired with
//! two live translation-only slots at (±0.25, 0, 0.35); object 1 = the
//! receiver, an unwired quad facing −z with its center at
//! (0.1041667, 0, 0.5) (the probe-rig's column-4 target). One receiver
//! texel is traced: the GI gather's emissive RIS sampler draws ONE alias
//! sample deterministically (fixed frame/seed), so the CPU oracle
//! replicates the draw exactly — the ported `pcg`/`rand2` below are
//! bit-copies of the kernel's, and the alias table for four equal-power
//! candidates has prob 1.0/self-alias, so the drawn index is
//! `floor(u1.x * 4)` into the candidate order [(t0,s0),(t0,s1),(t1,s0),
//! (t1,s1)].

use std::ffi::c_void;
use std::slice;

use manifold_gpu::raytrace::{
    ensure_normal_sources, GiMaterial, MetalShadowRayTracer, RtCasterParams, RtObjectGeometry,
    ShadowRayParams, ShadowRayTracer,
};
use manifold_gpu::{
    GpuBuffer, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};
use manifold_renderer::generators::mesh_common::InstanceTransform;

use crate::harness;

/// `pos` + `normal` interleaved vertex (stride 24, normal at offset 12) —
/// same record shape `rt_instancing.rs` uses.
#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertexN {
    pos: [f32; 3],
    normal: [f32; 3],
}

/// Emissive quad: two triangles in the local z=0 plane, x/y in
/// [-0.05, 0.05], normal (0,0,-1). Local origin = center, so a
/// translation-only slot's stored translation is the copy's world center.
const EMISSIVE_QUAD: [PackedVertexN; 6] = [
    PackedVertexN { pos: [-0.05, -0.05, 0.0], normal: [0.0, 0.0, -1.0] },
    PackedVertexN { pos: [ 0.05, -0.05, 0.0], normal: [0.0, 0.0, -1.0] },
    PackedVertexN { pos: [ 0.05,  0.05, 0.0], normal: [0.0, 0.0, -1.0] },
    PackedVertexN { pos: [-0.05, -0.05, 0.0], normal: [0.0, 0.0, -1.0] },
    PackedVertexN { pos: [ 0.05,  0.05, 0.0], normal: [0.0, 0.0, -1.0] },
    PackedVertexN { pos: [-0.05,  0.05, 0.0], normal: [0.0, 0.0, -1.0] },
];

/// Receiver quad: same shape, placed by its (unwired) model transform.
const RECEIVER_QUAD: [PackedVertexN; 6] = EMISSIVE_QUAD;

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

const EMISSIVE: [f32; 3] = [2.0, 2.0, 2.0];
/// Receiver surface point: the column-4 probe reconstructs
/// wp = (ndc 0.125, 0, depth) — with depth = the receiver's z, wp lands ON
/// the surface (sec_origin is biased from wp along the receiver normal).
const RECEIVER_WP: [f32; 3] = [0.125, 0.0, 0.5];
/// The sampler's lighting origin: the kernel biases `sec_origin` along the
/// receiver normal by `bias_eps = min(texel_scale·2, 0.02)` — texel_scale
/// is the screen-space neighbor delta, ~0.56 for this 8×1 fixture (texel
/// 5's void-depth neighbor reconstructs to (0.375,0,0)), so the 0.02 cap
/// binds. The oracle must light from the SAME origin.
const SEC_ORIGIN: [f32; 3] = [0.125, 0.0, 0.48];
/// The two live slot translations (z = 0.35, in front of the receiver).
const SLOT_POS: [[f32; 3]; 2] = [[0.25, 0.0, 0.35], [-0.25, 0.0, 0.35]];

// ─── Bit-exact ports of the kernel's RNG (MSL `pcg`/`rand2`) ───────────

fn pcg(mut v: u32) -> u32 {
    v = v.wrapping_mul(747796405).wrapping_add(2891336453);
    v = ((v >> ((v >> 28) + 4)) ^ v).wrapping_mul(277803737);
    (v >> 22) ^ v
}

fn rand2(p: [u32; 2], frame: u32, ray: u32) -> [f32; 2] {
    let s = pcg(p[0].wrapping_add(pcg(p[1].wrapping_add(pcg(frame.wrapping_mul(61).wrapping_add(ray))))));
    let t = pcg(s);
    [
        (s & 0xFFFFFF) as f32 / 16777216.0,
        (t & 0xFFFFFF) as f32 / 16777216.0,
    ]
}

fn write_shared_buffer<T: Copy>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
    let bytes = std::mem::size_of_val(data) as u64;
    let buf = device.create_buffer_shared(bytes.max(16));
    let ptr = buf.mapped_ptr().expect("shared buffer must expose a mapped pointer");
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
) -> GpuTexture {
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

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn len(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// D8's arbiter: both emissive copies contribute to the oracle through the
/// per-slot-duplicated proposal (the drawn entry's slot emits) and the
/// per-entry world-area weight (the OTHER copy is in the proposal mass).
#[test]
fn instanced_emissive_object_lights_receiver_both_copies_emit() {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);

    let emissive_buffer = write_shared_buffer(device, &EMISSIVE_QUAD);
    let receiver_buffer = write_shared_buffer(device, &RECEIVER_QUAD);
    let instances = [
        InstanceTransform { pos_scale: [SLOT_POS[0][0], SLOT_POS[0][1], SLOT_POS[0][2], 1.0], rot_pad: [0.0; 4] },
        InstanceTransform { pos_scale: [SLOT_POS[1][0], SLOT_POS[1][1], SLOT_POS[1][2], 1.0], rot_pad: [0.0; 4] },
    ];
    let instances_buffer = write_shared_buffer(device, &instances);

    let mut receiver_model = IDENTITY;
    receiver_model[3][0] = RECEIVER_WP[0];
    receiver_model[3][1] = RECEIVER_WP[1];
    receiver_model[3][2] = RECEIVER_WP[2];

    let objects = [
        RtObjectGeometry {
            vertex_buffer: &emissive_buffer,
            vertex_stride: std::mem::size_of::<PackedVertexN>() as u32,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 2,
            transform: IDENTITY,
            normal_offset: 12,
            uv_offset: 0,
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
            instances_addr: instances_buffer.gpu_address(),
            instances_buffer: Some(&instances_buffer),
            instance_slots: 2,
        },
        RtObjectGeometry {
            vertex_buffer: &receiver_buffer,
            vertex_stride: std::mem::size_of::<PackedVertexN>() as u32,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 2,
            transform: receiver_model,
            normal_offset: 12,
            uv_offset: 0,
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
            instances_addr: 0,
            instances_buffer: None,
            instance_slots: 1,
        },
    ];
    let materials = [
        GiMaterial::new([0.0; 3], EMISSIVE, [0.0, 1.0, 0.0, 0.0], [0.0; 4]),
        GiMaterial::new([0.8, 0.8, 0.8], [0.0; 3], [0.0, 1.0, 0.0, 0.0], [0.0; 4]),
    ];
    let accel = tracer.build_accel(device, &objects, &materials);
    let table = accel.emissive_table.as_ref().expect("emissive object must build a light table");
    assert!(table.entries_are_local, "wired instances => local-space emissive entries (D8)");
    assert_eq!(table.entry_count, 4, "2 triangles x 2 slots = 4 candidates (D8)");

    // D11 tables + per-slot gi materials (N + Σ = 2 + 3 = 5 rows).
    let mut nss = None;
    let mut nsc = 0usize;
    ensure_normal_sources(&mut nss, &mut nsc, device, &objects);
    let normal_sources = nss.unwrap();
    let gi_rows = 5usize;
    let gi_buffer = device.create_buffer_shared((gi_rows * std::mem::size_of::<GiMaterial>()) as u64);
    {
        let ptr = gi_buffer.mapped_ptr().expect("gi buffer must be shared") as *mut GiMaterial;
        let row_for = |oi: usize| materials[oi];
        let mut row = 0usize;
        let slots = [2usize, 1usize];
        for (oi, &s) in slots.iter().enumerate() {
            for _ in 0..=s {
                unsafe { ptr.add(row).write(row_for(oi)) };
                row += 1;
            }
        }
        debug_assert_eq!(row, gi_rows);
    }

    // Depth fixture: one texel at column 4 with depth = the receiver's z,
    // so the reconstructed wp is ON the receiver surface (the lighting
    // origin sec_origin is biased from wp along the receiver normal).
    let depth_px: [f32; 8] = [0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0];
    let depth_tex = upload_texture_f32(device, 8, 1, GpuTextureFormat::Depth32Float, &depth_px, "rt-emissive-inst-depth");
    let tex = |label: &str| {
        device.create_texture(&GpuTextureDesc {
            width: 8,
            height: 1,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
            label,
            mip_levels: 1,
        })
    };
    let (out_sv, out_sv2, out_svt, out_irr, out_n, out_refl) = (
        tex("ei-out_sv"),
        tex("ei-out_sv2"),
        tex("ei-out_svt"),
        tex("ei-out_irr"),
        tex("ei-out_n"),
        tex("ei-out_refl"),
    );
    let prefiltered_env = device.create_texture(&GpuTextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ,
        label: "ei-env-dummy",
        mip_levels: 1,
    });

    // Lighting dispatch: gi_spp = 1 turns the emissive RIS sampler on; the
    // env chain is a zeroed dummy so the estimator's only term is
    // emissive_direct. Same CPU oracle as the kernel for the SAME draw.
    let casters: [RtCasterParams; 0] = [];
    let params = ShadowRayParams::new(
        &casters,
        0,
        0,
        [8, 1],
        [8, 1],
        0.0,
        0,
        1,
        [0.0, 0.0, 0.0],
        IDENTITY,
        0,
        0.6,
        0.1,
        table.mean_power,
        table.entry_count,
        table.total_area,
        manifold_gpu::raytrace::SVT_SLOT_NONE,
    )
    .with_slot_row_base(objects.len() as u32)
    .with_emissive_entries_local(table.entries_are_local);
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);

    let mut encoder = device.create_encoder("rt-emissive-instancing");
    tracer.dispatch_shadow_rays(
        &mut encoder,
        &accel,
        &params,
        &params_buffer,
        &gi_buffer,
        &normal_sources,
        &[],
        &depth_tex,
        &out_sv,
        &out_sv2,
        &out_svt,
        &out_irr,
        &out_n,
        &out_refl,
        &prefiltered_env,
        &table.triangles,
        &table.aliases,
        false,
        "trace_shadow_rays-emissive-instancing",
    );
    encoder.commit_and_wait_completed();

    let readback = device.create_buffer_shared(8 * 4 * 4);
    let mut enc2 = device.create_encoder("rt-emissive-inst-readback");
    enc2.copy_texture_to_buffer(&out_irr, &readback, 8, 1, 8 * 4 * 4);
    enc2.commit_and_wait_completed();
    let ptr = readback.mapped_ptr().expect("readback must be mapped");
    let floats: &[f32] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<f32>(), 32) };
    let got = &floats[4 * 4..4 * 4 + 3];

    // Primary-hit contract: the texel must resolve to the RECEIVER
    // (object 1) with its −z normal — this is also what caught the
    // unwired-object-model descriptor bug during bring-up (an identity
    // world matrix instead of the model transform put the receiver's copy
    // at the local origin and the primary missed).
    let rb_n = device.create_buffer_shared(8 * 4 * 4);
    let mut enc3 = device.create_encoder("rt-emissive-inst-rb-n");
    enc3.copy_texture_to_buffer(&out_n, &rb_n, 8, 1, 8 * 4 * 4);
    enc3.commit_and_wait_completed();
    let n_ptr = rb_n.mapped_ptr().expect("readback must be mapped");
    let n_floats: &[f32] =
        unsafe { slice::from_raw_parts(n_ptr.cast::<c_void>().cast::<f32>(), 32) };
    let hit = &n_floats[4 * 4..4 * 4 + 4];
    assert!(
        (hit[0]).abs() < 1e-5 && (hit[1]).abs() < 1e-5 && (hit[2] + 1.0).abs() < 1e-5
            && hit[3] == 1.0,
        "primary must hit the receiver (normal (0,0,-1), object_index 1), got {hit:?}"
    );

    // ─── CPU oracle: replicate the draw and the D8 estimator exactly ───
    // Candidate order from build_emissive_table: per triangle, per slot:
    // [(t0,s0),(t0,s1),(t1,s0),(t1,s1)] — all four with equal local power
    // (same triangle area, same luma), so the alias is prob-1.0 self-alias
    // and the drawn index is floor(u1.x * 4).
    let tid = [4u32, 0u32];
    let u1 = rand2(tid, 0, 700);
    let i = ((u1[0] * 4.0) as u32).min(3);
    let tri = (i / 2) as usize; // triangle index (2 tris)
    let slot = (i % 2) as usize; // slot index
    let base = tri * 3;
    let local = |k: usize| EMISSIVE_QUAD[base + k].pos;
    // Translation-only slot: world = local + slot translation.
    let w = |k: usize| {
        let l = local(k);
        [l[0] + SLOT_POS[slot][0], l[1] + SLOT_POS[slot][1], l[2] + SLOT_POS[slot][2]]
    };
    let (w0, w1, w2) = (w(0), w(1), w(2));
    let u2 = rand2(tid, 0, 702);
    let su = u2[0].sqrt();
    let (a, b) = (1.0 - su, u2[1] * su);
    let q = [
        w0[0] * a + w1[0] * b + w2[0] * (1.0 - a - b),
        w0[1] * a + w1[1] * b + w2[1] * (1.0 - a - b),
        w0[2] * a + w1[2] * b + w2[2] * (1.0 - a - b),
    ];
    let l = sub(q, SEC_ORIGIN);
    let l_len = len(l);
    let l_hat = [l[0] / l_len, l[1] / l_len, l[2] / l_len];
    let n_r = [0.0f32, 0.0, -1.0];
    let cos_theta = (n_r[0] * l_hat[0] + n_r[1] * l_hat[1] + n_r[2] * l_hat[2]).max(0.0);
    let e1 = sub(w1, w0);
    let e2 = sub(w2, w0);
    let n_t = cross(e1, e2);
    let n_len = len(n_t);
    assert!(n_len > 1e-6, "oracle: degenerate emitter triangle");
    let n_t = [n_t[0] / n_len, n_t[1] / n_len, n_t[2] / n_len];
    let cos_emit = ((n_t[0] * -l_hat[0] + n_t[1] * -l_hat[1] + n_t[2] * -l_hat[2]) as f32).abs();
    // D8 weight: the sampled entry's TRUE world area (uniform scale 1 here
    // = local area 0.005) — the derivation the proof arbitrates.
    let area = 0.5 * n_len;
    let geom = cos_theta * cos_emit * area / (l_len * l_len);
    let expected = [EMISSIVE[0] * geom, EMISSIVE[1] * geom, EMISSIVE[2] * geom];

    eprintln!(
        "draw: entry {i} (tri {tri}, slot {slot}) | cos_theta {cos_theta:.4} cos_emit {cos_emit:.4} \
         area {area:.5} dist {l_len:.4} | expected {expected:?} got {got:?}"
    );
    for (c, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
        assert!(
            (g - e).abs() < 5e-3,
            "irradiance channel {c}: gpu {g}, CPU oracle {e} — the D8 weighting (per-slot \
             proposal duplication + per-entry world area) does not reproduce; if this is off \
             by the copy count or a scale factor the normalization is wrong"
        );
    }
}
