//! RAYTRACING_DESIGN.md P1 — Metal ray-query acceleration structures and
//! the hard-shadow-ray dispatch kernel.
//!
//! Ports `tools/rt_prototype/src/accel.rs` (acceleration-structure
//! build/refit) and the shadow-only slice of
//! `tools/rt_prototype/shaders/rt_trace.metal`'s `trace_lighting` +
//! `upsample_lighting` kernels (AO/GI sampling is P2/P3 scope — dropped
//! here, not ported). `ShadowRayTracer` is the D9 backend seam: all data
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
    MTLCommandQueue, MTLCompileOptions, MTLComputePipelineState, MTLDevice,
    MTLInstanceAccelerationStructureDescriptor, MTLIndexType, MTLLanguageVersion, MTLLibrary,
    MTLPackedFloat3, MTLPackedFloat4x3, MTLPrimitiveAccelerationStructureDescriptor,
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
    // RT-D3: ray origins come from the prepass DEPTH texture + this
    // inverse view-proj — no stored world-pos/normal G-buffer target in
    // P1. Column-major, matches `render_scene.rs`'s `mat4_inverse` output
    // and `render_scene.wgsl`'s `Uniforms.view_proj` convention.
    float4x4 inv_view_proj;
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
// forced on for RT-enabled scenes). Normal-for-bias is a screen-space
// finite-difference of reconstructed world positions (RT-D3: same
// technique as `ssao_gtao.rs`'s depth-only reconstruction — no new normal
// G-buffer target in P1). Output (trace_size): r = sun visibility [0,1].
kernel void trace_shadow_rays(
    instance_acceleration_structure  accel     [[buffer(0)]],
    constant ShadowRayParams&        p         [[buffer(1)]],
    depth2d<float>                   depth_tex [[texture(0)]],
    texture2d<float, access::write>  out_sv    [[texture(1)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.trace_size.x || tid.y >= p.trace_size.y) return;
    uint2 gpix = min(uint2((float2(tid) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size)), p.gbuffer_size - 1);

    bool valid;
    float3 wp = world_pos_from_depth(gpix, p.gbuffer_size, depth_tex.read(gpix, 0), p.inv_view_proj, valid);
    if (!valid) {
        out_sv.write(float4(1, 0, 0, 0), tid);
        return;
    }
    // Finite-difference normal from neighbor world positions (screen-space
    // reconstruction, RT-D3). Falls back to the +x/+y neighbor's delta
    // alone at the image edge (still a fine bias direction — this is a
    // ray-origin epsilon offset, not a shaded normal).
    uint2 gx = min(gpix + uint2(1, 0), p.gbuffer_size - 1);
    uint2 gy = min(gpix + uint2(0, 1), p.gbuffer_size - 1);
    bool vx, vy;
    float3 wpx = world_pos_from_depth(gx, p.gbuffer_size, depth_tex.read(gx, 0), p.inv_view_proj, vx);
    float3 wpy = world_pos_from_depth(gy, p.gbuffer_size, depth_tex.read(gy, 0), p.inv_view_proj, vy);
    float3 n = (vx && vy) ? normalize(cross(wpx - wp, wpy - wp)) : float3(0, 1, 0);
    if (!isfinite(n.x) || !isfinite(n.y) || !isfinite(n.z) || length_squared(n) < 1e-8) {
        n = float3(0, 1, 0);
    }
    // Bias origin along the sun direction too (not just the normal) —
    // guards the finite-difference normal's occasional near-degenerate
    // case (e.g. a silhouette edge) from immediately self-shadowing.
    float3 origin = wp + n * 1e-3 + p.sun_dir * 1e-3;

    intersector<triangle_data, instancing> shadow_i;
    shadow_i.assume_geometry_type(geometry_type::triangle);
    shadow_i.force_opacity(forced_opacity::opaque);
    shadow_i.accept_any_intersection(true);

    ray r;
    r.origin = origin;
    r.min_distance = 0.0;
    r.max_distance = INFINITY;

    uint spp = max(p.shadow_spp, 1u);
    float vis = 0.0;
    for (uint s = 0; s < spp; s++) {
        r.direction = cone_sample(p.sun_dir, p.sun_cone, rand2(tid, p.frame_index, s));
        if (shadow_i.intersect(r, accel).type == intersection_type::none) vis += 1.0;
    }
    vis /= float(spp);
    out_sv.write(float4(vis, 0, 0, 0), tid);
}

// Depth-aware bilateral upsample: half-res sun-visibility -> full res
// (RT-D3's "D11 trivial pass"). Guide: full-res depth only (raw NDC z —
// comparable directly without linearizing, since nearby screen pixels at
// similar depth have proportionally similar raw-z regardless of the
// projection's nonlinearity).
kernel void upsample_shadow(
    constant ShadowRayParams&       p         [[buffer(1)]],
    depth2d<float>                  depth_tex [[texture(0)]],
    texture2d<float>                lo_sv     [[texture(1)]],
    texture2d<float, access::write> hi_sv     [[texture(2)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.gbuffer_size.x || tid.y >= p.gbuffer_size.y) return;
    float d = depth_tex.read(tid, 0);
    if (d >= 1.0 - 1e-6) { hi_sv.write(float4(1, 0, 0, 0), tid); return; }

    float2 lo_uv = (float2(tid) + 0.5) / float2(p.gbuffer_size) * float2(p.trace_size);
    int2 lo_c = int2(lo_uv - 0.5);
    float acc = 0.0; float wsum = 0.0;
    for (int dy = 0; dy <= 1; dy++)
    for (int dx = 0; dx <= 1; dx++) {
        int2 q = clamp(lo_c + int2(dx, dy), int2(0), int2(p.trace_size) - 1);
        uint2 gq = min(uint2((float2(q) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size)), p.gbuffer_size - 1);
        float qd = depth_tex.read(gq, 0);
        float2 f = saturate(1.0 - fabs(lo_uv - 0.5 - float2(q)));
        float w_bilin = f.x * f.y;
        float w_depth = exp(-fabs(qd - d) / 0.001);
        float w = max(w_bilin * w_depth, 1e-5);
        acc += lo_sv.read(uint2(q)).r * w;
        wsum += w;
    }
    hi_sv.write(float4(acc / wsum, 0, 0, 0), tid);
}
"#;

/// CPU mirror of `ShadowRayParams` above — field order and packing MUST
/// match exactly (P0 §5.1 kernel lesson: `packed_float3` in MSL == dense
/// `[f32; 3]` here, no padding).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ShadowRayParams {
    pub sun_dir: [f32; 3],
    pub sun_cone: f32,
    pub shadow_spp: u32,
    pub frame_index: u32,
    pub trace_size: [u32; 2],
    pub gbuffer_size: [u32; 2],
    /// MSL's `float4x4` requires 16-byte alignment; the 40 bytes above it
    /// need 8 more to reach the next 16-byte boundary (48). `#[repr(C)]`
    /// does NOT know `[[f32; 4]; 4]` needs that (its natural alignment is
    /// 4, from `f32`) — without this, the GPU reads `inv_view_proj`
    /// starting 8 bytes early, same alignment-gotcha class as the
    /// `packed_float3` lesson (P0 §5.1), just for a matrix instead of a
    /// vec3. Caught by `mat4x4_alignment_matches_msl_float4x4` below —
    /// don't remove this padding without re-deriving the offset.
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
        inv_view_proj: [[f32; 4]; 4],
    ) -> Self {
        Self {
            sun_dir,
            sun_cone,
            shadow_spp,
            frame_index,
            trace_size,
            gbuffer_size,
            _pad_align_mat4: [0; 2],
            inv_view_proj,
        }
    }
}

// RT-D3 alignment gotcha (see `_pad_align_mat4`'s doc comment): this is
// the regression guard a GPU test alone wouldn't localize as clearly —
// if `inv_view_proj`'s offset ever drifts from 48 again (a field
// reordered/resized above it), this fails at compile time instead of
// silently reading garbage on the GPU.
const _: () = assert!(std::mem::offset_of!(ShadowRayParams, inv_view_proj) == 48);
const _: () = assert!(std::mem::size_of::<ShadowRayParams>() == 112);

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

    /// Dispatch the half-res hard-shadow-ray pass (RT-D3): ray origins +
    /// bias normal reconstructed in-kernel from `depth_tex` (the full-res
    /// opaque-depth prepass) + `params.inv_view_proj` — no world-pos/
    /// normal G-buffer target. Writes sun-visibility to `out_sv` at
    /// `params.trace_size`.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_shadow_rays(
        &self,
        encoder: &mut GpuEncoder,
        accel: &Self::Accel,
        params: &ShadowRayParams,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        out_sv: &GpuTexture,
        label: &str,
    );

    /// Depth-aware bilateral upsample of the half-res `lo_sv` term to
    /// full G-buffer resolution `hi_sv` (RT-D3's "D11 trivial pass").
    #[allow(clippy::too_many_arguments)]
    fn upsample_shadow(
        &self,
        encoder: &mut GpuEncoder,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        lo_sv: &GpuTexture,
        hi_sv: &GpuTexture,
        label: &str,
    );
}

/// Metal implementation of [`ShadowRayTracer`] — ray queries via
/// `metal_raytracing`, compiled once and kept resident (mirrors the
/// pipeline-cache pattern `GpuDevice` already uses for the WGSL path).
pub struct MetalShadowRayTracer {
    trace_pipeline: GpuComputePipeline,
    upsample_pipeline: GpuComputePipeline,
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
                (0, SlotKind::Texture),
                (1, SlotKind::Texture),
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
            ]),
        );

        Self {
            trace_pipeline,
            upsample_pipeline,
        }
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
        depth_tex: &GpuTexture,
        out_sv: &GpuTexture,
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
                GpuBinding::Texture {
                    binding: 0,
                    texture: depth_tex,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: out_sv,
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
