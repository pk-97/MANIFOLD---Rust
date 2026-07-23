//! `docs/RAYTRACING_DESIGN.md` §8 Tier-1 item 2 — value-level proof for
//! RT-T1-B's real vertex-normal interpolation
//! (`manifold_gpu::raytrace::{RtNormalSource, ensure_normal_sources,
//! MetalShadowRayTracer::debug_fetch_interpolated_normal}`), replacing the
//! RT trace kernel's depth finite-difference normal reconstruction.
//!
//! Fixture: ONE object, a flat (non-indexed) 2-triangle quad —
//! `render_scene.rs`'s ONLY RT-caster vertex-layout convention (every 3
//! consecutive vertices = 1 triangle, no index buffer):
//!
//! ```text
//! flat vertex list (pos, normal):
//!   0: (-1,-1,0), n=(0,0,1)   \  triangle 0 = {0,1,2}
//!   1: ( 1,-1,0), n=(0,0,1)   |
//!   2: ( 1, 1,0), n=(0,1,0)   /
//!   3: (-1,-1,0), n=(0,0,1)   \  triangle 1 = {3,4,5} (verts 3,4 are the
//!   4: ( 1, 1,0), n=(0,1,0)   |  SAME positions as 0,2 — a flat list
//!   5: (-1, 1,0), n=(1,0,0)   /  duplicates them, no shared index buffer)
//! ```
//!
//! Metal's ray-tracing barycentric convention: `hit = (1-u-v)*v0 + u*v1 +
//! v*v2`. Both triangles' expected interpolated-then-NORMALIZED normal are
//! hand-computed in Python (recorded in the constants below) — `identity`
//! transform, so `RtNormalSource::normal_matrix` is the identity and the
//! CPU oracle needs no matrix multiply, just the barycentric blend +
//! normalize.
//!
//! This dispatches `debug_fetch_interpolated_normal` directly (no ray
//! tracing, no RNG, no acceleration structure) — the interpolation +
//! bindless-fetch math alone is under test, deterministically, exactly the
//! helper `trace_shadow_rays` calls internally for its AO/GI-normal fetch
//! and its GI-bounce hit-normal fetch.

use manifold_gpu::raytrace::{ensure_normal_sources, MetalShadowRayTracer, RtObjectGeometry};

use crate::harness;

/// Flat (non-indexed) vertex layout: 12-byte position, 12-byte normal, no
/// padding — `packed_float3` mandatory (P0 §5.1 kernel lesson), stride 24.
#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertexN {
    pos: [f32; 3],
    normal: [f32; 3],
}

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn write_shared_buffer<T: Copy>(device: &manifold_gpu::GpuDevice, data: &[T]) -> manifold_gpu::GpuBuffer {
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

const TOLERANCE: f32 = 1e-4;

fn assert_close(got: [f32; 3], expected: [f32; 3], label: &str) {
    for i in 0..3 {
        assert!(
            (got[i] - expected[i]).abs() < TOLERANCE,
            "{label}: component {i} — got {got:?}, expected {expected:?} (tolerance {TOLERANCE})"
        );
    }
}

#[test]
fn fetch_interpolated_normal_2tri_matches_cpu_oracle() {
    let h = harness::shared();
    let device = &h.device;

    let verts = [
        PackedVertexN { pos: [-1.0, -1.0, 0.0], normal: [0.0, 0.0, 1.0] }, // tri0 v0
        PackedVertexN { pos: [1.0, -1.0, 0.0], normal: [0.0, 0.0, 1.0] },  // tri0 v1
        PackedVertexN { pos: [1.0, 1.0, 0.0], normal: [0.0, 1.0, 0.0] },   // tri0 v2
        PackedVertexN { pos: [-1.0, -1.0, 0.0], normal: [0.0, 0.0, 1.0] }, // tri1 v0 (dup of vert 0)
        PackedVertexN { pos: [1.0, 1.0, 0.0], normal: [0.0, 1.0, 0.0] },   // tri1 v1 (dup of vert 2)
        PackedVertexN { pos: [-1.0, 1.0, 0.0], normal: [1.0, 0.0, 0.0] },  // tri1 v2
    ];
    let vertex_buffer = write_shared_buffer(device, &verts);

    let objects = [RtObjectGeometry {
        vertex_buffer: &vertex_buffer,
        vertex_stride: std::mem::size_of::<PackedVertexN>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: 2,
        transform: IDENTITY,
        normal_offset: std::mem::size_of::<[f32; 3]>() as u32, // 12: normal follows position
    }];

    let mut normal_sources_slot = None;
    let mut normal_sources_capacity = 0usize;
    ensure_normal_sources(&mut normal_sources_slot, &mut normal_sources_capacity, device, &objects);
    let normal_sources = normal_sources_slot.expect("ensure_normal_sources must allocate");

    let tracer = MetalShadowRayTracer::new(device);

    // Triangle 0, barycentric (u=0.5, v=0.25) => w0=0.25 (vtx0), w1=0.5
    // (vtx1), w2=0.25 (vtx2). CPU oracle (Python):
    // normalize(0.25*(0,0,1) + 0.5*(0,0,1) + 0.25*(0,1,0))
    //   = (0.0, 0.31622776601683794, 0.9486832980505138).
    let got0 = tracer.debug_fetch_interpolated_normal(device, &normal_sources, 0, 0, [0.5, 0.25]);
    assert_close(got0, [0.0, 0.3162_2777, 0.9486_833], "triangle 0");

    // Triangle 1, barycentric (u=0.25, v=0.5) => w0=0.25 (vtx0=dup of
    // vert0), w1=0.25 (vtx1=dup of vert2), w2=0.5 (vtx2). CPU oracle:
    // normalize(0.25*(0,0,1) + 0.25*(0,1,0) + 0.5*(1,0,0))
    //   = (0.8164965809277261, 0.4082482904638631, 0.4082482904638631).
    let got1 = tracer.debug_fetch_interpolated_normal(device, &normal_sources, 0, 1, [0.25, 0.5]);
    assert_close(got1, [0.8164_966, 0.4082_483, 0.4082_483], "triangle 1");
}
