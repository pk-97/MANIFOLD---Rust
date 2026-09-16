//! SCENE_MODIFIER_RT_DESIGN.md P0 (BUG-e3p6.4) — deterministic ray-query
//! witnesses for dynamic RT geometry. A0's harness rules: the debug ray
//! query uses the production candidate-hit walk/source tables/descriptors/
//! AS; the CPU oracle is Möller–Trumbore over final GPU geometry bytes
//! (never a copy of modifier math); comparisons use the A0 thresholds
//! (distance ≤ max(1e-4, 1e-4*|expected|), barycentric/UV ≤ 2e-4, normal
//! dot ≥ 0.9999, minimum barycentric coordinate 0.05, no NaN/Inf). Nested
//! modules are named per A0 so `gpu_proofs_gate.py --filter` selects real
//! tests per phase; P0 owns only `rt_dynamic_baseline`.

use std::slice;

use manifold_gpu::raytrace::{
    ensure_normal_sources, DebugRayQueryHit, DebugRayQueryRay, MetalShadowRayTracer,
    RtObjectGeometry, ShadowRayTracer, DEBUG_RAY_INVALID,
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
