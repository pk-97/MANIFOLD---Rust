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
use crate::types::{GpuBinding, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

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
    /// RT-T2-A (RAYTRACING_DESIGN.md §8.2 Tier-2 item 4): byte offset of the
    /// per-vertex UV field within one `vertex_stride`-sized vertex record —
    /// same "name where it lives, no separate allocation" convention as
    /// `normal_offset`. Only read when `alpha_mask` is set; a fixture with
    /// no UV data may set this to any value as long as `alpha_mask` stays
    /// `false`.
    pub uv_offset: u32,
    /// RT-T2-A: this object's material is `AlphaMode::Mask` (cutout) —
    /// intersections against it run the per-candidate alpha test (a UV
    /// fetch and `base_color_texture` sample against `alpha_cutoff`)
    /// instead of the opaque fast path. `false` keeps the BLAS geometry
    /// `setOpaque(true)` (see `encode_blas_build`) and every ray against
    /// this object short-circuits at the hardware level, same cost as
    /// before this feature.
    pub alpha_mask: bool,
    /// RT-T2-A: cutout threshold in `[0, 1]` — mirrors `Material::
    /// alpha_cutoff`. Unused when `alpha_mask` is `false`.
    pub alpha_cutoff: f32,
    /// RT-T2-A: this object's base-color texture, sampled (alpha channel
    /// only) at the candidate hit's interpolated UV when `alpha_mask` is
    /// set. `None` degrades to "always pass" (documented at
    /// `ensure_normal_sources`'s call site) — an alpha-masked object with no
    /// texture wired is a material-authoring gap, not a crash.
    pub base_color_texture: Option<&'a GpuTexture>,
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
    // RT-T2-A (RAYTRACING_DESIGN.md §8.2 Tier-2 item 4): alpha-masked
    // objects must NOT be geometry-opaque — the hardware traversal would
    // auto-accept every candidate without giving the kernel's
    // `walk_with_alpha_test` a chance to reject a below-cutoff texel.
    // Non-alpha-masked objects stay `setOpaque(true)`, preserving the exact
    // fast-path cost they had before this feature.
    tri_desc.setOpaque(!obj.alpha_mask);
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
// RT-T2-A (RAYTRACING_DESIGN.md §8.2 Tier-2 item 4): fixed texture-argument-
// table slot count for alpha-masked base-color textures, bound individually
// via `setTexture:atIndex:` (no argument buffer/bindless addressing) — a
// scene needing more than this many DISTINCT alpha-masked base-color
// textures live at once is this constant's un-suppression trigger (grow it,
// or add real bindless texture addressing). Must match manifold-gpu's Rust
// `MAX_RT_ALPHA_TEXTURES` (no compiler-enforced link between an embedded
// MSL string constant and a Rust const — same manual-sync discipline this
// file already uses for `RtNormalSource`'s field-for-field CPU/GPU mirror).
#define MAX_RT_ALPHA_TEXTURES 4

struct RtNormalSource {
    ulong  vertex_base_addr;
    uint   vertex_stride;
    uint   normal_offset;
    packed_float3 normal_matrix_col0;
    packed_float3 normal_matrix_col1;
    packed_float3 normal_matrix_col2;
    // RT-T2-A additions below — extends this SAME per-object bindless
    // table rather than introducing a parallel one (RAYTRACING_DESIGN.md
    // §8.2 D21's "extends the T1-B bindless per-object table" brief).
    uint   uv_offset;
    uint   alpha_mask;
    float  alpha_cutoff;
    // Index into `alpha_textures` (the kernel's fixed texture-array param);
    // `MAX_RT_ALPHA_TEXTURES` or above means "no texture bound" (degrades
    // to always-pass in `sample_candidate_alpha`).
    uint   alpha_tex_index;
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

// RT-T2-A: fetch vertex `vi`'s LOCAL-space UV via the SAME bindless address
// `fetch_world_normal` uses (no transform — UV isn't a spatial quantity).
static float2 fetch_uv(constant RtNormalSource& src, uint vi) {
    device const uchar* base = (device const uchar*)src.vertex_base_addr;
    device const packed_float2* uv_ptr =
        (device const packed_float2*)(base + (ulong)vi * (ulong)src.vertex_stride + (ulong)src.uv_offset);
    return float2(*uv_ptr);
}

// RT-T2-A: barycentric-interpolate triangle `primitive_id`'s UV (same flat,
// non-indexed convention as `fetch_interpolated_normal`).
static float2 fetch_interpolated_uv(constant RtNormalSource* normal_sources, uint instance_id, uint primitive_id, float2 bary) {
    constant RtNormalSource& src = normal_sources[instance_id];
    uint v0 = primitive_id * 3u, v1 = v0 + 1u, v2 = v0 + 2u;
    float2 uv0 = fetch_uv(src, v0);
    float2 uv1 = fetch_uv(src, v1);
    float2 uv2 = fetch_uv(src, v2);
    float w0 = 1.0 - bary.x - bary.y;
    return uv0 * w0 + uv1 * bary.x + uv2 * bary.y;
}

// RT-T2-A: sample this candidate triangle's base-color alpha at its
// interpolated UV. NEAREST + `address::repeat`: exact-match discipline for
// the value-level gate's checkerboard fixture, `repeat` matching every
// other UV-wrap convention this codebase's base-color sampling already
// uses.
static float sample_candidate_alpha(
    constant RtNormalSource& src,
    constant RtNormalSource* normal_sources,
    array<texture2d<float>, MAX_RT_ALPHA_TEXTURES> alpha_textures,
    uint instance_id, uint primitive_id, float2 bary)
{
    if (src.alpha_tex_index >= MAX_RT_ALPHA_TEXTURES) return 1.0; // no texture bound: degrade to always-pass
    float2 uv = fetch_interpolated_uv(normal_sources, instance_id, primitive_id, bary);
    constexpr sampler alpha_sampler(coord::normalized, address::repeat, filter::nearest);
    return alpha_textures[src.alpha_tex_index].sample(alpha_sampler, uv).a;
}

// RT-T2-A (RAYTRACING_DESIGN.md §8.2 D21): shared candidate walk for ALL of
// this kernel's ray casts (primary visibility, shadow, AO, GI + its
// sun-bounce) — ONE alpha-test mechanism, not a per-ray-class copy (the
// gate's "one mechanism, not three copies" requirement). Per-object BLAS
// opacity (`encode_blas_build`'s `setOpaque(!alpha_mask)`) already gives
// OPAQUE objects the hardware early-termination fast path; this manual walk
// only pays a per-candidate texture sample for objects actually flagged
// `alpha_mask` (a non-alpha-masked candidate's `pass` is unconditionally
// true, no texture touch). `any_hit`: true stops at the first accepted
// candidate (shadow/AO/GI occlusion tests only need existence — the
// original `accept_any_intersection(true)` semantics); false walks every
// candidate so the query commits its true CLOSEST accepted hit (primary
// visibility + the GI ray's own hit need real shading data, not just
// "something's there").
static bool walk_with_alpha_test(
    thread intersection_query<triangle_data, instancing>& q,
    constant RtNormalSource* normal_sources,
    array<texture2d<float>, MAX_RT_ALPHA_TEXTURES> alpha_textures,
    bool any_hit)
{
    while (q.next()) {
        if (q.get_candidate_intersection_type() != intersection_type::triangle) continue;
        uint iid = q.get_candidate_instance_id();
        constant RtNormalSource& src = normal_sources[iid];
        bool pass = true;
        if (src.alpha_mask != 0u) {
            float alpha = sample_candidate_alpha(
                src, normal_sources, alpha_textures,
                iid, q.get_candidate_primitive_id(), q.get_candidate_triangle_barycentric_coord());
            pass = alpha >= src.alpha_cutoff;
        }
        if (pass) {
            // `commit_triangle_intersection()`, not `accept_intersection()`
            // (the intersector convenience API's name, which does not exist
            // on `intersection_query` — confirmed by the real Metal
            // compiler rejecting the latter).
            q.commit_triangle_intersection();
            if (any_hit) return true;
        }
    }
    return q.get_committed_intersection_type() != intersection_type::none;
}

// RT-P2/D3 (extended RT-T1-C, BUG-311): mirrors the Rust `AccumulateParams`
// below field-for-field. `inv_view_proj` (current frame) reconstructs this
// texel's world position from `depth_tex`; `prev_view_proj` reprojects that
// world position into the PREVIOUS frame to locate the history sample to
// validate/blend — both matrices already exist on `RenderScene` for MetalFX
// (RAYTRACING_DESIGN.md §8 Tier-1 item 1), no new CPU-side computation.
struct AccumulateParams {
    uint2 size;
    float alpha;
    uint  reset;
    // RT-T2-C (object motion): number of entries in the `obj_motion`
    // buffer; a per-pixel object id at or beyond this count reprojects
    // camera-only (identity object motion). Explicit pad keeps the
    // float4x4s at the same 16-byte-aligned offsets the CPU mirror
    // asserts.
    uint  obj_count;
    uint  pad0; uint pad1; uint pad2;
    float4x4 inv_view_proj;
    float4x4 prev_view_proj;
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

// RT-T1-D (RAYTRACING_DESIGN.md §8 Tier-1 item 3, BUG-312): low-discrepancy
// sample for AO/GI hemisphere directions ONLY (shadow rays keep `rand2`+
// `cone_sample` — T1-D's brief scopes blue noise to AO/GI). R2 (Roberts
// 2018) additive-recurrence sequence via the plastic-constant irrationals
// — points 0..N of this sequence are far more evenly spread than N
// independent white-noise draws, which is exactly what `AO_SAMPLES_PER_
// PIXEL`=4 / `GI_SAMPLES_PER_PIXEL`=2 need (too few samples for white
// noise's clustering/gaps not to show up as salt-and-pepper speckle,
// BUG-312's symptom). Cranley-Patterson-rotated per pixel (a `pcg` hash of
// the pixel as a fractional offset, wrapped with `fract`) so neighboring
// pixels get DECORRELATED sample sets — without the rotation every pixel
// would sample the identical directions, producing banding instead of
// noise-like (but low-discrepancy) dithering.
static float2 r2_sequence(uint index) {
    const float a1 = 0.754877666246692760049508896358532874940835564978200; // 1/g
    const float a2 = 0.569840290998053265911429807193052839282807640205691; // 1/g^2
    float2 v = float2(a1 * float(index), a2 * float(index));
    return v - floor(v);
}
static float2 blue_noise_sample(uint2 p, uint frame, uint ray, uint spp) {
    uint index = frame * spp + ray;
    float2 base = r2_sequence(index);
    uint h = pcg(p.x ^ pcg(p.y));
    float2 offset = float2((h & 0xFFFFu) / 65536.0, ((h >> 16u) & 0xFFFFu) / 65536.0);
    float2 u = base + offset;
    return u - floor(u);
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
    texture2d<float, access::write>  out_n          [[texture(3)]],
    // RT-T2-A: fixed slots for alpha-masked objects' base-color textures —
    // see `MAX_RT_ALPHA_TEXTURES`'s doc comment.
    array<texture2d<float>, MAX_RT_ALPHA_TEXTURES> alpha_textures [[texture(4)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.trace_size.x || tid.y >= p.trace_size.y) return;
    uint2 gpix = min(uint2((float2(tid) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size)), p.gbuffer_size - 1);

    bool valid;
    float3 wp = world_pos_from_depth(gpix, p.gbuffer_size, depth_tex.read(gpix, 0), p.inv_view_proj, valid);
    if (!valid) {
        // Void background: unoccluded either way (matches the prototype's
        // `out_sv.write(float4(1,1,0,0), tid)` void case) — irradiance is
        // ambient-only (no surface to shadow-test against). `.w = -1`:
        // no object (RT-T2-C).
        out_sv.write(float4(1, 1, 0, 0), tid);
        out_irr.write(float4(p.ambient_color, 0), tid);
        out_n.write(float4(0, 1, 0, -1.0), tid);
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
    // RT-T2-C (object motion): this pixel's primary-hit instance id, or
    // -1 when unknown (no primary ray cast, or it missed). Rides in
    // `out_n.w` — free channel, already threaded through the upsample and
    // à-trous stages — so `accumulate_irradiance` can reproject a MOVING
    // object's pixels through that object's own prev-frame transform
    // instead of discarding their history as disocclusion (the motion-
    // shimmer BUG-320 left behind). Stored as float: instance counts are
    // far below f32's 2^24 exact-integer range.
    float obj_id = -1.0;
    if (p.ao_spp > 0u || p.gi_spp > 0u) {
        float3 to_surface = wp - float3(p.camera_pos);
        float dist = length(to_surface);
        if (dist > 1e-6) {
            ray pr;
            pr.origin = float3(p.camera_pos);
            pr.direction = to_surface / dist;
            pr.min_distance = 0.0;
            pr.max_distance = dist + dist * 1e-3 + 1e-4;
            intersection_query<triangle_data, instancing> primary_q;
            primary_q.reset(pr, accel);
            if (walk_with_alpha_test(primary_q, normal_sources, alpha_textures, false)) {
                uint primary_iid = primary_q.get_committed_instance_id();
                n = fetch_interpolated_normal(normal_sources, primary_iid, primary_q.get_committed_primitive_id(), primary_q.get_committed_triangle_barycentric_coord());
                obj_id = float(primary_iid);
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
        intersection_query<triangle_data, instancing> shadow_q;
        shadow_q.reset(r, accel);
        bool blocked = walk_with_alpha_test(shadow_q, normal_sources, alpha_textures, true);
        if (!blocked) vis += 1.0;
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
            ao_r.direction = cosine_hemisphere(n, blue_noise_sample(tid, p.frame_index, s, p.ao_spp));
            intersection_query<triangle_data, instancing> ao_q;
            ao_q.reset(ao_r, accel);
            if (!walk_with_alpha_test(ao_q, normal_sources, alpha_textures, true)) ao += 1.0;
        }
        ao /= float(p.ao_spp);
    }
    out_sv.write(float4(vis, ao, 0, 0), tid);
    // RT-T1-C (BUG-311): expose the SAME real interpolated vertex normal
    // (`n`) already computed above for AO/GI cosine sampling, so
    // `accumulate_irradiance`'s reprojection validity test can compare a
    // real surface normal instead of reconstructing one from depth.
    // RT-T2-C: `.w` carries the primary-hit object id (see `obj_id` above).
    out_n.write(float4(n, obj_id), tid);

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
        ray gr;
        gr.origin = origin;
        gr.min_distance = bias_eps * 0.5;
        gr.max_distance = INFINITY;
        for (uint s = 0; s < p.gi_spp; s++) {
            gr.direction = cosine_hemisphere(n, blue_noise_sample(tid, p.frame_index, s, p.gi_spp));
            intersection_query<triangle_data, instancing> gi_q;
            gi_q.reset(gr, accel);
            bool gi_hit = walk_with_alpha_test(gi_q, normal_sources, alpha_textures, false);
            if (gi_hit) {
                uint oi = gi_q.get_committed_instance_id();
                uint gi_pid = gi_q.get_committed_primitive_id();
                float2 gi_bary = gi_q.get_committed_triangle_barycentric_coord();
                float gi_dist = gi_q.get_committed_distance();
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
                float3 hit_pos = gr.origin + gr.direction * gi_dist;
                float3 hit_n = fetch_interpolated_normal(normal_sources, oi, gi_pid, gi_bary);
                ray sun_r;
                sun_r.origin = hit_pos + p.sun_dir * bias_eps;
                sun_r.direction = cone_sample(p.sun_dir, p.sun_cone, rand2(tid, p.frame_index, 400u + s));
                sun_r.min_distance = bias_eps * 0.5;
                sun_r.max_distance = INFINITY;
                intersection_query<triangle_data, instancing> sun_q;
                sun_q.reset(sun_r, accel);
                float hit_sun_vis = walk_with_alpha_test(sun_q, normal_sources, alpha_textures, true) ? 0.0 : 1.0;
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

// RT-T1-D shared luminance weighting (Rec.709) — used by both the
// upsample gather below and `atrous_filter`'s edge-stopping function.
static float luma(float3 c) { return dot(c, float3(0.2126, 0.7152, 0.0722)); }

// Depth+normal-aware bilateral upsample: half-res (sun-visibility, AO) +
// demod. irradiance -> full res (RT-D3's "D11 trivial pass"; RT-P2 widened
// the SAME kernel to also carry the AO channel + the irradiance texture —
// one dispatch, one guide, not a second upsample pass; RT-T1-D adds a
// normal-dot weight on top of the existing depth+bilinear gather — the
// half-res `lo_n` primary-hit vertex normal T1-C already produces is
// available here for free). Guide: full-res depth (raw NDC z — comparable
// directly without linearizing) + the tap nearest the destination texel's
// own normal as the edge-stop reference. VARIANCE guiding is applied in
// the dilated `atrous_filter` passes that follow this stage (T1-D's
// deliverable 2) — this initial half->full gather only ever has ONE
// frame's raw (unaccumulated) signal to compare against, no temporal
// variance estimate yet at this point in the pipeline.
kernel void upsample_shadow(
    constant ShadowRayParams&       p         [[buffer(1)]],
    depth2d<float>                  depth_tex [[texture(0)]],
    texture2d<float>                lo_sv     [[texture(1)]],
    texture2d<float, access::write> hi_sv     [[texture(2)]],
    texture2d<float>                lo_irr    [[texture(3)]],
    texture2d<float, access::write> hi_irr    [[texture(4)]],
    // RT-T1-C (BUG-311): the SAME bilateral upsample widened once more (D16's
    // seam note) to carry the primary-hit vertex normal `trace_shadow_rays`
    // now writes to `out_n` — `accumulate_irradiance`'s reprojection
    // validity test needs a full-res CURRENT-frame normal, same as it
    // already needed full-res CURRENT irradiance.
    texture2d<float>                lo_n      [[texture(5)]],
    texture2d<float, access::write> hi_n      [[texture(6)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.gbuffer_size.x || tid.y >= p.gbuffer_size.y) return;
    float d = depth_tex.read(tid, 0);
    if (d >= 1.0 - 1e-6) {
        hi_sv.write(float4(1, 1, 0, 0), tid);
        hi_irr.write(float4(p.ambient_color, 0), tid);
        hi_n.write(float4(0, 1, 0, -1.0), tid);
        return;
    }

    float2 lo_uv = (float2(tid) + 0.5) / float2(p.gbuffer_size) * float2(p.trace_size);
    int2 lo_c = int2(lo_uv - 0.5);
    // RT-T1-D: reference normal for the edge-stop weight below — the tap
    // nearest the destination texel (round, not floor/ceil, so it's
    // whichever of the 2x2 gather's four taps this pixel is closest to).
    int2 nearest_lo = clamp(int2(round(lo_uv - 0.5)), int2(0), int2(p.trace_size) - 1);
    float4 ref_n4 = lo_n.read(uint2(nearest_lo));
    float3 ref_n = ref_n4.xyz;
    // UPSAMPLE_NORMAL_POWER: cosine power on the tap-vs-reference normal
    // dot product — named per the P2 constants rule. Range 8-64: lower
    // tolerates more silhouette blur across the 2x2 gather, higher rejects
    // a differing-surface tap more sharply; 32 rejects a >~10 degree
    // normal divergence to near-zero weight while still full-weighting a
    // shared flat surface's own precision noise.
    const float UPSAMPLE_NORMAL_POWER = 32.0;
    float2 acc_sv = 0.0; float3 acc_irr = 0.0; float3 acc_n = 0.0; float wsum = 0.0;
    for (int dy = 0; dy <= 1; dy++)
    for (int dx = 0; dx <= 1; dx++) {
        int2 q = clamp(lo_c + int2(dx, dy), int2(0), int2(p.trace_size) - 1);
        uint2 gq = min(uint2((float2(q) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size)), p.gbuffer_size - 1);
        float qd = depth_tex.read(gq, 0);
        float3 qn = lo_n.read(uint2(q)).xyz;
        float2 f = saturate(1.0 - fabs(lo_uv - 0.5 - float2(q)));
        float w_bilin = f.x * f.y;
        float w_depth = exp(-fabs(qd - d) / 0.001);
        float w_normal = pow(max(dot(ref_n, qn), 0.0), UPSAMPLE_NORMAL_POWER);
        float w = max(w_bilin * w_depth * w_normal, 1e-5);
        acc_sv += lo_sv.read(uint2(q)).rg * w;
        acc_irr += lo_irr.read(uint2(q)).rgb * w;
        acc_n += qn * w;
        wsum += w;
    }
    hi_sv.write(float4(acc_sv / wsum, 0, 0), tid);
    hi_irr.write(float4(acc_irr / wsum, 0), tid);
    float3 n_avg = acc_n / wsum;
    float n_len = length(n_avg);
    // RT-T2-C: object ids never blend — carry the nearest tap's id (the
    // same tap already trusted as the edge-stop reference normal).
    hi_n.write(float4(n_len > 1e-4 ? n_avg / n_len : float3(0, 1, 0), ref_n4.w), tid);
}

// RT-T1-D (RAYTRACING_DESIGN.md §8 Tier-1 item 3, BUG-312): CPU mirror
// below is `AtrousParams`. `history_valid` is 0 only on the very first
// RT-ready frame of a fresh (or just-resized) irradiance history — before
// `accumulate_irradiance` has ever written a moments texture, reading it
// would be garbage, so the filter falls back to a fixed (non-variance)
// luma sigma that frame (still depth+normal edge-stopped, just not yet
// variance-adaptive).
struct AtrousParams {
    uint2 size;
    uint  step;
    uint  history_valid;
};

// RT-T1-D: edge-aware À-TROUS spatial filter — dilated by `p.step`
// (Dammertz et al. 2010's "a-trous", French for "with holes": each
// dispatch samples the SAME 4-tap cross pattern but at `step`-texel
// spacing, so successive calls with step=1,2,4... cover an exponentially
// widening support without extra taps per pass). REPLACES the old
// depth-only bilateral upsample as the sole full-res spatial filter
// (`upsample_shadow` above still does the half->full RESAMPLE with its
// own depth+normal weights; this kernel is the denoiser proper, run
// `ATROUS_ITERATIONS`-1 times full-res-to-full-res after it — see
// `render_scene.rs`'s dispatch sequence). Edge-stopping weights:
// - DEPTH: raw NDC-z, same discipline as `upsample_shadow`'s guide.
// - NORMAL: cosine power against the center texel's own normal.
// - LUMA/VARIANCE: SVGF's key trick — the luma edge-stop's sigma SCALES
//   with sqrt(this texel's temporally-accumulated variance) (read from
//   `moments_read`, RT-T1-D's moment-tracking addition to
//   `accumulate_irradiance`, ONE FRAME LAGGED — same ping-pong-history
//   lag convention `depth_history_read`/`normal_history_read` already
//   use): a converged (low-variance) texel trusts its own signal and
//   rejects a differing tap sharply (preserves detail); a noisy
//   (high-variance) texel tolerates more difference before rejecting
//   (blurs harder specifically where the noise is, not uniformly).
kernel void atrous_filter(
    constant AtrousParams&           p            [[buffer(1)]],
    depth2d<float>                   depth_tex    [[texture(0)]],
    texture2d<float>                 moments_read [[texture(1)]],
    texture2d<float>                 src_sv       [[texture(2)]],
    texture2d<float, access::write>  dst_sv       [[texture(3)]],
    texture2d<float>                 src_irr      [[texture(4)]],
    texture2d<float, access::write>  dst_irr      [[texture(5)]],
    texture2d<float>                 src_n        [[texture(6)]],
    texture2d<float, access::write>  dst_n        [[texture(7)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.size.x || tid.y >= p.size.y) return;
    float center_depth = depth_tex.read(tid, 0);
    if (center_depth >= 1.0 - 1e-6) {
        // Void background: pass through unfiltered (nothing to edge-stop
        // against; matches every other stage's void-background handling).
        dst_sv.write(src_sv.read(tid), tid);
        dst_irr.write(src_irr.read(tid), tid);
        dst_n.write(src_n.read(tid), tid);
        return;
    }
    float4 center_n4 = src_n.read(tid);
    float3 center_n = center_n4.xyz;
    float3 center_irr = src_irr.read(tid).rgb;
    float center_luma = luma(center_irr);
    float center_var = 0.0;
    if (p.history_valid != 0u) {
        float2 mo = moments_read.read(tid).rg;
        center_var = max(mo.g - mo.r * mo.r, 0.0);
    }
    // ATROUS_DEPTH_SIGMA: raw NDC-z units, same scale `upsample_shadow`'s
    // 0.001 depth guide uses. ATROUS_NORMAL_POWER: same range/rationale as
    // `upsample_shadow`'s `UPSAMPLE_NORMAL_POWER` above. ATROUS_LUMA_
    // SIGMA_FLOOR/SCALE: range 4-16 for the scale (lower = more aggressive
    // blur at a given variance; the SVGF paper's reference is ~4, we start
    // conservative at 8) — the floor (0.05) keeps `history_valid==0`'s
    // first frame and any genuinely zero-variance texel from collapsing
    // to a near-infinitely-sharp (effectively unfiltered) luma weight.
    const float ATROUS_DEPTH_SIGMA = 3e-3;
    const float ATROUS_NORMAL_POWER = 16.0;
    const float ATROUS_LUMA_SIGMA_SCALE = 8.0;
    const float ATROUS_LUMA_SIGMA_FLOOR = 0.15;
    float luma_sigma = max(ATROUS_LUMA_SIGMA_SCALE * sqrt(center_var), ATROUS_LUMA_SIGMA_FLOOR);
    // Full 3x3 neighborhood (8 taps, diagonals included) rather than a
    // 4-tap cross: with only `ATROUS_ITERATIONS`=3 total passes budgeted
    // (T1-D's 2-3 range), each pass needs to average enough independent
    // noisy AO/GI samples on its own — a cross-only kernel left visible
    // residual speckle at this scene's sample counts even after 2 dilated
    // passes; the diagonal taps roughly double the averaged sample count
    // per pass for the same dilation radius.
    const int2 offsets[8] = {
        int2(1, 0), int2(-1, 0), int2(0, 1), int2(0, -1),
        int2(1, 1), int2(1, -1), int2(-1, 1), int2(-1, -1)
    };
    float3 acc_irr = center_irr;
    float2 acc_sv = src_sv.read(tid).rg;
    float wsum = 1.0;
    for (int i = 0; i < 8; i++) {
        int2 q = int2(tid) + offsets[i] * int(p.step);
        if (q.x < 0 || q.y < 0 || q.x >= int(p.size.x) || q.y >= int(p.size.y)) continue;
        uint2 uq = uint2(q);
        float qd = depth_tex.read(uq, 0);
        if (qd >= 1.0 - 1e-6) continue;
        float3 qn = src_n.read(uq).xyz;
        float3 qirr = src_irr.read(uq).rgb;
        float w_depth = exp(-fabs(qd - center_depth) / ATROUS_DEPTH_SIGMA);
        float w_normal = pow(max(dot(center_n, qn), 0.0), ATROUS_NORMAL_POWER);
        float w_luma = exp(-fabs(luma(qirr) - center_luma) / luma_sigma);
        float w = w_depth * w_normal * w_luma;
        acc_irr += qirr * w;
        acc_sv += src_sv.read(uq).rg * w;
        wsum += w;
    }
    dst_irr.write(float4(acc_irr / wsum, 0), tid);
    dst_sv.write(float4(acc_sv / wsum, 0, 0), tid);
    // RT-T2-C: `.w` = object id, passed through untouched (never blended).
    dst_n.write(float4(center_n, center_n4.w), tid);
}

// RT-P2/D3, extended RT-T1-C (BUG-311): temporal accumulation of the
// demodulated irradiance texture — the next stage of the SAME lighting pass
// (not a parallel denoiser system). `reset` (driven by the SHARED
// `crate::node_graph::temporal_reset::TemporalResetDetector` — RT-D2; the
// negative-rg gate enforces there is exactly one reset-detection call
// site) discards history outright (cold start / post-cut). Otherwise this
// texel's world position (reconstructed from `depth_tex` + `p.inv_view_proj`)
// is reprojected into the PREVIOUS frame via `p.prev_view_proj` to find
// where this surface point was last frame — same-texel blending (the P2
// baseline) ghosts behind ANY motion because it never asks "is this still
// the same surface point"; reprojection is the fix. The reprojected sample
// is REJECTED (falls back to this frame's raw value, no history blend) on
// a depth or normal mismatch against `*_history_read` (an off-screen
// reprojection also rejects) — SVGF's standard disocclusion test. Every
// history channel is PING-PONGED (`*_read`/`*_write` are two distinct
// textures, swapped by the caller each frame): a single read_write texture
// would race, since one thread's write destination (`tid`) can be another
// thread's read source (`prev_tid`) within the same dispatch, with no
// ordering guarantee between compute threads.
kernel void accumulate_irradiance(
    constant AccumulateParams&           p                    [[buffer(1)]],
    // RT-T2-C (object motion): per-object world→prev-world delta
    // (`prev_model * inverse(model)`), indexed by the primary-hit object
    // id carried in `hi_normal.w`. Identity for a static object.
    constant float4x4*                   obj_motion           [[buffer(2)]],
    texture2d<float>                     hi_irr               [[texture(0)]],
    depth2d<float>                       depth_tex            [[texture(1)]],
    texture2d<float>                     hi_normal            [[texture(2)]],
    texture2d<float>                     history_read         [[texture(3)]],
    texture2d<float, access::write>      history_write        [[texture(4)]],
    texture2d<float>                     depth_history_read   [[texture(5)]],
    texture2d<float, access::write>      depth_history_write  [[texture(6)]],
    texture2d<float>                     normal_history_read  [[texture(7)]],
    texture2d<float, access::write>      normal_history_write [[texture(8)]],
    // RT-T1-D (BUG-312): per-texel luminance moments (r=mean, g=mean-of-
    // squares) — the SAME ping-pong-history discipline as the depth/
    // normal pairs above, feeding `atrous_filter`'s variance-adaptive luma
    // sigma (one-frame-lagged, like every other history read here).
    // `Rg32Float` (not `Rg16Float`): `moment2 - moment1*moment1` is a
    // difference of two close, similarly-scaled numbers — half-float's
    // ~3-decimal-digit precision would swallow variances at the 1e-4 to
    // 1e-5 scale this filter needs to resolve (catastrophic cancellation).
    texture2d<float>                     moments_read         [[texture(9)]],
    texture2d<float, access::write>      moments_write        [[texture(10)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.size.x || tid.y >= p.size.y) return;
    float4 cur = hi_irr.read(tid);
    float  cur_depth = depth_tex.read(tid, 0);
    float4 cur_n4 = hi_normal.read(tid);
    float3 cur_normal = cur_n4.xyz;
    float  cur_luma = luma(cur.xyz);

    if (p.reset != 0u) {
        history_write.write(cur, tid);
        depth_history_write.write(float4(cur_depth, 0, 0, 0), tid);
        normal_history_write.write(float4(cur_normal, 0), tid);
        moments_write.write(float4(cur_luma, cur_luma * cur_luma, 0, 0), tid);
        return;
    }

    // RT-T2-C: camera motion AND object motion. `wp` (this frame, world
    // space) is first carried back to where this OBJECT placed that
    // surface point last frame via `obj_motion` (world→prev-world,
    // identity for static objects; camera-only when the pixel has no
    // object id — void, or a shadow-only frame that cast no primary
    // ray), then reprojected through the previous camera. Without the
    // object term, a moving object's pixels failed the depth/normal test
    // below every frame and lost ALL temporal amortization mid-gesture —
    // visible shimmer until motion stopped (the residual BUG-320 left).
    bool valid = false;
    float3 blended = cur.xyz;
    float moment1 = cur_luma;
    float moment2 = cur_luma * cur_luma;
    if (cur_depth < 1.0 - 1e-6) {
        float2 uv = (float2(tid) + 0.5) / float2(p.size);
        float4 clip = float4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, cur_depth, 1.0);
        float4 wh = p.inv_view_proj * clip;
        float3 wp = wh.xyz / wh.w;
        if (cur_n4.w >= 0.0) {
            uint oid = uint(cur_n4.w + 0.5);
            if (oid < p.obj_count) {
                wp = (obj_motion[oid] * float4(wp, 1.0)).xyz;
            }
        }

        float4 prev_clip = p.prev_view_proj * float4(wp, 1.0);
        if (prev_clip.w > 1e-6) {
            float3 prev_ndc = prev_clip.xyz / prev_clip.w;
            float2 prev_uv = float2(prev_ndc.x * 0.5 + 0.5, 0.5 - prev_ndc.y * 0.5);
            if (all(prev_uv >= 0.0) && all(prev_uv <= 1.0) && prev_ndc.z >= 0.0 && prev_ndc.z <= 1.0) {
                int2 pt = clamp(int2(prev_uv * float2(p.size)), int2(0), int2(p.size) - 1);
                uint2 prev_tid = uint2(pt);
                float  stored_depth  = depth_history_read.read(prev_tid).r;
                float3 stored_normal = normal_history_read.read(prev_tid).xyz;
                // DEPTH_REJECT_THRESHOLD: raw NDC-z units — directly
                // comparable without linearizing (same discipline
                // `upsample_shadow`'s depth guide already uses). 5e-3
                // rejects a genuinely different surface/depth layer while
                // tolerating one shared surface's own NDC-z precision
                // noise across a single frame of camera motion.
                const float DEPTH_REJECT_THRESHOLD = 5e-3;
                // NORMAL_REJECT_COS_THRESHOLD: cosine of the angle between
                // this frame's and the reprojected history's normal — 0.9
                // (~26 degrees) rejects a silhouette/edge texel whose
                // reprojection lands on a different face while tolerating
                // the same surface's normal drifting slightly under one
                // frame of camera motion or animation.
                const float NORMAL_REJECT_COS_THRESHOLD = 0.9;
                bool depth_ok = fabs(stored_depth - prev_ndc.z) < DEPTH_REJECT_THRESHOLD;
                bool normal_ok = dot(normalize(stored_normal), cur_normal) > NORMAL_REJECT_COS_THRESHOLD;
                if (depth_ok && normal_ok) {
                    float4 hist = history_read.read(prev_tid);
                    blended = mix(hist.xyz, cur.xyz, p.alpha);
                    valid = true;
                    float2 stored_moments = moments_read.read(prev_tid).rg;
                    moment1 = mix(stored_moments.r, cur_luma, p.alpha);
                    moment2 = mix(stored_moments.g, cur_luma * cur_luma, p.alpha);
                }
            }
        }
    }
    history_write.write(valid ? float4(blended, 0) : cur, tid);
    depth_history_write.write(float4(cur_depth, 0, 0, 0), tid);
    normal_history_write.write(float4(cur_normal, 0), tid);
    moments_write.write(float4(moment1, moment2, 0, 0), tid);
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
    /// RT-T2-A (RAYTRACING_DESIGN.md §8.2 Tier-2 item 4): extends this SAME
    /// bindless table (D21's brief) rather than a parallel one — see the
    /// MSL mirror's doc comment for the field-by-field extension.
    pub uv_offset: u32,
    pub alpha_mask: u32,
    pub alpha_cutoff: f32,
    /// Index into `trace_shadow_rays`'s fixed `alpha_textures` array;
    /// `>= MAX_RT_ALPHA_TEXTURES` means "no texture bound" (degrades to
    /// always-pass — see `ensure_normal_sources`).
    pub alpha_tex_index: u32,
}

const _: () = assert!(std::mem::size_of::<RtNormalSource>() == 72);

/// RT-T2-A: fixed texture-argument-table slot count for alpha-masked
/// base-color textures — MUST match the embedded MSL's
/// `#define MAX_RT_ALPHA_TEXTURES` (manual-sync discipline, same as every
/// other CPU/GPU struct mirror in this file). A scene needing more than
/// this many DISTINCT alpha-masked base-color textures live at once is
/// this constant's un-suppression trigger.
pub const MAX_RT_ALPHA_TEXTURES: usize = 4;
/// Sentinel `alpha_tex_index` meaning "no base-color texture bound" —
/// `sample_candidate_alpha` (MSL) degrades this to always-pass.
pub const RT_ALPHA_TEX_INDEX_NONE: u32 = u32::MAX;

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
///
/// RT-T2-A: also assigns each alpha-masked object a slot in the returned
/// texture list — `objects[i].base_color_texture` becomes `alpha_textures[k]`
/// where `k` is that object's position among alpha-masked objects with a
/// texture wired, in `objects` order, capped at [`MAX_RT_ALPHA_TEXTURES`].
/// An alpha-masked object beyond the cap, or with no `base_color_texture`
/// wired, gets [`RT_ALPHA_TEX_INDEX_NONE`] — degrades to "always pass" in
/// the kernel (a material-authoring/scale gap, not a crash). The caller
/// (`render_scene.rs`) passes the returned list straight through to
/// [`ShadowRayTracer::dispatch_shadow_rays`]'s `alpha_textures` parameter.
pub fn ensure_normal_sources<'a>(
    slot: &mut Option<GpuBuffer>,
    capacity: &mut usize,
    device: &GpuDevice,
    objects: &[RtObjectGeometry<'a>],
) -> Vec<&'a GpuTexture> {
    let needed = objects.len().max(1);
    if slot.is_none() || *capacity < needed {
        *slot = Some(device.create_buffer_shared((needed * std::mem::size_of::<RtNormalSource>()) as u64));
        *capacity = needed;
    }
    let buf = slot.as_ref().expect("just ensured above");
    let ptr = buf
        .mapped_ptr()
        .expect("RT normal-source buffer must be CPU-mapped");
    let mut alpha_textures: Vec<&'a GpuTexture> = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        let alpha_tex_index = if obj.alpha_mask {
            match obj.base_color_texture {
                Some(tex) if alpha_textures.len() < MAX_RT_ALPHA_TEXTURES => {
                    alpha_textures.push(tex);
                    (alpha_textures.len() - 1) as u32
                }
                _ => RT_ALPHA_TEX_INDEX_NONE,
            }
        } else {
            RT_ALPHA_TEX_INDEX_NONE
        };
        let src = RtNormalSource {
            vertex_base_addr: obj.vertex_buffer.gpu_address() + obj.vertex_offset as u64,
            vertex_stride: obj.vertex_stride,
            normal_offset: obj.normal_offset,
            normal_matrix: normal_matrix_from_model(obj.transform),
            uv_offset: obj.uv_offset,
            alpha_mask: obj.alpha_mask as u32,
            alpha_cutoff: obj.alpha_cutoff,
            alpha_tex_index,
        };
        unsafe {
            std::ptr::write_unaligned(ptr.add(i * std::mem::size_of::<RtNormalSource>()) as *mut _, src);
        }
    }
    alpha_textures
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
    /// RT-T2-C (object motion): entry count of the `obj_motion` buffer
    /// bound alongside — a per-pixel object id at or beyond this count
    /// (stale texture content across a topology change) reprojects
    /// camera-only instead of reading out of bounds.
    pub obj_count: u32,
    /// Explicit pad — keeps the two `float4x4`s below on the 16-byte
    /// offsets the MSL struct's own padding puts them at (asserted below).
    pub _pad: [u32; 3],
    /// RT-T1-C (BUG-311): current-frame inverse view-proj, for
    /// reconstructing this texel's world position from `depth_tex` — SAME
    /// matrix `ShadowRayParams::inv_view_proj` already carries this frame.
    pub inv_view_proj: [[f32; 4]; 4],
    /// RT-T1-C (BUG-311): PREVIOUS frame's view-proj, for reprojecting the
    /// reconstructed world position to locate/validate the history sample.
    /// Already threaded through `RenderScene` for MetalFX
    /// (RAYTRACING_DESIGN.md §8 Tier-1 item 1); no new CPU-side matrix.
    pub prev_view_proj: [[f32; 4]; 4],
}

// `size`(8) + `alpha`(4) + `reset`(4) + `obj_count`(4) + pad(12) = 32
// bytes — a multiple of 16, so both `float4x4`s that follow land on a
// 16-byte boundary (RT-T2-C widened the pre-matrix block from 16 to 32).
// Asserted directly rather than re-derived, same discipline as the
// `ShadowRayParams` guard above.
const _: () = assert!(std::mem::offset_of!(AccumulateParams, inv_view_proj) == 32);
const _: () = assert!(std::mem::offset_of!(AccumulateParams, prev_view_proj) == 96);
const _: () = assert!(std::mem::size_of::<AccumulateParams>() == 160);

impl AccumulateParams {
    pub fn new(
        size: [u32; 2],
        alpha: f32,
        reset: bool,
        obj_count: u32,
        inv_view_proj: [[f32; 4]; 4],
        prev_view_proj: [[f32; 4]; 4],
    ) -> Self {
        Self {
            size,
            alpha,
            reset: reset as u32,
            obj_count,
            _pad: [0; 3],
            inv_view_proj,
            prev_view_proj,
        }
    }
}

/// CPU mirror of the MSL `AtrousParams` struct backing `atrous_filter`
/// (RT-T1-D, BUG-312). Plain POD, all `u32`, no alignment surprises.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AtrousParams {
    pub size: [u32; 2],
    /// Dilation step in texels (1, 2, 4, ... — see the kernel doc comment).
    pub step: u32,
    /// 0 on the first RT-ready frame of a fresh/resized irradiance
    /// history (before `accumulate_irradiance` has ever written a moments
    /// texture) — the kernel falls back to a fixed luma sigma that frame.
    pub history_valid: u32,
}

const _: () = assert!(std::mem::size_of::<AtrousParams>() == 16);

impl AtrousParams {
    pub fn new(size: [u32; 2], step: u32, history_valid: bool) -> Self {
        Self {
            size,
            step,
            history_valid: history_valid as u32,
        }
    }
}

fn atrous_params_bytes(params: &AtrousParams) -> &[u8] {
    // SAFETY: `AtrousParams` is `#[repr(C)]`, all-POD (u32 fields only),
    // no padding, no interior pointers — same discipline as
    // `bytemuck_bytes`/`accumulate_params_bytes`.
    unsafe {
        std::slice::from_raw_parts(
            (params as *const AtrousParams) as *const u8,
            std::mem::size_of::<AtrousParams>(),
        )
    }
}

// RT-T2-A: a 1x1 fully-opaque (alpha=1.0) texture — bound into every
// `alpha_textures` slot a frame's `dispatch_shadow_rays` call doesn't fill
// with a real base-color texture. Fully opaque so an accidental sample
// (should never happen: only reached via a `RtNormalSource::alpha_tex_index`
// that names a real, populated slot) degrades safely to "not cutout" rather
// than an unpredictable un-initialized read.
fn create_dummy_alpha_texture(device: &GpuDevice) -> GpuTexture {
    let tex = device.create_texture(&GpuTextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba8Unorm,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label: "rt-t2a-dummy-alpha",
        mip_levels: 1,
    });
    device.upload_texture(&tex, &[255u8, 255, 255, 255]);
    tex
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
    /// sample against, and the GI bounce's hit-point normal. RT-T2-A:
    /// `alpha_textures` is the ordered list [`ensure_normal_sources`]
    /// returns — every alpha-masked object's base-color texture, indexed by
    /// `RtNormalSource::alpha_tex_index`; missing/extra slots up to
    /// [`MAX_RT_ALPHA_TEXTURES`] are padded with a 1x1 opaque dummy.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_shadow_rays(
        &self,
        encoder: &mut GpuEncoder,
        accel: &Self::Accel,
        params: &ShadowRayParams,
        params_buffer: &GpuBuffer,
        gi_materials: &GpuBuffer,
        normal_sources: &GpuBuffer,
        alpha_textures: &[&GpuTexture],
        depth_tex: &GpuTexture,
        out_sv: &GpuTexture,
        out_irr: &GpuTexture,
        out_n: &GpuTexture,
        label: &str,
    );

    /// Depth-aware bilateral upsample of the half-res `lo_sv`/`lo_irr`/
    /// `lo_n` terms to full G-buffer resolution `hi_sv`/`hi_irr`/`hi_n`
    /// (RT-D3's "D11 trivial pass"; RT-P2 widened the SAME upsample to
    /// also carry irradiance; RT-T1-C widens it once more to carry the
    /// primary-hit vertex normal `accumulate_irradiance`'s reprojection
    /// validity test needs).
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
        lo_n: &GpuTexture,
        hi_n: &GpuTexture,
        label: &str,
    );

    /// RT-T1-D (RAYTRACING_DESIGN.md §8 Tier-1 item 3, BUG-312): one
    /// dilated edge-aware à-trous pass, full-res to full-res, guided by
    /// `depth_tex` + `src_n`'s own normal + `moments_read`'s variance
    /// (one-frame-lagged, from the LAST `accumulate_irradiance` call —
    /// same lag convention as the depth/normal history reads). Called
    /// `ATROUS_ITERATIONS`-1 times by the caller with an increasing
    /// `step` (1, 2, ...), after `upsample_shadow` has already produced
    /// the initial full-res `src_*` set.
    #[allow(clippy::too_many_arguments)]
    fn atrous_pass(
        &self,
        encoder: &mut GpuEncoder,
        params: &AtrousParams,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        moments_read: &GpuTexture,
        src_sv: &GpuTexture,
        dst_sv: &GpuTexture,
        src_irr: &GpuTexture,
        dst_irr: &GpuTexture,
        src_n: &GpuTexture,
        dst_n: &GpuTexture,
        label: &str,
    );

    /// RT-P2/D3, extended RT-T1-C (BUG-311): temporal-accumulate `hi_irr`
    /// (this frame's raw demodulated irradiance) into `history_write`,
    /// reprojecting `history_read` through `params.prev_view_proj` and
    /// validating against `depth_history_read`/`normal_history_read`
    /// before trusting it (falls back to `hi_irr` alone on mismatch or
    /// disocclusion) — `params.reset` discards history outright (cold
    /// start / post-cut, driven by the SHARED `TemporalResetDetector` —
    /// RT-D2). Every history channel is a `(read, write)` PING-PONG PAIR:
    /// the caller must pass last frame's write-target as this frame's
    /// read-target and swap after the call — a single read_write texture
    /// would race (see the kernel's own doc comment).
    #[allow(clippy::too_many_arguments)]
    fn accumulate_irradiance(
        &self,
        encoder: &mut GpuEncoder,
        params: &AccumulateParams,
        params_buffer: &GpuBuffer,
        // RT-T2-C: per-object world→prev-world motion matrices
        // (`params.obj_count` entries of column-major `[[f32; 4]; 4]`).
        obj_motion: &GpuBuffer,
        hi_irr: &GpuTexture,
        depth_tex: &GpuTexture,
        hi_normal: &GpuTexture,
        history_read: &GpuTexture,
        history_write: &GpuTexture,
        depth_history_read: &GpuTexture,
        depth_history_write: &GpuTexture,
        normal_history_read: &GpuTexture,
        normal_history_write: &GpuTexture,
        // RT-T1-D (BUG-312): per-texel luminance moments ping-pong pair —
        // see the `atrous_filter`/`accumulate_irradiance` MSL kernel doc
        // comments.
        moments_read: &GpuTexture,
        moments_write: &GpuTexture,
        label: &str,
    );
}

/// Metal implementation of [`ShadowRayTracer`] — ray queries via
/// `metal_raytracing`, compiled once and kept resident (mirrors the
/// pipeline-cache pattern `GpuDevice` already uses for the WGSL path).
pub struct MetalShadowRayTracer {
    trace_pipeline: GpuComputePipeline,
    upsample_pipeline: GpuComputePipeline,
    /// RT-T1-D (BUG-312): the dilated edge-aware à-trous filter pipeline.
    atrous_pipeline: GpuComputePipeline,
    accumulate_pipeline: GpuComputePipeline,
    /// RT-T1-B value-test-only surface (`debug_fetch_interpolated_normal`'s
    /// only caller) — see the MSL `debug_fetch_interpolated_normal` kernel's
    /// doc comment. Always compiled (tiny kernel, negligible cost); never
    /// dispatched by the production `render_scene.rs` path.
    debug_fetch_normal_pipeline: GpuComputePipeline,
    /// RT-T2-A: 1x1 fully-opaque texture bound into every one of
    /// `trace_shadow_rays`'s `alpha_textures` slots that this frame's
    /// `dispatch_shadow_rays` call doesn't supply a real texture for —
    /// Metal requires a valid resource bound at every argument-table index
    /// a compiled kernel references, even one `sample_candidate_alpha`
    /// (MSL) never actually indexes at runtime.
    dummy_alpha_tex: GpuTexture,
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
                (3, SlotKind::Texture), // RT-T1-C: out_n, MSL [[texture(3)]]
                // RT-T2-A: alpha_textures[MAX_RT_ALPHA_TEXTURES], MSL
                // [[texture(4)]] — occupies MAX_RT_ALPHA_TEXTURES
                // consecutive argument-table slots starting at 4.
                (4, SlotKind::Texture),
                (5, SlotKind::Texture),
                (6, SlotKind::Texture),
                (7, SlotKind::Texture),
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
                (5, SlotKind::Texture), // RT-T1-C: lo_n
                (6, SlotKind::Texture), // RT-T1-C: hi_n
            ]),
        );
        let atrous_pipeline = compile_pipeline(
            device,
            &library,
            "atrous_filter",
            identity_slot_map(&[
                (1, SlotKind::Buffer),
                (0, SlotKind::Texture), // depth_tex
                (1, SlotKind::Texture), // moments_read
                (2, SlotKind::Texture), // src_sv
                (3, SlotKind::Texture), // dst_sv
                (4, SlotKind::Texture), // src_irr
                (5, SlotKind::Texture), // dst_irr
                (6, SlotKind::Texture), // src_n
                (7, SlotKind::Texture), // dst_n
            ]),
        );
        let accumulate_pipeline = compile_pipeline(
            device,
            &library,
            "accumulate_irradiance",
            identity_slot_map(&[
                (1, SlotKind::Buffer),
                (2, SlotKind::Buffer), // RT-T2-C: obj_motion, MSL [[buffer(2)]]
                (0, SlotKind::Texture), // RT-T1-C: hi_irr
                (1, SlotKind::Texture), // RT-T1-C: depth_tex
                (2, SlotKind::Texture), // RT-T1-C: hi_normal
                (3, SlotKind::Texture), // RT-T1-C: history_read
                (4, SlotKind::Texture), // RT-T1-C: history_write
                (5, SlotKind::Texture), // RT-T1-C: depth_history_read
                (6, SlotKind::Texture), // RT-T1-C: depth_history_write
                (7, SlotKind::Texture), // RT-T1-C: normal_history_read
                (8, SlotKind::Texture), // RT-T1-C: normal_history_write
                (9, SlotKind::Texture),  // RT-T1-D: moments_read
                (10, SlotKind::Texture), // RT-T1-D: moments_write
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

        let dummy_alpha_tex = create_dummy_alpha_texture(device);

        Self {
            trace_pipeline,
            upsample_pipeline,
            atrous_pipeline,
            accumulate_pipeline,
            debug_fetch_normal_pipeline,
            dummy_alpha_tex,
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
        alpha_textures: &[&GpuTexture],
        depth_tex: &GpuTexture,
        out_sv: &GpuTexture,
        out_irr: &GpuTexture,
        out_n: &GpuTexture,
        label: &str,
    ) {
        params_buffer.upload(bytemuck_bytes(params));
        let groups = dispatch_groups_2d(params.trace_size, SHADOW_WORKGROUP);
        let mut bindings = vec![
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
            GpuBinding::Texture {
                binding: 3,
                texture: out_n,
            },
        ];
        // RT-T2-A: fill all MAX_RT_ALPHA_TEXTURES argument-table slots —
        // real textures first (caller-supplied order matches
        // `RtNormalSource::alpha_tex_index`), the 1x1 dummy for the rest
        // (Metal requires every slot a compiled kernel references bound to
        // a valid resource).
        for i in 0..MAX_RT_ALPHA_TEXTURES {
            let tex = alpha_textures.get(i).copied().unwrap_or(&self.dummy_alpha_tex);
            bindings.push(GpuBinding::Texture {
                binding: 4 + i as u32,
                texture: tex,
            });
        }
        encoder.dispatch_compute_with_accel(&self.trace_pipeline, 0, accel, &bindings, groups, label);
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
        lo_n: &GpuTexture,
        hi_n: &GpuTexture,
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
                GpuBinding::Texture {
                    binding: 5,
                    texture: lo_n,
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: hi_n,
                },
            ],
            groups,
            label,
        );
    }

    fn atrous_pass(
        &self,
        encoder: &mut GpuEncoder,
        params: &AtrousParams,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        moments_read: &GpuTexture,
        src_sv: &GpuTexture,
        dst_sv: &GpuTexture,
        src_irr: &GpuTexture,
        dst_irr: &GpuTexture,
        src_n: &GpuTexture,
        dst_n: &GpuTexture,
        label: &str,
    ) {
        params_buffer.upload(atrous_params_bytes(params));
        let groups = dispatch_groups_2d(params.size, SHADOW_WORKGROUP);
        encoder.dispatch_compute(
            &self.atrous_pipeline,
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
                    texture: moments_read,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: src_sv,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: dst_sv,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: src_irr,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: dst_irr,
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: src_n,
                },
                GpuBinding::Texture {
                    binding: 7,
                    texture: dst_n,
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
        // RT-T2-C: per-object world→prev-world motion matrices
        // (`params.obj_count` entries of column-major `[[f32; 4]; 4]`).
        obj_motion: &GpuBuffer,
        hi_irr: &GpuTexture,
        depth_tex: &GpuTexture,
        hi_normal: &GpuTexture,
        history_read: &GpuTexture,
        history_write: &GpuTexture,
        depth_history_read: &GpuTexture,
        depth_history_write: &GpuTexture,
        normal_history_read: &GpuTexture,
        normal_history_write: &GpuTexture,
        moments_read: &GpuTexture,
        moments_write: &GpuTexture,
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
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: obj_motion,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 0,
                    texture: hi_irr,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: depth_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: hi_normal,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: history_read,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: history_write,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: depth_history_read,
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: depth_history_write,
                },
                GpuBinding::Texture {
                    binding: 7,
                    texture: normal_history_read,
                },
                GpuBinding::Texture {
                    binding: 8,
                    texture: normal_history_write,
                },
                GpuBinding::Texture {
                    binding: 9,
                    texture: moments_read,
                },
                GpuBinding::Texture {
                    binding: 10,
                    texture: moments_write,
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
