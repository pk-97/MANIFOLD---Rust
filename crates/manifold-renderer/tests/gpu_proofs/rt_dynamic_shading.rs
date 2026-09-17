//! SCENE_MODIFIER_RT_DESIGN.md §5.1 / A4 (BUG-e3p6.4) — the
//! `rt_dynamic_shading` group, P4a half: the emissive light table is
//! prepared on the GPU (`encode_emissive_table` — enumerate → radix sort →
//! gather → alias → stats) and the trace/firefly kernels read the 16-byte
//! GPU stats buffer. Every assertion compares against a CPU oracle computed
//! FROM THE FIXTURE INPUTS (never from production modifier code). A4's
//! "deliberately stale CPU mirror must not affect results" is structural
//! now — the CPU params fields are deleted; nothing CPU-side can go stale.
//! P4b adds `rt_dynamic_coverage_and_attributes` to this file.

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
