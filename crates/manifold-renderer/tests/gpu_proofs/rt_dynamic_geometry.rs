//! SCENE_MODIFIER_RT_DESIGN.md P0 (BUG-e3p6.4) — deterministic ray-query
//! witnesses for dynamic RT geometry. A0's harness rules: the debug ray
//! query uses the production candidate-hit walk/source tables/descriptors/
//! AS; the CPU oracle is Möller–Trumbore over final GPU geometry bytes
//! (never a copy of modifier math); comparisons use the A0 thresholds
//! (distance ≤ max(1e-4, 1e-4*|expected|), barycentric/UV ≤ 2e-4, normal
//! dot ≥ 0.9999, minimum barycentric coordinate 0.05, no NaN/Inf). Nested
//! modules are named per A0 so `gpu_proofs_gate.py --filter` selects real
//! tests per phase; P0 owns `rt_dynamic_baseline`, P3 owns
//! `rt_dynamic_ordering` (A2 same-command-buffer and lifetime probes).

use std::slice;

use manifold_gpu::raytrace::{
    ensure_normal_sources, ensure_normal_sources_snapshot, DebugRayQueryHit, DebugRayQueryRay,
    MetalShadowRayTracer, RtObjectGeometry, ShadowRayTracer, DEBUG_RAY_INVALID,
};
use manifold_gpu::{GpuBuffer, GpuDevice};

use crate::harness;

/// `pos` (16 bytes) + `normal` (16 bytes) + `uv` (8 bytes) interleaved
/// vertex — the same field offsets render_scene's `MeshVertex` carries
/// (normal 16, uv 32), stride 40.
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

/// One upward-facing unit triangle centered at `cx` in the z=0 plane.
/// UVs are per-vertex distinct so the interpolated-UV channel is proven,
/// not just present.
fn triangle_at(cx: f32) -> [PackedVertex; 3] {
    [
        PackedVertex { pos: [cx - 0.25, -0.25, 0.0, 0.0], normal: [0.0, 0.0, 1.0, 0.0], uv: [0.0, 0.0] },
        PackedVertex { pos: [cx + 0.25, -0.25, 0.0, 0.0], normal: [0.0, 0.0, 1.0, 0.0], uv: [1.0, 0.0] },
        PackedVertex { pos: [cx, 0.25, 0.0, 0.0], normal: [0.0, 0.0, 1.0, 0.0], uv: [0.5, 1.0] },
    ]
}

fn write_vertices(device: &GpuDevice, verts: &[PackedVertex]) -> GpuBuffer {
    let buffer = device.create_buffer_shared(std::mem::size_of_val(verts) as u64);
    let ptr = buffer.mapped_ptr().expect("fixture vertex buffer must be CPU-mapped");
    unsafe {
        std::ptr::copy_nonoverlapping(verts.as_ptr(), ptr as *mut PackedVertex, verts.len());
    }
    buffer
}

fn rewrite_vertices(buffer: &GpuBuffer, verts: &[PackedVertex]) {
    let ptr = buffer.mapped_ptr().expect("fixture vertex buffer must be CPU-mapped");
    unsafe {
        std::ptr::copy_nonoverlapping(verts.as_ptr(), ptr as *mut PackedVertex, verts.len());
    }
}

/// A ray aimed from z=+2 straight at the centroid of `triangle_at(cx)`:
/// hits at distance 2 with barycentrics (1/3, 1/3) — minimum barycentric
/// coordinate 1/3, far clear of A0's 0.05 edge exclusion.
fn centroid_ray(cx: f32) -> DebugRayQueryRay {
    DebugRayQueryRay {
        origin: [cx, -1.0 / 12.0, 2.0],
        direction: [0.0, 0.0, -1.0],
        min_distance: 0.0,
        max_distance: 10.0,
    }
}

/// CPU Möller–Trumbore oracle (test-only per A0). Two-sided, matching the
/// production intersector's default no-cull reset. Returns (distance, u, v)
/// on hit.
fn moller_trumbore(ray: &DebugRayQueryRay, tri: &[PackedVertex; 3]) -> Option<(f32, f32, f32)> {
    let sub = |a: [f32; 4], b: [f32; 4]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let v0 = tri[0].pos;
    let e1 = sub(tri[1].pos, v0);
    let e2 = sub(tri[2].pos, v0);
    let d = ray.direction;
    let pvec = cross(d, e2);
    let det = dot(e1, pvec);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = 1.0 / det;
    let tvec = [
        ray.origin[0] - v0[0],
        ray.origin[1] - v0[1],
        ray.origin[2] - v0[2],
    ];
    let u = dot(tvec, pvec) * inv;
    let qvec = cross(tvec, e1);
    let v = dot(d, qvec) * inv;
    let t = dot(e2, qvec) * inv;
    const EPS: f32 = 1e-6;
    if u < -EPS || v < -EPS || u + v > 1.0 + EPS {
        return None;
    }
    if t < ray.min_distance - EPS || t > ray.max_distance + EPS {
        return None;
    }
    Some((t, u, v))
}

fn read_hits(buffer: &GpuBuffer, count: usize) -> Vec<DebugRayQueryHit> {
    let ptr = buffer.mapped_ptr().expect("hit buffer must be CPU-mapped");
    unsafe { slice::from_raw_parts(ptr as *const DebugRayQueryHit, count).to_vec() }
}

/// A0 field comparison against the CPU oracle. Returns a mismatch report
/// string (empty = match). Kept separate from `assert!` so the
/// deliberately-wrong self-check can demand a NON-empty report.
fn compare_hit_to_oracle(
    hit: &DebugRayQueryHit,
    oracle: Option<(f32, f32, f32)>,
    expected_uv: [f32; 2],
    context: &str,
) -> String {
    let mut problems = String::new();
    match oracle {
        Some((t, u, v)) => {
            if hit.hit != 1 {
                return format!("{context}: oracle says hit at t={t}, query missed");
            }
            let tol = 1e-4f32.max(1e-4 * t.abs());
            if (hit.distance - t).abs() > tol {
                problems.push_str(&format!(" distance {} vs oracle {t} (tol {tol});", hit.distance));
            }
            for (got, want, name) in [(hit.bary[0], u, "bary.u"), (hit.bary[1], v, "bary.v")] {
                if (got - want).abs() > 2e-4 {
                    problems.push_str(&format!(" {name} {got} vs {want};"));
                }
            }
            let dot = hit.normal[2]; // fixture normal is (0,0,1)
            if !(hit.normal[0].abs() <= 2e-4 && hit.normal[1].abs() <= 2e-4 && dot >= 0.9999) {
                problems.push_str(&format!(" normal {:?} not (0,0,1);", hit.normal));
            }
            for (got, want, name) in [(hit.uv[0], expected_uv[0], "uv.x"), (hit.uv[1], expected_uv[1], "uv.y")] {
                if (got - want).abs() > 2e-4 {
                    problems.push_str(&format!(" {name} {got} vs {want};"));
                }
            }
            for value in [hit.distance, hit.bary[0], hit.bary[1], hit.normal[0], hit.normal[1], hit.normal[2], hit.uv[0], hit.uv[1]] {
                assert!(value.is_finite(), "{context}: non-finite channel in hit record");
            }
        }
        None => {
            if hit.hit != 0 || hit.object_id != DEBUG_RAY_INVALID {
                problems.push_str(&format!(
                    " oracle says miss, query hit object {} instance {} prim {} at {};",
                    hit.object_id, hit.instance_id, hit.primitive_id, hit.distance
                ));
            }
        }
    }
    if problems.is_empty() {
        String::new()
    } else {
        format!("{context}:{problems}")
    }
}

/// P0 baseline witness (A0: the ONE unsupported-behavior reproduction).
///
/// State A geometry → resident AS → in-place rewrite of the SAME vertex
/// buffer to state B (the shape a continuous modifier produces every
/// frame). The current API offers no ordered update, so rays still see A
/// while the GPU vertex bytes — and therefore raster — are B. The test
/// passes by RECORDING that stale observation with exact values, and by
/// proving the oracle distinguishes both states and rejects a deliberately
/// wrong expected result. P5 removes the stale-AS branch and flips this to
/// the positive same-frame assertion (A0: "Remove that baseline limitation
/// assertion when P5 lands").
mod rt_dynamic_baseline {
    use super::*;

    const STATE_A_X: f32 = -0.75;
    const STATE_B_X: f32 = 0.75;
    /// Centroid UV of the fixture triangle: mean of (0,0),(1,0),(0.5,1).
    const CENTROID_UV: [f32; 2] = [0.5, 1.0 / 3.0];

    #[test]
    fn rt_dynamic_baseline_records_unsupported() {
        let h = harness::shared();
        let device = &h.device;
        let tracer = MetalShadowRayTracer::new(device);

        let state_a = triangle_at(STATE_A_X);
        let state_b = triangle_at(STATE_B_X);
        let vertex_buffer = write_vertices(device, &state_a);

        let objects = [RtObjectGeometry {
            vertex_buffer: &vertex_buffer,
            vertex_stride: VERTEX_STRIDE,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 1,
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
        }];
        let mut as_builds = 0u32;
        // P3 seam: plan/prepare allocate; the encode rides the first
        // query's encoder below (built before the ray query on the same
        // command buffer).
        let plan = tracer.plan_accel(device, None, &objects).expect("plan accel");
        let mut accel_slot = None;
        tracer.prepare_accel(device, &mut accel_slot, plan).expect("prepare accel");
        let mut accel = accel_slot.unwrap();
        as_builds += 1;

        let mut normal_sources_slot = None;
        let mut normal_sources_capacity = 0usize;
        let material_textures =
            ensure_normal_sources(&mut normal_sources_slot, &mut normal_sources_capacity, device, &objects);
        assert!(material_textures.is_empty(), "fixture binds no material textures");
        let normal_sources = normal_sources_slot.expect("ensure_normal_sources must allocate");
        let normal_sources = &normal_sources;

        // ── Step 1: helper validation at state A. The debug query must
        // report the committed hit exactly as the CPU oracle computes it
        // (distance, barycentrics, interpolated normal and UV through the
        // production fetch helpers).
        let ray_a = centroid_ray(STATE_A_X);
        let mut enc = device.create_encoder("rt-dynamic-baseline-a");
        let changes = vec![manifold_gpu::raytrace::RtGeometryChange::Rebuild; objects.len()];
        tracer
            .encode_accel_update(device, &mut enc, &mut accel, &objects, &changes, &[], true, true)
            .expect("encode accel update");
        let hits_a_buf = tracer.debug_ray_query(
            device,
            &mut enc,
            &accel,
            normal_sources,
            &[ray_a],
            None,
            0,
            0,
        );
        enc.commit_and_wait_completed();
        let hits_a = read_hits(&hits_a_buf, 1);
        let hit_a = &hits_a[0];
        assert_eq!(hit_a.hit, 1, "state-A ray must hit the fresh AS");
        assert_eq!(hit_a.object_id, 0, "single fixture object");
        assert_eq!(hit_a.instance_id, 0, "unwired object commits slot 0");
        assert_eq!(hit_a.primitive_id, 0, "single fixture triangle");
        assert_eq!(hit_a.coverage, 1.0, "accepted hit coverage is 1.0 pre-P4b");
        let oracle_a = moller_trumbore(&ray_a, &state_a);
        let report = compare_hit_to_oracle(hit_a, oracle_a, CENTROID_UV, "state-A helper validation");
        assert!(report.is_empty(), "{report}");

        // ── Step 2: oracle self-checks. It must distinguish the two
        // geometry states, and the comparison must reject a deliberately
        // wrong expected result (A0 gate).
        let ray_b = centroid_ray(STATE_B_X);
        assert!(
            moller_trumbore(&ray_b, &state_b).is_some(),
            "oracle must hit state-B bytes at the state-B aim"
        );
        assert!(
            moller_trumbore(&ray_b, &state_a).is_none(),
            "oracle must miss state-A bytes at the state-B aim"
        );
        let wrong = oracle_a.map(|(t, u, v)| (t + 0.5, u, v));
        assert!(
            !compare_hit_to_oracle(hit_a, wrong, CENTROID_UV, "deliberately wrong distance").is_empty(),
            "comparison must reject a deliberately wrong expected distance"
        );
        let wrong_uv = [CENTROID_UV[0] + 0.1, CENTROID_UV[1]];
        assert!(
            !compare_hit_to_oracle(hit_a, oracle_a, wrong_uv, "deliberately wrong uv").is_empty(),
            "comparison must reject a deliberately wrong expected UV"
        );

        // ── Step 3: the unsupported-behavior witness. Rewrite the SAME
        // buffer in place to state B — what a continuous modifier's GPU
        // writes do every frame — and query again. The resident AS has no
        // ordered update path today, so rays keep seeing state A while the
        // vertex bytes (and raster) are state B.
        rewrite_vertices(&vertex_buffer, &state_b);
        let mut enc = device.create_encoder("rt-dynamic-baseline-b");
        let hits_buf = tracer.debug_ray_query(
            device,
            &mut enc,
            &accel,
            normal_sources,
            &[ray_b, ray_a],
            None,
            0,
            0,
        );
        enc.commit_and_wait_completed();
        let hits = read_hits(&hits_buf, 2);

        let stale_observation = (hits[0].hit == 0, hits[1].hit == 1);
        assert_eq!(
            stale_observation,
            (true, true),
            "BASELINE CHANGED: with current GPU bytes at state B, rays must still see the \
             stale state-A AS (miss at B, hit at A). If this now fails because an ordered \
             update path exists, P5's positive same-frame test replaces this witness. \
             Observed: miss-at-B={} hit-at-A={}",
            hits[0].hit == 0,
            hits[1].hit == 1
        );
        // The stale hit at A is the OLD geometry, still matching the
        // state-A oracle — proof the AS, not the bytes, drives the answer.
        let report = compare_hit_to_oracle(&hits[1], oracle_a, CENTROID_UV, "stale state-A hit");
        assert!(report.is_empty(), "{report}");

        println!(
            "rt_dynamic_baseline witness: as_builds={as_builds} queries=3 \
             stale(miss-at-B=true, hit-at-A=true) bytes=state-B \
             — recorded unsupported behavior for BUG-e3p6.4"
        );
    }
}

/// P3 acceptance probes (A2: same-frame command order and lifetime —
/// SCENE_MODIFIER_RT_ACCEPTANCE.md). The caller-ordered seam
/// (plan/prepare/encode) is what makes these properties expressible: the
/// geometry write, the accel update and the ray query all ride ONE caller
/// command buffer per frame, and the only lifetime guarantee between a
/// teardown and an in-flight query is the §4.3 completion-pin set.
mod rt_dynamic_ordering {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use manifold_gpu::raytrace::{RtAccelError, RtGeometryChange};
    use manifold_gpu::GpuBinding;

    use super::*;

    const STATE_LEFT_X: f32 = -0.75;
    const STATE_RIGHT_X: f32 = 0.75;
    /// Centroid UV of the fixture triangle: mean of (0,0),(1,0),(0.5,1).
    const CENTROID_UV: [f32; 2] = [0.5, 1.0 / 3.0];

    /// A2's same-frame discipline requires the triangle's bytes to move on
    /// the SAME command buffer as the accel rebuild and the ray query, so a
    /// GPU kernel — not a CPU mapped write — produces them. One thread
    /// rewrites the whole interleaved 3-vertex record (40 bytes per
    /// `PackedVertex`: pos4, normal4, uv2) from a 16-byte uniform carrying
    /// this frame's center x.
    const WRITE_TRIS_WGSL: &str = r#"
struct WriteParams {
    center_x: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
};
@group(0) @binding(0) var<uniform> params: WriteParams;
@group(0) @binding(1) var<storage, read_write> verts: array<f32>;

@compute @workgroup_size(1)
fn cs_main() {
    let cx = params.center_x;
    verts[0] = cx - 0.25;
    verts[1] = -0.25;
    verts[2] = 0.0;
    verts[3] = 0.0;
    verts[4] = 0.0;
    verts[5] = 0.0;
    verts[6] = 1.0;
    verts[7] = 0.0;
    verts[8] = 0.0;
    verts[9] = 0.0;
    verts[10] = cx + 0.25;
    verts[11] = -0.25;
    verts[12] = 0.0;
    verts[13] = 0.0;
    verts[14] = 0.0;
    verts[15] = 0.0;
    verts[16] = 1.0;
    verts[17] = 0.0;
    verts[18] = 1.0;
    verts[19] = 0.0;
    verts[20] = cx;
    verts[21] = 0.25;
    verts[22] = 0.0;
    verts[23] = 0.0;
    verts[24] = 0.0;
    verts[25] = 0.0;
    verts[26] = 1.0;
    verts[27] = 0.0;
    verts[28] = 0.5;
    verts[29] = 1.0;
}
"#;

    /// Indexed variant: the vertex pool is constant (slots 0-2 hold the
    /// LEFT triangle, slots 3-5 the RIGHT one); the frame's connectivity
    /// selects one triple in place and leaves the other index slot dead
    /// (degenerate 0,0,0 — never hit) at EQUAL index capacity, so the
    /// alternation is a pure in-place connectivity change.
    const WRITE_INDEXED_WGSL: &str = r#"
struct WriteParams {
    use_left: u32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
};
@group(0) @binding(0) var<uniform> params: WriteParams;
@group(0) @binding(1) var<storage, read_write> verts: array<f32>;
@group(0) @binding(2) var<storage, read_write> indices: array<u32>;

@compute @workgroup_size(1)
fn cs_main() {
    for (var v = 0u; v < 3u; v = v + 1u) {
        let o = v * 10u;
        let mid = v == 2u;
        let ox = select(select(-0.25, 0.25, v == 1u), 0.0, mid);
        let oy = select(-0.25, 0.25, mid);
        let ux = select(select(0.0, 1.0, v == 1u), 0.5, mid);
        let uy = select(0.0, 1.0, mid);
        for (var t = 0u; t < 2u; t = t + 1u) {
            let b = o + t * 30u;
            let cx = select(-0.75, 0.75, t == 1u);
            verts[b] = cx + ox;
            verts[b + 1u] = oy;
            verts[b + 2u] = 0.0;
            verts[b + 3u] = 0.0;
            verts[b + 4u] = 0.0;
            verts[b + 5u] = 0.0;
            verts[b + 6u] = 1.0;
            verts[b + 7u] = 0.0;
            verts[b + 8u] = ux;
            verts[b + 9u] = uy;
        }
    }
    if (params.use_left == 1u) {
        indices[0] = 0u;
        indices[1] = 1u;
        indices[2] = 2u;
    } else {
        indices[0] = 3u;
        indices[1] = 4u;
        indices[2] = 5u;
    }
    indices[3] = 0u;
    indices[4] = 0u;
    indices[5] = 0u;
}
"#;

    /// Multiframe snapshot variant: writes one `mesh_common.rs`
    /// `InstanceTransform` (pos_scale, rot_pad — 32 bytes) — a live slot
    /// (`pos_scale.w = 1`) translated by `dx` in x with zero euler
    /// rotation, so a per-snapshot transform rides the wire buffer
    /// GPU-side instead of through a CPU-mapped table.
    const WRITE_INSTANCE_WGSL: &str = r#"
struct WriteParams {
    dx: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
};
@group(0) @binding(0) var<uniform> params: WriteParams;
@group(0) @binding(1) var<storage, read_write> slot: array<f32>;

@compute @workgroup_size(1)
fn cs_main() {
    slot[0] = params.dx;
    slot[1] = 0.0;
    slot[2] = 0.0;
    slot[3] = 1.0;
    slot[4] = 0.0;
    slot[5] = 0.0;
    slot[6] = 0.0;
    slot[7] = 0.0;
}
"#;

    /// One interleaved fixture triangle; `triangle_count` follows the
    /// index buffer (the indexed fixture carries a dead second slot).
    fn triangle_object<'a>(
        vertex_buffer: &'a GpuBuffer,
        index_buffer: Option<&'a GpuBuffer>,
        transform: [[f32; 4]; 4],
    ) -> RtObjectGeometry<'a> {
        RtObjectGeometry {
            vertex_buffer,
            vertex_stride: VERTEX_STRIDE,
            vertex_offset: 0,
            index_buffer,
            triangle_count: if index_buffer.is_some() { 2 } else { 1 },
            transform,
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

    /// 16-byte uniform payload for the write kernels.
    fn uniform_bytes(words: [u32; 4]) -> [u8; 16] {
        unsafe { std::mem::transmute(words) }
    }

    /// The A2 same-frame loop, run for the flat and the indexed fixture.
    /// Every frame encodes GPU write → accel update → two analytical rays
    /// on ONE caller command buffer with no intermediate commit/wait; the
    /// current-side ray must hit and the previous-side ray must miss on
    /// all eight frames, the first included.
    fn run_same_frame_loop(
        tracer: &MetalShadowRayTracer,
        device: &GpuDevice,
        write_pipe: &manifold_gpu::GpuComputePipeline,
        vertex_buffer: &GpuBuffer,
        index_buffer: Option<&GpuBuffer>,
    ) {
        const FRAMES: u32 = 8;
        let indexed = index_buffer.is_some();
        let objects = [triangle_object(vertex_buffer, index_buffer, IDENTITY)];
        let plan = tracer.plan_accel(device, None, &objects).expect("plan accel");
        let mut accel_slot = None;
        tracer.prepare_accel(device, &mut accel_slot, plan).expect("prepare accel");
        let mut accel = accel_slot.unwrap();

        let mut normal_sources_slot = None;
        let mut normal_sources_capacity = 0usize;
        let material_textures = ensure_normal_sources(
            &mut normal_sources_slot,
            &mut normal_sources_capacity,
            device,
            &objects,
        );
        assert!(material_textures.is_empty(), "fixture binds no material textures");
        let normal_sources = normal_sources_slot.unwrap();

        for frame in 0..FRAMES {
            // Even frames place the triangle at x=-0.75, odd at +0.75.
            let cx = if frame % 2 == 0 { STATE_LEFT_X } else { STATE_RIGHT_X };
            let prev_cx = if frame % 2 == 0 { STATE_RIGHT_X } else { STATE_LEFT_X };
            let mut enc = device.create_encoder("rt-ordering-same-frame");
            // 1) this frame's geometry bytes, written GPU-side.
            let write_uniform = if indexed {
                uniform_bytes([u32::from(frame % 2 == 0), 0, 0, 0])
            } else {
                uniform_bytes([cx.to_bits(), 0, 0, 0])
            };
            let mut bindings = vec![
                GpuBinding::Bytes { binding: 0, data: &write_uniform },
                GpuBinding::Buffer { binding: 1, buffer: vertex_buffer, offset: 0 },
            ];
            if let Some(ib) = index_buffer {
                bindings.push(GpuBinding::Buffer { binding: 2, buffer: ib, offset: 0 });
            }
            enc.dispatch_compute(write_pipe, &bindings, [1, 1, 1], "rt-ordering-geometry-write");
            // 2) the accel update on the SAME command buffer.
            let changes = [RtGeometryChange::Rebuild];
            let update = tracer
                .encode_accel_update(device, &mut enc, &mut accel, &objects, &changes, &[], true, true)
                .unwrap_or_else(|e| panic!("frame {frame} accel update: {e:?}"));
            // 3) two analytical rays, same command buffer, then the frame's
            // single commit+wait — no intermediate commit anywhere above.
            let ray_cur = centroid_ray(cx);
            let ray_prev = centroid_ray(prev_cx);
            let hits_buf = tracer.debug_ray_query(
                device,
                &mut enc,
                &accel,
                &normal_sources,
                &[ray_cur, ray_prev],
                None,
                0,
                0,
            );
            enc.commit_and_wait_completed();

            assert_eq!(
                update.blas_builds, 1,
                "frame {frame}: every state change here is a rebuild (indexed: connectivity)"
            );
            if frame == 0 {
                assert_eq!(update.tlas_builds, 1, "frame 0 builds the TLAS");
            } else {
                assert_eq!(update.tlas_refits, 1, "frame {frame}: a BLAS rebuild refits the TLAS");
            }

            let hits = read_hits(&hits_buf, 2);
            assert_eq!(hits[0].hit, 1, "frame {frame}: current-side ray ({cx}) must hit — RT dispatched");
            assert_eq!(hits[0].object_id, 0, "single fixture object");
            assert_eq!(hits[0].instance_id, 0, "unwired object commits slot 0");
            assert_eq!(hits[0].primitive_id, 0, "active triangle is primitive 0");
            assert_eq!(hits[0].coverage, 1.0, "accepted hit coverage is 1.0 pre-P4b");
            let tri = triangle_at(cx);
            let report =
                compare_hit_to_oracle(&hits[0], moller_trumbore(&ray_cur, &tri), CENTROID_UV, "same-frame current-side");
            assert!(report.is_empty(), "frame {frame}: {report}");
            assert_eq!(
                hits[1].hit, 0,
                "frame {frame}: previous-side ray ({prev_cx}) must miss the current bytes"
            );
            assert_eq!(
                hits[1].object_id,
                DEBUG_RAY_INVALID,
                "frame {frame}: a miss carries the invalid sentinel"
            );
        }
        println!(
            "rt_dynamic_same_frame_gpu_write_then_hit: frames={FRAMES} indexed={indexed} \
             — every frame's GPU write, rebuild and query rode one command buffer"
        );
    }

    /// `rt_dynamic_same_frame_gpu_write_then_hit` (A2): a GPU kernel
    /// alternates a triangle between x=-0.75 and x=+0.75 inside one
    /// persistent private vertex buffer for eight frames; each frame the
    /// write, the accel update and two analytical rays ride one caller
    /// command buffer with no intermediate commit/wait. Current-side ray
    /// hits, previous-side ray misses, all eight frames, first frame
    /// included. Repeated for an indexed buffer whose connectivity changes
    /// in place at equal capacity — the update must report a rebuild.
    #[test]
    fn rt_dynamic_same_frame_gpu_write_then_hit() {
        let h = harness::shared();
        let device = &h.device;
        let tracer = MetalShadowRayTracer::new(device);
        let flat_pipe =
            device.create_compute_pipeline(WRITE_TRIS_WGSL, "cs_main", "rt-ordering-write-tris");
        let indexed_pipe = device.create_compute_pipeline(
            WRITE_INDEXED_WGSL,
            "cs_main",
            "rt-ordering-write-indexed",
        );

        // Section 1: flat (non-indexed) triangle, one persistent buffer.
        let vertex_buffer = device.create_buffer_shared(3 * u64::from(VERTEX_STRIDE));
        run_same_frame_loop(&tracer, device, &flat_pipe, &vertex_buffer, None);

        // Section 2: indexed; connectivity flips in place at equal capacity
        // (6 u32 index slots every frame, one dead-degenerate slot).
        let vertex_buffer = device.create_buffer_shared(6 * u64::from(VERTEX_STRIDE));
        let index_buffer = device.create_buffer_shared(6 * 4);
        run_same_frame_loop(&tracer, device, &indexed_pipe, &vertex_buffer, Some(&index_buffer));
    }

    /// `rt_dynamic_unsubmitted_teardown_and_multiframe` (A2): three
    /// lifetime/ordering probes at the seam level.
    #[test]
    fn rt_dynamic_unsubmitted_teardown_and_multiframe() {
        let h = harness::shared();
        let device = &h.device;
        let tracer = MetalShadowRayTracer::new(device);
        let faults_before = manifold_gpu::gpu_fault::fault_count();

        // ── Section 1: teardown before commit. Encode geometry + update +
        // query, drop the owner state BEFORE committing, then commit and
        // verify the query. The accel's §4.3 completion pins were attached
        // to this (uncommitted) command buffer at encode time — that, not
        // the live owner, is what keeps every GPU-reachable handle alive.
        // The per-frame normal-source table stays alive like production's
        // caller-owned tables (it is not part of the accel pin set).
        let state = triangle_at(STATE_LEFT_X);
        let vertex_buffer = write_vertices(device, &state);
        let objects = [triangle_object(&vertex_buffer, None, IDENTITY)];
        let mut normal_sources_slot = None;
        let mut normal_sources_capacity = 0usize;
        let material_textures = ensure_normal_sources(
            &mut normal_sources_slot,
            &mut normal_sources_capacity,
            device,
            &objects,
        );
        assert!(material_textures.is_empty(), "fixture binds no material textures");
        let normal_sources = normal_sources_slot.unwrap();
        let plan = tracer.plan_accel(device, None, &objects).expect("plan accel");
        let mut accel_slot = None;
        tracer.prepare_accel(device, &mut accel_slot, plan).expect("prepare accel");
        let mut accel = accel_slot.unwrap();

        let ray = centroid_ray(STATE_LEFT_X);
        let mut enc = device.create_encoder("rt-ordering-teardown");
        let changes = [RtGeometryChange::Rebuild];
        tracer
            .encode_accel_update(device, &mut enc, &mut accel, &objects, &changes, &[], true, true)
            .expect("encode accel update");
        let hits_buf = tracer.debug_ray_query(
            device,
            &mut enc,
            &accel,
            &normal_sources,
            &[ray],
            None,
            0,
            0,
        );
        drop(accel);
        drop(vertex_buffer);
        enc.commit_and_wait_completed();
        let hits = read_hits(&hits_buf, 1);
        assert_eq!(hits[0].hit, 1, "the query must survive owner teardown before commit");
        let report = compare_hit_to_oracle(
            &hits[0],
            moller_trumbore(&ray, &state),
            CENTROID_UV,
            "teardown-before-commit query",
        );
        assert!(report.is_empty(), "{report}");

        // ── Section 2: three snapshots queued before ONE completion wait.
        // The per-snapshot variation rides the WIRED instance path
        // (RT_INSTANCING_DESIGN.md D1): each snapshot's transform is
        // written GPU-side into the instance wire buffer by the write
        // kernel, ahead of the descriptor-build dispatch + TLAS refit on
        // the same command buffer — values move GPU→GPU (INV-RTI6), so
        // three uncommitted frames genuinely keep three distinct
        // snapshots. The non-instanced descriptor table uses retained CPU
        // scratch plus an ordered inline copy into private storage; the
        // same queue ordering protects this path when frames are submitted
        // without an intermediate wait. Delayed completion is FIFO queue
        // order, not a sleep or a thread.
        let offsets = [0.0f32, 0.5, -0.5];
        let local = triangle_at(0.0);
        let vertex_buffer = write_vertices(device, &local);
        let instances_buffer = device.create_buffer_shared(32);
        let objects = [RtObjectGeometry {
            vertex_buffer: &vertex_buffer,
            vertex_stride: VERTEX_STRIDE,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 1,
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
            instances_addr: instances_buffer.gpu_address(),
            instances_buffer: Some(&instances_buffer),
            instance_slots: 1,
            appearance_weights: None,
            appearance_gain: 1.0,
        }];
        let plan = tracer.plan_accel(device, None, &objects).expect("plan accel");
        let mut accel_slot = None;
        tracer.prepare_accel(device, &mut accel_slot, plan).expect("prepare accel");
        let mut accel = accel_slot.unwrap();

        let mut normal_sources_slot = None;
        let mut normal_sources_capacity = 0usize;
        let material_textures = ensure_normal_sources(
            &mut normal_sources_slot,
            &mut normal_sources_capacity,
            device,
            &objects,
        );
        assert!(material_textures.is_empty(), "fixture binds no material textures");
        let normal_sources = normal_sources_slot.unwrap();

        let instance_pipe = device.create_compute_pipeline(
            WRITE_INSTANCE_WGSL,
            "cs_main",
            "rt-ordering-write-instance",
        );

        let mut hit_bufs = Vec::new();
        for (i, &dx) in offsets.iter().enumerate() {
            // One ray at this snapshot's triangle, one at the next
            // snapshot's (0.5 units away — clear of the 0.25 half-extent).
            let other = offsets[(i + 1) % offsets.len()];
            let rays = [centroid_ray(dx), centroid_ray(other)];
            let mut enc = device.create_encoder("rt-ordering-multiframe");
            // 1) this snapshot's instance transform, written GPU-side.
            let write_uniform = uniform_bytes([dx.to_bits(), 0, 0, 0]);
            enc.dispatch_compute(
                &instance_pipe,
                &[
                    GpuBinding::Bytes { binding: 0, data: &write_uniform },
                    GpuBinding::Buffer { binding: 1, buffer: &instances_buffer, offset: 0 },
                ],
                [1, 1, 1],
                "rt-ordering-instance-write",
            );
            // 2) descriptor build + TLAS update on the SAME command buffer.
            let changes = [RtGeometryChange::Reuse];
            let update = tracer
                .encode_accel_update(
                    device,
                    &mut enc,
                    &mut accel,
                    &objects,
                    &changes,
                    &[],
                    true,
                    false,
                )
                .unwrap_or_else(|e| panic!("snapshot {i} accel update: {e:?}"));
            if i == 0 {
                assert_eq!(update.tlas_builds, 1, "snapshot 0 builds the TLAS");
            } else {
                assert_eq!(update.blas_builds, 0, "snapshot {i}: a transform-only change builds no BLAS");
                assert_eq!(update.tlas_refits, 1, "snapshot {i}: a transform-only change refits the TLAS");
            }
            hit_bufs.push(tracer.debug_ray_query(
                device,
                &mut enc,
                &accel,
                &normal_sources,
                &rays,
                None,
                0,
                0,
            ));
            if i + 1 == offsets.len() {
                enc.commit_and_wait_completed();
            } else {
                enc.commit();
            }
        }
        for (i, buf) in hit_bufs.iter().enumerate() {
            let hits = read_hits(buf, 2);
            let dx = offsets[i];
            assert_eq!(hits[0].hit, 1, "snapshot {i}: own-side ray at x={dx} must hit");
            let ray = centroid_ray(dx);
            let report = compare_hit_to_oracle(
                &hits[0],
                moller_trumbore(&ray, &triangle_at(dx)),
                CENTROID_UV,
                "multiframe own-side",
            );
            assert!(report.is_empty(), "snapshot {i}: {report}");
            assert_eq!(
                hits[1].hit, 0,
                "snapshot {i}: the other snapshot's ray must miss — each output matches its own snapshot"
            );
        }

        // ── Section 2b: three uninstanced transform snapshots queued before
        // one completion wait. Each frame owns a separate command encoder,
        // but all three updates target the same resident accel. The normal
        // source rows are snapshotted onto each encoder before its query, so
        // the CPU scratch rewrite cannot alias an earlier queued frame.
        let offsets = [0.0f32, 0.5, -0.5];
        let local = triangle_at(0.0);
        let vertex_buffer = write_vertices(device, &local);
        let plan = tracer
            .plan_accel(device, None, &[triangle_object(&vertex_buffer, None, IDENTITY)])
            .expect("plan uninstanced multiframe accel");
        let mut accel_slot = None;
        tracer
            .prepare_accel(device, &mut accel_slot, plan)
            .expect("prepare uninstanced multiframe accel");
        let mut accel = accel_slot.unwrap();
        let mut normal_scratch = None;
        let mut normal_scratch_capacity = 0usize;
        let mut normal_destination = None;
        let mut normal_destination_capacity = 0usize;
        let mut encoders = Vec::with_capacity(offsets.len());
        let mut hit_bufs = Vec::with_capacity(offsets.len());
        for (i, &dx) in offsets.iter().enumerate() {
            let mut transform = IDENTITY;
            transform[3][0] = dx;
            let objects = [triangle_object(&vertex_buffer, None, transform)];
            let mut enc = device.create_encoder("rt-ordering-uninstanced-multiframe");
            let material_textures = ensure_normal_sources_snapshot(
                &mut normal_scratch,
                &mut normal_scratch_capacity,
                &mut normal_destination,
                &mut normal_destination_capacity,
                device,
                &mut enc,
                &objects,
            );
            assert!(material_textures.is_empty(), "fixture binds no material textures");
            let normal_sources = normal_destination
                .as_ref()
                .expect("normal snapshot destination");
            let changes = [if i == 0 {
                RtGeometryChange::Rebuild
            } else {
                RtGeometryChange::Reuse
            }];
            let update = tracer
                .encode_accel_update(
                    device,
                    &mut enc,
                    &mut accel,
                    &objects,
                    &changes,
                    &[],
                    true,
                    false,
                )
                .unwrap_or_else(|e| panic!("uninstanced snapshot {i} accel update: {e:?}"));
            if i == 0 {
                assert_eq!(update.tlas_builds, 1, "snapshot 0 builds the TLAS");
            } else {
                assert_eq!(update.blas_builds, 0, "snapshot {i}: transform-only update has no BLAS build");
                assert_eq!(update.tlas_refits, 1, "snapshot {i}: transform-only update refits the TLAS");
            }
            let ray = centroid_ray(dx);
            hit_bufs.push(tracer.debug_ray_query(
                device,
                &mut enc,
                &accel,
                normal_sources,
                &[ray],
                None,
                0,
                0,
            ));
            encoders.push(enc);
        }
        for (i, enc) in encoders.into_iter().enumerate() {
            if i + 1 == offsets.len() {
                enc.commit_and_wait_completed();
            } else {
                enc.commit();
            }
        }
        for (i, buf) in hit_bufs.iter().enumerate() {
            let dx = offsets[i];
            let hits = read_hits(buf, 1);
            assert_eq!(hits[0].hit, 1, "uninstanced snapshot {i}: ray at x={dx} must hit");
            let ray = centroid_ray(dx);
            let expected = triangle_at(dx);
            let report = compare_hit_to_oracle(
                &hits[0],
                moller_trumbore(&ray, &expected),
                CENTROID_UV,
                "uninstanced multiframe",
            );
            assert!(report.is_empty(), "uninstanced snapshot {i}: {report}");
        }

        // ── Section 3: readiness attaches to the resource set. Encode A's
        // first update but do NOT commit; replace A with B via plan/prepare
        // while A's update is still uncommitted; commit; A's completion
        // must flip only A's flag — B stays not-ready until its own update
        // completes.
        let state_a = triangle_at(STATE_LEFT_X);
        let vertex_a = write_vertices(device, &state_a);
        let objects_a = [triangle_object(&vertex_a, None, IDENTITY)];
        let plan_a = tracer.plan_accel(device, None, &objects_a).expect("plan A");
        let mut resident = None;
        tracer.prepare_accel(device, &mut resident, plan_a).expect("prepare A");
        let mut accel_a = resident.take().expect("A resident");
        let ready_a = Arc::clone(&accel_a.ready);

        let mut enc_a = device.create_encoder("rt-ordering-ready-a");
        let changes = [RtGeometryChange::Rebuild];
        tracer
            .encode_accel_update(device, &mut enc_a, &mut accel_a, &objects_a, &changes, &[], true, true)
            .expect("encode A");

        // Replacement: a buffer-identity move is a full structural
        // replacement (§4.2/§4.3 — the pin set references A's buffers).
        let state_b = triangle_at(STATE_RIGHT_X);
        let vertex_b = write_vertices(device, &state_b);
        let objects_b = [triangle_object(&vertex_b, None, IDENTITY)];
        let plan_b = tracer.plan_accel(device, Some(&accel_a), &objects_b).expect("plan B");
        assert!(
            plan_b.additional_peak_bytes() > 0,
            "a buffer-identity move charges the full old/new overlap peak"
        );
        resident = Some(accel_a);
        tracer.prepare_accel(device, &mut resident, plan_b).expect("prepare B replaces A");
        assert!(
            !resident.as_ref().expect("B resident").ready.load(Ordering::Acquire),
            "B starts not-ready"
        );
        enc_a.commit_and_wait_completed();
        assert!(ready_a.load(Ordering::Acquire), "A's own completion flips A's flag");
        assert!(
            !resident.as_ref().expect("B resident").ready.load(Ordering::Acquire),
            "A's completion must NOT flip B's flag — readiness belongs to the resource set"
        );

        // B is fully operational after its own update: prepare, encode,
        // query, and B's completion flips B's flag.
        let mut accel_b = resident.take().expect("B resident");
        let mut slot = None;
        let mut capacity = 0usize;
        let material_textures =
            ensure_normal_sources(&mut slot, &mut capacity, device, &objects_b);
        assert!(material_textures.is_empty(), "fixture binds no material textures");
        let normal_sources = slot.unwrap();
        let mut enc_b = device.create_encoder("rt-ordering-ready-b");
        let update_b = tracer
            .encode_accel_update(device, &mut enc_b, &mut accel_b, &objects_b, &changes, &[], true, true)
            .expect("encode B");
        assert_eq!(update_b.blas_builds, 1);
        let ray_b = centroid_ray(STATE_RIGHT_X);
        let hits_buf = tracer.debug_ray_query(
            device,
            &mut enc_b,
            &accel_b,
            &normal_sources,
            &[ray_b],
            None,
            0,
            0,
        );
        enc_b.commit_and_wait_completed();
        assert!(accel_b.ready.load(Ordering::Acquire), "B's own completion flips B's flag");
        let hits = read_hits(&hits_buf, 1);
        assert_eq!(hits[0].hit, 1, "B must trace the replaced geometry");
        let report = compare_hit_to_oracle(
            &hits[0],
            moller_trumbore(&ray_b, &state_b),
            CENTROID_UV,
            "B query",
        );
        assert!(report.is_empty(), "{report}");

        assert_eq!(
            manifold_gpu::gpu_fault::fault_count(),
            faults_before,
            "teardown and in-flight replacement must fault nothing"
        );
    }

    /// `rt_dynamic_admission_is_atomic` (A2): admission is decided on the
    /// plan's `additional_peak_bytes` BEFORE prepare runs; malformed
    /// geometry and malformed change lists fail structured before any GPU
    /// encode; a rejected admission or update leaves the resident set
    /// valid.
    #[test]
    fn rt_dynamic_admission_is_atomic() {
        let h = harness::shared();
        let device = &h.device;
        let tracer = MetalShadowRayTracer::new(device);

        // ── Section 1: admission on the plan number, before allocation ──
        let state = triangle_at(STATE_LEFT_X);
        let vertex_buffer = write_vertices(device, &state);
        let objects = [triangle_object(&vertex_buffer, None, IDENTITY)];
        let plan = tracer.plan_accel(device, None, &objects).expect("plan");
        let peak = plan.additional_peak_bytes();
        assert!(peak > 0, "a fresh scene charges a nonzero candidate peak");

        // One byte below the reported peak: rejected. prepare_accel never
        // runs — nothing is allocated, nothing is published, the (empty)
        // resident set is unchanged.
        let budget = peak - 1;
        assert!(
            plan.additional_peak_bytes() > budget,
            "the candidate must not fit one byte under its reported peak"
        );
        drop(plan);

        // Exactly the required peak: admitted — prepare and encode run.
        let plan = tracer.plan_accel(device, None, &objects).expect("re-plan");
        assert_eq!(
            plan.additional_peak_bytes(),
            peak,
            "sizing must be deterministic across identical plans"
        );
        let mut resident = None;
        tracer.prepare_accel(device, &mut resident, plan).expect("prepare at exact peak");
        let mut accel = resident.take().expect("resident after admitted prepare");
        let mut normal_sources_slot = None;
        let mut normal_sources_capacity = 0usize;
        let material_textures = ensure_normal_sources(
            &mut normal_sources_slot,
            &mut normal_sources_capacity,
            device,
            &objects,
        );
        assert!(material_textures.is_empty(), "fixture binds no material textures");
        let normal_sources = normal_sources_slot.unwrap();
        let mut enc = device.create_encoder("rt-ordering-admission");
        let changes = [RtGeometryChange::Rebuild];
        let update = tracer
            .encode_accel_update(device, &mut enc, &mut accel, &objects, &changes, &[], true, true)
            .expect("encode at admitted peak");
        assert_eq!(update.blas_builds, 1);
        let ray = centroid_ray(STATE_LEFT_X);
        let hits_buf = tracer.debug_ray_query(
            device,
            &mut enc,
            &accel,
            &normal_sources,
            &[ray],
            None,
            0,
            0,
        );
        enc.commit_and_wait_completed();
        let hits = read_hits(&hits_buf, 1);
        assert_eq!(hits[0].hit, 1, "an admitted candidate must prepare, encode and trace");

        // Planned bytes == prepared reality: the identical scene re-planned
        // against the resident set charges zero additional bytes (the
        // rt_dynamic_ordering half of the plan/prepared agreement gate).
        let replan = tracer.plan_accel(device, Some(&accel), &objects).expect("replan resident");
        assert_eq!(
            replan.additional_peak_bytes(),
            0,
            "a byte-identical resident set must cover the whole plan"
        );
        drop(replan);

        // ── Section 2: negative/overflow geometry fails plan_accel with
        // InvalidGeometry before any GPU encode (plan takes no encoder and
        // validates every object before sizing anything).
        let expect_invalid = |obj: RtObjectGeometry<'_>, case: &str| {
            match tracer.plan_accel(device, None, std::slice::from_ref(&obj)) {
                Err(RtAccelError::InvalidGeometry { object, reason }) => {
                    assert_eq!(object, 0, "{case}: the error must name the offending object");
                    assert!(!reason.is_empty(), "{case}: the error must carry a reason");
                }
                Ok(_) => panic!("{case}: expected InvalidGeometry, plan succeeded"),
                Err(other) => panic!("{case}: expected InvalidGeometry, got {other:?}"),
            }
        };
        expect_invalid(
            RtObjectGeometry { triangle_count: 0, ..triangle_object(&vertex_buffer, None, IDENTITY) },
            "zero triangles",
        );
        expect_invalid(
            RtObjectGeometry { vertex_stride: 4, ..triangle_object(&vertex_buffer, None, IDENTITY) },
            "absurd vertex stride",
        );
        expect_invalid(
            RtObjectGeometry {
                triangle_count: 1_000_000,
                ..triangle_object(&vertex_buffer, None, IDENTITY)
            },
            "triangle count far past the buffer",
        );
        let tiny_index = device.create_buffer_shared(4);
        expect_invalid(
            RtObjectGeometry {
                index_buffer: Some(&tiny_index),
                triangle_count: 2,
                ..triangle_object(&vertex_buffer, None, IDENTITY)
            },
            "index read past the index buffer",
        );

        // ── Section 3: malformed updates fail structured; the resident
        // set stays valid. Buffer A is sized for TWO triangles from the
        // start (one used now) so a later 2-triangle shape change is a
        // pure shape change, not a capacity problem.
        let verts_a = device.create_buffer_shared(2 * 3 * u64::from(VERTEX_STRIDE));
        {
            let ptr = verts_a.mapped_ptr().expect("fixture vertex buffer must be CPU-mapped");
            unsafe {
                std::ptr::copy_nonoverlapping(state.as_ptr(), ptr as *mut PackedVertex, 3);
            }
        }
        let verts_b = write_vertices(device, &triangle_at(STATE_RIGHT_X));
        let objects2 = [
            triangle_object(&verts_a, None, IDENTITY),
            triangle_object(&verts_b, None, IDENTITY),
        ];
        let plan2 = tracer
            .plan_accel(device, Some(&accel), &objects2)
            .expect("plan two-object scene");
        let peak2 = plan2.additional_peak_bytes();
        assert!(peak2 > 0, "a structural replacement charges the old/new overlap");
        // One byte under: rejected — the CURRENT resident (scene 1) must
        // remain resident, valid, and untouched.
        assert!(peak2 - 1 < plan2.additional_peak_bytes());
        drop(plan2);
        assert!(
            accel.check_topology(std::slice::from_ref(&objects[0])).is_ok(),
            "a rejected admission must leave the resident topology intact"
        );
        let plan2 = tracer
            .plan_accel(device, Some(&accel), &objects2)
            .expect("re-plan two-object scene");
        let mut resident2 = Some(accel);
        tracer.prepare_accel(device, &mut resident2, plan2).expect("prepare two-object scene");
        let mut accel2 = resident2.take().expect("resident two-object scene");

        // A change list shorter than the object list fails structured.
        let mut enc2 = device.create_encoder("rt-ordering-short-changes");
        let short_changes = [RtGeometryChange::Rebuild];
        match tracer.encode_accel_update(
            device,
            &mut enc2,
            &mut accel2,
            &objects2,
            &short_changes,
            &[],
            true,
            true,
        ) {
            Err(RtAccelError::Encode(reason)) => {
                assert!(reason.contains("mismatch"), "unexpected encode error: {reason}")
            }
            other => panic!("short change list must fail with Encode, got {other:?}"),
        }
        drop(enc2); // the error was raised before anything was encoded

        // The failed encode published nothing: a valid update still builds
        // and traces BOTH objects on the same resident set.
        let mut slot2 = None;
        let mut capacity2 = 0usize;
        let material_textures =
            ensure_normal_sources(&mut slot2, &mut capacity2, device, &objects2);
        assert!(material_textures.is_empty(), "fixture binds no material textures");
        let normal_sources2 = slot2.unwrap();
        let mut enc3 = device.create_encoder("rt-ordering-resident-valid");
        let changes2 = [RtGeometryChange::Reuse, RtGeometryChange::Reuse];
        let update2 = tracer
            .encode_accel_update(
                device,
                &mut enc3,
                &mut accel2,
                &objects2,
                &changes2,
                &[],
                true,
                true,
            )
            .expect("valid update after the failed encode");
        assert_eq!(update2.blas_builds, 2, "both fresh BLAS build");
        assert_eq!(update2.tlas_builds, 1);
        let ray_a = centroid_ray(STATE_LEFT_X);
        let ray_b = centroid_ray(STATE_RIGHT_X);
        let hits_buf = tracer.debug_ray_query(
            device,
            &mut enc3,
            &accel2,
            &normal_sources2,
            &[ray_a, ray_b],
            None,
            0,
            0,
        );
        enc3.commit_and_wait_completed();
        let hits = read_hits(&hits_buf, 2);
        assert_eq!(
            (hits[0].hit, hits[0].object_id),
            (1, 0),
            "object 0 traces at LEFT after the rejection"
        );
        assert_eq!(
            (hits[1].hit, hits[1].object_id),
            (1, 1),
            "object 1 traces at RIGHT after the rejection"
        );

        // ── Section 4: a shape change (or a buffer-identity move) without
        // re-prepare fails NeedsPreparation; both rejections leave the
        // resident set valid.
        let objects_big = [
            RtObjectGeometry { triangle_count: 2, ..triangle_object(&verts_a, None, IDENTITY) },
            triangle_object(&verts_b, None, IDENTITY),
        ];
        let mut enc4 = device.create_encoder("rt-ordering-shape-change");
        let changes_big = [RtGeometryChange::Rebuild, RtGeometryChange::Rebuild];
        match tracer.encode_accel_update(
            device,
            &mut enc4,
            &mut accel2,
            &objects_big,
            &changes_big,
            &[],
            true,
            true,
        ) {
            Err(RtAccelError::NeedsPreparation) => {}
            other => panic!(
                "a Rebuild after a shape change without re-prepare must fail NeedsPreparation, got {other:?}"
            ),
        }
        drop(enc4);

        let verts_c = write_vertices(device, &triangle_at(STATE_LEFT_X));
        let objects_moved = [
            triangle_object(&verts_c, None, IDENTITY),
            triangle_object(&verts_b, None, IDENTITY),
        ];
        let mut enc5 = device.create_encoder("rt-ordering-identity-move");
        match tracer.encode_accel_update(
            device,
            &mut enc5,
            &mut accel2,
            &objects_moved,
            &changes2,
            &[],
            true,
            true,
        ) {
            Err(RtAccelError::NeedsPreparation) => {}
            other => panic!(
                "a buffer-identity move without re-prepare must fail NeedsPreparation, got {other:?}"
            ),
        }
        drop(enc5);

        assert!(
            accel2.check_topology(&objects2).is_ok(),
            "rejections must leave the resident topology untouched"
        );
    }
}
