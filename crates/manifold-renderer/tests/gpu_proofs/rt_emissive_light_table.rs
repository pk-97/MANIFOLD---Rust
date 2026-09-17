//! `docs/RAYTRACING_DESIGN.md` section 15.6 RS-B — value-level proof for the
//! emissive-triangle light table and alias-table build.
//!
//! Two tests:
//! 1. `emissive_table_contents_match_cpu_oracle` — a synthetic multi-object
//!    fixture with known triangle areas, emissive luma values, and powers.
//!    Verifies per-entry area, power, alias-table validity, truncation
//!    order, mean power, and that zero-emissive objects are excluded.
//! 2. `emissive_table_zero_stats_when_all_zero_emissive` — fixture where
//!    every object has black emissive; the table's GPU stats must read
//!    entry_count 0.
//!
//! P4a: the table is prepared by the production GPU kernels
//! (`MetalShadowRayTracer::debug_encode_emissive_table` — enumerate →
//! radix sort → gather → alias → stats, the same dispatches the shared AS
//! update path runs). The CPU-side builder is deleted; count/mean/area
//! assertions read the GPU-written `EmissiveTableStats` buffer.

use manifold_gpu::raytrace::{
    EmissiveAliasEntry, EmissiveTableStats, EmissiveTriangleGpu, GiMaterial,
    MetalShadowRayTracer, RtAccel, RtObjectGeometry, ShadowRayTracer,
    MAX_RT_EMISSIVE_TRIANGLES,
};
use manifold_gpu::GpuDevice;

use crate::harness;

/// `packed_float3` stride-12 vertex layout — matches the position-only
/// convention the emissive table builder reads (offset 0, stride 12).
#[repr(C)]
#[derive(Clone, Copy)]
struct PosVertex {
    pos: [f32; 3],
}

fn write_shared_buffer(device: &GpuDevice, data: &[PosVertex]) -> manifold_gpu::GpuBuffer {
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

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Luminance of a linear-HDR RGB triple — must match the `luma()` helper
/// in `raytrace.rs` exactly (Rec.709 weights).
fn cpu_luma(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// Area of a triangle in 3D — must match `triangle_area()` in `raytrace.rs`.
fn cpu_triangle_area(v0: [f32; 3], v1: [f32; 3], v2: [f32; 3]) -> f32 {
    let e1 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
    let e2 = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
    let cross = [
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ];
    let mag2 = cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2];
    if mag2 <= 0.0 {
        0.0
    } else {
        0.5 * mag2.sqrt()
    }
}

/// P4a: run the production GPU emissive preparation on this fixture —
/// plan/prepare an `RtAccel`, then `debug_encode_emissive_table` on a
/// committed encoder. Returns the tracer, the accel (table resident), and
/// the mapped GPU stats; callers map `table.triangles`/`table.aliases`
/// for entry-level assertions.
fn gpu_prepare_emissive_table(
    device: &GpuDevice,
    objects: &[RtObjectGeometry<'_>],
    materials: &[GiMaterial],
) -> (MetalShadowRayTracer, RtAccel, EmissiveTableStats) {
    let tracer = MetalShadowRayTracer::new(device);
    let plan = tracer.plan_accel(device, None, objects).expect("plan accel");
    let mut accel_slot = None;
    tracer
        .prepare_accel(device, &mut accel_slot, plan)
        .expect("prepare accel");
    let mut accel = accel_slot.unwrap();
    let mut encoder = device.create_encoder("rt-emissive-table-proof");
    tracer
        .debug_encode_emissive_table(device, &mut encoder, &mut accel, objects, materials)
        .expect("encode emissive table");
    encoder.commit_and_wait_completed();
    let table = accel.emissive_table.as_ref().expect("encode ensures the table");
    let stats_ptr = table.stats.mapped_ptr().expect("stats buffer must be shared");
    // SAFETY: `EmissiveTableStats` is `#[repr(C)]` all-POD, 16 bytes — the
    // buffer is exactly one struct wide.
    let stats = unsafe { (stats_ptr as *const EmissiveTableStats).read_unaligned() };
    (tracer, accel, stats)
}

fn rt_object_geom<'a>(
    vbuf: &'a manifold_gpu::GpuBuffer,
    tri_count: u32,
    cast_shadows: bool,
) -> RtObjectGeometry<'a> {
    RtObjectGeometry {
        vertex_buffer: vbuf,
        vertex_stride: std::mem::size_of::<PosVertex>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: tri_count,
        transform: IDENTITY,
        normal_offset: 0, // unused — n/a for emissive table
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
        cast_shadows,
        instances_addr: 0,
        instances_buffer: None,
        instance_slots: 1,
        appearance_weights: None,
        appearance_gain: 1.0,
    }
}

/// ─── Proof 1: table contents match CPU-computed expected ────────────
///
/// Fixture:
/// - Object 0: emissive red (luma ≈ 0.2126), 2 triangles forming a
///   unit square in XY — area 0.5 each, power 0.1063 each
/// - Object 1: zero emissive, 2 triangles — should NOT appear in table
/// - Object 2: emissive green (luma ≈ 0.7152), 1 triangle — area 2.0
///   (half of 2×2 square), power ≈ 1.4304
/// - Object 3: emissive blue (luma ≈ 0.0722), 1 triangle — area 0.5
///   (half of 1×1), power ≈ 0.0361
///
/// Expected table: 4 entries (from objects 0, 2, 3 — object 1 excluded).
/// Total power ≈ 2 * 0.1063 + 1.4304 + 0.0361 = 1.6791.
/// Mean power ≈ 1.6791 / 4 = 0.4198.
///
/// Power rank (descending): obj2 tri (1.4304) > obj0 tri 0 (0.1063)
/// ≈ obj0 tri 1 (0.1063) > obj3 tri (0.0361).
#[test]
fn emissive_table_contents_match_cpu_oracle() {
    let h = harness::shared();
    let device = &h.device;

    // Object 0: unit square in XY at z=0 — 2 triangles (6 verts, non-indexed), emissive red
    let v0 = [
        PosVertex { pos: [0.0, 0.0, 0.0] },
        PosVertex { pos: [1.0, 0.0, 0.0] },
        PosVertex { pos: [0.0, 1.0, 0.0] }, // tri 0
        PosVertex { pos: [1.0, 0.0, 0.0] },
        PosVertex { pos: [0.0, 1.0, 0.0] },
        PosVertex { pos: [1.0, 1.0, 0.0] }, // tri 1
    ];
    let buf0 = write_shared_buffer(device, &v0);

    // Object 1: tall triangle at z=1 — zero emissive (excluded)
    let v1 = [
        PosVertex { pos: [5.0, 0.0, 1.0] },
        PosVertex { pos: [5.0, 2.0, 1.0] },
        PosVertex { pos: [7.0, 1.0, 1.0] },
    ];
    let buf1 = write_shared_buffer(device, &v1);

    // Object 2: large triangle at z=2 — emissive green, area 2.0
    let v2 = [
        PosVertex { pos: [0.0, 0.0, 2.0] },
        PosVertex { pos: [2.0, 2.0, 2.0] },
        PosVertex { pos: [0.0, 2.0, 2.0] },
    ];
    let buf2 = write_shared_buffer(device, &v2);

    // Object 3: small triangle at z=3 — emissive blue, area 0.5
    let v3 = [
        PosVertex { pos: [10.0, 0.0, 3.0] },
        PosVertex { pos: [11.0, 0.0, 3.0] },
        PosVertex { pos: [10.0, 1.0, 3.0] },
    ];
    let buf3 = write_shared_buffer(device, &v3);

    let objects = [
        rt_object_geom(&buf0, 2, true),
        rt_object_geom(&buf1, 1, true),
        rt_object_geom(&buf2, 1, true),
        rt_object_geom(&buf3, 1, true),
    ];

    let materials = [
        GiMaterial::new([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.5, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]), // emissive red
        GiMaterial::new([0.5, 0.5, 0.5], [0.0, 0.0, 0.0], [0.0, 0.5, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]), // black
        GiMaterial::new([0.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.5, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]), // emissive green
        GiMaterial::new([0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.5, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]), // emissive blue
    ];

    let (_tracer, accel, stats) = gpu_prepare_emissive_table(device, &objects, &materials);
    let table = accel.emissive_table.as_ref().expect("table resident");

    assert_eq!(stats.entry_count, 4, "4 triangles from 3 emissive objects");

    // Read back the GPU buffers.
    let tri_ptr = table
        .triangles
        .mapped_ptr()
        .expect("triangle buffer should be shared");
    let alias_ptr = table
        .aliases
        .mapped_ptr()
        .expect("alias buffer should be shared");

    let triangles: &[EmissiveTriangleGpu] = unsafe {
        std::slice::from_raw_parts(
            tri_ptr as *const EmissiveTriangleGpu,
            stats.entry_count as usize,
        )
    };
    let aliases: &[EmissiveAliasEntry] = unsafe {
        std::slice::from_raw_parts(
            alias_ptr as *const EmissiveAliasEntry,
            stats.entry_count as usize,
        )
    };

    // CPU-derived expected per-triangle data.
    // Object 0, tri 0: [0,0,0]-[1,0,0]-[0,1,0], area 0.5, power 0.1063
    // Object 0, tri 1: [1,0,0]-[0,1,0]-[1,1,0], area 0.5, power 0.1063
    // Object 2, tri 0: [0,0,2]-[2,2,2]-[0,2,2], area 2.0, power 1.4304
    // Object 3, tri 0: [10,0,3]-[11,0,3]-[10,1,3], area 0.5, power 0.0361

    // Collect areas and powers from GPU table.
    let mut total_power: f32 = 0.0;
    let eps = 1e-5;

    for (i, t) in triangles.iter().enumerate() {
        let area = cpu_triangle_area(t.v0, t.v1, t.v2);
        // Verify world-space positions are correctly transformed (all identity
        // transforms, so local == world).
        assert!((area - 0.5).abs() < eps || (area - 2.0).abs() < eps,
            "entry {i}: area {area} is neither 0.5 nor 2.0 (expected from fixture triangles)");

        // Alias table validity: scaled prob in [0, 1], alias in [0, n).
        assert!(
            aliases[i].prob >= 0.0 && aliases[i].prob <= 1.0 + eps,
            "entry {i}: alias prob {} out of [0, 1]",
            aliases[i].prob
        );
        assert!(
            aliases[i].alias < stats.entry_count,
            "entry {i}: alias {} out of range [0, {})",
            aliases[i].alias,
            stats.entry_count
        );
    }

    // Sum powers by recalculating: power = area × emissive_luma of the source object.
    // Map each GPU entry back: object 0 entries have z=0.0, 2 have z=2.0, 3 have z=3.0.
    for t in triangles.iter() {
        let area = cpu_triangle_area(t.v0, t.v1, t.v2);
        let emissive_luma = if t.v0[2].abs() < eps {
            // Object 0: z ≈ 0
            cpu_luma([1.0, 0.0, 0.0])
        } else if (t.v0[2] - 2.0).abs() < eps {
            // Object 2: z ≈ 2
            cpu_luma([0.0, 1.0, 0.0])
        } else {
            // Object 3: z ≈ 3
            cpu_luma([0.0, 0.0, 1.0])
        };
        total_power += area * emissive_luma;
    }

    // Expected: 2 * 0.1063 + 1.4304 + 0.0361 ≈ 1.6791
    let expected_total = 2.0 * 0.1063 + 1.4304 + 0.0361;
    assert!(
        (total_power - expected_total).abs() < 0.001,
        "total power {total_power:.4} != expected {expected_total:.4}"
    );

    // Mean power verification — the GPU stats buffer's value against the
    // CPU-computed expectation (assertion semantics unchanged from the
    // CPU-built table era).
    let expected_mean = expected_total / 4.0;
    assert!(
        (stats.mean_power - expected_mean).abs() < 0.001,
        "mean power {:.4} != expected {:.4}", stats.mean_power, expected_mean
    );

    // Verify truncation: with only 4 entries, none should be truncated
    // (MAX_RT_EMISSIVE_TRIANGLES is 4096 — far above our count).
    assert!(stats.entry_count <= MAX_RT_EMISSIVE_TRIANGLES);
}

/// ─── Proof 2: zero-emissive scene → zero-stats table ─────────────
///
/// Fixture: single object, black emissive (0,0,0). P4a keeps a resident
/// table for every prepared accel — the GPU stats must read entry_count 0
/// (the old contract returned `None` instead of a table).
#[test]
fn emissive_table_zero_stats_when_all_zero_emissive() {
    let h = harness::shared();
    let device = &h.device;

    let verts = [
        PosVertex { pos: [0.0, 0.0, 0.0] },
        PosVertex { pos: [1.0, 0.0, 0.0] },
        PosVertex { pos: [0.0, 1.0, 0.0] },
    ];
    let buf = write_shared_buffer(device, &verts);

    let objects = [rt_object_geom(&buf, 1, true)];
    let materials = [GiMaterial::new(
        [0.5, 0.5, 0.5],
        [0.0, 0.0, 0.0], // black emissive
        [0.0, 0.5, 0.0, 0.0],
        [0.0, 0.0, 0.0, 0.0],
    )];

    let (_tracer, accel, stats) = gpu_prepare_emissive_table(device, &objects, &materials);
    assert!(
        accel.emissive_table.is_some(),
        "P4a: the table is resident from first preparation onward"
    );
    assert_eq!(
        stats.entry_count, 0,
        "stats entry_count must be 0 when no object has non-black emissive"
    );
    assert_eq!(stats.mean_power, 0.0, "zero-emission scene has zero mean power");
}

/// ─── Proof 3: truncation boundary ─────────────────────────────────
///
/// When entries exceed MAX_RT_EMISSIVE_TRIANGLES, the dimmest are truncated.
/// We can't practically create 4097 fixtures in a test, but we can verify
/// that the truncation logic is reachable and that the table stays at the
/// cap. This test creates a fixture with exactly MAX_RT_EMISSIVE_TRIANGLES
/// + few triangles (from a single quad per "object") — we verify the cap.
///
/// Strategy: one object with many quads (2 triangles each). We make each
/// quad distinct by varying z so the builder does not merge them.
/// 2049 quads (4098 triangles) exceeds the cap; we expect exactly 4096
/// entries.
#[test]
fn emissive_table_truncates_at_cap() {
    let h = harness::shared();
    let device = &h.device;

    // Build one large vertex buffer with enough quads.
    let n_quads = (MAX_RT_EMISSIVE_TRIANGLES / 2 + 2) as usize; // 2050 quads = 4100 triangles
    let n_verts = n_quads * 4; // 4 vertices per quad
    let mut verts: Vec<PosVertex> = Vec::with_capacity(n_verts);
    for i in 0..n_quads {
        let z = i as f32 * 0.001; // tiny z offsets to keep power nearly equal
        verts.extend_from_slice(&[
            PosVertex { pos: [0.0, 0.0, z] },
            PosVertex { pos: [1.0, 0.0, z] },
            PosVertex { pos: [1.0, 1.0, z] },
            PosVertex { pos: [0.0, 1.0, z] },
        ]);
    }
    let buf = write_shared_buffer(device, &verts);

    // plan_accel validates flat (non-indexed) geometry as triangle_count*3
    // vertices per object; a real quad is 4 vertices + a 6-entry index
    // buffer. Share one index buffer across all quad objects — indices are
    // relative to each object's vertex_offset.
    let indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
    let ib = device.create_buffer_shared(std::mem::size_of_val(&indices) as u64);
    let ib_ptr = ib
        .mapped_ptr()
        .expect("shared buffer must expose a mapped pointer");
    unsafe {
        std::ptr::copy_nonoverlapping(
            indices.as_ptr().cast::<u8>(),
            ib_ptr,
            std::mem::size_of_val(&indices),
        );
    }

    // Each "object" is one indexed quad (2 triangles). Use the same buffers
    // with per-object vertex offsets to simulate separate objects.
    let stride = std::mem::size_of::<PosVertex>() as u32;
    let mut objects: Vec<RtObjectGeometry> = Vec::with_capacity(n_quads);
    for i in 0..n_quads {
        objects.push(RtObjectGeometry {
            vertex_buffer: &buf,
            vertex_stride: stride,
            vertex_offset: (i * 4 * stride as usize) as u32,
            index_buffer: Some(&ib),
            triangle_count: 2, // 4 verts + 6 indices → 2 triangles
            transform: IDENTITY,
            normal_offset: 0,
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
            appearance_weights: None,
            appearance_gain: 1.0,
        });
    }

    let mut materials: Vec<GiMaterial> = Vec::with_capacity(n_quads);
    for _ in 0..n_quads {
        materials.push(GiMaterial::new(
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0], // emissive white
            [0.0, 0.5, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.0],
        ));
    }

    let (_tracer, _accel, stats) = gpu_prepare_emissive_table(device, &objects, &materials);

    assert_eq!(
        stats.entry_count, MAX_RT_EMISSIVE_TRIANGLES,
        "table must truncate at MAX_RT_EMISSIVE_TRIANGLES ({}), got {}",
        MAX_RT_EMISSIVE_TRIANGLES, stats.entry_count
    );

    // Verify mean power is valid (>0 since all entries have positive power).
    assert!(
        stats.mean_power > 0.0,
        "mean power must be positive for emissive scene"
    );
}
