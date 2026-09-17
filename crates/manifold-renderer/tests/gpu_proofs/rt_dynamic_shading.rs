//! SCENE_MODIFIER_RT_DESIGN.md §5.1 / A4 (BUG-e3p6.4) — the
//! `rt_dynamic_shading` group, P4a half: the emissive light table is
//! prepared on the GPU (`encode_emissive_table` — enumerate → radix sort →
//! gather → alias → stats) and the trace/firefly kernels read the 16-byte
//! GPU stats buffer. Every assertion compares against a CPU oracle computed
//! FROM THE FIXTURE INPUTS (never from production modifier code). A4's
//! "deliberately stale CPU mirror must not affect results" is structural
//! now — the CPU params fields are deleted; nothing CPU-side can go stale.
//! The P4b half is `rt_dynamic_coverage_and_attributes` below.

use manifold_gpu::raytrace::{
    EmissiveAliasEntry, EmissiveTableStats, EmissiveTriangleGpu, GiMaterial,
    MetalShadowRayTracer, RtAccel, RtGeometryChange, RtObjectGeometry, ShadowRayTracer,
    MAX_RT_EMISSIVE_TRIANGLES,
};
use manifold_gpu::{GpuBuffer, GpuDevice};
use manifold_renderer::generators::mesh_common::InstanceTransform;

use crate::harness;

/// Same interleaved layout the other RT fixtures use: pos(16) + normal(16)
/// + uv(8), stride 40 — normal offset 16, uv offset 32.
#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertex {
    pos: [f32; 4],
    normal: [f32; 4],
    uv: [f32; 2],
}

const VERTEX_STRIDE: u32 = std::mem::size_of::<PackedVertex>() as u32;
const NORMAL_OFFSET: u32 = 16;
const UV_OFFSET: u32 = 32;

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn vertex(pos: [f32; 3], uv: [f32; 2]) -> PackedVertex {
    PackedVertex {
        pos: [pos[0], pos[1], pos[2], 0.0],
        normal: [0.0, 0.0, 1.0, 0.0],
        uv,
    }
}

/// Upward triangle in the z=0 plane, leg length `leg` (area leg²/2),
/// centered at (`cx`, 0, 0), per-vertex distinct UVs derived from `cx`.
fn triangle_at(cx: f32, leg: f32) -> [PackedVertex; 3] {
    let h = leg / 2.0;
    [
        vertex([cx - h, -h, 0.0], [cx, 0.0]),
        vertex([cx + h, -h, 0.0], [cx + 0.5, 0.0]),
        vertex([cx, h, 0.0], [cx + 0.25, 1.0]),
    ]
}

fn write_shared<T: Copy>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
    let bytes = std::mem::size_of_val(data) as u64;
    let buf = device.create_buffer_shared(bytes.max(16));
    let ptr = buf.mapped_ptr().expect("fixture buffer must be CPU-mapped");
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), ptr, bytes as usize);
    }
    buf
}

fn rewrite_shared<T: Copy>(buf: &GpuBuffer, data: &[T]) {
    let bytes = std::mem::size_of_val(data) as u64;
    let ptr = buf.mapped_ptr().expect("fixture buffer must be CPU-mapped");
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), ptr, bytes as usize);
    }
}

fn flat_object(vb: &GpuBuffer, triangle_count: u32) -> RtObjectGeometry<'_> {
    RtObjectGeometry {
        vertex_buffer: vb,
        vertex_stride: VERTEX_STRIDE,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count,
        transform: IDENTITY,
        normal_offset: NORMAL_OFFSET,
        uv_offset: UV_OFFSET,
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
    }
}

fn emissive_material(emissive: [f32; 3]) -> GiMaterial {
    GiMaterial::new([0.8, 0.8, 0.8], emissive, [0.0; 4], [0.0; 4])
}

/// plan → prepare → production encode (`encode_accel_update`, the shared AS
/// update path the emissive preparation hangs off) → commit+wait.
fn prepare_scene(
    device: &GpuDevice,
    tracer: &MetalShadowRayTracer,
    objects: &[RtObjectGeometry<'_>],
    materials: &[GiMaterial],
) -> RtAccel {
    let plan = tracer.plan_accel(device, None, objects).expect("plan accel");
    let mut slot = None;
    tracer.prepare_accel(device, &mut slot, plan).expect("prepare accel");
    let mut accel = slot.expect("prepare produces an accel");
    let changes = vec![RtGeometryChange::Rebuild; objects.len()];
    let mut enc = device.create_encoder("rt-dynamic-shading-prepare");
    tracer
        .encode_accel_update(device, &mut enc, &mut accel, objects, &changes, materials, true, true)
        .expect("encode accel update");
    enc.commit_and_wait_completed();
    accel
}

/// One more frame on the shared update path. `instance_changed` /
/// `emissive_changed` select the §5.1 refresh tier exactly as production's
/// per-frame decisions do.
fn refresh_frame(
    device: &GpuDevice,
    tracer: &MetalShadowRayTracer,
    accel: &mut RtAccel,
    objects: &[RtObjectGeometry<'_>],
    materials: &[GiMaterial],
    instance_changed: bool,
    emissive_changed: bool,
) {
    let changes = vec![RtGeometryChange::Reuse; objects.len()];
    let mut enc = device.create_encoder("rt-dynamic-shading-refresh");
    tracer
        .encode_accel_update(device, &mut enc, accel, objects, &changes, materials, instance_changed, emissive_changed)
        .expect("encode accel update");
    enc.commit_and_wait_completed();
}

fn read_stats(accel: &RtAccel) -> EmissiveTableStats {
    let table = accel.emissive_table.as_ref().expect("table resident since prepare");
    let ptr = table.stats.mapped_ptr().expect("stats is shared");
    unsafe { (ptr as *const EmissiveTableStats).read_unaligned() }
}

fn read_triangles(accel: &RtAccel, n: usize) -> Vec<EmissiveTriangleGpu> {
    let table = accel.emissive_table.as_ref().expect("table resident since prepare");
    let ptr = table.triangles.mapped_ptr().expect("triangles is shared");
    unsafe { std::slice::from_raw_parts(ptr as *const EmissiveTriangleGpu, n).to_vec() }
}

fn read_aliases(accel: &RtAccel, n: usize) -> Vec<EmissiveAliasEntry> {
    let table = accel.emissive_table.as_ref().expect("table resident since prepare");
    let ptr = table.aliases.mapped_ptr().expect("aliases is shared");
    unsafe { std::slice::from_raw_parts(ptr as *const EmissiveAliasEntry, n).to_vec() }
}

// ─── CPU oracle (test-side, from fixture inputs — §5.1's current policy) ─

fn luma(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// CPU Vose alias construction — the same operation order the deleted CPU
/// path used, applied to the oracle's final entry powers.
fn oracle_alias_table(weights: &[f32]) -> Vec<EmissiveAliasEntry> {
    let n = weights.len();
    let total: f32 = weights.iter().sum();
    let inv_total = 1.0 / total;
    let n_f = n as f32;
    let avg = 1.0 / n_f;
    let mut probs: Vec<f32> = weights.iter().map(|w| w * inv_total).collect();
    let mut aliases: Vec<u32> = (0..n as u32).collect();
    let mut small: Vec<usize> = Vec::new();
    let mut large: Vec<usize> = Vec::new();
    for (i, &p) in probs.iter().enumerate() {
        if p < avg { small.push(i); } else { large.push(i); }
    }
    while let (Some(&s), Some(&l)) = (small.last(), large.last()) {
        probs[s] *= n_f;
        aliases[s] = l as u32;
        probs[l] = (probs[l] + probs[s] / n_f) - avg;
        small.pop();
        if probs[l] < avg {
            large.pop();
            small.push(l);
        }
    }
    for &s in &small { probs[s] = 1.0; aliases[s] = s as u32; }
    for &l in &large { probs[l] = 1.0; aliases[l] = l as u32; }
    probs.iter().zip(aliases).map(|(&prob, alias)| EmissiveAliasEntry { prob, alias }).collect()
}

/// A4: derive each entry's true selection probability FROM the alias table
/// (p_i = Σ_j [j==i ? prob_j : (alias_j==i ? 1-prob_j : 0)] / n) and assert
/// the distribution invariants.
fn assert_alias_distribution(aliases: &[EmissiveAliasEntry], context: &str) {
    let n = aliases.len();
    assert!(n > 0, "{context}: nonempty table");
    let mut derived = vec![0.0f32; n];
    for (j, a) in aliases.iter().enumerate() {
        assert!((a.alias as usize) < n, "{context}: alias[{j}] in range");
        assert!((0.0..=1.0).contains(&a.prob), "{context}: prob[{j}] in [0,1]");
        derived[j] += a.prob;
        derived[a.alias as usize] += 1.0 - a.prob;
    }
    let sum: f32 = derived.iter().sum::<f32>() / n as f32;
    assert!((sum - 1.0).abs() <= 2e-4, "{context}: probabilities sum {sum} within 2e-4 of one");
}

/// A4 gate, P4a half. One test function per the acceptance naming contract;
/// `gpu_proofs_gate.py --filter rt_dynamic_shading` selects it.
#[test]
fn rt_dynamic_emissive_gpu_geometry() {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);

    // ── Section 1: static table matches the CPU oracle — 3 emissive
    // candidates with UNEQUAL known powers (ranked order proven), indexed
    // data, a non-emissive object that must not appear, exact stats, exact
    // alias table, and the alias-derived distribution invariants.
    {
        // Object 0: indexed 2-triangle quad, area 2.0 per triangle.
        let quad = [
            vertex([-1.0, -1.0, 0.0], [0.0, 0.0]),
            vertex([1.0, -1.0, 0.0], [1.0, 0.0]),
            vertex([1.0, 1.0, 0.0], [1.0, 1.0]),
            vertex([-1.0, 1.0, 0.0], [0.0, 1.0]),
        ];
        let quad_indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
        let quad_vb = write_shared(device, &quad);
        let quad_ib = write_shared(device, &quad_indices);
        let mut quad_obj = flat_object(&quad_vb, 2);
        quad_obj.index_buffer = Some(&quad_ib);

        // Object 1: non-emissive triangle (rejected by material luma).
        let dark = triangle_at(5.0, 1.0);
        let dark_vb = write_shared(device, &dark);
        let dark_obj = flat_object(&dark_vb, 1);

        // Object 2: smaller single triangle (area 0.125) with a hotter
        // material — its power lands BETWEEN the quad's two... no: quad
        // power is 2.0·luma(white); object 2's is 0.125·luma(hot). Pick
        // hot = [40,0,0] (luma 8.504) → power ≈ 1.063 < 2.0 — ranked last.
        let small = triangle_at(9.0, 0.5);
        let small_vb = write_shared(device, &small);
        let small_obj = flat_object(&small_vb, 1);

        let objects = [quad_obj, dark_obj, small_obj];
        let materials = [
            emissive_material([1.0, 1.0, 1.0]),
            emissive_material([0.0; 3]),
            emissive_material([40.0, 0.0, 0.0]),
        ];
        let accel = prepare_scene(device, &tracer, &objects, &materials);

        let stats = read_stats(&accel);
        let quad_power = 2.0 * luma([1.0, 1.0, 1.0]);
        let small_power = 0.125 * luma([40.0, 0.0, 0.0]);
        let mean = (2.0 * quad_power + small_power) / 3.0;
        assert_eq!(stats.entry_count, 3, "three emissive candidates");
        assert_eq!(stats.entries_are_local, 0, "unwired scene is the fast path");
        assert!((stats.mean_power - mean).abs() <= 2e-4 * mean,
            "mean power {} vs oracle {}", stats.mean_power, mean);
        assert!((stats.total_area - 4.125).abs() <= 1e-4,
            "total world area {} vs oracle 4.125", stats.total_area);

        let tris = read_triangles(&accel, 3);
        // Ranked: quad triangle 0, quad triangle 1 (equal powers, identity
        // tie order), then the small triangle.
        assert_eq!(tris[0].v0, [-1.0, -1.0, 0.0], "rank 0 is quad triangle 0");
        assert_eq!(tris[0].v2, [1.0, 1.0, 0.0]);
        assert_eq!(tris[0].uv1, [1.0, 0.0], "indexed UV fetch");
        assert_eq!(tris[1].v1, [1.0, 1.0, 0.0], "rank 1 is quad triangle 1");
        assert_eq!(tris[1].v2, [-1.0, 1.0, 0.0]);
        assert_eq!(tris[2].object_index, 2, "rank 2 is the small triangle");
        assert!((tris[2].v0[0] - 8.75).abs() <= 1e-4, "small triangle position");
        for t in &tris {
            assert_eq!(t.descriptor_index, t.object_index, "fast path: descriptor == object");
        }

        let oracle = oracle_alias_table(&[quad_power, quad_power, small_power]);
        let gpu_aliases = read_aliases(&accel, 3);
        for i in 0..3 {
            assert!((gpu_aliases[i].prob - oracle[i].prob).abs() <= 1e-6,
                "alias prob[{i}] {} vs oracle {}", gpu_aliases[i].prob, oracle[i].prob);
            assert_eq!(gpu_aliases[i].alias, oracle[i].alias, "alias[{i}]");
        }
        assert_alias_distribution(&gpu_aliases, "section 1");
    }

    // ── Section 2: a deformed emitter refreshes current vertices and UVs —
    // the table NEVER shows the previous frame's geometry ("all sample data
    // must refer to the current frame").
    {
        let verts = triangle_at(0.0, 1.0);
        let vb = write_shared(device, &verts);
        let objects = [flat_object(&vb, 1)];
        let materials = [emissive_material([1.0, 1.0, 1.0])];
        let mut accel = prepare_scene(device, &tracer, &objects, &materials);
        let before = read_triangles(&accel, 1);
        assert_eq!(before[0].v0, [-0.5, -0.5, 0.0], "frame 1 position");

        let mut moved = triangle_at(10.0, 1.0);
        for v in &mut moved {
            v.uv = [v.uv[0] + 3.0, v.uv[1] + 7.0];
        }
        rewrite_shared(&vb, &moved);
        // Geometry changed → the caller reports Rebuild + emissive changed.
        {
            let changes = vec![RtGeometryChange::Rebuild; objects.len()];
            let mut enc = device.create_encoder("rt-dynamic-shading-deform");
            tracer
                .encode_accel_update(device, &mut enc, &mut accel, &objects, &changes, &materials, false, true)
                .expect("encode accel update");
            enc.commit_and_wait_completed();
        }
        let after = read_triangles(&accel, 1);
        assert_eq!(after[0].v0, [9.5, -0.5, 0.0], "current-frame position, not stale");
        assert_eq!(after[0].uv0, [13.0, 7.0], "current-frame UV, not stale");
        let stats = read_stats(&accel);
        assert_eq!(stats.entry_count, 1, "deform keeps the entry live");
    }

    // ── Section 3: emission zero → positive, positive → zero, and
    // collapse → revival — all WITHOUT topology edits (the candidate
    // workspace was sized over ALL objects at preparation).
    {
        let verts = triangle_at(0.0, 1.0);
        let vb = write_shared(device, &verts);
        let objects = [flat_object(&vb, 1)];
        let dark = [emissive_material([0.0; 3])];
        let mut accel = prepare_scene(device, &tracer, &objects, &dark);
        let stats = read_stats(&accel);
        assert_eq!(stats.entry_count, 0, "non-emissive scene has valid zero stats");
        assert_eq!(stats.mean_power, 0.0);
        assert_eq!(stats.total_area, 0.0);

        // zero → positive
        let lit = [emissive_material([4.0, 0.0, 0.0])];
        refresh_frame(device, &tracer, &mut accel, &objects, &lit, false, true);
        let stats = read_stats(&accel);
        assert_eq!(stats.entry_count, 1, "emission appears without topology edits");
        let expect_power = 0.5 * luma([4.0, 0.0, 0.0]);
        assert!((stats.mean_power - expect_power).abs() <= 2e-4 * expect_power,
            "mean power {} vs oracle {}", stats.mean_power, expect_power);

        // positive → zero: stats return to valid zeros (the zero-emission
        // refresh still writes the buffer).
        refresh_frame(device, &tracer, &mut accel, &objects, &dark, false, true);
        let stats = read_stats(&accel);
        assert_eq!(stats.entry_count, 0, "positive→zero returns valid zero stats");
        assert_eq!(stats.mean_power, 0.0);
        assert_eq!(stats.total_area, 0.0);

        // collapse → revival: degenerate (zero-area) geometry rejects all
        // candidates even with a lit material; restoring revives the entry.
        let collapsed = [vertex([0.0, 0.0, 0.0], [0.0; 2]); 3];
        rewrite_shared(&vb, &collapsed);
        {
            let changes = vec![RtGeometryChange::Rebuild; objects.len()];
            let mut enc = device.create_encoder("rt-dynamic-shading-collapse");
            tracer
                .encode_accel_update(device, &mut enc, &mut accel, &objects, &changes, &lit, false, true)
                .expect("encode accel update");
            enc.commit_and_wait_completed();
        }
        let stats = read_stats(&accel);
        assert_eq!(stats.entry_count, 0, "zero-area candidates are rejected");
        rewrite_shared(&vb, &triangle_at(0.0, 1.0));
        {
            let changes = vec![RtGeometryChange::Rebuild; objects.len()];
            let mut enc = device.create_encoder("rt-dynamic-shading-revival");
            tracer
                .encode_accel_update(device, &mut enc, &mut accel, &objects, &changes, &lit, false, true)
                .expect("encode accel update");
            enc.commit_and_wait_completed();
        }
        let stats = read_stats(&accel);
        assert_eq!(stats.entry_count, 1, "revival restores the entry");
    }

    // ── Section 4: 4097 candidates with UNEQUAL known powers plus an
    // equal-power tie at the truncation boundary — the selected set is the
    // exact top-4096, ties in identity order (the ONLY intentional
    // selection-order change vs the deleted CPU path).
    {
        let n_tris = MAX_RT_EMISSIVE_TRIANGLES as usize + 1;
        let mut verts = Vec::with_capacity(n_tris * 3);
        // Triangle i has leg length scaled so power is strictly increasing
        // with i EXCEPT the two weakest, which tie — the identity order
        // (triangle 0) must win the 4096th slot... not quite: selection is
        // top-4096 of 4097, so exactly ONE candidate is dropped. Make
        // triangles 0 and 1 tie for WEAKEST: identity tie order keeps
        // triangle 0 (ascending) and drops triangle 1.
        verts.extend_from_slice(&triangle_at(0.0, 1.0)); // tri 0: area 0.5
        verts.extend_from_slice(&triangle_at(4.0, 1.0)); // tri 1: area 0.5 (tie, dropped)
        for i in 2..n_tris {
            // Strictly increasing areas from tri 2 onward.
            let leg = 1.0 + i as f32 * 0.001;
            verts.extend_from_slice(&triangle_at(i as f32 * 4.0, leg));
        }
        let vb = write_shared(device, &verts);
        let objects = [flat_object(&vb, n_tris as u32)];
        let materials = [emissive_material([1.0, 1.0, 1.0])];
        let accel = prepare_scene(device, &tracer, &objects, &materials);
        let stats = read_stats(&accel);
        assert_eq!(stats.entry_count, MAX_RT_EMISSIVE_TRIANGLES, "cap truncates at 4096");
        let tris = read_triangles(&accel, MAX_RT_EMISSIVE_TRIANGLES as usize);
        // Highest power LAST tri; weakest kept is tri 0 (identity tie order
        // over tri 1). The first entry is the strongest triangle.
        let top = &tris[0];
        let strongest = (n_tris - 1) as f32 * 4.0;
        assert!((top.v0[0] - (strongest - (1.0 + (n_tris - 1) as f32 * 0.001) / 2.0)).abs() <= 1e-2,
            "rank 0 is the strongest triangle, got v0.x={}", top.v0[0]);
        // The weakest kept entry (last) is triangle 0, not triangle 1.
        let last = &tris[MAX_RT_EMISSIVE_TRIANGLES as usize - 1];
        assert!((last.v0[0] - (-0.5)).abs() <= 1e-3,
            "weakest kept is triangle 0 (identity tie order), got v0.x={}", last.v0[0]);
        // Triangle 1 (v0.x = 3.5) is the dropped one: no entry at 3.5.
        assert!(tris.iter().all(|t| (t.v0[0] - 3.5).abs() > 1e-3),
            "triangle 1 lost the tie and was dropped");
        let aliases = read_aliases(&accel, MAX_RT_EMISSIVE_TRIANGLES as usize);
        assert_alias_distribution(&aliases, "section 4");
    }

    // ── Section 5: instanced mode — entries stay LOCAL with per-slot
    // descriptor indices, stats carry the local aggregate (D8). A dead
    // slot still gets an entry (sample-time exclusion is kernel-side,
    // covered by rt_emissive_instancing).
    {
        let slots = [
            InstanceTransform { pos_scale: [0.0, 0.0, 0.0, 1.0], rot_pad: [0.0; 4] },
            InstanceTransform { pos_scale: [3.0, 0.0, 0.0, 1.0], rot_pad: [0.0; 4] },
            // D2/INV-RTI1: zero pos_scale.w = dead slot.
            InstanceTransform { pos_scale: [6.0, 0.0, 0.0, 0.0], rot_pad: [0.0; 4] },
        ];
        let slots_buf = write_shared(device, &slots);
        let verts = triangle_at(0.0, 1.0);
        let vb = write_shared(device, &verts);
        let mut obj = flat_object(&vb, 1);
        obj.instances_addr = slots_buf.gpu_address();
        obj.instances_buffer = Some(&slots_buf);
        obj.instance_slots = 3;
        let objects = [obj];
        let materials = [emissive_material([1.0, 1.0, 1.0])];
        let accel = prepare_scene(device, &tracer, &objects, &materials);
        let stats = read_stats(&accel);
        assert_eq!(stats.entries_are_local, 1, "instanced mode marks local entries");
        assert_eq!(stats.entry_count, 3, "one entry per (triangle, slot)");
        let tris = read_triangles(&accel, 3);
        for (i, t) in tris.iter().enumerate() {
            assert_eq!(t.v0, [-0.5, -0.5, 0.0], "entry {i} stays LOCAL-space");
            assert_eq!(t.descriptor_index, i as u32, "entry {i} names slot {i}");
        }
        assert!((stats.total_area - 1.5).abs() <= 1e-4,
            "instanced stats carry the LOCAL aggregate {} vs 1.5", stats.total_area);
    }

    // ── Section 6: fast-path transform updates recompose world positions
    // and areas through GatherOnly — scale (2,1,0.5) doubles the z=0
    // triangle's area; mirroring keeps area but flips x. Descriptor 0.
    {
        let verts = triangle_at(0.0, 1.0);
        let vb = write_shared(device, &verts);
        let mut obj = flat_object(&vb, 1);
        let scale: [[f32; 4]; 4] = [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 0.5, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        obj.transform = scale;
        let objects = [obj];
        let materials = [emissive_material([1.0, 1.0, 1.0])];
        let mut accel = prepare_scene(device, &tracer, &objects, &materials);
        let tris = read_triangles(&accel, 1);
        assert!((tris[0].v0[0] - (-1.0)).abs() <= 1e-4, "frame-1 world x scaled");
        let stats = read_stats(&accel);
        assert!((stats.total_area - 1.0).abs() <= 1e-4,
            "scaled world area {} vs 1.0", stats.total_area);
        // Mean power is LOCAL (area × luma = 0.5), unchanged by transform.
        assert!((stats.mean_power - 0.5).abs() <= 1e-4,
            "mean power is local {} vs 0.5", stats.mean_power);

        // Transform-only change → GatherOnly (no rebuild): mirror in x.
        let mut obj2 = flat_object(&vb, 1);
        obj2.transform = [
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let objects2 = [obj2];
        refresh_frame(device, &tracer, &mut accel, &objects2, &materials, true, false);
        let tris = read_triangles(&accel, 1);
        assert!((tris[0].v0[0] - 0.5).abs() <= 1e-4,
            "GatherOnly recomposed mirrored world position, got {}", tris[0].v0[0]);
        let stats = read_stats(&accel);
        assert!((stats.total_area - 0.5).abs() <= 1e-4,
            "mirrored world area {} vs 0.5", stats.total_area);
        assert_eq!(tris[0].descriptor_index, 0, "fast path descriptor");
    }

    // ── Section 7: the firefly clamp consumes the GPU stats mean on frame
    // one — a bright center clamps at the fixed floor with a zero mean and
    // passes unclamped when the stats carry a large mean power.
    {
        let mut color = [[0.01f32; 4]; 9];
        color[4] = [100.0, 100.0, 100.0, 1.0];
        let depth = [0.5f32; 9];
        // Zero mean: threshold = 8 * max(median≈0.01, max(4.0, 0)) = 32.
        let clamped = tracer.debug_firefly_clamp(device, &color, &depth, 8.0, 4.0, 0.0);
        assert!((clamped[0] - 32.0).abs() <= 0.5,
            "zero-mean clamps to gain*floor, got {}", clamped[0]);
        // Mean 100: threshold = 8 * max(0.01, 100) = 800 → unclamped.
        let passed = tracer.debug_firefly_clamp(device, &color, &depth, 8.0, 4.0, 100.0);
        assert!((passed[0] - 100.0).abs() <= 0.5,
            "GPU-stats mean lifts the floor, got {}", passed[0]);
    }
}

// ─── P4b: appearance coverage and indexed hit attributes (A4) ────────────

use manifold_gpu::raytrace::{
    DebugRayQueryHit, DebugRayQueryRay, RtCasterParams, ShadowRayParams,
};
use manifold_gpu::{GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

/// CPU mirror of the raster appearance helper (`apply_appearance`,
/// render_scene.wgsl:889): `level = gain * weight`,
/// `coverage = clamp(level, 0, 1)`, `brightness = max(level, 1)` — the
/// oracle the GPU fields are compared against (≤ 1e-6).
fn raster_appearance(gain: f32, weight: f32) -> (f32, f32) {
    let level = gain * weight;
    (level.clamp(0.0, 1.0), level.max(1.0))
}

/// Barycentric weight interpolation — the CPU oracle for the kernel's
/// corner fetch + barycentric mix.
fn weight_at(weights: [f32; 3], bary: [f32; 2]) -> f32 {
    weights[0] * (1.0 - bary[0] - bary[1]) + weights[1] * bary[0] + weights[2] * bary[1]
}

/// A ray from z=+2 straight down at `triangle_at(0.0, 1.0)`'s point with
/// barycentrics `(u, v)`. Vertex layout: v0 = (-0.5,-0.5), v1 = (0.5,-0.5),
/// v2 = (0, 0.5), so p = v0 + u·(1,0) + v·(0.5,1).
fn bary_ray(bary: [f32; 2]) -> DebugRayQueryRay {
    let px = -0.5 + bary[0] + 0.5 * bary[1];
    let py = -0.5 + bary[1];
    DebugRayQueryRay {
        origin: [px, py, 2.0],
        direction: [0.0, 0.0, -1.0],
        min_distance: 0.0,
        max_distance: 10.0,
    }
}

/// plan → prepare → encode (rebuild) → commit+wait, plus the per-frame
/// normal-source table. Returns the accel, the table slot, and the
/// material texture list (empty for texture-less fixtures).
fn prepare_query_scene<'a>(
    device: &GpuDevice,
    tracer: &MetalShadowRayTracer,
    objects: &[RtObjectGeometry<'a>],
) -> (RtAccel, Option<GpuBuffer>, Vec<&'a GpuTexture>) {
    let plan = tracer.plan_accel(device, None, objects).expect("plan accel");
    let mut slot = None;
    tracer.prepare_accel(device, &mut slot, plan).expect("prepare accel");
    let mut accel = slot.expect("prepare produces an accel");
    let mut ns_slot = None;
    let mut ns_capacity = 0usize;
    let textures = manifold_gpu::raytrace::ensure_normal_sources(
        &mut ns_slot, &mut ns_capacity, device, objects,
    );
    let materials =
        vec![GiMaterial::new([0.8, 0.8, 0.8], [0.0; 3], [0.0; 4], [0.0; 4]); objects.len()];
    let changes = vec![RtGeometryChange::Rebuild; objects.len()];
    let mut enc = device.create_encoder("rt-p4b-query-prepare");
    tracer
        .encode_accel_update(device, &mut enc, &mut accel, objects, &changes, &materials, true, true)
        .expect("encode accel update");
    enc.commit_and_wait_completed();
    (accel, ns_slot, textures)
}

/// Re-write the normal-source table after an appearance change (gain or
/// weights buffer content/identity — the property class stays nonopaque,
//  so the resident BLAS is untouched, per the fractional-to-fractional
/// source-table-only rule). Production does this every RT-ready frame.
fn refresh_normal_sources(
    device: &GpuDevice,
    slot: &mut Option<GpuBuffer>,
    objects: &[RtObjectGeometry<'_>],
) {
    let mut capacity = 0usize;
    manifold_gpu::raytrace::ensure_normal_sources(slot, &mut capacity, device, objects);
}

fn run_ray_query(
    device: &GpuDevice,
    tracer: &MetalShadowRayTracer,
    accel: &RtAccel,
    normal_sources: &GpuBuffer,
    rays: &[DebugRayQueryRay],
    material_textures: Option<&[&GpuTexture]>,
    seed_base: u32,
) -> Vec<DebugRayQueryHit> {
    let mut enc = device.create_encoder("rt-p4b-ray-query");
    let hits_buf = tracer.debug_ray_query(
        device, &mut enc, accel, normal_sources, rays, material_textures, 0, seed_base,
    );
    enc.commit_and_wait_completed();
    let ptr = hits_buf.mapped_ptr().expect("hit buffer must be CPU-mapped");
    unsafe {
        std::slice::from_raw_parts(ptr as *const DebugRayQueryHit, rays.len()).to_vec()
    }
}

/// A4 `rt_dynamic_coverage_and_attributes` (P4b). Weights [0, 0.5, 1] ×
/// gains [0, 0.5, 1, 2] at specified barycentrics: the coverage/brightness
/// fields of accepted hits match the raster formula mirror within 1e-6;
/// coverage 0 always misses, 1 always accepts; fractional cases run 65,536
/// fixed-seed samples with acceptance-frequency error ≤ 0.01. All walkers
/// share the one helper (the debug query exercises the production
/// closest-hit walk; the machine-checked walker/source contracts live in
/// manifold-gpu's `p4b_appearance_source_contracts`).
#[test]
fn rt_dynamic_coverage_and_attributes() {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);

    let weights_buf = write_shared(device, &[0.0f32, 0.5, 1.0]);

    // ── Section 1: formula match and acceptance distribution. One flat
    // triangle, weights [0, 0.5, 1], one accel for all gains (weights stay
    // wired, so the nonopaque property never flips and the BLAS stands).
    let verts = triangle_at(0.0, 1.0);
    let vb = write_shared(device, &verts);
    let centroid = [1.0f32 / 3.0, 1.0f32 / 3.0]; // weight 0.5
    let off_a = [0.1f32, 0.1];   // weight 0.15
    let off_b = [0.45f32, 0.45]; // weight 0.675
    let off_c = [0.1f32, 0.7];   // weight 0.75
    for bary in [centroid, off_a, off_b, off_c] {
        let w0 = 1.0 - bary[0] - bary[1];
        assert!(w0 >= 0.05 && bary[0] >= 0.05 && bary[1] >= 0.05,
            "bary {bary:?} clears A0's 0.05 edge exclusion");
    }
    let objects = [RtObjectGeometry {
        appearance_weights: Some(&weights_buf),
        appearance_gain: 1.0,
        ..flat_object(&vb, 1)
    }];
    let (accel, mut ns_slot, _) = prepare_query_scene(device, &tracer, &objects);
    let mut report_lines: Vec<String> = Vec::new();

    let mut run_case = |gain: f32, bary: [f32; 2], n: usize, seed_base: u32| {
        let objects = [RtObjectGeometry {
            appearance_weights: Some(&weights_buf),
            appearance_gain: gain,
            ..flat_object(&vb, 1)
        }];
        refresh_normal_sources(device, &mut ns_slot, &objects);
        let ns = ns_slot.as_ref().expect("normal sources resident");
        let rays = vec![bary_ray(bary); n];
        let hits = run_ray_query(device, &tracer, &accel, ns, &rays, None, seed_base);
        let accepted = hits.iter().filter(|h| h.hit == 1).count();
        (accepted, hits)
    };

    // Deterministic cases (64 samples each — no distribution needed):
    // coverage 0 always misses; coverage 1 always accepts.
    let (accepted, _) = run_case(0.0, centroid, 64, 11);
    assert_eq!(accepted, 0, "gain 0 (level 0, coverage 0) must always miss");
    let (accepted, hits) = run_case(2.0, centroid, 64, 12);
    assert_eq!(accepted, 64, "gain 2 at weight 0.5 (level 1, coverage 1) must always accept");
    for (i, hit) in hits.iter().enumerate() {
        let (cov, bri) = raster_appearance(2.0, weight_at([0.0, 0.5, 1.0], centroid));
        assert!((hit.coverage - cov).abs() <= 1e-6,
            "sample {i}: coverage {} vs raster helper {cov}", hit.coverage);
        assert!((hit.brightness - bri).abs() <= 1e-6,
            "sample {i}: brightness {} vs raster helper {bri}", hit.brightness);
    }
    // HDR brightness: level 1.5 → coverage 1 (always accept), brightness 1.5.
    let (accepted, hits) = run_case(2.0, off_c, 64, 13);
    assert_eq!(accepted, 64, "level 1.5 (coverage clamped to 1) must always accept");
    for (i, hit) in hits.iter().enumerate() {
        let (cov, bri) = raster_appearance(2.0, weight_at([0.0, 0.5, 1.0], off_c));
        assert_eq!((cov, bri), (1.0, 1.5), "raster mirror sanity");
        assert!((hit.coverage - cov).abs() <= 1e-6, "sample {i}: coverage {}", hit.coverage);
        assert!((hit.brightness - bri).abs() <= 1e-6,
            "sample {i}: brightness {} vs 1.5 (HDR gain reaches RT once)", hit.brightness);
    }

    // Fractional cases: 65,536 fixed-seed samples, frequency error ≤ 0.01.
    // Every accepted hit's coverage/brightness fields match the raster
    // mirror within 1e-6.
    const SAMPLES: usize = 65_536;
    let fractional = [
        (0.5f32, centroid, 0.25f32),
        (1.0, centroid, 0.5),
        (2.0, off_a, 0.3),
        (1.0, off_b, 0.675),
    ];
    for (case, &(gain, bary, expect)) in fractional.iter().enumerate() {
        let weight = weight_at([0.0, 0.5, 1.0], bary);
        let (cov, bri) = raster_appearance(gain, weight);
        assert!((cov - expect).abs() <= 1e-6, "oracle sanity: gain {gain} bary {bary:?}");
        let seed_base = 1000 + case as u32;
        let (accepted, hits) = run_case(gain, bary, SAMPLES, seed_base);
        let freq = accepted as f32 / SAMPLES as f32;
        assert!((freq - cov).abs() <= 0.01,
            "gain {gain} bary {bary:?}: acceptance frequency {freq} vs coverage {cov} (seed {seed_base})");
        for (i, hit) in hits.iter().enumerate() {
            if hit.hit == 0 { continue; }
            assert!((hit.coverage - cov).abs() <= 1e-6,
                "sample {i}: coverage {} vs raster helper {cov}", hit.coverage);
            assert!((hit.brightness - bri).abs() <= 1e-6,
                "sample {i}: brightness {} vs raster helper {bri}", hit.brightness);
            assert!(hit.distance.is_finite() && hit.coverage.is_finite() && hit.brightness.is_finite(),
                "sample {i}: nonfinite channel");
        }
        report_lines.push(format!(
            "fractional gain={gain} bary={bary:?} weight={weight:.4} coverage={cov:.4} brightness={bri:.4} samples={SAMPLES} accepted={accepted} freq={freq:.5} seed_base={seed_base}"
        ));
    }

    // ── Section 2: unwired + gain 1 is the pre-P4b behavior — every ray
    // hits, coverage/brightness read 1.0.
    {
        let objects = [flat_object(&vb, 1)];
        let (accel, ns, _) = prepare_query_scene(device, &tracer, &objects);
        let rays = vec![bary_ray(centroid); 64];
        let hits = run_ray_query(device, &tracer, &accel, ns.as_ref().expect("normal sources"), &rays, None, 77);
        for (i, hit) in hits.iter().enumerate() {
            assert_eq!(hit.hit, 1, "sample {i}: plain object accepts");
            assert_eq!((hit.coverage, hit.brightness), (1.0, 1.0),
                "sample {i}: no appearance wired — coverage/brightness 1.0");
        }
    }

    // ── Section 3: the material alpha-mask cutoff test stays first.
    // 3×1 texture, alpha [0.49, 0.5, 0.6] against cutoff 0.5 (just below /
    // equal / just above), appearance wired at coverage 1 (weights all 1,
    // gain 1) so alpha alone decides; then gain 0 proves the appearance
    // rejection still applies when alpha passes.
    {
        let mut alpha_verts = triangle_at(0.0, 1.0);
        // UVs: the triangle maps interior points across the 3 texels
        // (nearest sampling; uv.y irrelevant for a 1-row texture).
        alpha_verts[0].uv = [0.05, 0.5];
        alpha_verts[1].uv = [0.95, 0.5];
        alpha_verts[2].uv = [0.5, 0.5];
        let avb = write_shared(device, &alpha_verts);
        // texel alphas 0.49 / 0.5 / 0.6 (rgb unused = 1).
        let tex_px: [f32; 12] = [
            1.0, 1.0, 1.0, 0.49,
            1.0, 1.0, 1.0, 0.5,
            1.0, 1.0, 1.0, 0.6,
        ];
        let alpha_tex = device.create_texture(&GpuTextureDesc {
            width: 3,
            height: 1,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "rt-p4b-alpha",
            mip_levels: 1,
        });
        device.upload_texture(&alpha_tex, unsafe {
            std::slice::from_raw_parts(tex_px.as_ptr().cast::<u8>(), std::mem::size_of_val(&tex_px))
        });
        let ones_buf = write_shared(device, &[1.0f32, 1.0, 1.0]);
        let mk_objects = |gain: f32| [RtObjectGeometry {
            appearance_weights: Some(&ones_buf),
            appearance_gain: gain,
            alpha_mask: true,
            alpha_cutoff: 0.5,
            base_color_texture: Some(&alpha_tex),
            ..flat_object(&avb, 1)
        }];
        let objects = mk_objects(1.0);
        let (accel, ns, textures) = prepare_query_scene(device, &tracer, &objects);
        assert_eq!(textures.len(), 1, "the alpha texture is bound at index 0");
        // barys landing in texels 0 / 1 / 2 (uv.x 0.185 / 0.5 / 0.815).
        let rays = [
            bary_ray([0.1, 0.1]), // uv.x = 0.05·0.8 + 0.95·0.1 + 0.5·0.1 = 0.185 → texel 0 (below)
            bary_ray([0.2, 0.6]), // uv.x = 0.05·0.2 + 0.95·0.2 + 0.5·0.6 = 0.5  → texel 1 (equal)
            bary_ray([0.8, 0.1]), // uv.x = 0.05·0.1 + 0.95·0.8 + 0.5·0.1 = 0.815 → texel 2 (above)
        ];
        let hits = run_ray_query(device, &tracer, &accel, ns.as_ref().expect("normal sources"), &rays, Some(&textures), 78);
        assert_eq!(hits[0].hit, 0, "alpha 0.49 < cutoff 0.5 rejects even at coverage 1");
        assert_eq!(hits[1].hit, 1, "alpha 0.5 == cutoff 0.5 accepts (>=)");
        assert_eq!(hits[2].hit, 1, "alpha 0.6 > cutoff 0.5 accepts");
        for (i, hit) in hits.iter().enumerate().skip(1) {
            assert_eq!((hit.coverage, hit.brightness), (1.0, 1.0),
                "sample {i}: weights [1,1,1] gain 1 — full coverage");
        }
        // Appearance reject after alpha accept: gain 0 misses everywhere.
        let objects0 = mk_objects(0.0);
        let mut ns0 = None;
        refresh_normal_sources(device, &mut ns0, &objects0);
        let hits = run_ray_query(device, &tracer, &accel, ns0.as_ref().unwrap(), &rays, Some(&textures), 79);
        assert!(hits.iter().all(|h| h.hit == 0),
            "gain 0 (coverage 0) rejects even above the alpha cutoff");
    }

    // ── Section 4: indexed UV/normal — the shared index helper resolves
    // corners for the trace path's attribute fetches. The quad's first
    // triangle is indexed (3, 0, 1): flat-layout corner math (0, 1, 2)
    // would interpolate a DIFFERENT normal/UV set, so a match to the
    // indexed oracle proves the index path.
    {
        let quad = [
            PackedVertex { pos: [-1.0, -1.0, 0.0, 0.0], normal: [1.0, 0.0, 0.0, 0.0], uv: [0.0, 0.0] },
            PackedVertex { pos: [1.0, -1.0, 0.0, 0.0], normal: [0.0, 1.0, 0.0, 0.0], uv: [1.0, 0.0] },
            PackedVertex { pos: [1.0, 1.0, 0.0, 0.0], normal: [0.0, 0.0, 1.0, 0.0], uv: [1.0, 1.0] },
            PackedVertex { pos: [-1.0, 1.0, 0.0, 0.0], normal: [std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2, 0.0, 0.0], uv: [0.0, 1.0] },
        ];
        // Tri 0 = (3,0,1) — centroid (-1/3,-1/3), away from the diagonal.
        let indices: [u32; 6] = [3, 0, 1, 1, 3, 2];
        let qvb = write_shared(device, &quad);
        let qib = write_shared(device, &indices);
        let mut obj = flat_object(&qvb, 2);
        obj.index_buffer = Some(&qib);
        let objects = [obj];
        let (accel, ns, _) = prepare_query_scene(device, &tracer, &objects);
        let ray = DebugRayQueryRay {
            origin: [-1.0 / 3.0, -1.0 / 3.0, 2.0],
            direction: [0.0, 0.0, -1.0],
            min_distance: 0.0,
            max_distance: 10.0,
        };
        let hits = run_ray_query(device, &tracer, &accel, ns.as_ref().expect("normal sources"), &[ray], None, 80);
        let hit = hits[0];
        assert_eq!(hit.hit, 1, "indexed triangle must hit");
        assert_eq!(hit.primitive_id, 0, "the committed triangle is primitive 0");
        // Indexed oracle: corners (3,0,1), bary (1/3,1/3) →
        // n = (n3+n0+n1)/3, uv = (uv3+uv0+uv1)/3 = ((0,1)+(0,0)+(1,0))/3.
        let s2 = std::f32::consts::FRAC_1_SQRT_2;
        let exp_n = [ (s2 + 1.0 + 0.0) / 3.0, (s2 + 0.0 + 1.0) / 3.0, (0.0 + 0.0 + 0.0) / 3.0 ];
        let len = (exp_n[0] * exp_n[0] + exp_n[1] * exp_n[1]).sqrt();
        let exp_n = [exp_n[0] / len, exp_n[1] / len, 0.0];
        let exp_uv = [1.0f32 / 3.0, 1.0 / 3.0];
        for (got, want, name) in [
            (hit.normal[0], exp_n[0], "normal.x"), (hit.normal[1], exp_n[1], "normal.y"),
            (hit.normal[2], exp_n[2], "normal.z"),
            (hit.uv[0], exp_uv[0], "uv.x"), (hit.uv[1], exp_uv[1], "uv.y"),
        ] {
            assert!((got - want).abs() <= 2e-4, "indexed {name}: {got} vs oracle {want}");
        }
        // The flat-layout answer (corners (0,1,2)) is measurably different
        // — the index path is what produced the match above.
        let flat_uv_x = 2.0f32 / 3.0; // ((0,0)+(1,0)+(1,1))/3
        assert!((hit.uv[0] - flat_uv_x).abs() > 0.1,
            "flat corner math would give uv.x {flat_uv_x} — the indexed read is distinct");
        assert!((hit.distance - 2.0).abs() <= 1e-4, "distance {}", hit.distance);
        assert!((hit.bary[0] - 1.0 / 3.0).abs() <= 2e-4 && (hit.bary[1] - 1.0 / 3.0).abs() <= 2e-4,
            "bary {:?}", hit.bary);
        report_lines.push(format!(
            "indexed tri (3,0,1): normal={:?} (oracle {exp_n:?}) uv={:?} (oracle {exp_uv:?}) — flat would give uv.x={:.4}",
            hit.normal, hit.uv, flat_uv_x
        ));
    }

    // ── Section 5: structured geometry error + nonfinite input. A wired
    // weights buffer shorter than the mesh vertex count is
    // InvalidGeometry at plan; a NaN weight rejects every sample (never an
    // unchecked acceptance).
    {
        // write_shared rounds up to 16 bytes (4 floats) — a genuinely short
        // weights buffer needs a mesh with more than 4 vertices.
        let two_tris = [triangle_at(0.0, 1.0), triangle_at(4.0, 1.0)].concat();
        let vb2 = write_shared(device, &two_tris);
        let short_weights = write_shared(device, &[1.0f32, 1.0]); // 4 floats, mesh has 6 vertices
        let bad = [RtObjectGeometry {
            appearance_weights: Some(&short_weights),
            ..flat_object(&vb2, 2)
        }];
        match tracer.plan_accel(device, None, &bad) {
            Err(manifold_gpu::raytrace::RtAccelError::InvalidGeometry { object, reason }) => {
                assert_eq!(object, 0);
                assert!(reason.contains("appearance weights"), "reason: {reason}");
            }
            other => panic!("a short weights buffer must fail plan_accel with InvalidGeometry, got {:?}", other.map(|_| ())),
        }
        let nan_weights = write_shared(device, &[f32::NAN, 0.5, 1.0]);
        let nan_objects = [RtObjectGeometry {
            appearance_weights: Some(&nan_weights),
            appearance_gain: 1.0,
            ..flat_object(&vb, 1)
        }];
        let (nan_accel, nan_ns, _) = prepare_query_scene(device, &tracer, &nan_objects);
        let rays = vec![bary_ray(centroid); 64];
        let hits = run_ray_query(device, &tracer, &nan_accel, nan_ns.as_ref().expect("normal sources"), &rays, None, 81);
        assert!(hits.iter().all(|h| h.hit == 0),
            "a nonfinite weight must reject every sample, never accept garbage");
    }

    // ── Section 6: current emitter appearance — the light table's baked
    // corner weights track the weights buffer, and a weights-content change
    // refreshes through the Reuse + emissive-changed tier (no BLAS work).
    {
        let em_weights = write_shared(device, &[0.25f32, 0.5, 0.75]);
        let evb = write_shared(device, &triangle_at(0.0, 1.0));
        let objects = [RtObjectGeometry {
            appearance_weights: Some(&em_weights),
            appearance_gain: 1.0,
            ..flat_object(&evb, 1)
        }];
        let materials = [emissive_material([1.0, 1.0, 1.0])];
        let mut accel = prepare_scene(device, &tracer, &objects, &materials);
        let tris = read_triangles(&accel, 1);
        assert_eq!((tris[0].w0, tris[0].w1, tris[0].w2), (0.25, 0.5, 0.75),
            "gather bakes the corner weights");
        rewrite_shared(&em_weights, &[0.5f32, 0.75, 1.0]);
        let update_counts_before = accel.emissive_table.is_some();
        assert!(update_counts_before);
        // Appearance-only refresh: Reuse changes, emissive_data_changed.
        refresh_frame(device, &tracer, &mut accel, &objects, &materials, false, true);
        let tris = read_triangles(&accel, 1);
        assert_eq!((tris[0].w0, tris[0].w1, tris[0].w2), (0.5, 0.75, 1.0),
            "weights refresh reaches the baked corners — no stale emitter appearance");
        report_lines.push("emissive corner weights: [0.25,0.5,0.75] -> refresh -> [0.5,0.75,1.0] (Reuse tier, no BLAS work)".to_string());
    }

    // ── Sections 7-9: full-dispatch radiance multipliers. Floor (the
    // shaded receiver at y=0, identity inv_view_proj maps texels to world
    // (x, 0, 0.3)) + ceiling quad at y=1. Camera (0, 1.8, 2) clears the
    // ceiling on the primary ray. All RNG streams are gain-independent, so
    // same-seed runs differ ONLY by the appearance factor.
    let floor_verts = [
        vertex([-2.0, 0.0, -2.0], [0.0, 0.0]),
        vertex([2.0, 0.0, -2.0], [1.0, 0.0]),
        vertex([2.0, 0.0, 2.0], [1.0, 1.0]),
        vertex([-2.0, 0.0, -2.0], [0.0, 0.0]),
        vertex([2.0, 0.0, 2.0], [1.0, 1.0]),
        vertex([-2.0, 0.0, 2.0], [0.0, 1.0]),
    ]
    .map(|v| PackedVertex { normal: [0.0, 1.0, 0.0, 0.0], ..v });
    let ceil_verts = [
        vertex([-1.0, 1.0, -1.0], [0.0, 0.0]),
        vertex([1.0, 1.0, -1.0], [1.0, 0.0]),
        vertex([1.0, 1.0, 1.0], [1.0, 1.0]),
        vertex([-1.0, 1.0, -1.0], [0.0, 0.0]),
        vertex([1.0, 1.0, 1.0], [1.0, 1.0]),
        vertex([-1.0, 1.0, 1.0], [0.0, 1.0]),
    ]
    // Vertex normals UP (data, not geometry): the sun-bounce term needs
    // dot(hit_n, sun_dir) = 1 on the ceiling's underside hits.
    .map(|v| PackedVertex { normal: [0.0, 1.0, 0.0, 0.0], ..v });
    let floor_vb = write_shared(device, &floor_verts);
    let ceil_vb = write_shared(device, &ceil_verts);

    #[allow(clippy::too_many_arguments)]
    fn run_full_trace(
        device: &GpuDevice,
        tracer: &MetalShadowRayTracer,
        floor_vb: &GpuBuffer,
        ceil_vb: &GpuBuffer,
        ceil_weights: Option<&GpuBuffer>,
        ceil_gain: f32,
        ceil_emissive: [f32; 3],
        sun: bool,
        width: u32,
        gi_spp: u32,
        refl_spp: u32,
        label: &str,
    ) -> (Vec<f32>, Vec<f32>) {
        let objects = [
            flat_object(floor_vb, 2),
            RtObjectGeometry {
                appearance_weights: ceil_weights,
                appearance_gain: ceil_gain,
                ..flat_object(ceil_vb, 2)
            },
        ];
        let materials = [
            GiMaterial::new([0.8, 0.8, 0.8], [0.0; 3], [0.0; 4], [0.0; 4]),
            GiMaterial::new([0.8, 0.8, 0.8], ceil_emissive, [0.0; 4], [0.0; 4]),
        ];
        let plan = tracer.plan_accel(device, None, &objects).expect("plan accel");
        let mut slot = None;
        tracer.prepare_accel(device, &mut slot, plan).expect("prepare accel");
        let mut accel = slot.expect("prepare produces an accel");
        let mut ns_slot = None;
        let mut ns_cap = 0usize;
        let textures = manifold_gpu::raytrace::ensure_normal_sources(
            &mut ns_slot, &mut ns_cap, device, &objects,
        );
        assert!(textures.is_empty());
        let normal_sources = ns_slot.expect("normal sources");

        let depth_px = vec![0.3f32; width as usize];
        let depth_tex = device.create_texture(&GpuTextureDesc {
            width, height: 1, depth: 1,
            format: GpuTextureFormat::Depth32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "rt-p4b-depth",
            mip_levels: 1,
        });
        device.upload_texture(&depth_tex, unsafe {
            std::slice::from_raw_parts(depth_px.as_ptr().cast::<u8>(), std::mem::size_of_val(&depth_px[..]))
        });
        let mk_out = |format: GpuTextureFormat, name: &str| {
            device.create_texture(&GpuTextureDesc {
                width, height: 1, depth: 1, format,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
                label: name,
                mip_levels: 1,
            })
        };
        let out_sv = mk_out(GpuTextureFormat::Rgba16Float, "rt-p4b-sv");
        let out_sv2 = mk_out(GpuTextureFormat::Rgba16Float, "rt-p4b-sv2");
        let out_svt = mk_out(GpuTextureFormat::Rgba16Float, "rt-p4b-svt");
        let out_irr = mk_out(GpuTextureFormat::Rgba32Float, "rt-p4b-irr");
        let out_n = mk_out(GpuTextureFormat::Rgba16Float, "rt-p4b-n");
        let out_refl = mk_out(GpuTextureFormat::Rgba32Float, "rt-p4b-refl");
        let prefiltered_env = device.create_texture(&GpuTextureDesc {
            width: 1, height: 1, depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "rt-p4b-env-dummy",
            mip_levels: 1,
        });
        device.upload_texture(&prefiltered_env, &[0u8; 8]);

        let casters = if sun {
            vec![RtCasterParams::new([0.0, 1.0, 0.0], 0.0, [1.0, 1.0, 1.0], 0)]
        } else {
            vec![]
        };
        let params = ShadowRayParams::new(
            &casters, 0, 1, [width, 1], [width, 1], 0.0, 0, gi_spp,
            [0.0, 1.8, 2.0], IDENTITY, refl_spp, 0.6, 0.1,
            manifold_gpu::raytrace::SVT_SLOT_NONE,
        );
        let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);
        let gi_materials_buffer = write_shared(device, &materials);

        let mut enc = device.create_encoder(label);
        let changes = vec![RtGeometryChange::Rebuild; objects.len()];
        tracer
            .encode_accel_update(device, &mut enc, &mut accel, &objects, &changes, &materials, true, true)
            .expect("encode accel update");
        let table = accel.emissive_table.as_ref().expect("table resident since P4a");
        tracer.dispatch_shadow_rays(
            &mut enc, device, &accel, &table.stats, &params, &params_buffer,
            &gi_materials_buffer, &normal_sources, &objects, &[],
            &depth_tex, &out_sv, &out_sv2, &out_svt, &out_irr, &out_n, &out_refl,
            &prefiltered_env, &table.triangles, &table.aliases, false, label,
        );
        enc.commit_and_wait_completed();

        let row_bytes = width as usize * 4 * 4;
        let irr_buf = device.create_buffer_shared(row_bytes as u64);
        let refl_buf = device.create_buffer_shared(row_bytes as u64);
        let mut enc2 = device.create_encoder("rt-p4b-readback");
        enc2.copy_texture_to_buffer(&out_irr, &irr_buf, width, 1, row_bytes as u32);
        enc2.copy_texture_to_buffer(&out_refl, &refl_buf, width, 1, row_bytes as u32);
        enc2.commit_and_wait_completed();
        let read = |buf: &GpuBuffer| -> Vec<f32> {
            let ptr = buf.mapped_ptr().expect("readback buffer must be CPU-mapped");
            unsafe { std::slice::from_raw_parts(ptr as *const f32, row_bytes / 4).to_vec() }
        };
        (read(&irr_buf), read(&refl_buf))
    }

    // ── Section 7: accepted-hit brightness multiplies the GI gather's
    // evaluated radiance ONCE. Ceiling sun-bounce term is a constant per
    // accepted hit (albedo 0.8/π), so the gain-2/gain-1 ratio is exactly 2
    // (not 4 — coverage is not multiplied again). gain 0.5 (coverage 0.5)
    // halves the texel-sum ratio within tolerance (fixed seeds, Bernoulli
    // acceptance among the identical geometric hits of the two runs).
    {
        let (irr1, _) = run_full_trace(device, &tracer, &floor_vb, &ceil_vb, None, 1.0, [0.0; 3], true, 256, 4, 0, "rt-p4b-gi-gain1");
        let (irr2, _) = run_full_trace(device, &tracer, &floor_vb, &ceil_vb, None, 2.0, [0.0; 3], true, 256, 4, 0, "rt-p4b-gi-gain2");
        let sum = |v: &[f32]| v.chunks_exact(4).map(|c| (c[0] + c[1] + c[2]) as f64).sum::<f64>();
        let (s1, s2) = (sum(&irr1), sum(&irr2));
        assert!(s1 > 0.0, "gain-1 GI gather is lit (sun bounce off the ceiling)");
        let ratio = s2 / s1;
        assert!((ratio - 2.0).abs() <= 2e-6 * 2.0,
            "gain 2 doubles the accepted hit's radiance exactly once: ratio {ratio} (not 4)");
        report_lines.push(format!("gi brightness: sum(gain1)={s1:.6} sum(gain2)={s2:.6} ratio={ratio:.6} (exactly 2 = brightness once)"));

        let half_weights = write_shared(device, &[0.5f32; 6]); // one per ceiling vertex (flat 2-triangle quad)
        let (irrh, _) = run_full_trace(device, &tracer, &floor_vb, &ceil_vb, Some(&half_weights), 1.0, [0.0; 3], true, 256, 4, 0, "rt-p4b-gi-cov-half");
        let sh = sum(&irrh);
        let ratio = sh / s1;
        assert!((ratio - 0.5).abs() <= 0.06,
            "coverage 0.5 halves the gathered radiance on average: ratio {ratio} (≈4σ tolerance, fixed seeds)");
        report_lines.push(format!("gi coverage: sum(cov=0.5)={sh:.6} ratio={ratio:.5} vs 0.5 (256 texels × 4 spp, fixed seeds)"));
    }

    // ── Section 8: explicit emitter samples multiply coverage × brightness
    // ONCE at the sampled barycentrics (they never passed the hit test).
    // Pure RIS term: no sun casters, empty env, gather's own bounce-0
    // emissive substituted out. Deterministic same-seed ratios.
    {
        let half_weights = write_shared(device, &[0.5f32; 6]); // one per ceiling vertex (flat 2-triangle quad)
        let run = |weights: Option<&GpuBuffer>, gain: f32, label: &str| {
            let (irr, _) = run_full_trace(device, &tracer, &floor_vb, &ceil_vb, weights, gain, [2.0, 2.0, 2.0], false, 2, 2, 0, label);
            irr.chunks_exact(4).map(|c| (c[0] + c[1] + c[2]) as f64).sum::<f64>()
        };
        let base = run(None, 1.0, "rt-p4b-ris-base");
        assert!(base > 0.0, "the RIS sampler lights the receiver");
        let gain2 = run(None, 2.0, "rt-p4b-ris-gain2");
        let ratio = gain2 / base;
        assert!((ratio - 2.0).abs() <= 1e-5,
            "emitter gain 2: coverage×brightness = 1×2 — ratio {ratio} (exactly 2, not 4)");
        let cov_half = run(Some(&half_weights), 1.0, "rt-p4b-ris-cov-half");
        let ratio = cov_half / base;
        assert!((ratio - 0.5).abs() <= 1e-5,
            "emitter weights 0.5: coverage×brightness = 0.5×1 — ratio {ratio} (exactly 0.5)");
        let neutral = run(Some(&half_weights), 2.0, "rt-p4b-ris-neutral");
        let ratio = neutral / base;
        assert!((ratio - 1.0).abs() <= 1e-5,
            "weights 0.5 × gain 2 = level 1 — ratio {ratio} (exactly 1: coverage and brightness cancel)");
        let zero = run(None, 0.0, "rt-p4b-ris-zero");
        assert_eq!(zero, 0.0, "emitter gain 0 (coverage 0) zeroes the sample");
        report_lines.push(format!(
            "emissive RIS: base={base:.6} gain2={gain2:.6} (×{:.6}) cov0.5={cov_half:.6} (×{:.6}) level1={neutral:.6} gain0={zero}",
            gain2 / base, cov_half / base
        ));
    }

    // ── Section 9: reflection hit shading multiplies brightness ONCE.
    // Mirror floor (roughness 0) reflects the emissive ceiling; env/sunset
    // to zero, so traced radiance is the ceiling's emission × brightness.
    {
        let run = |gain: f32, label: &str| {
            let (_, refl) = run_full_trace(device, &tracer, &floor_vb, &ceil_vb, None, gain, [4.0, 4.0, 4.0], false, 2, 0, 1, label);
            refl.chunks_exact(4).map(|c| (c[0] + c[1] + c[2]) as f64).sum::<f64>()
        };
        let r1 = run(1.0, "rt-p4b-refl-gain1");
        let r2 = run(2.0, "rt-p4b-refl-gain2");
        assert!(r1 > 0.0, "reflection sees the emissive ceiling (hit_dist > 0 path)");
        let ratio = r2 / r1;
        assert!((ratio - 2.0).abs() <= 1e-5,
            "reflection hit brightness once: ratio {ratio} (exactly 2, not 4)");
        report_lines.push(format!("reflection brightness: gain1={r1:.6} gain2={r2:.6} ratio={ratio:.6}"));
    }

    // Numeric report (the P4b demo artifact's text half; the PNG half is
    // the coverage acceptance map below).
    println!("rt_dynamic_coverage_and_attributes report:");
    for line in &report_lines {
        println!("  {line}");
    }

    // Diagnostic PNG: the gain-1/centroid fractional case's 65,536-sample
    // acceptance map (256×256, white = accepted) — the stochastic coverage
    // pattern Peter can eyeball for structure.
    {
        let (accepted, hits) = run_case(1.0, centroid, SAMPLES, 4242);
        assert!(accepted > 0);
        let side = 256usize;
        let mut img = vec![0u8; side * side * 4];
        for (i, hit) in hits.iter().enumerate() {
            let v = if hit.hit == 1 { 255u8 } else { 0u8 };
            img[i * 4] = v;
            img[i * 4 + 1] = v;
            img[i * 4 + 2] = v;
            img[i * 4 + 3] = 255;
        }
        let path = "/tmp/manifold-rt-dynamic/p4b-coverage-map.png".to_string();
        std::fs::create_dir_all("/tmp/manifold-rt-dynamic").expect("create artifact dir");
        image::save_buffer(&path, &img, side as u32, side as u32, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("write {path}: {e}"));
        println!("  coverage acceptance map: {path} (freq {:.5} vs coverage 0.5)", accepted as f32 / SAMPLES as f32);
    }
}
