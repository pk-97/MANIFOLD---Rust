//! RAYTRACING_DESIGN.md P1–P3 — Metal ray-query acceleration structures and
//! the shadow/AO/GI-ray dispatch kernel.
//!
//! Ports `tools/rt_prototype/src/accel.rs` (acceleration-structure
//! build/refit) and `tools/rt_prototype/shaders/rt_trace.metal`'s
//! `trace_lighting` + `upsample_lighting` kernels: P1 ported the shadow-only
//! slice; P2 added the AO gather; P3 (§5.2, D4) adds the one-bounce GI
//! gather (emissive-hit + sun-bounce, `gi_spp`/`GiMaterial` below) — the P0
//! prototype's per-triangle `Material`/`mat_index` indirection is unneeded
//! here since P1's per-object BLAS/TLAS layout already makes Metal's own
//! `instance_id` the material index. `ShadowRayTracer` is the D9 backend seam: all data
//! crosses it as manifold-gpu's own cross-backend types (`GpuDevice`,
//! `GpuBuffer`, `GpuTexture`, `GpuEncoder`); Apple/objc2 types stay behind
//! `MetalShadowRayTracer` and this module.
//!
//! A Vulkan implementation (`VK_KHR_ray_query`, activated at trace time
//! from a compute shader rather than a distinct dispatch call) fits this
//! same trait shape: `build_accel`/`refit_accel` map onto
//! `vkCreateAccelerationStructureKHR` + build/update commands,
//! `dispatch_shadow_rays`/`upsample_shadow` onto ordinary compute
//! dispatches that happen to read a ray-query-capable TLAS binding — no
//! per-call shape assumed here is Metal-specific.
//!
//! manifold-gpu's existing pipeline path (`shader_compiler.rs`) is
//! WGSL-only (naga → SPIR-V → MSL) and has no acceleration-structure API
//! (`metal_raytracing` intrinsics and `MTLAccelerationStructure` don't
//! round-trip through naga) — confirmed by the prototype's own `gpu.rs`
//! doc comment. This module compiles the raw MSL source below directly via
//! `MTLDevice::newLibraryWithSource`, exactly as the prototype does, and
//! wraps the resulting `MTLComputePipelineState` in the *same*
//! `GpuComputePipeline`/`SlotMap` types the WGSL path produces (their
//! `state` field is `pub(crate)`, reachable from here) — so dispatch
//! still runs through the one dispatch system a caller already knows,
//! not a parallel one. Only the acceleration-structure binding (no WGSL
//! equivalent) needs a new `GpuEncoder` method,
//! `dispatch_compute_with_accel` in `encoder.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSArray;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLAccelerationStructure, MTLAccelerationStructureCommandEncoder,
    MTLAccelerationStructureGeometryDescriptor, MTLAccelerationStructureInstanceDescriptor,
    MTLAccelerationStructureInstanceOptions, MTLAccelerationStructureTriangleGeometryDescriptor,
    MTLAccelerationStructureUsage, MTLAttributeFormat, MTLCommandBuffer, MTLCommandEncoder,
    MTLCommandQueue, MTLCompileOptions, MTLComputeCommandEncoder, MTLComputePipelineState,
    MTLDevice, MTLInstanceAccelerationStructureDescriptor, MTLIndexType, MTLLanguageVersion,
    MTLLibrary, MTLPackedFloat3, MTLPackedFloat4x3, MTLPrimitiveAccelerationStructureDescriptor,
    MTLSize,
};

use super::device::GpuDevice;
use super::types::{GpuBuffer, GpuComputePipeline, GpuTexture};
use super::{GpuEncoder, Slot, SlotKind, SlotMap};
use crate::types::GpuBinding;

// ─── Acceleration structure: per-object BLAS + one instance TLAS ───────
//
// RT-D3/P1-part-2: render_scene's `objects` are independent meshes, each
// with its own (possibly-animated) world transform — a single flat
// acceleration structure over one combined vertex buffer would need a
// per-frame CPU transform + re-upload of every object's geometry (a
// GPU->CPU->GPU round trip render_scene's other passes never pay). Metal's
// designed answer is a two-level structure: one bottom-level acceleration
// structure (BLAS) per object's LOCAL-space geometry (built directly from
// its existing GPU vertex/index buffers — no CPU involvement), instanced
// into one top-level acceleration structure (TLAS) via a small per-object
// transform-matrix buffer. Moving an object only touches the TLAS's
// (cheap) instance transforms — refit, not rebuild; the BLAS themselves
// are untouched unless a mesh's own vertex data deforms.

/// One object's LOCAL-space bottom-level acceleration structure. P1 never
/// refits a BLAS (only the TLAS's instance transforms move — deforming-
/// mesh per-BLAS refit is P2+ scope, un-suppression trigger for a
/// `descriptor`/`refit_scratch` field re-add here), so only the built
/// `structure` handle needs to survive — kept in `RtAccel.blas` for
/// `object_count()`'s dirty-check guard below and so a future per-BLAS
/// refit is a field access away instead of a rebuild from scratch.
struct Blas {
    structure: Retained<ProtocolObject<dyn MTLAccelerationStructure>>,
}

/// The resident RT scene: N per-object BLAS instanced into one TLAS via
/// `transform`. Built once (scene load / topology change — dirty-checked
/// by the caller, e.g. render_scene.rs's existing shadow-map cache-key
/// idiom); kept resident across frames (RAYTRACING_DESIGN.md P1
/// performer-gesture gate — never built mid-frame).
pub struct RtAccel {
    pub(crate) structure: Retained<ProtocolObject<dyn MTLAccelerationStructure>>,
    descriptor: Retained<MTLInstanceAccelerationStructureDescriptor>,
    refit_scratch: GpuBuffer,
    /// Kept alive: the TLAS descriptor's `instancedAccelerationStructures`
    /// array holds retained references to each BLAS regardless, but owning
    /// them here too makes a future per-BLAS refit (deforming mesh) a
    /// simple field access instead of an NSArray walk.
    blas: Vec<Blas>,
    /// CPU-writable instance-descriptor buffer (transform per object).
    /// Retained here so `refit_accel` can rewrite transforms in place.
    instance_buffer: GpuBuffer,
    /// BUG-308/RT-D4: `build_accel`/`refit_accel` are async (a single
    /// command buffer is `commit()`-ed, never `waitUntilCompleted()`-ed,
    /// mid-frame) — set `true` by that buffer's completion handler once
    /// the GPU has actually finished building/refitting. `render_scene.rs`
    /// must not read this structure via `dispatch_shadow_rays` until this
    /// is `true` (falls back to the raster shadow-map path meanwhile);
    /// starts `false` the instant a fresh build is enqueued, including
    /// across a refit (briefly not-ready while the refit's async build
    /// runs — the OLD instance transforms stay valid to read until then,
    /// this flag exists so the caller can choose to wait for the FRESH
    /// ones instead of racing the read against the in-flight refit).
    pub ready: Arc<AtomicBool>,
}

// Safety: matches every other manifold-gpu resource wrapper (`GpuTexture`,
// `GpuBuffer`, `GpuComputePipeline`, ...) — Metal objects are safe to move
// across threads; MANIFOLD's actual access pattern is single-threaded
// (content thread owns the whole render_scene primitive that holds this).
unsafe impl Send for RtAccel {}
unsafe impl Sync for RtAccel {}

/// One object's geometry + world transform for [`build_accel`]/
/// [`ShadowRayTracer::build_accel`]. `transform` is manifold's own
/// column-major `[[f32; 4]; 4]` convention (matches `render_scene.rs`'s
/// `model_matrix`) — the same layout `render_scene.wgsl`'s `Uniforms.model`
/// already uses. `vertex_buffer`/`vertex_stride`/`vertex_offset` read
/// straight from an existing interleaved vertex buffer (e.g.
/// `render_scene.rs`'s `MeshVertex`, stride 48, position at offset 0) —
/// no position-only repack. `index_buffer: None` means a flat,
/// non-indexed triangle list (every 3 consecutive vertices = 1 triangle
/// — `render_scene.rs`'s own draw convention), matching Metal's
/// triangle-geometry descriptor, which supports either.
pub struct RtObjectGeometry<'a> {
    pub vertex_buffer: &'a GpuBuffer,
    pub vertex_stride: u32,
    pub vertex_offset: u32,
    pub index_buffer: Option<&'a GpuBuffer>,
    pub triangle_count: u32,
    pub transform: [[f32; 4]; 4],
    /// RT-T1-B: byte offset of the per-vertex NORMAL field within one
    /// `vertex_stride`-sized vertex record in `vertex_buffer` — no separate
    /// normal allocation; `MeshVertex` (render_scene.rs's production vertex
    /// layout) already interleaves position/normal/uv, so this just names
    /// where the normal lives (offset 16 for `MeshVertex`). Consumed by
    /// [`build_normal_sources`] to build the per-object bindless indirection
    /// table `trace_shadow_rays` reads at ray-hit time (real interpolated
    /// vertex normals, replacing the depth finite-difference reconstruction
    /// — RAYTRACING_DESIGN.md §8 Tier-1 item 2). A fixture whose geometry
    /// carries no normal data at all (e.g. `rt_p1_shadow.rs`'s
    /// position-only `PackedVertex`) may set this to any value AS LONG AS
    /// `ao_spp`/`gi_spp` stay 0 — the only two consumers of the fetched
    /// normal.
    pub normal_offset: u32,
}

/// Encode this object's BLAS build onto an ALREADY-OPEN acceleration-
/// structure encoder (BUG-308/RT-D4 — see `build_accel`'s doc comment for
/// why this is no longer its own command buffer). Returns the built
/// `Blas` handle (valid to reference immediately — Metal resolves the
/// GPU-side build asynchronously) plus the scratch buffer, which the
/// caller must keep alive until the ENCLOSING command buffer's completion
/// handler fires (the GPU reads it for the duration of the build).
fn encode_blas_build(
    device: &GpuDevice,
    enc: &ProtocolObject<dyn MTLAccelerationStructureCommandEncoder>,
    obj: &RtObjectGeometry,
) -> (Blas, GpuBuffer) {
    let tri_desc = MTLAccelerationStructureTriangleGeometryDescriptor::descriptor();
    tri_desc.setVertexBuffer(Some(obj.vertex_buffer.raw()));
    tri_desc.setVertexFormat(MTLAttributeFormat::Float3);
    tri_desc.setVertexStride(obj.vertex_stride as usize);
    unsafe { tri_desc.setVertexBufferOffset(obj.vertex_offset as usize) };
    if let Some(index_buffer) = obj.index_buffer {
        tri_desc.setIndexBuffer(Some(index_buffer.raw()));
        tri_desc.setIndexType(MTLIndexType::UInt32);
    }
    tri_desc.setTriangleCount(obj.triangle_count as usize);
    tri_desc.setOpaque(true);
    let geom: Retained<MTLAccelerationStructureGeometryDescriptor> = tri_desc.into_super();
    let array = NSArray::from_retained_slice(&[geom]);
    let descriptor = MTLPrimitiveAccelerationStructureDescriptor::descriptor();
    descriptor.setGeometryDescriptors(Some(&array));
    descriptor.setUsage(MTLAccelerationStructureUsage::Refit);

    let raw_device = device.raw_device();
    let sizes = raw_device.accelerationStructureSizesWithDescriptor(&descriptor);
    let structure = raw_device
        .newAccelerationStructureWithSize(sizes.accelerationStructureSize)
        .expect("newAccelerationStructureWithSize failed");
    let scratch = device.create_buffer(sizes.buildScratchBufferSize.max(16) as u64);

    enc.buildAccelerationStructure_descriptor_scratchBuffer_scratchBufferOffset(
        &structure,
        &descriptor,
        scratch.raw(),
        0,
    );

    (Blas { structure }, scratch)
}

/// Column-major `[[f32; 4]; 4]` -> Metal's `MTLPackedFloat4x3` (4 columns,
/// 3 rows — the implicit affine bottom row `[0,0,0,1]` is dropped, matching
/// every transform `render_scene.rs` builds via `model_matrix`).
fn to_packed_4x3(m: [[f32; 4]; 4]) -> MTLPackedFloat4x3 {
    let col = |c: usize| MTLPackedFloat3 {
        x: m[c][0],
        y: m[c][1],
        z: m[c][2],
    };
    MTLPackedFloat4x3 {
        columns: [col(0), col(1), col(2), col(3)],
    }
}

fn build_instance_buffer(device: &GpuDevice, objects: &[RtObjectGeometry]) -> GpuBuffer {
    let stride = std::mem::size_of::<MTLAccelerationStructureInstanceDescriptor>();
    let buf = device.create_buffer_shared((stride * objects.len().max(1)) as u64);
    let ptr = buf
        .mapped_ptr()
        .expect("RT instance-descriptor buffer must be CPU-mapped");
    for (i, obj) in objects.iter().enumerate() {
        let desc = MTLAccelerationStructureInstanceDescriptor {
            transformationMatrix: to_packed_4x3(obj.transform),
            options: MTLAccelerationStructureInstanceOptions::None,
            mask: 0xFF,
            intersectionFunctionTableOffset: 0,
            accelerationStructureIndex: i as u32,
        };
        unsafe {
            std::ptr::write_unaligned(ptr.add(i * stride) as *mut _, desc);
        }
    }
    buf
}

/// Build the resident two-level RT scene over `objects` — one BLAS per
/// object (local-space geometry, no CPU transform) instanced into one
/// TLAS via each object's world `transform`.
///
/// BUG-308/RT-D4: every BLAS build + the TLAS build are encoded onto ONE
/// acceleration-structure command buffer, `commit()`-ed WITHOUT
/// `waitUntilCompleted()` — no synchronous mid-frame stall (RAYTRACING_
/// DESIGN.md P1's no-hitch performer gate: a synchronous wait here cost
/// 110-167ms, a guaranteed dropped-frame class). The caller
/// (`render_scene.rs`) must not use the returned `RtAccel` for a shadow-
/// ray dispatch until `accel.ready` flips `true` (falls back to the
/// raster shadow-map path meanwhile — see BUG-308's backlog entry for the
/// full root-cause history: this ALSO fixes the actual bug, since this
/// same command buffer is committed to the queue strictly after whatever
/// this frame's shared per-frame `GpuEncoder` has already committed by
/// the time this fn runs — `render_scene.rs` only calls this on the frame
/// AFTER a topology/transform change is first observed, once the
/// PREVIOUS frame's mesh-generation writes are guaranteed complete (the
/// per-frame content-thread cycle commits+waits before the next frame's
/// evaluate() ever runs) — never racing this frame's own still-encoding,
/// uncommitted mesh-gen work).
pub(crate) fn build_accel(device: &GpuDevice, objects: &[RtObjectGeometry]) -> RtAccel {
    let cb = device
        .raw_queue()
        .commandBuffer()
        .expect("Failed to acquire command buffer for RT accel build");
    let enc = cb
        .accelerationStructureCommandEncoder()
        .expect("accelerationStructureCommandEncoder failed");

    let mut blas = Vec::with_capacity(objects.len());
    let mut blas_scratch = Vec::with_capacity(objects.len());
    for o in objects {
        let (b, scratch) = encode_blas_build(device, &enc, o);
        blas.push(b);
        blas_scratch.push(scratch);
    }
    let blas_structures: Vec<Retained<ProtocolObject<dyn MTLAccelerationStructure>>> =
        blas.iter().map(|b| b.structure.clone()).collect();
    let instance_buffer = build_instance_buffer(device, objects);

    let descriptor = MTLInstanceAccelerationStructureDescriptor::descriptor();
    descriptor.setInstanceCount(objects.len());
    unsafe {
        descriptor.setInstanceDescriptorBuffer(Some(instance_buffer.raw()));
    }
    descriptor.setInstancedAccelerationStructures(Some(&NSArray::from_retained_slice(&blas_structures)));
    descriptor.setUsage(MTLAccelerationStructureUsage::Refit);

    let raw_device = device.raw_device();
    let sizes = raw_device.accelerationStructureSizesWithDescriptor(&descriptor);
    let structure = raw_device
        .newAccelerationStructureWithSize(sizes.accelerationStructureSize)
        .expect("newAccelerationStructureWithSize failed");
    let build_scratch = device.create_buffer(sizes.buildScratchBufferSize.max(16) as u64);
    let refit_scratch = device.create_buffer(sizes.refitScratchBufferSize.max(16) as u64);

    enc.buildAccelerationStructure_descriptor_scratchBuffer_scratchBufferOffset(
        &structure,
        &descriptor,
        build_scratch.raw(),
        0,
    );
    enc.endEncoding();

    let ready = Arc::new(AtomicBool::new(false));
    add_ready_completion_handler(&cb, Arc::clone(&ready), (blas_scratch, build_scratch));
    cb.commit();

    RtAccel {
        structure,
        descriptor,
        refit_scratch,
        blas,
        instance_buffer,
        ready,
    }
}

/// Register a completion handler on `cb` that flips `ready` once the GPU
/// finishes, keeping `keep_alive` (the build's scratch buffers) referenced
/// until then — they're read by the GPU for the build's whole async
/// duration, so dropping them any earlier (e.g. right after `commit()`
/// returns, as their local-variable scope would otherwise do) would free
/// memory the GPU is still using.
fn add_ready_completion_handler<T: Send + 'static>(
    cb: &ProtocolObject<dyn MTLCommandBuffer>,
    ready: Arc<AtomicBool>,
    keep_alive: T,
) {
    use block2::RcBlock;
    let block = RcBlock::new(move |_buf: std::ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
        let _keep_alive = &keep_alive;
        ready.store(true, Ordering::Release);
    });
    unsafe {
        cb.addCompletedHandler(RcBlock::as_ptr(&block));
    }
}

/// Refit `accel`'s TLAS in place — cheap (instance-transform-only) update,
/// used when an object's transform changes but its topology/vertex count
/// doesn't (so the BLAS list is unchanged). Rewrites the instance buffer's
/// transforms from `objects` first, then refits.
pub(crate) fn refit_accel(device: &GpuDevice, accel: &RtAccel, objects: &[RtObjectGeometry]) {
    debug_assert_eq!(
        objects.len(),
        accel.blas.len(),
        "refit_accel called with a different object COUNT than build_accel built — the BLAS \
         list (and instance buffer) don't match; call build_accel again instead (topology change)"
    );
    let stride = std::mem::size_of::<MTLAccelerationStructureInstanceDescriptor>();
    let ptr = accel
        .instance_buffer
        .mapped_ptr()
        .expect("RT instance-descriptor buffer must be CPU-mapped");
    for (i, obj) in objects.iter().enumerate() {
        unsafe {
            let field_ptr = ptr.add(i * stride) as *mut MTLPackedFloat4x3;
            field_ptr.write_unaligned(to_packed_4x3(obj.transform));
        }
    }

    // BUG-308/RT-D4: async, same as `build_accel` — no mid-frame
    // `waitUntilCompleted()`. Unlike a topology-changing rebuild, refit
    // touches only this ALREADY-BUILT, ALREADY-resident structure's
    // instance transforms (CPU-authored above, no upstream GPU write to
    // race against) — safe to enqueue in the SAME frame the transform
    // changed, no one-frame defer needed (that's `render_scene.rs`'s
    // concern for `build_accel`, not this fn's). `ready` flips false for
    // the refit's async duration so a caller that wants the FRESH
    // transform can wait for it; the OLD transform is still valid to
    // read from `accel.structure` in the meantime (Metal doesn't mutate
    // it destructively until the refit command actually runs).
    accel.ready.store(false, Ordering::Release);
    let cb = device
        .raw_queue()
        .commandBuffer()
        .expect("Failed to acquire command buffer for RT TLAS refit");
    let enc = cb
        .accelerationStructureCommandEncoder()
        .expect("accelerationStructureCommandEncoder failed");
    unsafe {
        enc.refitAccelerationStructure_descriptor_destination_scratchBuffer_scratchBufferOffset(
            &accel.structure,
            &accel.descriptor,
            Some(&accel.structure),
            Some(accel.refit_scratch.raw()),
            0,
        );
    }
    enc.endEncoding();
    add_ready_completion_handler(&cb, Arc::clone(&accel.ready), ());
    cb.commit();
}

// ─── Raw MSL kernels (shadow-only slice of rt_trace.metal) ────────────

/// Shadow-only trim of the prototype's `TraceParams`/`trace_lighting` +
/// `upsample_lighting` kernels. AO (`ao_spp`) and one-bounce GI
/// (`gi_spp`, `Material`/`mat_index` buffers) are P2/P3 scope — dropped,
/// not ported. `packed_float3` is mandatory (P0 §5.1 kernel lesson):
/// bare MSL `float3` is sizeof 16 and desyncs from `#[repr(C)] [f32; 3]`.
const SHADOW_RAYS_MSL: &str = r#"
#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace metal::raytracing;

struct ShadowRayParams {
    packed_float3 sun_dir;   // normalized, points FROM surface TOWARD sun
    float  sun_cone;         // cone half-angle radians; 0.0 = hard shadows
    uint   shadow_spp;
    uint   frame_index;
    uint2  trace_size;       // half-res (mode B, D11)
    uint2  gbuffer_size;     // full-res G-buffer / output resolution
    float  ao_radius;        // RT-P2: world-space AO ray max distance
    uint   ao_spp;           // RT-P2: AO rays/pixel; 0 = AO gather skipped
    // RT-P3 (RAYTRACING_DESIGN.md §5.2 P3, D4): one-bounce GI gather rays
    // per pixel — emissive-hit + sun-bounce (closes the §5.1 "no sun-bounce
    // term" gap). 0 = GI gather skipped, matching the ao_spp==0 discipline.
    uint   gi_spp;
    packed_float3 sun_color;     // RT-P2: premultiplied sun color*intensity
    packed_float3 ambient_color; // RT-P2: flat ambient/env color
    // RT-T1-B: world-space camera eye — origin of the primary visibility
    // ray cast to find the real hit triangle at this pixel (see
    // `fetch_interpolated_normal` below). Unused when ao_spp==0 && gi_spp==0.
    packed_float3 camera_pos;
    // RT-D3: ray origins come from the prepass DEPTH texture + this
    // inverse view-proj — no stored world-pos/normal G-buffer target in
    // P1. Column-major, matches `render_scene.rs`'s `mat4_inverse` output
    // and `render_scene.wgsl`'s `Uniforms.view_proj` convention.
    float4x4 inv_view_proj;
};

// RT-P3: one entry per RT object (SAME order as `RtObjectGeometry`'s
// `objects` slice at accel-build time, which is also Metal's per-instance
// `instance_id` order — the TLAS is built with `accelerationStructureIndex:
// i` for `objects[i]`, so `hit.instance_id` indexes this array directly, no
// separate per-primitive `mat_index` indirection like the P0 prototype
// needed). `packed_float3` mandatory (P0 §5.1 kernel lesson).
struct GiMaterial {
    packed_float3 albedo;   float _p0;
    packed_float3 emissive; float _p1;   // linear HDR, premultiplied by intensity
};

// RT-T1-B (RAYTRACING_DESIGN.md §8 Tier-1 item 2): per-object bindless
// vertex-normal indirection — mirrors the Rust `RtNormalSource` field-for-
// field (P0 §5.1 kernel lesson). `vertex_base_addr` is a raw GPU virtual
// address (`MTLBuffer::gpuAddress()`, CPU-computed once per rebuild);
// `normal_matrix_colN` are the object's world-space normal-transform
// columns (uniform-scale assumption, see the Rust struct's doc comment).
struct RtNormalSource {
    ulong  vertex_base_addr;
    uint   vertex_stride;
    uint   normal_offset;
    packed_float3 normal_matrix_col0;
    packed_float3 normal_matrix_col1;
    packed_float3 normal_matrix_col2;
};

// RT-T1-B: fetch this object's (`src`) vertex `vi`'s LOCAL-space normal via
// its bindless GPU address, then transform to world space with `src`'s
// normal matrix. `vi` is a flat, non-indexed triangle-list vertex index
// (`primitive_id*3 + which_vertex` — render_scene.rs's ONLY RT-caster
// convention today; an indexed RT-caster would need its own index-buffer
// GPU address threaded too — un-suppression trigger if that ever shows up).
static float3 fetch_world_normal(constant RtNormalSource& src, uint vi) {
    device const uchar* base = (device const uchar*)src.vertex_base_addr;
    device const packed_float3* n_ptr =
        (device const packed_float3*)(base + (ulong)vi * (ulong)src.vertex_stride + (ulong)src.normal_offset);
    float3 n_local = float3(*n_ptr);
    float3x3 m = float3x3(float3(src.normal_matrix_col0), float3(src.normal_matrix_col1), float3(src.normal_matrix_col2));
    return m * n_local;
}

// RT-T1-B: barycentric-interpolate the three vertices of triangle
// `primitive_id` (flat, non-indexed layout) in `normal_sources[instance_id]`
// and return the NORMALIZED world-space normal. Metal's ray-tracing
// barycentric convention: hit = (1-u-v)*v0 + u*v1 + v*v2.
static float3 fetch_interpolated_normal(constant RtNormalSource* normal_sources, uint instance_id, uint primitive_id, float2 bary) {
    constant RtNormalSource& src = normal_sources[instance_id];
    uint v0 = primitive_id * 3u, v1 = v0 + 1u, v2 = v0 + 2u;
    float3 n0 = fetch_world_normal(src, v0);
    float3 n1 = fetch_world_normal(src, v1);
    float3 n2 = fetch_world_normal(src, v2);
    float w0 = 1.0 - bary.x - bary.y;
    float3 n = n0 * w0 + n1 * bary.x + n2 * bary.y;
    float len2 = length_squared(n);
    if (!isfinite(len2) || len2 < 1e-12) return float3(0, 1, 0);
    return n * rsqrt(len2);
}

// RT-P2/D3: mirrors the Rust `AccumulateParams` below field-for-field —
// plain POD, no matrix, no alignment surprises.
struct AccumulateParams {
    uint2 size;
    float alpha;
    uint  reset;
};

static uint pcg(uint v) { v = v * 747796405u + 2891336453u; v = ((v >> ((v >> 28u) + 4u)) ^ v) * 277803737u; return (v >> 22u) ^ v; }
static float2 rand2(uint2 p, uint frame, uint ray) {
    uint s = pcg(p.x + pcg(p.y + pcg(frame * 61u + ray)));
    uint t = pcg(s);
    return float2((s & 0xFFFFFFu) / 16777216.0, (t & 0xFFFFFFu) / 16777216.0);
}
static float3 ortho_basis_x(float3 n) {
    return normalize(fabs(n.x) > 0.9 ? cross(n, float3(0, 1, 0)) : cross(n, float3(1, 0, 0)));
}
static float3 cone_sample(float3 dir, float half_angle, float2 u) {
    if (half_angle <= 0.0) return dir;
    float cos_t = mix(1.0, cos(half_angle), u.x);
    float sin_t = sqrt(max(0.0, 1.0 - cos_t * cos_t));
    float phi = 6.2831853 * u.y;
    float3 t = ortho_basis_x(dir), b = cross(dir, t);
    return normalize(t * (sin_t * cos(phi)) + b * (sin_t * sin(phi)) + dir * cos_t);
}
// RT-P2: cosine-weighted hemisphere sample around `n` — ported verbatim
// from `tools/rt_prototype/shaders/rt_trace.metal`'s `cosine_hemisphere`
// (the AO/GI gather this kernel's AO term reuses; GI/emissive gather
// itself stays P3 scope, not ported here). Declared after `ortho_basis_x`
// (which it calls).
static float3 cosine_hemisphere(float3 n, float2 u) {
    float3 t = ortho_basis_x(n), b = cross(n, t);
    float r = sqrt(u.x), phi = 6.2831853 * u.y;
    return normalize(t * (r * cos(phi)) + b * (r * sin(phi)) + n * sqrt(max(0.0, 1.0 - u.x)));
}

// RT-D3: reconstruct world position from a full-res depth texel + the
// inverse view-proj matrix — the SAME NDC<->UV convention
// `render_scene.wgsl`'s `project_to_shadow_uv` uses (`uv.y = -ndc.y*0.5 +
// 0.5`), inverted. `raw_depth` is Metal's native [0,1] clip.z/clip.w
// range (no linearization — `inv_view_proj` already undoes the whole
// projection, linear or not). Returns false (void background — the
// prepass never wrote this texel) via `out_valid` when `raw_depth >=
// 1.0 - 1e-6` (the depth-clear value).
static float3 world_pos_from_depth(uint2 pix, uint2 gbuffer_size, float raw_depth, constant float4x4& inv_view_proj, thread bool& out_valid) {
    if (raw_depth >= 1.0 - 1e-6) { out_valid = false; return float3(0.0); }
    out_valid = true;
    float2 uv = (float2(pix) + 0.5) / float2(gbuffer_size);
    float ndc_x = uv.x * 2.0 - 1.0;
    float ndc_y = 1.0 - uv.y * 2.0;
    float4 clip = float4(ndc_x, ndc_y, raw_depth, 1.0);
    float4 wh = inv_view_proj * clip;
    return wh.xyz / wh.w;
}

// Dispatch: trace_size (half-res, D11) grid. `depth_tex` is the full-res
// opaque-depth prepass (RT-D3 — render_scene.rs's `opaque_depth_snapshot`,
// forced on for RT-enabled scenes). RT-T1-B: the AO/GI cosine-sampling
// normal is a REAL interpolated vertex normal, fetched via a PRIMARY
// visibility ray + [`RtNormalSource`]'s bindless per-object indirection —
// replacing the P1-era screen-space depth finite-difference reconstruction
// (camera-facing, wrong at silhouettes/thin geometry). Output (trace_size): out_sv.r = sun visibility
// [0,1], out_sv.g = AO [0,1] (RT-P2: extends the SAME kernel/dispatch, not
// a parallel pass — RAYTRACING_DESIGN.md §5.2 P2's D16 seam note). out_irr
// (RT-P2): demodulated (no-albedo) irradiance = ambient_color*ao + gi —
// the D3 "accumulate lighting separated from albedo" term, temporally
// accumulated downstream by `accumulate_irradiance`. No direct-sun term:
// the raster light loop owns the sun (see the write site's comment).
kernel void trace_shadow_rays(
    instance_acceleration_structure  accel          [[buffer(0)]],
    constant ShadowRayParams&        p              [[buffer(1)]],
    constant GiMaterial*             gi_materials   [[buffer(2)]],
    constant RtNormalSource*         normal_sources [[buffer(3)]],
    depth2d<float>                   depth_tex      [[texture(0)]],
    texture2d<float, access::write>  out_sv         [[texture(1)]],
    texture2d<float, access::write>  out_irr        [[texture(2)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.trace_size.x || tid.y >= p.trace_size.y) return;
    uint2 gpix = min(uint2((float2(tid) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size)), p.gbuffer_size - 1);

    bool valid;
    float3 wp = world_pos_from_depth(gpix, p.gbuffer_size, depth_tex.read(gpix, 0), p.inv_view_proj, valid);
    if (!valid) {
        // Void background: unoccluded either way (matches the prototype's
        // `out_sv.write(float4(1,1,0,0), tid)` void case) — irradiance is
        // ambient-only (no surface to shadow-test against).
        out_sv.write(float4(1, 1, 0, 0), tid);
        out_irr.write(float4(p.ambient_color, 0), tid);
        return;
    }
    // Neighbor world positions (screen-space reconstruction, RT-D3) — kept
    // ONLY for `texel_scale` below (the bias epsilon's scale-awareness);
    // RT-T1-B moved normal reconstruction off this finite difference (see
    // the primary-ray cast below). Falls back to the +x/+y neighbor's delta
    // alone at the image edge.
    uint2 gx = min(gpix + uint2(1, 0), p.gbuffer_size - 1);
    uint2 gy = min(gpix + uint2(0, 1), p.gbuffer_size - 1);
    bool vx, vy;
    float3 wpx = world_pos_from_depth(gx, p.gbuffer_size, depth_tex.read(gx, 0), p.inv_view_proj, vx);
    float3 wpy = world_pos_from_depth(gy, p.gbuffer_size, depth_tex.read(gy, 0), p.inv_view_proj, vy);

    // RT-T1-B (RAYTRACING_DESIGN.md §8 Tier-1 item 2): real interpolated
    // vertex normal via a PRIMARY visibility ray from the camera through
    // `wp` — only cast when a consumer needs it (AO/GI cosine-hemisphere
    // sampling below; the shadow ray itself biases along `sun_dir`, not
    // `n` — BUG-309 follow-up, further down). Falls back to a default
    // up-normal if the primary ray somehow misses (should not happen: `wp`
    // itself came from this same accel's geometry via the depth prepass,
    // but a grazing-angle/epsilon edge case shouldn't crash the kernel).
    float3 n = float3(0, 1, 0);
    if (p.ao_spp > 0u || p.gi_spp > 0u) {
        float3 to_surface = wp - float3(p.camera_pos);
        float dist = length(to_surface);
        if (dist > 1e-6) {
            intersector<triangle_data, instancing> primary_i;
            primary_i.assume_geometry_type(geometry_type::triangle);
            primary_i.force_opacity(forced_opacity::opaque);
            ray pr;
            pr.origin = float3(p.camera_pos);
            pr.direction = to_surface / dist;
            pr.min_distance = 0.0;
            pr.max_distance = dist + dist * 1e-3 + 1e-4;
            auto phit = primary_i.intersect(pr, accel);
            if (phit.type != intersection_type::none) {
                n = fetch_interpolated_normal(normal_sources, phit.instance_id, phit.primitive_id, phit.triangle_barycentric_coord);
            }
        }
    }
    // BUG-309: a FIXED 1e-3 world-unit bias self-intersects almost
    // everywhere at real scene scale (confirmed via a per-pixel hit-t
    // dump: median false-hit distance ~1.8e-4, ~500x below even a
    // generous 1e-2*scene-scale self-intersection threshold, while the
    // OCCLUDER's real shadow hits land at ~1.0-1.5 — i.e. self-
    // intersection, not a mislocated shadow). `texel_scale` is the
    // world-space distance this SCREEN PIXEL step covers (the same
    // `wpx`/`wpy` neighbor deltas already computed for `n`) — it grows
    // with view distance and surface obliquity exactly the way the bias
    // needs to (RT-D4 debug pass's brief: "constant epsilon that works up
    // close fails at scene scale"), with no new per-frame CPU parameter.
    // MAX, not MIN: taking the smaller neighbor delta sounded safer but a
    // per-pixel dump showed EITHER axis (or both) can legitimately spike
    // at grazing/near-horizon angles (a tiny screen-space step covering a
    // huge world-space distance under perspective) — MIN just meant
    // whichever axis happened to be small that pixel, still occasionally
    // letting a huge bias through. MAX is the one that actually needs
    // capping, not avoiding: `BIAS_EPS_CAP` below is a hard, ABSOLUTE
    // ceiling (independent of scene scale, unlike the rest of this
    // epsilon) that exists ONLY to catch the pathological case a per-
    // pixel derivative can't rule out in-kernel — the 2x1 synthetic
    // fixture (`rt_p1_shadow.rs`) is the sharpest example: one axis has
    // zero resolution, so its neighbor delta is a full frustum-width
    // jump, and an uncapped `texel_scale*2.0` (~2.0 world units, vs. the
    // fixture's occluder ~0.7 units away) biased the ray clean past it.
    const float BIAS_EPS_CAP = 0.02;
    float texel_scale = max(length(wpx - wp), length(wpy - wp));
    if (!isfinite(texel_scale) || texel_scale < 1e-6) {
        texel_scale = 1e-3; // degenerate/singular reconstruction fallback
    }
    float bias_eps = min(texel_scale * 2.0, BIAS_EPS_CAP);
    // BUG-309 follow-up: bias along `sun_dir` ONLY, not `n` — originally
    // because the (now-removed) depth finite-difference normal was noisy
    // at this scene's depth-precision scale and produced a visibly
    // scattered, wide false-shadow footprint even after the epsilon-scale
    // fix above. RT-T1-B's `n` is a real interpolated vertex normal now
    // (no longer noisy), but `sun_dir` stays the bias direction anyway —
    // it's exact (a CPU-computed light direction, never reconstructed) and
    // this bias is a shadow-ray-only concern unrelated to AO/GI's `n`
    // consumers; changing it is a separate, unscoped decision (T1-B's
    // brief is normals, not shadow-bias direction).
    float3 origin = wp + p.sun_dir * bias_eps;

    intersector<triangle_data, instancing> shadow_i;
    shadow_i.assume_geometry_type(geometry_type::triangle);
    shadow_i.force_opacity(forced_opacity::opaque);
    shadow_i.accept_any_intersection(true);

    ray r;
    r.origin = origin;
    // t_min: reject any hit closer than the bias itself outright — the
    // in-kernel self-intersection filter (Fable's brief's "often the
    // cleanest fix") on top of the scale-aware origin offset above, so a
    // pathological normal/winding case that still lands inside its own
    // triangle can't register as a false shadow.
    r.min_distance = bias_eps * 0.5;
    r.max_distance = INFINITY;

    uint spp = max(p.shadow_spp, 1u);
    float vis = 0.0;
    for (uint s = 0; s < spp; s++) {
        r.direction = cone_sample(p.sun_dir, p.sun_cone, rand2(tid, p.frame_index, s));
        if (shadow_i.intersect(r, accel).type == intersection_type::none) vis += 1.0;
    }
    vis /= float(spp);

    // RT-P2: AO gather — cosine-weighted hemisphere around the SAME bias
    // normal/origin the shadow ray uses (ported from the prototype's
    // `trace_lighting`'s `ao` block; the emissive/env one-bounce GI term
    // that kernel also computes is P3 scope, not ported here). `ao_spp ==
    // 0` skips the gather outright (ao stays 1.0 = no darkening),
    // matching P1's shadow_spp==0-never-happens discipline but explicit
    // here since AO is the new, optional term.
    float ao = 1.0;
    if (p.ao_spp > 0) {
        ao = 0.0;
        ray ao_r;
        ao_r.origin = origin;
        ao_r.min_distance = bias_eps * 0.5;
        ao_r.max_distance = p.ao_radius;
        for (uint s = 0; s < p.ao_spp; s++) {
            ao_r.direction = cosine_hemisphere(n, rand2(tid, p.frame_index, 100u + s));
            if (shadow_i.intersect(ao_r, accel).type == intersection_type::none) ao += 1.0;
        }
        ao /= float(p.ao_spp);
    }
    out_sv.write(float4(vis, ao, 0, 0), tid);

    // RT-P3 (RAYTRACING_DESIGN.md §5.2 P3, D4): one-bounce GI gather —
    // ported from the P0 prototype's `trace_lighting` GI block (ARC
    // `rt_trace.metal`'s "one-bounce gather: emissive on hit, env on
    // miss"), extended with the sun-bounce term the P0 §5.1 results
    // explicitly flagged as missing ("P0's GI gathers env+emissive only,
    // no sun-bounce term"). Reuses the SAME bias origin/normal the
    // shadow+AO rays above already computed — one dispatch, not a
    // parallel pass (D16's seam note). Demodulated (no local albedo
    // multiply — same D3 discipline as the sun/AO terms above); env-miss
    // contributes NOTHING here (not double-counted with `ambient_color *
    // ao` above, which is this kernel's existing flat-env term — the P0
    // prototype had no separate ambient/AO term to double against, ours
    // does, so the gather's own job narrows to emissive + sun-bounce).
    float3 gi = float3(0.0);
    if (p.gi_spp > 0) {
        intersector<triangle_data, instancing> gi_i;
        gi_i.assume_geometry_type(geometry_type::triangle);
        gi_i.force_opacity(forced_opacity::opaque);
        ray gr;
        gr.origin = origin;
        gr.min_distance = bias_eps * 0.5;
        gr.max_distance = INFINITY;
        for (uint s = 0; s < p.gi_spp; s++) {
            gr.direction = cosine_hemisphere(n, rand2(tid, p.frame_index, 300u + s));
            auto hit = gi_i.intersect(gr, accel);
            if (hit.type != intersection_type::none) {
                uint oi = hit.instance_id;
                float3 hit_emissive = float3(gi_materials[oi].emissive);
                float3 hit_albedo = float3(gi_materials[oi].albedo);
                // Sun-bounce: does sunlight reach the GI ray's hit point?
                // One more any-hit ray, hit-point origin, same cone
                // sampling as the primary shadow ray above. RT-T1-B: the
                // hit-surface normal is now REAL (interpolated via
                // [`RtNormalSource`], same GI ray's own hit — no extra
                // trace needed), replacing the flat average-cosine
                // stand-in this bounce used before a per-object
                // vertex-normal buffer existed.
                float3 hit_pos = gr.origin + gr.direction * hit.distance;
                float3 hit_n = fetch_interpolated_normal(normal_sources, hit.instance_id, hit.primitive_id, hit.triangle_barycentric_coord);
                ray sun_r;
                sun_r.origin = hit_pos + p.sun_dir * bias_eps;
                sun_r.direction = cone_sample(p.sun_dir, p.sun_cone, rand2(tid, p.frame_index, 400u + s));
                sun_r.min_distance = bias_eps * 0.5;
                sun_r.max_distance = INFINITY;
                float hit_sun_vis = (shadow_i.intersect(sun_r, accel).type == intersection_type::none) ? 1.0 : 0.0;
                float hit_ndotl = max(dot(hit_n, p.sun_dir), 0.0);
                // Named, documented, tunable (RAYTRACING_DESIGN.md §5.2 P2's
                // "denoiser/accumulation parameters are named constants"
                // rule, extended to P3/T1-B): folds the diffuse BRDF's 1/pi
                // energy normalization into one scale factor (the RECEIVING
                // point's own albedo divide happens once downstream in
                // `render_scene.wgsl`, per D3's demodulated-irradiance
                // discipline) — `hit_ndotl` above now supplies the real
                // cosine term this scale used to approximate outright.
                // Peter's morning gate tuned this range against the OLD
                // flat-cosine stand-in; `hit_ndotl` only ever makes the
                // bounce dimmer or equal (never brighter) than that
                // baseline, so the committed 0.02-0.3 range still holds.
                const float SUN_BOUNCE_INTENSITY_SCALE = 0.08;
                float3 bounce = hit_albedo * float3(p.sun_color) * hit_sun_vis * hit_ndotl * SUN_BOUNCE_INTENSITY_SCALE;
                gi += hit_emissive + bounce;
            }
        }
        gi /= float(p.gi_spp);
    }

    // RT-P2/D3: demodulated irradiance — AO-occluded flat ambient plus
    // RT-P3's gathered emissive/sun-bounce term. NO direct-sun term
    // (Peter 2026-07-23): `render_scene.wgsl`'s raster light loop already
    // shades the sun with the full material model (specular, clearcoat)
    // using this dispatch's shadow mask for visibility, and it consumes
    // this texture as its ambient slot on top — a sun*n·l*vis copy here
    // was counted twice and blew every sunlit surface out. No albedo
    // multiply here either (that happens once, downstream, in
    // `render_scene.wgsl` — D3's "accumulate lighting separated from
    // albedo" is what lets a same-clip light-intensity strobe keep
    // temporal history instead of being treated as a cut).
    float3 irradiance = float3(p.ambient_color) * ao + gi;
    out_irr.write(float4(irradiance, 0), tid);
}

// Depth-aware bilateral upsample: half-res (sun-visibility, AO) + demod.
// irradiance -> full res (RT-D3's "D11 trivial pass"; RT-P2 widens the
// SAME kernel to also carry the AO channel + the irradiance texture — one
// dispatch, one guide, not a second upsample pass). Guide: full-res depth
// only (raw NDC z — comparable directly without linearizing, since nearby
// screen pixels at similar depth have proportionally similar raw-z
// regardless of the projection's nonlinearity).
kernel void upsample_shadow(
    constant ShadowRayParams&       p         [[buffer(1)]],
    depth2d<float>                  depth_tex [[texture(0)]],
    texture2d<float>                lo_sv     [[texture(1)]],
    texture2d<float, access::write> hi_sv     [[texture(2)]],
    texture2d<float>                lo_irr    [[texture(3)]],
    texture2d<float, access::write> hi_irr    [[texture(4)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.gbuffer_size.x || tid.y >= p.gbuffer_size.y) return;
    float d = depth_tex.read(tid, 0);
    if (d >= 1.0 - 1e-6) {
        hi_sv.write(float4(1, 1, 0, 0), tid);
        hi_irr.write(float4(p.ambient_color, 0), tid);
        return;
    }

    float2 lo_uv = (float2(tid) + 0.5) / float2(p.gbuffer_size) * float2(p.trace_size);
    int2 lo_c = int2(lo_uv - 0.5);
    float2 acc_sv = 0.0; float3 acc_irr = 0.0; float wsum = 0.0;
    for (int dy = 0; dy <= 1; dy++)
    for (int dx = 0; dx <= 1; dx++) {
        int2 q = clamp(lo_c + int2(dx, dy), int2(0), int2(p.trace_size) - 1);
        uint2 gq = min(uint2((float2(q) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size)), p.gbuffer_size - 1);
        float qd = depth_tex.read(gq, 0);
        float2 f = saturate(1.0 - fabs(lo_uv - 0.5 - float2(q)));
        float w_bilin = f.x * f.y;
        float w_depth = exp(-fabs(qd - d) / 0.001);
        float w = max(w_bilin * w_depth, 1e-5);
        acc_sv += lo_sv.read(uint2(q)).rg * w;
        acc_irr += lo_irr.read(uint2(q)).rgb * w;
        wsum += w;
    }
    hi_sv.write(float4(acc_sv / wsum, 0, 0), tid);
    hi_irr.write(float4(acc_irr / wsum, 0), tid);
}

// RT-P2/D3: temporal accumulation of the demodulated irradiance texture —
// the next stage of the SAME lighting pass (not a parallel denoiser
// system). `reset` (driven by the SHARED
// `crate::node_graph::temporal_reset::TemporalResetDetector` — RT-D2; the
// negative-rg gate enforces there is exactly one reset-detection call
// site) discards history outright (cold start / post-cut); otherwise an
// exponential moving average toward this frame's value at `alpha` keeps
// history — this is the numeric mechanism that makes a same-clip light-
// intensity strobe differ from a cold-start render (D3's "strobes are not
// cuts"). `history` is read_write: read this frame's stale value, write
// the blended (or copied) result in place.
kernel void accumulate_irradiance(
    constant AccumulateParams&           p       [[buffer(1)]],
    texture2d<float>                     hi_irr  [[texture(0)]],
    texture2d<float, access::read_write> history [[texture(1)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.size.x || tid.y >= p.size.y) return;
    float4 cur = hi_irr.read(tid);
    if (p.reset != 0u) {
        history.write(cur, tid);
        return;
    }
    float4 prev = history.read(tid);
    history.write(mix(prev, cur, p.alpha), tid);
}

// RT-T1-B value-level test surface ONLY (`docs/RAYTRACING_DESIGN.md` §8
// Tier-1 item 2's gate: "kernel-visible normal for a known 2-triangle
// fixture matches CPU expected"). Exercises the EXACT SAME
// `fetch_interpolated_normal` helper `trace_shadow_rays` calls internally,
// against caller-supplied instance/primitive/barycentric inputs — no ray
// tracing or RNG involved, so the interpolation math alone is under test,
// deterministically. Not part of the production dispatch path (never
// called by `render_scene.rs`) — see `manifold_gpu::raytrace::
// debug_fetch_interpolated_normal`, its only caller.
struct DebugFetchNormalParams {
    uint instance_id;
    uint primitive_id;
    packed_float2 bary;
};

kernel void debug_fetch_interpolated_normal(
    constant RtNormalSource*         normal_sources [[buffer(0)]],
    constant DebugFetchNormalParams& p              [[buffer(1)]],
    device packed_float3*            out_normal     [[buffer(2)]],
    uint tid [[thread_position_in_grid]])
{
    if (tid != 0u) return;
    float3 n = fetch_interpolated_normal(normal_sources, p.instance_id, p.primitive_id, float2(p.bary));
    out_normal[0] = packed_float3(n);
}
"#;

/// CPU mirror of `ShadowRayParams` above — field order and packing MUST
/// match exactly (P0 §5.1 kernel lesson: `packed_float3` in MSL == dense
/// `[f32; 3]` here, no padding).
///
/// RAYTRACING_DESIGN.md §5.2 P2 extended this in place (same struct, same
/// binding(1) slot, same single half-res dispatch — D11/D16's "P2 joins
/// the SAME half-res dispatch and SAME upsample" seam, not a parallel
/// pass): `ao_radius`/`ao_spp` drive the added AO-ray gather, `sun_color`/
/// `ambient_color` are the demodulated-irradiance term's inputs (no
/// albedo folded in here — that happens once, downstream, in
/// `render_scene.wgsl`'s shading step, per D3's "accumulate lighting
/// separated from albedo").
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ShadowRayParams {
    pub sun_dir: [f32; 3],
    pub sun_cone: f32,
    pub shadow_spp: u32,
    pub frame_index: u32,
    pub trace_size: [u32; 2],
    pub gbuffer_size: [u32; 2],
    /// World-space max AO ray distance (RT-P2). 0 samples (`ao_spp == 0`)
    /// skips the AO gather entirely, leaving `out_sv.g` at its cleared
    /// value.
    pub ao_radius: f32,
    /// AO rays per pixel (RT-P2 half-res dispatch).
    pub ao_spp: u32,
    /// RT-P3: one-bounce GI gather rays/pixel (emissive-hit + sun-bounce).
    /// 0 skips the gather entirely (same discipline as `ao_spp == 0`).
    pub gi_spp: u32,
    /// Sun light color, PREMULTIPLIED with intensity (linear HDR) — same
    /// convention as `render_scene.rs`'s `Light::color`.
    pub sun_color: [f32; 3],
    /// Flat ambient/env color (scene `atmosphere.ambient_tint` scaled by
    /// a named constant — RAYTRACING_DESIGN.md §5.2 P2's "denoiser/
    /// accumulation parameters are named constants" rule; the exact
    /// intensity is Peter's morning-gate tuning call, not baked in here).
    pub ambient_color: [f32; 3],
    /// RT-T1-B: world-space camera eye position — the origin of the
    /// PRIMARY visibility ray `trace_shadow_rays` now casts (closest-hit,
    /// toward the depth-reconstructed `wp`) to find which triangle/instance
    /// is actually visible at this pixel, so the AO/GI cosine-hemisphere
    /// sampling normal can be a REAL interpolated vertex normal (via
    /// [`RtNormalSource`]) instead of a depth finite-difference
    /// reconstruction. Unused (may be left zeroed) when `ao_spp == 0 &&
    /// gi_spp == 0` — the only two consumers of that normal.
    pub camera_pos: [f32; 3],
    /// MSL's `float4x4` requires 16-byte alignment; the 88 bytes above it
    /// need 8 more to reach the next 16-byte boundary (96) — RT-T1-B added
    /// `camera_pos` (12 bytes) to the prefix, shrinking this pad from 4 to
    /// 2 `u32`s; the total struct size (160) and `inv_view_proj`'s offset
    /// (96) are UNCHANGED from what they'd otherwise be (see the offset/
    /// size asserts below). `#[repr(C)]` does NOT know `[[f32; 4]; 4]`
    /// needs 16-byte alignment (its natural alignment is 4, from `f32`) —
    /// without this pad, the GPU reads `inv_view_proj` starting early, same
    /// alignment-gotcha class as the `packed_float3` lesson (P0 §5.1), just
    /// for a matrix instead of a vec3. Caught by the offset assert below —
    /// don't resize this padding without re-deriving the offset.
    _pad_align_mat4: [u32; 2],
    /// Column-major, matches `render_scene.rs`'s `mat4_inverse` output.
    pub inv_view_proj: [[f32; 4]; 4],
}

impl ShadowRayParams {
    /// Construct with the alignment padding zeroed — callers never set
    /// `_pad_align_mat4` directly (it's not `pub`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sun_dir: [f32; 3],
        sun_cone: f32,
        shadow_spp: u32,
        frame_index: u32,
        trace_size: [u32; 2],
        gbuffer_size: [u32; 2],
        ao_radius: f32,
        ao_spp: u32,
        gi_spp: u32,
        sun_color: [f32; 3],
        ambient_color: [f32; 3],
        camera_pos: [f32; 3],
        inv_view_proj: [[f32; 4]; 4],
    ) -> Self {
        Self {
            sun_dir,
            sun_cone,
            shadow_spp,
            frame_index,
            trace_size,
            gbuffer_size,
            ao_radius,
            ao_spp,
            gi_spp,
            sun_color,
            ambient_color,
            camera_pos,
            _pad_align_mat4: [0; 2],
            inv_view_proj,
        }
    }
}

/// CPU mirror of the MSL `GiMaterial` struct — RT-P3's per-instance
/// emissive/albedo table for the GI gather's emissive-hit + sun-bounce
/// terms. Field order and packing MUST match exactly (P0 §5.1 kernel
/// lesson).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GiMaterial {
    pub albedo: [f32; 3],
    _pad0: f32,
    pub emissive: [f32; 3],
    _pad1: f32,
}

const _: () = assert!(std::mem::size_of::<GiMaterial>() == 32);

impl GiMaterial {
    pub fn new(albedo: [f32; 3], emissive: [f32; 3]) -> Self {
        Self {
            albedo,
            _pad0: 0.0,
            emissive,
            _pad1: 0.0,
        }
    }
}

// RT-D3/RT-P2 alignment gotcha (see `_pad_align_mat4`'s doc comment): this
// is the regression guard a GPU test alone wouldn't localize as clearly —
// if `inv_view_proj`'s offset ever drifts from 96 again (a field
// reordered/resized above it), this fails at compile time instead of
// silently reading garbage on the GPU.
const _: () = assert!(std::mem::offset_of!(ShadowRayParams, inv_view_proj) == 96);
const _: () = assert!(std::mem::size_of::<ShadowRayParams>() == 160);

/// RT-T1-B (RAYTRACING_DESIGN.md §8 Tier-1 item 2): per-object bindless
/// indirection for real vertex-normal interpolation in the RT trace kernel
/// — one entry per object, SAME order as the `objects` slice `build_accel`
/// was called with (so `hit.instance_id` at any ray hit indexes this
/// directly, identical convention to [`GiMaterial`]). `vertex_base_addr` is
/// `MTLBuffer::gpuAddress()` (via [`GpuBuffer::gpu_address`]) PLUS the
/// object's `vertex_offset` already folded in — the kernel reads
/// `vertex_base_addr + vertex_index * vertex_stride + normal_offset` as a
/// raw `packed_float3`. Reading an arbitrary object's vertex buffer this
/// way needs no separate `useResource` call: the SAME buffers are already
/// referenced by the bound acceleration structure (`build_accel`'s BLAS
/// geometry descriptors), and Metal makes every resource an acceleration
/// structure transitively references resident when the structure itself is
/// bound (`setAccelerationStructure_atBufferIndex`) — confirmed by this
/// exact kernel already ray-tracing against these same buffers for the
/// hardware intersection test.
///
/// `normal_matrix` is the object's WORLD-space transform for normals — RT-
/// T1-B takes the model matrix's upper-left 3x3 directly (a NAMED,
/// documented simplification: correct for uniform scale, wrong for
/// non-uniform scale, which needs the inverse-transpose instead — same
/// "named, documented simplification, not invented physics" discipline as
/// `SUN_BOUNCE_INTENSITY_SCALE` above; un-suppression trigger: a real
/// RT-caster scene using non-uniform scale on an RT-shadowed object).
/// Column-major, 3 `packed_float3` columns in MSL.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RtNormalSource {
    pub vertex_base_addr: u64,
    pub vertex_stride: u32,
    pub normal_offset: u32,
    pub normal_matrix: [[f32; 3]; 3],
}

const _: () = assert!(std::mem::size_of::<RtNormalSource>() == 56);

/// Column-major `[[f32; 4]; 4]` model matrix -> its upper-left 3x3 (see
/// [`RtNormalSource`]'s doc comment for the uniform-scale assumption).
fn normal_matrix_from_model(m: [[f32; 4]; 4]) -> [[f32; 3]; 3] {
    [
        [m[0][0], m[0][1], m[0][2]],
        [m[1][0], m[1][1], m[1][2]],
        [m[2][0], m[2][1], m[2][2]],
    ]
}

/// (Re)allocate-if-needed + rewrite in place the per-object
/// [`RtNormalSource`] indirection table from the SAME `objects` slice
/// `build_accel`/`refit_accel` use — same "grow, never shrink-then-
/// reallocate every frame" idiom as `render_scene.rs`'s `ensure_rt_gi_
/// materials`; rewritten every RT-ready frame (cheap: N small POD structs,
/// same cadence as that file's `gi_materials_data` rebuild). Never requires
/// a GPU readback of the actual vertex data itself — the bindless address
/// does that lookup on the GPU, at ray-hit time.
pub fn ensure_normal_sources(
    slot: &mut Option<GpuBuffer>,
    capacity: &mut usize,
    device: &GpuDevice,
    objects: &[RtObjectGeometry],
) {
    let needed = objects.len().max(1);
    if slot.is_none() || *capacity < needed {
        *slot = Some(device.create_buffer_shared((needed * std::mem::size_of::<RtNormalSource>()) as u64));
        *capacity = needed;
    }
    let buf = slot.as_ref().expect("just ensured above");
    let ptr = buf
        .mapped_ptr()
        .expect("RT normal-source buffer must be CPU-mapped");
    for (i, obj) in objects.iter().enumerate() {
        let src = RtNormalSource {
            vertex_base_addr: obj.vertex_buffer.gpu_address() + obj.vertex_offset as u64,
            vertex_stride: obj.vertex_stride,
            normal_offset: obj.normal_offset,
            normal_matrix: normal_matrix_from_model(obj.transform),
        };
        unsafe {
            std::ptr::write_unaligned(ptr.add(i * std::mem::size_of::<RtNormalSource>()) as *mut _, src);
        }
    }
}

/// CPU mirror of the MSL `AccumulateParams` struct backing
/// `accumulate_irradiance` — RAYTRACING_DESIGN.md §5.2 P2/D3's temporal-
/// accumulation reset. Plain POD, no alignment surprises (no matrix
/// field).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AccumulateParams {
    pub size: [u32; 2],
    pub alpha: f32,
    /// Non-zero: this frame COPIES `current` into `history` (cold start /
    /// post-cut — RT-D2's `TemporalResetDetector`), discarding whatever
    /// history held. Zero: blend `history` toward `current` by `alpha`
    /// (D3's "strobes are not cuts" case — a same-clip light-intensity
    /// flip keeps the blend, which is exactly what makes the numeric
    /// strobe-proof differ from a cold start).
    pub reset: u32,
}

const _: () = assert!(std::mem::size_of::<AccumulateParams>() == 16);

impl AccumulateParams {
    pub fn new(size: [u32; 2], alpha: f32, reset: bool) -> Self {
        Self {
            size,
            alpha,
            reset: reset as u32,
        }
    }
}

const SHADOW_WORKGROUP: [u32; 3] = [8, 8, 1];

fn dispatch_groups_2d(size: [u32; 2], workgroup: [u32; 3]) -> [u32; 3] {
    [
        size[0].div_ceil(workgroup[0]),
        size[1].div_ceil(workgroup[1]),
        1,
    ]
}

fn compile_pipeline(
    device: &GpuDevice,
    library: &ProtocolObject<dyn MTLLibrary>,
    entry: &str,
    slot_map: SlotMap,
) -> GpuComputePipeline {
    let name = NSString::from_str(entry);
    let func = library
        .newFunctionWithName(&name)
        .unwrap_or_else(|| panic!("RT kernel entry point '{entry}' not found"));
    let state: Retained<ProtocolObject<dyn MTLComputePipelineState>> = device
        .raw_device()
        .newComputePipelineStateWithFunction_error(&func)
        .unwrap_or_else(|e| panic!("{entry}: compute PSO error: {}", e.localizedDescription()));
    GpuComputePipeline {
        state,
        slot_map,
        label: entry.to_string(),
        workgroup_size: SHADOW_WORKGROUP,
        needs_sizes_buffer: false,
    }
}

fn identity_slot_map(bindings: &[(u32, SlotKind)]) -> SlotMap {
    let mut map = SlotMap::new();
    for (binding, kind) in bindings {
        map.insert(
            *binding,
            Slot {
                kind: *kind,
                metal_index: *binding,
            },
        );
    }
    map
}

// ─── Backend seam (D9) ──────────────────────────────────────────────────

/// Hardware ray-tracing seam for the RAYTRACING_DESIGN.md hard-shadow-ray
/// pass. Metal ray queries implement this now (`MetalShadowRayTracer`);
/// Vulkan `VK_KHR_ray_query` fits the same method shape when the Vulkan
/// backend lands (D9) — no method here assumes a Metal-specific call
/// order beyond "build once, dispatch many, refit only for deforming
/// geometry".
pub trait ShadowRayTracer {
    /// Backend-specific resident acceleration structure handle.
    type Accel;

    /// Build the resident two-level RT scene (one BLAS per object,
    /// instanced into one TLAS — see the module doc). Call once at scene
    /// load / topology change for an RT-enabled scene; never mid-frame.
    fn build_accel(&self, device: &GpuDevice, objects: &[RtObjectGeometry]) -> Self::Accel;

    /// Refit `accel`'s instance transforms in place from `objects` — cheap
    /// (TLAS-only update), used when objects move but the object SET and
    /// each object's topology are unchanged (mirrors `objects.len()` and
    /// vertex/index buffer identity against what `accel` was built from —
    /// caller's dirty-check, e.g. render_scene.rs's shadow-map cache-key
    /// idiom). A topology change calls `build_accel` again instead.
    fn refit_accel(&self, device: &GpuDevice, accel: &Self::Accel, objects: &[RtObjectGeometry]);

    /// Dispatch the half-res shadow/AO-ray pass (RT-D3; RT-P2 widens this
    /// SAME dispatch to add the AO gather + demodulated-irradiance term —
    /// D16's seam note, not a parallel pass; RT-P3 widens it again with the
    /// emissive/sun-bounce GI gather, reading `gi_materials` — one entry
    /// per object, SAME order as the `objects` slice `build_accel` was
    /// called with, so `instance_id` at a GI ray hit indexes it directly):
    /// ray origins + bias normal reconstructed in-kernel from `depth_tex`
    /// (the full-res opaque-depth prepass) + `params.inv_view_proj` — no
    /// world-pos/normal G-buffer target. Writes (sun visibility, AO) to
    /// `out_sv` and demodulated irradiance (now including the GI gather)
    /// to `out_irr`, both at `params.trace_size`. RT-T1-B: `normal_sources`
    /// is the per-object [`RtNormalSource`] bindless table (built via
    /// [`build_normal_sources`] from the SAME `objects` slice `accel` was
    /// built from) — feeds the primary-ray-cast real vertex normal AO/GI
    /// sample against, and the GI bounce's hit-point normal.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_shadow_rays(
        &self,
        encoder: &mut GpuEncoder,
        accel: &Self::Accel,
        params: &ShadowRayParams,
        params_buffer: &GpuBuffer,
        gi_materials: &GpuBuffer,
        normal_sources: &GpuBuffer,
        depth_tex: &GpuTexture,
        out_sv: &GpuTexture,
        out_irr: &GpuTexture,
        label: &str,
    );

    /// Depth-aware bilateral upsample of the half-res `lo_sv`/`lo_irr`
    /// terms to full G-buffer resolution `hi_sv`/`hi_irr` (RT-D3's "D11
    /// trivial pass"; RT-P2 widens the SAME upsample to also carry
    /// irradiance).
    #[allow(clippy::too_many_arguments)]
    fn upsample_shadow(
        &self,
        encoder: &mut GpuEncoder,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        lo_sv: &GpuTexture,
        hi_sv: &GpuTexture,
        lo_irr: &GpuTexture,
        hi_irr: &GpuTexture,
        label: &str,
    );

    /// RT-P2/D3: temporal-accumulate `hi_irr` (this frame's raw
    /// demodulated irradiance) into `history` in place — `params.reset`
    /// discards history (cold start / post-cut, driven by the SHARED
    /// `TemporalResetDetector` — RT-D2), else blends toward `hi_irr` at
    /// `params.alpha`. `history`'s CURRENT content is read back
    /// in-kernel, so it must already hold either a prior frame's result
    /// or be freshly allocated (any content — the very first call after
    /// allocation should pass `reset: true`, which never reads it).
    fn accumulate_irradiance(
        &self,
        encoder: &mut GpuEncoder,
        params: &AccumulateParams,
        params_buffer: &GpuBuffer,
        hi_irr: &GpuTexture,
        history: &GpuTexture,
        label: &str,
    );
}

/// Metal implementation of [`ShadowRayTracer`] — ray queries via
/// `metal_raytracing`, compiled once and kept resident (mirrors the
/// pipeline-cache pattern `GpuDevice` already uses for the WGSL path).
pub struct MetalShadowRayTracer {
    trace_pipeline: GpuComputePipeline,
    upsample_pipeline: GpuComputePipeline,
    accumulate_pipeline: GpuComputePipeline,
    /// RT-T1-B value-test-only surface (`debug_fetch_interpolated_normal`'s
    /// only caller) — see the MSL `debug_fetch_interpolated_normal` kernel's
    /// doc comment. Always compiled (tiny kernel, negligible cost); never
    /// dispatched by the production `render_scene.rs` path.
    debug_fetch_normal_pipeline: GpuComputePipeline,
}

impl MetalShadowRayTracer {
    pub fn new(device: &GpuDevice) -> Self {
        let opts = MTLCompileOptions::init(MTLCompileOptions::alloc());
        // Ray tracing needs the default (latest) language version, not
        // the WGSL path's pinned older version — matches the prototype's
        // `Gpu::compile_library`.
        opts.setLanguageVersion(MTLLanguageVersion::Version3_1);
        let src_ns = NSString::from_str(SHADOW_RAYS_MSL);
        let library = device
            .raw_device()
            .newLibraryWithSource_options_error(&src_ns, Some(&opts))
            .unwrap_or_else(|e| {
                panic!(
                    "RT shadow-ray MSL library compile error: {}",
                    e.localizedDescription()
                )
            });

        let trace_pipeline = compile_pipeline(
            device,
            &library,
            "trace_shadow_rays",
            identity_slot_map(&[
                (1, SlotKind::Buffer),
                (2, SlotKind::Buffer), // RT-P3: gi_materials, MSL [[buffer(2)]]
                (3, SlotKind::Buffer), // RT-T1-B: normal_sources, MSL [[buffer(3)]]
                (0, SlotKind::Texture),
                (1, SlotKind::Texture),
                (2, SlotKind::Texture),
            ]),
        );
        let upsample_pipeline = compile_pipeline(
            device,
            &library,
            "upsample_shadow",
            identity_slot_map(&[
                (1, SlotKind::Buffer),
                (0, SlotKind::Texture),
                (1, SlotKind::Texture),
                (2, SlotKind::Texture),
                (3, SlotKind::Texture),
                (4, SlotKind::Texture),
            ]),
        );
        let accumulate_pipeline = compile_pipeline(
            device,
            &library,
            "accumulate_irradiance",
            identity_slot_map(&[
                (1, SlotKind::Buffer),
                (0, SlotKind::Texture),
                (1, SlotKind::Texture),
            ]),
        );
        let debug_fetch_normal_pipeline = compile_pipeline(
            device,
            &library,
            "debug_fetch_interpolated_normal",
            identity_slot_map(&[
                (0, SlotKind::Buffer),
                (1, SlotKind::Buffer),
                (2, SlotKind::Buffer),
            ]),
        );

        Self {
            trace_pipeline,
            upsample_pipeline,
            accumulate_pipeline,
            debug_fetch_normal_pipeline,
        }
    }

    /// RT-T1-B value-test-only entry point (`docs/RAYTRACING_DESIGN.md` §8
    /// Tier-1 item 2's gate) — dispatches the SAME `fetch_interpolated_normal`
    /// MSL helper `trace_shadow_rays` uses internally, against caller-
    /// supplied `(instance_id, primitive_id, barycentric)` inputs, no ray
    /// tracing/RNG involved. Synchronous (commits and waits) — test-only
    /// call pattern, never used on a hot path.
    pub fn debug_fetch_interpolated_normal(
        &self,
        device: &GpuDevice,
        normal_sources: &GpuBuffer,
        instance_id: u32,
        primitive_id: u32,
        bary: [f32; 2],
    ) -> [f32; 3] {
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct DebugFetchNormalParams {
            instance_id: u32,
            primitive_id: u32,
            bary: [f32; 2],
        }
        let params = DebugFetchNormalParams {
            instance_id,
            primitive_id,
            bary,
        };
        let params_buffer = device.create_buffer_shared(std::mem::size_of::<DebugFetchNormalParams>() as u64);
        let params_ptr = params_buffer
            .mapped_ptr()
            .expect("debug params buffer must be CPU-mapped");
        unsafe {
            std::ptr::write_unaligned(params_ptr as *mut DebugFetchNormalParams, params);
        }
        let out_buffer = device.create_buffer_shared(16); // packed_float3, rounded up
        out_buffer.zero_fill();

        let cb = device
            .raw_queue()
            .commandBuffer()
            .expect("Failed to acquire command buffer for RT-T1-B debug dispatch");
        let enc: Retained<ProtocolObject<dyn MTLComputeCommandEncoder>> = cb
            .computeCommandEncoder()
            .expect("computeCommandEncoder failed");
        unsafe {
            enc.setComputePipelineState(&self.debug_fetch_normal_pipeline.state);
            enc.setBuffer_offset_atIndex(Some(normal_sources.raw()), 0, 0);
            enc.setBuffer_offset_atIndex(Some(params_buffer.raw()), 0, 1);
            enc.setBuffer_offset_atIndex(Some(out_buffer.raw()), 0, 2);
            enc.dispatchThreadgroups_threadsPerThreadgroup(
                MTLSize { width: 1, height: 1, depth: 1 },
                MTLSize { width: 1, height: 1, depth: 1 },
            );
        }
        enc.endEncoding();
        cb.commit();
        unsafe { cb.waitUntilCompleted() };

        let out_ptr = out_buffer
            .mapped_ptr()
            .expect("debug output buffer must be CPU-mapped");
        let mut result = [0.0f32; 3];
        unsafe {
            std::ptr::copy_nonoverlapping(out_ptr as *const f32, result.as_mut_ptr(), 3);
        }
        result
    }
}

impl ShadowRayTracer for MetalShadowRayTracer {
    type Accel = RtAccel;

    fn build_accel(&self, device: &GpuDevice, objects: &[RtObjectGeometry]) -> Self::Accel {
        build_accel(device, objects)
    }

    fn refit_accel(&self, device: &GpuDevice, accel: &Self::Accel, objects: &[RtObjectGeometry]) {
        refit_accel(device, accel, objects);
    }

    fn dispatch_shadow_rays(
        &self,
        encoder: &mut GpuEncoder,
        accel: &Self::Accel,
        params: &ShadowRayParams,
        params_buffer: &GpuBuffer,
        gi_materials: &GpuBuffer,
        normal_sources: &GpuBuffer,
        depth_tex: &GpuTexture,
        out_sv: &GpuTexture,
        out_irr: &GpuTexture,
        label: &str,
    ) {
        params_buffer.upload(bytemuck_bytes(params));
        let groups = dispatch_groups_2d(params.trace_size, SHADOW_WORKGROUP);
        encoder.dispatch_compute_with_accel(
            &self.trace_pipeline,
            0,
            accel,
            &[
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: params_buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: gi_materials,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: normal_sources,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 0,
                    texture: depth_tex,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: out_sv,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: out_irr,
                },
            ],
            groups,
            label,
        );
    }

    fn upsample_shadow(
        &self,
        encoder: &mut GpuEncoder,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        lo_sv: &GpuTexture,
        hi_sv: &GpuTexture,
        lo_irr: &GpuTexture,
        hi_irr: &GpuTexture,
        label: &str,
    ) {
        // `params.gbuffer_size` (already uploaded by `dispatch_shadow_rays`
        // this frame — both calls share one params buffer per P1's single
        // pass) drives the dispatch grid.
        let Some(gbuffer_size) = params_buffer_gbuffer_size(params_buffer) else {
            return;
        };
        let groups = dispatch_groups_2d(gbuffer_size, SHADOW_WORKGROUP);
        encoder.dispatch_compute(
            &self.upsample_pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: params_buffer,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 0,
                    texture: depth_tex,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: lo_sv,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: hi_sv,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: lo_irr,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: hi_irr,
                },
            ],
            groups,
            label,
        );
    }

    fn accumulate_irradiance(
        &self,
        encoder: &mut GpuEncoder,
        params: &AccumulateParams,
        params_buffer: &GpuBuffer,
        hi_irr: &GpuTexture,
        history: &GpuTexture,
        label: &str,
    ) {
        params_buffer.upload(accumulate_params_bytes(params));
        let groups = dispatch_groups_2d(params.size, SHADOW_WORKGROUP);
        encoder.dispatch_compute(
            &self.accumulate_pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: params_buffer,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 0,
                    texture: hi_irr,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: history,
                },
            ],
            groups,
            label,
        );
    }
}

/// Read back `gbuffer_size` from an uploaded `ShadowRayParams` buffer —
/// avoids threading a second copy of the params struct through the
/// `upsample_shadow` call. `None` if the buffer isn't CPU-mapped (should
/// not happen for the shared-storage params buffer P1 always allocates).
fn params_buffer_gbuffer_size(buffer: &GpuBuffer) -> Option<[u32; 2]> {
    let ptr = buffer.mapped_ptr()?;
    // Offset of `gbuffer_size` within `ShadowRayParams`: sun_dir(12) +
    // sun_cone(4) + shadow_spp(4) + frame_index(4) + trace_size(8) = 32.
    const GBUFFER_SIZE_OFFSET: usize = 32;
    unsafe {
        let p = ptr.add(GBUFFER_SIZE_OFFSET) as *const u32;
        Some([p.read_unaligned(), p.add(1).read_unaligned()])
    }
}

fn bytemuck_bytes(params: &ShadowRayParams) -> &[u8] {
    // SAFETY: `ShadowRayParams` is `#[repr(C)]`, all-POD (f32/u32 fields
    // only), no padding, no interior pointers.
    unsafe {
        std::slice::from_raw_parts(
            (params as *const ShadowRayParams) as *const u8,
            std::mem::size_of::<ShadowRayParams>(),
        )
    }
}

fn accumulate_params_bytes(params: &AccumulateParams) -> &[u8] {
    // SAFETY: `AccumulateParams` is `#[repr(C)]`, all-POD (u32/f32 fields
    // only), no padding, no interior pointers — same discipline as
    // `bytemuck_bytes` above.
    unsafe {
        std::slice::from_raw_parts(
            (params as *const AccumulateParams) as *const u8,
            std::mem::size_of::<AccumulateParams>(),
        )
    }
}

trait UploadBytes {
    fn upload(&self, bytes: &[u8]);
}

impl UploadBytes for GpuBuffer {
    fn upload(&self, bytes: &[u8]) {
        let Some(ptr) = self.mapped_ptr() else {
            panic!("ShadowRayParams buffer must be CPU-mapped (create_buffer_shared)");
        };
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        }
    }
}
