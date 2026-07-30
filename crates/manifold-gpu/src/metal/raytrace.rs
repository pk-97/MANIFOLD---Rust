//! RAYTRACING_DESIGN.md P1–P3 — Metal ray-query acceleration structures and
//! the shadow/AO/GI-ray dispatch kernel.
//!
//! Ports `tools/rt_prototype/src/accel.rs` (acceleration-structure
//! build/refit) and `tools/rt_prototype/shaders/rt_trace.metal`'s
//! `trace_lighting` + `upsample_lighting` kernels: P1 ported the shadow-only
//! slice; P2 added the AO gather; P3 (section 5.2, D4) adds the one-bounce GI
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
pub(crate) struct Blas {
    pub(crate) structure: Retained<ProtocolObject<dyn MTLAccelerationStructure>>,
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
    /// simple field access instead of an NSArray walk. pub(crate):
    /// encoder.rs's dispatch useResource coverage (BUG-jddy arm 5).
    pub(crate) blas: Vec<Blas>,
    /// CPU-writable instance-descriptor buffer (transform per object).
    /// Retained here so `refit_accel` can rewrite transforms in place.
    /// pub(crate): encoder.rs's dispatch useResource coverage (BUG-jddy
    /// arm 5) declares both BLASes and this buffer.
    pub(crate) instance_buffer: GpuBuffer,
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
    /// — RAYTRACING_DESIGN.md section 8 Tier-1 item 2). A fixture whose geometry
    /// carries no normal data at all (e.g. `rt_p1_shadow.rs`'s
    /// position-only `PackedVertex`) may set this to any value AS LONG AS
    /// `ao_spp`/`gi_spp` stay 0 — the only two consumers of the fetched
    /// normal.
    pub normal_offset: u32,
    /// RT-T2-A (RAYTRACING_DESIGN.md section 8.2 Tier-2 item 4): byte offset of the
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
    /// Textured roughness (R3) (RAYTRACING_DESIGN.md section 9.6): this object's
    /// metallic-roughness texture, sampled (G=roughness, B=metallic — glTF
    /// packing) at the reflection ray's primary-hit interpolated UV.
    /// `None` degrades to the flat `GiMaterial::metallic_roughness` factor
    /// (documented at `ensure_normal_sources`'s call site) — an object with
    /// no map wired renders exactly as before this feature. Consumed ONLY
    /// in the reflection lobe at the primary hit; GI/AO/shadow rays and the
    /// reflection-HIT shading stay flat-factor (out of this phase's scope).
    pub mr_texture: Option<&'a GpuTexture>,
    /// Per-object shadow-cast toggle (`node.scene_object`'s `cast_shadows`
    /// param, threaded through `render_scene.rs`'s `ObjectDraw`). `false`
    /// clears `RT_MASK_SHADOW_CASTER` from this instance's mask (see
    /// [`build_instance_buffer`]) — it still carries `RT_MASK_VISIBLE`, so
    /// it stays hit by every query EXCEPT the shadow/sun-bounce rays that
    /// mask against `RT_MASK_SHADOW_CASTER` alone.
    pub cast_shadows: bool,
}

/// RT instance mask bits (`MTLAccelerationStructureInstanceDescriptor::mask`,
/// matched by `intersection_query::reset`'s mask argument). Every instance
/// carries `RT_MASK_VISIBLE`; `RT_MASK_SHADOW_CASTER` is additionally set
/// only when the object's `cast_shadows` is on. Manual-sync discipline: kept
/// in lockstep with the MSL `constant uint` pair of the same name in
/// `SHADOW_RAYS_MSL` below.
pub const RT_MASK_VISIBLE: u32 = 0x01;
pub const RT_MASK_SHADOW_CASTER: u32 = 0x02;

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
    // RT-T2-A (RAYTRACING_DESIGN.md section 8.2 Tier-2 item 4): alpha-masked
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

/// Every instance always carries [`RT_MASK_VISIBLE`]; [`RT_MASK_SHADOW_CASTER`]
/// is added only when the object's `cast_shadows` is on.
fn instance_mask(cast_shadows: bool) -> u32 {
    RT_MASK_VISIBLE | if cast_shadows { RT_MASK_SHADOW_CASTER } else { 0 }
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
            mask: instance_mask(obj.cast_shadows),
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
    add_ready_completion_handler(&cb, "RT accel build", Arc::clone(&ready), (blas_scratch, build_scratch));
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
///
/// Also logs any GPU error on this buffer under `label`: these buffers
/// commit async with no other observer, so a fault here otherwise shows
/// up only as "innocent victim" errors on the Compositor buffer while the
/// culprit stays invisible.
fn add_ready_completion_handler<T: Send + 'static>(
    cb: &ProtocolObject<dyn MTLCommandBuffer>,
    label: &'static str,
    ready: Arc<AtomicBool>,
    keep_alive: T,
) {
    use block2::RcBlock;
    use objc2_metal::MTLCommandBufferStatus;
    let block = RcBlock::new(move |buf: std::ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
        let _keep_alive = &keep_alive;
        let cb = unsafe { buf.as_ref() };
        if unsafe { cb.status() } == MTLCommandBufferStatus::Error {
            let (code, desc) = match unsafe { cb.error() } {
                None => (-1i64, String::from("(nil)")),
                Some(err) => (err.code() as i64, err.localizedDescription().to_string()),
            };
            log::error!("[GPU] Command buffer '{label}' error (code={code}): {desc}");
        }
        ready.store(true, Ordering::Release);
    });
    unsafe {
        cb.addCompletedHandler(RcBlock::as_ptr(&block));
    }
}

/// Refit `accel`'s TLAS in place — cheap (instance-transform-and-mask-only)
/// update, used when an object's transform or `cast_shadows` toggle changes
/// but its topology/vertex count doesn't (so the BLAS list is unchanged).
/// Rewrites the instance buffer's transforms AND masks from `objects` first,
/// then refits — the mask must be kept in lockstep here or a `cast_shadows`
/// toggle with no accompanying transform change would refit the TLAS
/// (`render_scene.rs`'s `accel_key` folds `cast_shadows` in alongside the
/// transform) without ever updating the mask this fn is the only writer of
/// outside `build_instance_buffer`.
pub(crate) fn refit_accel(device: &GpuDevice, accel: &RtAccel, objects: &[RtObjectGeometry]) {
    debug_assert_eq!(
        objects.len(),
        accel.blas.len(),
        "refit_accel called with a different object COUNT than build_accel built — the BLAS \
         list (and instance buffer) don't match; call build_accel again instead (topology change)"
    );
    let stride = std::mem::size_of::<MTLAccelerationStructureInstanceDescriptor>();
    let mask_offset = std::mem::offset_of!(MTLAccelerationStructureInstanceDescriptor, mask);
    let ptr = accel
        .instance_buffer
        .mapped_ptr()
        .expect("RT instance-descriptor buffer must be CPU-mapped");
    for (i, obj) in objects.iter().enumerate() {
        unsafe {
            let field_ptr = ptr.add(i * stride) as *mut MTLPackedFloat4x3;
            field_ptr.write_unaligned(to_packed_4x3(obj.transform));
            let mask_ptr = ptr.add(i * stride + mask_offset) as *mut u32;
            mask_ptr.write_unaligned(instance_mask(obj.cast_shadows));
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
    add_ready_completion_handler(&cb, "RT TLAS refit", Arc::clone(&accel.ready), ());
    cb.commit();
}

// ─── Raw MSL kernels (shadow-only slice of rt_trace.metal) ────────────

/// Shadow-only trim of the prototype's `TraceParams`/`trace_lighting` +
/// `upsample_lighting` kernels. AO (`ao_spp`) and one-bounce GI
/// (`gi_spp`, `Material`/`mat_index` buffers) are P2/P3 scope — dropped,
/// not ported. `packed_float3` is mandatory (P0 section 5.1 kernel lesson):
/// bare MSL `float3` is sizeof 16 and desyncs from `#[repr(C)] [f32; 3]`.
const SHADOW_RAYS_MSL: &str = r#"
#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace metal::raytracing;

// Per-caster shadow support (multi-caster fix): mirrors the Rust
// `RtCasterParams` field-for-field (P0 section 5.1 kernel lesson). `kind`
// 0 = sun (`dir_or_pos` = normalized direction FROM surface TOWARD the
// sun, `cone_or_size` = cone half-angle radians); `kind` 1 = point
// (`dir_or_pos` = world-space light position, `cone_or_size` = world-units
// light diameter, 0.0 = hard shadows). `color` is premultiplied
// color*intensity.
struct RtCasterParams {
    packed_float3 dir_or_pos;
    float  cone_or_size;
    packed_float3 color;
    uint   kind;
};

// MAX_RT_CASTERS: fixed shadow-caster slot count, matches manifold-gpu's
// Rust `MAX_RT_CASTERS` (no compiler-enforced link between an embedded MSL
// string constant and a Rust const — same manual-sync discipline this file
// already uses for `MAX_RT_MATERIAL_TEXTURES`).
constant uint MAX_RT_CASTERS = 4;

// RT instance mask bits — matches manifold-gpu's Rust `RT_MASK_VISIBLE` /
// `RT_MASK_SHADOW_CASTER` (same manual-sync discipline as `MAX_RT_CASTERS`
// above). Shadow rays (direct-light visibility + the GI/reflection
// sun-bounce rays) mask against `RT_MASK_SHADOW_CASTER` alone, so a
// `cast_shadows = false` object drops out of shadowing ONLY — every other
// query (primary, AO, GI, reflection) masks against `RT_MASK_VISIBLE`,
// which every instance always carries.
constant uint RT_MASK_VISIBLE = 0x01;
constant uint RT_MASK_SHADOW_CASTER = 0x02;

struct ShadowRayParams {
    uint   shadow_spp;
    uint   frame_index;
    uint2  trace_size;       // half-res (mode B, D11)
    uint2  gbuffer_size;     // full-res G-buffer / output resolution
    float  ao_radius;        // RT-P2: world-space AO ray max distance
    uint   ao_spp;           // RT-P2: AO rays/pixel; 0 = AO gather skipped
    // RT-P3 (RAYTRACING_DESIGN.md section 5.2 P3, D4): one-bounce GI gather rays
    // per pixel — emissive-hit + sun-bounce (closes the section 5.1 "no sun-bounce
    // term" gap). 0 = GI gather skipped, matching the ao_spp==0 discipline.
    uint   gi_spp;
    // Multi-caster fix: number of valid entries in `casters` below. Slots
    // at/beyond this count are skipped by `trace_shadow_rays` and read back
    // as visibility 1.0 (unshadowed).
    uint   caster_count;
    RtCasterParams casters[MAX_RT_CASTERS];
    packed_float3 ambient_color; // RT-P2: flat ambient/env color
    // RT-T1-B: world-space camera eye — origin of the primary visibility
    // ray cast to find the real hit triangle at this pixel (see
    // `fetch_interpolated_normal` below). Unused when ao_spp==0 && gi_spp==0.
    packed_float3 camera_pos;
    // RT-R1 (section 9.3): reflection-ray config — mirrors the Rust fields
    // field-for-field (refl_spp / refl_max_roughness / refl_rough_band /
    // _pad_refl). Inert in T3 (kernel reads these in T5).
    uint   refl_spp;
    float  refl_max_roughness;
    float  refl_rough_band;
    uint   _pad_refl;
    // RT-D3: ray origins come from the prepass DEPTH texture + this
    // inverse view-proj — no stored world-pos/normal G-buffer target in
    // P1. Column-major, matches `render_scene.rs`'s `mat4_inverse` output
    // and `render_scene.wgsl`'s `Uniforms.view_proj` convention. `casters`'
    // fixed 4*32=128B size lands this at byte offset 208 (a 16-byte
    // multiple) with no extra alignment padding needed — see the Rust
    // mirror's offset assert.
    float4x4 inv_view_proj;
};

// RT-P3: one entry per RT object (SAME order as `RtObjectGeometry`'s
// `objects` slice at accel-build time, which is also Metal's per-instance
// `instance_id` order — the TLAS is built with `accelerationStructureIndex:
// i` for `objects[i]`, so `hit.instance_id` indexes this array directly, no
// separate per-primitive `mat_index` indirection like the P0 prototype
// needed). `packed_float3` mandatory (P0 section 5.1 kernel lesson).
struct GiMaterial {
    packed_float3 albedo;   float _p0;
    packed_float3 emissive; float _p1;   // linear HDR, premultiplied by intensity
    float4 metallic_roughness;   // RT-R1: x=metallic, y=roughness (z/w reserved)
};

// RT-T1-B (RAYTRACING_DESIGN.md section 8 Tier-1 item 2): per-object bindless
// vertex-normal indirection — mirrors the Rust `RtNormalSource` field-for-
// field (P0 section 5.1 kernel lesson). `vertex_base_addr` is a raw GPU virtual
// address (`MTLBuffer::gpuAddress()`, CPU-computed once per rebuild);
// `normal_matrix_colN` are the object's world-space normal-transform
// columns (uniform-scale assumption, see the Rust struct's doc comment).
// RT-T2-A (RAYTRACING_DESIGN.md section 8.2 Tier-2 item 4): fixed texture-argument-
// table slot count for alpha-masked base-color textures, bound individually
// via `setTexture:atIndex:` (no argument buffer/bindless addressing) — a
// scene needing more than this many DISTINCT alpha-masked base-color
// textures live at once is this constant's un-suppression trigger (grow it,
// or add real bindless texture addressing). Must match manifold-gpu's Rust
// `MAX_RT_ALPHA_TEXTURES` (no compiler-enforced link between an embedded
// MSL string constant and a Rust const — same manual-sync discipline this
// file already uses for `RtNormalSource`'s field-for-field CPU/GPU mirror).
// MAX_RT_MATERIAL_TEXTURES: bindless table for per-object material textures
// (alpha-mask + base-color; roughness/metallic/normals consume this same cap).
// Raise when a hero scene's RT-caster set needs more; cost is one more
// fixed texture-array binding (4 bytes/table-entry GPU, negligible CPU).
#define MAX_RT_MATERIAL_TEXTURES 64

// RT-R2 (RD6): specular accumulation blend — range 0.05–0.3, untuned
// (tuning is Peter's look). Smaller = more temporal amortization.
constant float RT_REFL_ACCUM_ALPHA = 0.1;
// RT-R2 (RD6): roughness at/above which reprojection is plain surface
// reprojection (the GGX-perturbed ray ≈ the surface lobe there).
// Range 0.3–0.7, untuned.
constant float RT_REFL_VIRTUAL_REPROJ_ROUGHNESS_BLEND = 0.5;

// RT-R2 clamp (BUG-dx6w): variance-clip width in standard deviations for
// the specular-history neighborhood clamp. Range 0.5–3.0, untuned
// (tuning is Peter's look). Smaller = ghosts die faster but more
// re-noising under motion.
constant float RT_REFL_CLAMP_GAMMA = 1.0;

struct RtNormalSource {
    ulong  vertex_base_addr;
    uint   vertex_stride;
    uint   normal_offset;
    packed_float3 normal_matrix_col0;
    packed_float3 normal_matrix_col1;
    packed_float3 normal_matrix_col2;
    // RT-T2-A additions below — extends this SAME per-object bindless
    // table rather than introducing a parallel one (RAYTRACING_DESIGN.md
    // section 8.2 D21's "extends the T1-B bindless per-object table" brief).
    uint   uv_offset;
    uint   alpha_mask;
    float  alpha_cutoff;
    // Index into `material_textures` (the kernel's fixed texture-array param);
    // `MAX_RT_MATERIAL_TEXTURES` or above means "no texture bound" (degrades
    // to always-pass in `sample_candidate_alpha`).
    uint   alpha_tex_index;
    // Raster-parity reflections (RAYTRACING_DESIGN.md section 9.6): base-color texture
    // index for hit-point material sampling; `MAX_RT_MATERIAL_TEXTURES` or above
    // means "no texture bound" (flat gi_materials albedo is the fallback).
    uint   base_color_tex_index;
    // Textured roughness (R3) (RAYTRACING_DESIGN.md section 9.6): metallic-roughness
    // texture index for the reflection lobe's primary-hit sampling;
    // `MAX_RT_MATERIAL_TEXTURES` or above means "no texture bound" (flat
    // gi_materials metallic_roughness factor is the fallback).
    uint   mr_tex_index;
    uint   _pad;
};

// RT-T1-B: fetch this object's (`src`) vertex `vi`'s LOCAL-space normal via
// its bindless GPU address, then transform to world space with `src`'s
// normal matrix. `vi` is a flat, non-indexed triangle-list vertex index
// (`primitive_id*3 + which_vertex` — render_scene.rs's ONLY RT-caster
// convention today; an indexed RT-caster would need its own index-buffer
// GPU address threaded too — un-suppression trigger if that ever shows up).
static float3 fetch_world_normal(device RtNormalSource& src, uint vi) {
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
static float3 fetch_interpolated_normal(device RtNormalSource* normal_sources, uint instance_id, uint primitive_id, float2 bary) {
    device RtNormalSource& src = normal_sources[instance_id];
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
static float2 fetch_uv(device RtNormalSource& src, uint vi) {
    device const uchar* base = (device const uchar*)src.vertex_base_addr;
    device const packed_float2* uv_ptr =
        (device const packed_float2*)(base + (ulong)vi * (ulong)src.vertex_stride + (ulong)src.uv_offset);
    return float2(*uv_ptr);
}

// RT-T2-A: barycentric-interpolate triangle `primitive_id`'s UV (same flat,
// non-indexed convention as `fetch_interpolated_normal`).
static float2 fetch_interpolated_uv(device RtNormalSource* normal_sources, uint instance_id, uint primitive_id, float2 bary) {
    device RtNormalSource& src = normal_sources[instance_id];
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
    device RtNormalSource& src,
    device RtNormalSource* normal_sources,
    array<texture2d<float>, MAX_RT_MATERIAL_TEXTURES> material_textures,
    uint instance_id, uint primitive_id, float2 bary)
{
    if (src.alpha_tex_index >= MAX_RT_MATERIAL_TEXTURES) return 1.0; // no texture bound: degrade to always-pass
    float2 uv = fetch_interpolated_uv(normal_sources, instance_id, primitive_id, bary);
    constexpr sampler alpha_sampler(coord::normalized, address::repeat, filter::nearest);
    return material_textures[src.alpha_tex_index].sample(alpha_sampler, uv).a;
}

// RT-T2-A (RAYTRACING_DESIGN.md section 8.2 D21): shared candidate walk for ALL of
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
    device RtNormalSource* normal_sources,
    array<texture2d<float>, MAX_RT_MATERIAL_TEXTURES> material_textures,
    bool any_hit)
{
    while (q.next()) {
        if (q.get_candidate_intersection_type() != intersection_type::triangle) continue;
        uint iid = q.get_candidate_instance_id();
        device RtNormalSource& src = normal_sources[iid];
        bool pass = true;
        if (src.alpha_mask != 0u) {
            float alpha = sample_candidate_alpha(
                src, normal_sources, material_textures,
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
// (RAYTRACING_DESIGN.md section 8 Tier-1 item 1), no new CPU-side computation.
struct AccumulateParams {
    uint2 size;
    float alpha;
    uint  reset;
    // RT-T2-C (object motion): number of entries in the `obj_motion`
    // buffer; a per-pixel object id at or beyond this count reprojects
    // camera-only (identity object motion).
    uint  obj_count;
    // RT-R2 (RD6): camera world position for the virtual-hit-point
    // reprojection — replaces the three-pad layout at the same byte
    // offset so the float4x4s below stay 16-byte aligned.
    packed_float3 camera_pos;
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

// The shadow ray's cone jitter is seeded WITHOUT the frame index, unlike
// every other ray class in this kernel. AO, GI and reflections all feed
// `accumulate_irradiance`, which averages their per-frame noise away over
// time — for those, a new seed each frame is the whole point. The shadow
// visibility mask has no such history: it goes to the shader raw, so any
// per-frame variation in it IS visible flicker on a static scene.
//
// Fixing the seed makes the mask a pure function of scene + camera: a
// still scene gives a bit-identical mask every frame. What's left is a
// fixed per-pixel dither, which the three edge-aware atrous passes
// (`atrous_pass`) already filter — that's what they're for. This
// mattered most on dense alpha-masked cutout geometry (blossom petals),
// where nearly every pixel sits in some occluder's penumbra and the
// alpha test flips as the ray jitters, so the raw mask read as a
// crawling hatched pattern rather than a shadow.
//
// If the mask ever gains real temporal accumulation, restore
// `p.frame_index` here and the noise becomes useful again. Proof that a
// still scene stays still: `rt_t2c_shadow_temporal_stability.rs`.
constant uint RT_SHADOW_JITTER_SEED = 0u;

// Multi-bounce GI MB2 (RAYTRACING_DESIGN.md section 11): ONE home for the
// sun-bounce caster loop (invariant I-MB3) — called by the GI gather at
// every path vertex and by the reflection block's hit shading.
// `seed_base` preserves each call site's historical rand2 stream exactly
// (load-bearing for I-MB1's byte identity). Folds the diffuse BRDF's
// 1/pi via SUN_BOUNCE_INTENSITY_SCALE, named + tunable (0.02-0.3).
// Declared after `walk_with_alpha_test`, `rand2`, and `cone_sample`
// (which it calls) — MSL requires a function be declared before use.
constant float SUN_BOUNCE_INTENSITY_SCALE = 0.08;

static float3 sun_bounce_at_hit(
    instance_acceleration_structure accel,
    device RtNormalSource* normal_sources,
    array<texture2d<float>, MAX_RT_MATERIAL_TEXTURES> material_textures,
    constant ShadowRayParams& p,
    uint n_casters,
    float3 hit_pos,
    float3 hit_n,
    float3 hit_albedo,
    float bias_eps,
    uint2 tid,
    uint seed_base)
{
    float3 term = float3(0.0);
    for (uint sc = 0; sc < n_casters; sc++) {
        RtCasterParams sun_cst = p.casters[sc];
        if (sun_cst.kind != 0u) continue;
        float3 sdir = float3(sun_cst.dir_or_pos);
        ray sun_r;
        sun_r.origin = hit_pos + sdir * bias_eps;
        sun_r.direction = cone_sample(sdir, sun_cst.cone_or_size, rand2(tid, p.frame_index, seed_base + sc));
        sun_r.min_distance = bias_eps * 0.5;
        sun_r.max_distance = INFINITY;
        intersection_query<triangle_data, instancing> sun_q;
        sun_q.reset(sun_r, accel, RT_MASK_SHADOW_CASTER);
        float hit_sun_vis = walk_with_alpha_test(sun_q, normal_sources, material_textures, true) ? 0.0 : 1.0;
        float hit_ndotl = max(dot(hit_n, sdir), 0.0);
        term += hit_albedo * float3(sun_cst.color) * hit_sun_vis * hit_ndotl * SUN_BOUNCE_INTENSITY_SCALE;
    }
    return term;
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

// RT-R1 (RAYTRACING_DESIGN.md section 9.3 kernel flow step 2): GGX-importance-
// sampled reflection direction — a half-vector drawn from the GGX NDF at
// the surface roughness (same tangent-frame construction
// `cosine_hemisphere` uses), then the view vector mirrored about it. A
// NEW sampling FUNCTION riding the SAME `blue_noise_sample` sequence as
// AO/GI, per the design — not a new sampling system. roughness == 0
// skips this entirely (the caller keeps the exact mirror direction).
static float3 ggx_reflection_dir(float3 n, float3 v, float roughness, float2 u) {
    float a = roughness * roughness;
    float a2 = a * a;
    float phi = 6.2831853 * u.x;
    float cos_theta = sqrt((1.0 - u.y) / (1.0 + (a2 - 1.0) * u.y));
    float sin_theta = sqrt(max(0.0, 1.0 - cos_theta * cos_theta));
    float3 t = ortho_basis_x(n), b = cross(n, t);
    float3 h = normalize(t * (sin_theta * cos(phi)) + b * (sin_theta * sin(phi)) + n * cos_theta);
    return reflect(-v, h);
}

// RT-R1 (section 9.3 RD4): the SAME equirect mapping + roughness mip selection
// `render_scene.wgsl`'s split-sum IBL applies to `prefiltered_specular`
// (lines 1506-1510) — miss radiance and the RD7 cutoff/band env value
// MUST equal what the raster would have fetched, or the substitution
// changes pixels the ray never wanted to (I-R1's empty-scene equality
// gate). RT_REFL_PREFILTER_MAX_MIP mirrors WGSL's `PREFILTER_MAX_MIP`
// (= PREFILTER_MIP_COUNT - 1) — keep in sync (same cross-mirror
// discipline as the WGSL constant's own comment).
constant float RT_REFL_PREFILTER_MAX_MIP = 5.0;
static float3 refl_env_sample(texture2d<float> env, float3 dir, float roughness) {
    constexpr sampler env_sampler(coord::normalized, address::repeat, filter::linear, mip_filter::linear);
    float azimuth = atan2(dir.z, dir.x);
    float elevation = asin(clamp(dir.y, -1.0, 1.0));
    float2 uv = float2(azimuth / 6.2831853 + 0.5, elevation / 3.14159265 + 0.5);
    return env.sample(env_sampler, uv, level(roughness * RT_REFL_PREFILTER_MAX_MIP)).rgb;
}

// RT-T1-D (RAYTRACING_DESIGN.md section 8 Tier-1 item 3, BUG-312): low-discrepancy
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
// (camera-facing, wrong at silhouettes/thin geometry). Output (trace_size):
// out_sv = per-caster visibility [0,1], one channel per shadow-caster slot
// (r=slot 0 .. a=slot 3; slots >= caster_count read 1.0, unshadowed) —
// multi-caster fix (RT shadows previously traced only casters[0]). AO is
// gathered in-kernel but folded straight into out_irr below, never written
// to out_sv. out_irr
// (RT-P2): demodulated (no-albedo) irradiance = ambient_color*ao + gi —
// the D3 "accumulate lighting separated from albedo" term, temporally
// accumulated downstream by `accumulate_irradiance`. No direct-sun term:
// the raster light loop owns the sun (see the write site's comment).
kernel void trace_shadow_rays(
    instance_acceleration_structure  accel          [[buffer(0)]],
    constant ShadowRayParams&        p              [[buffer(1)]],
    device GiMaterial*             gi_materials   [[buffer(2)]],
    device RtNormalSource*         normal_sources [[buffer(3)]],
    depth2d<float>                   depth_tex      [[texture(0)]],
    texture2d<float, access::write>  out_sv         [[texture(1)]],
    texture2d<float, access::write>  out_irr        [[texture(2)]],
    texture2d<float, access::write>  out_n          [[texture(3)]],
    // RT-T2-A / Raster-parity reflections: fixed slots for per-object material
    // textures (alpha-mask + base-color; roughness/metallic/normals consume
    // this same cap) — see `MAX_RT_MATERIAL_TEXTURES`'s doc comment.
    array<texture2d<float>, MAX_RT_MATERIAL_TEXTURES> material_textures [[texture(4)]],
    texture2d<float, access::write> out_refl [[texture(68)]],   // RT-R1 (section 9.3): .rgb = incident radiance along R, .a = hit distance (>0), env-miss (0), no-value (-1, BUG-88m)
    // RT-R1 (section 9.3 RD4): the node's prefiltered-specular env mip chain —
    // the reflection ray's MISS radiance, sampled at the ray's roughness
    // mip with the SAME equirect mapping `render_scene.wgsl`'s split-sum
    // IBL uses (one wire away, section 9.1). Always bound (dummy when the scene
    // has no env chain — the miss branch then reads the same nothing the
    // raster IBL would).
    texture2d<float>               prefiltered_env [[texture(69)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.trace_size.x || tid.y >= p.trace_size.y) return;
    uint2 gpix = min(uint2((float2(tid) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size)), p.gbuffer_size - 1);

    bool valid;
    float3 wp = world_pos_from_depth(gpix, p.gbuffer_size, depth_tex.read(gpix, 0), p.inv_view_proj, valid);
    if (!valid) {
        // Void background: unoccluded either way, every caster slot —
        // irradiance is ambient-only (no surface to shadow-test against).
        // `.w = -1`: no object (RT-T2-C).
        out_sv.write(float4(1, 1, 1, 1), tid);
        out_irr.write(float4(p.ambient_color, 0), tid);
        out_n.write(float4(0, 1, 0, -1.0), tid);
        // BUG-88m: `.a = -1` = "no traced value at this texel". Blend
        // fragments DO shade here (the depth prepass excludes them, so
        // they read as void) and must keep their prefiltered-env IBL —
        // `render_scene.wgsl` gates the rt_reflection substitution on
        // `.a >= 0`. Alpha semantics: >0 hit distance, 0 env-miss
        // (RT_REFL_MISS_HIT_DIST), -1 no valid value.
        out_refl.write(float4(0, 0, 0, -1.0), tid);
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

    // RT-T1-B (RAYTRACING_DESIGN.md section 8 Tier-1 item 2): real interpolated
    // vertex normal via a PRIMARY visibility ray from the camera through
    // `wp` — only cast when a consumer needs it (AO/GI cosine-hemisphere
    // sampling below; each shadow ray biases along its OWN caster
    // direction, not `n` — BUG-309 follow-up, further down). Falls back to a default
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
    // Textured roughness (R3) (RAYTRACING_DESIGN.md section 9.6): the primary hit's
    // primitive id + barycentric coord, hoisted to kernel scope (invalid
    // until the primary ray commits) so the reflection block below can
    // re-derive the SAME hit's UV for per-texel metallic-roughness —
    // no second primary-visibility trace.
    uint primary_pid = 0u;
    float2 primary_bary = float2(0.0);
    // RT-R1: the primary ray is also the reflection block's source of `n`
    // and `obj_id` (RD3 — vertex normal, not shading normal), so it must
    // cast whenever reflections are on too.
    if (p.ao_spp > 0u || p.gi_spp > 0u || p.refl_spp > 0u) {
        float3 to_surface = wp - float3(p.camera_pos);
        float dist = length(to_surface);
        if (dist > 1e-6) {
            ray pr;
            pr.origin = float3(p.camera_pos);
            pr.direction = to_surface / dist;
            pr.min_distance = 0.0;
            pr.max_distance = dist + dist * 1e-3 + 1e-4;
            intersection_query<triangle_data, instancing> primary_q;
            primary_q.reset(pr, accel, RT_MASK_VISIBLE);
            if (walk_with_alpha_test(primary_q, normal_sources, material_textures, false)) {
                uint primary_iid = primary_q.get_committed_instance_id();
                primary_pid = primary_q.get_committed_primitive_id();
                primary_bary = primary_q.get_committed_triangle_barycentric_coord();
                n = fetch_interpolated_normal(normal_sources, primary_iid, primary_pid, primary_bary);
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
    // BUG-8p1h: secondary rays (AO / GI / reflection) get their OWN origin,
    // biased along the interpolated vertex normal `n` (real since RT-T1-B)
    // — never along a caster's own direction. Sharing a caster-biased
    // origin meant a caster BELOW the surface sank every secondary-ray
    // origin inside the geometry (self-intersection: ao→0, GI dead,
    // reflections hitting backfaces), so moving a zero-intensity caster
    // visibly changed lighting — a lights-out cue integrity bug. A
    // caster's position must affect nothing but its own (intensity-scaled)
    // terms.
    float3 sec_origin = wp + n * bias_eps;

    // Multi-caster fix: trace each caster's own shadow ray independently
    // (RT shadows previously traced only casters[0], so every other
    // shadow-casting light rendered as fully lit). Slot i's visibility
    // rides `sv[i]`; slots >= caster_count stay at their unshadowed
    // default (1.0).
    //
    // BUG-309 follow-up: each shadow ray biases along ITS OWN
    // toward-light direction, not `n` — the (long-removed) depth
    // finite-difference normal was noisy at this scene's depth-precision
    // scale and produced a visibly scattered, wide false-shadow footprint
    // even after the epsilon-scale fix above. A caster's direction is
    // exact (CPU-computed, never reconstructed), and biasing toward the
    // light is correct for a shadow ray.
    float4 sv = float4(1.0, 1.0, 1.0, 1.0);
    uint spp = max(p.shadow_spp, 1u);
    uint n_casters = min(p.caster_count, MAX_RT_CASTERS);
    for (uint c = 0; c < n_casters; c++) {
        RtCasterParams cst = p.casters[c];
        float3 to_light;
        float cone_half_angle;
        float max_dist;
        if (cst.kind == 0u) {
            // Sun: dir_or_pos is the normalized toward-sun direction.
            to_light = float3(cst.dir_or_pos);
            cone_half_angle = cst.cone_or_size;
            max_dist = INFINITY;
        } else {
            // Point: dir_or_pos is the world-space light position.
            // Occluders BEYOND the light itself must not count, so the
            // ray stops just short of it.
            float3 delta = float3(cst.dir_or_pos) - wp;
            float dist = length(delta);
            to_light = dist > 1e-6 ? delta / dist : float3(0.0, 1.0, 0.0);
            cone_half_angle = cst.cone_or_size > 0.0 ? atan(0.5 * cst.cone_or_size / max(dist, 1e-6)) : 0.0;
            max_dist = max(dist - bias_eps, 0.0);
        }
        ray r;
        r.origin = wp + to_light * bias_eps;
        // t_min: reject any hit closer than the bias itself outright — the
        // in-kernel self-intersection filter on top of the scale-aware
        // origin offset above, so a pathological normal/winding case that
        // still lands inside its own triangle can't register as a false
        // shadow.
        r.min_distance = bias_eps * 0.5;
        r.max_distance = max_dist;
        float vis = 0.0;
        for (uint s = 0; s < spp; s++) {
            r.direction = cone_sample(to_light, cone_half_angle, rand2(tid, RT_SHADOW_JITTER_SEED, c * spp + s));
            intersection_query<triangle_data, instancing> shadow_q;
            shadow_q.reset(r, accel, RT_MASK_SHADOW_CASTER);
            bool blocked = walk_with_alpha_test(shadow_q, normal_sources, material_textures, true);
            if (!blocked) vis += 1.0;
        }
        vis /= float(spp);
        sv[c] = vis;
    }

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
        ao_r.origin = sec_origin;
        ao_r.min_distance = bias_eps * 0.5;
        ao_r.max_distance = p.ao_radius;
        for (uint s = 0; s < p.ao_spp; s++) {
            ao_r.direction = cosine_hemisphere(n, blue_noise_sample(tid, p.frame_index, s, p.ao_spp));
            intersection_query<triangle_data, instancing> ao_q;
            ao_q.reset(ao_r, accel, RT_MASK_VISIBLE);
            if (!walk_with_alpha_test(ao_q, normal_sources, material_textures, true)) ao += 1.0;
        }
        ao /= float(p.ao_spp);
    }
    out_sv.write(sv, tid);
    // RT-T1-C (BUG-311): expose the SAME real interpolated vertex normal
    // (`n`) already computed above for AO/GI cosine sampling, so
    // `accumulate_irradiance`'s reprojection validity test can compare a
    // real surface normal instead of reconstructing one from depth.
    // RT-T2-C: `.w` carries the primary-hit object id (see `obj_id` above).
    out_n.write(float4(n, obj_id), tid);

    // RT-P3 (RAYTRACING_DESIGN.md section 5.2 P3, D4): one-bounce GI gather —
    // ported from the P0 prototype's `trace_lighting` GI block (ARC
    // `rt_trace.metal`'s "one-bounce gather: emissive on hit, env on
    // miss"), extended with the sun-bounce term the P0 section 5.1 results
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
    // MB4 (RAYTRACING_DESIGN.md section 11.2): fixed path depth + per-extension
    // energy fold. MB-B: depth 2 — one extension bounce carrying intermediate
    // albedo (colour bleed). Range 1-3.
    const uint RT_GI_MAX_BOUNCES = 2u;
    // ~1/pi, range 0.1-0.5. Consumed only when RT_GI_MAX_BOUNCES > 1: each
    // path extension multiplies throughput by the intermediate surface's
    // albedo times this fold (MB5 — the primary surface stays demodulated,
    // D3 discipline; carried intermediate albedo IS the colour bleed).
    const float RT_GI_THROUGHPUT_FOLD = 0.318;
    if (p.gi_spp > 0) {
        for (uint s = 0; s < p.gi_spp; s++) {
            ray gr;
            gr.origin = sec_origin;
            gr.min_distance = bias_eps * 0.5;
            gr.max_distance = INFINITY;
            gr.direction = cosine_hemisphere(n, blue_noise_sample(tid, p.frame_index, s, p.gi_spp));
            float3 throughput = float3(1.0);
            for (uint bounce = 0u; bounce < RT_GI_MAX_BOUNCES; bounce++) {
                intersection_query<triangle_data, instancing> gi_q;
                gi_q.reset(gr, accel, RT_MASK_VISIBLE);
                if (!walk_with_alpha_test(gi_q, normal_sources, material_textures, false)) { break; }
                uint oi = gi_q.get_committed_instance_id();
                uint gi_pid = gi_q.get_committed_primitive_id();
                float2 gi_bary = gi_q.get_committed_triangle_barycentric_coord();
                float gi_dist = gi_q.get_committed_distance();
                float3 hit_emissive = float3(gi_materials[oi].emissive);
                float3 hit_albedo = float3(gi_materials[oi].albedo);
                float3 hit_pos = gr.origin + gr.direction * gi_dist;
                float3 hit_n = fetch_interpolated_normal(normal_sources, oi, gi_pid, gi_bary);
                float3 bounce_term = sun_bounce_at_hit(
                    accel, normal_sources, material_textures, p, n_casters,
                    hit_pos, hit_n, hit_albedo, bias_eps, tid,
                    400u + s * MAX_RT_CASTERS);
                gi += throughput * (hit_emissive + bounce_term);
                if (bounce + 1u < RT_GI_MAX_BOUNCES) {
                    throughput *= hit_albedo * RT_GI_THROUGHPUT_FOLD;
                    gr.origin = hit_pos + hit_n * bias_eps;
                    // Extension directions use the plain hash stream (seed
                    // base 600u), NOT blue_noise_sample — the blue-noise
                    // sequence is budgeted per first-bounce sample index.
                    gr.direction = cosine_hemisphere(hit_n, rand2(tid, p.frame_index, 600u + s * MAX_RT_CASTERS + bounce));
                }
            }
        }
        gi /= float(p.gi_spp);
    }

    // RT-R1 (RAYTRACING_DESIGN.md section 9.3 kernel flow): traced specular for
    // the PBR base lobe, inside the SAME thread/dispatch as shadow/AO/GI
    // (RD2 — D16's seam, no reflection pass). Reuses their `origin`, `n`,
    // `bias_eps`, `obj_id`. `.rgb` = incident radiance along R
    // (SUBSTITUTES the raster's prefiltered-env fetch in fs_pbr — RD1);
    // `.a` = hit distance for R2's virtual-hit-point reprojection (RD6),
    // RT_REFL_MISS_HIT_DIST on miss/cutoff so R2's reprojection
    // degenerates to plain surface reprojection. `-1` marks "no traced
    // value" (BUG-88m) for fs_pbr's substitution gate — R2 must treat
    // `.a < 0` as invalid too when it lands.
    //
    // `sec_origin` biases along the surface normal (BUG-8p1h — never a
    // caster direction): the t_min rejection below is what protects the
    // reflection ray from self-intersection, same as the shadow ray's.
    const float RT_REFL_MISS_HIT_DIST = 0.0;
    if (p.refl_spp > 0u && obj_id >= 0.0) {
        uint roi = uint(obj_id);
        float4 mr = gi_materials[roi].metallic_roughness;
        float roughness = mr.y;
        // Textured roughness (R3) (RAYTRACING_DESIGN.md section 9.6): per-texel
        // roughness at the primary hit's UV REPLACES the flat factor,
        // matching raster `resolve_mr`'s G=roughness/B=metallic glTF
        // packing exactly (`max(t.g, 0.01)`, same floor as the raster's
        // GGX clamp). Deviation from raster parity, named: no
        // `mr_uv_m`/`mr_uv_t` UV transform applied here — same omission as
        // the existing RT base-color hit sampling. Metallic has no
        // in-kernel consumer at the primary hit (Fresnel weighting against
        // metallic already happens per-texel in raster `fs_pbr`); a
        // per-texel metallic read here would compute a value nothing uses.
        device RtNormalSource& rsrc = normal_sources[roi];
        if (rsrc.mr_tex_index < MAX_RT_MATERIAL_TEXTURES) {
            float2 primary_uv = fetch_interpolated_uv(normal_sources, roi, primary_pid, primary_bary);
            constexpr sampler mr_sampler(coord::normalized, address::repeat, filter::linear);
            float mr_g = material_textures[rsrc.mr_tex_index].sample(mr_sampler, primary_uv).g;
            roughness = max(mr_g, 0.01);
        }
        float3 V = normalize(float3(p.camera_pos) - wp);
        float3 R = reflect(-V, n);
        // RD7's env value: direction R at this pixel's roughness mip —
        // byte-equal to what fs_pbr would have fetched (I-R1).
        float3 env = refl_env_sample(prefiltered_env, R, roughness);
        if (roughness > p.refl_max_roughness + p.refl_rough_band) {
            // RD7: above the cutoff+band the prefiltered env IS the
            // approximation — no ray cast.
            out_refl.write(float4(env, RT_REFL_MISS_HIT_DIST), tid);
        } else {
            float3 rdir = R;
            if (roughness > 0.0) {
                rdir = ggx_reflection_dir(n, V, roughness, blue_noise_sample(tid, p.frame_index, 0u, p.refl_spp));
            }
            ray rr;
            rr.origin = sec_origin;
            rr.direction = rdir;
            rr.min_distance = bias_eps * 0.5;
            rr.max_distance = INFINITY;
            intersection_query<triangle_data, instancing> refl_q;
            refl_q.reset(rr, accel, RT_MASK_VISIBLE);
            float3 traced;
            float hit_dist = RT_REFL_MISS_HIT_DIST;
            if (walk_with_alpha_test(refl_q, normal_sources, material_textures, false)) {
                // Raster-parity reflections (RAYTRACING_DESIGN.md section 9.6): hit
                // shading now includes the hit surface's own environment
                // contribution (diffuse irradiance + one-bounce specular), so
                // the traced reflection matches what the raster would shade at
                // that virtual surface point — I-R1 preserved (one env-specular
                // per lobe per pixel; the hit-point env is the VIRTUAL surface's
                // contribution, not a second env at the primary pixel).
                uint hoi = refl_q.get_committed_instance_id();
                uint hpid = refl_q.get_committed_primitive_id();
                float2 hbary = refl_q.get_committed_triangle_barycentric_coord();
                hit_dist = refl_q.get_committed_distance();
                float3 hit_emissive = float3(gi_materials[hoi].emissive);
                // Sample base-color texture if bound (RtNormalSource.base_color_tex_index),
                // otherwise flat gi_materials albedo is the fallback.
                float3 hit_albedo = float3(gi_materials[hoi].albedo);
                device RtNormalSource& hsrc = normal_sources[hoi];
                if (hsrc.base_color_tex_index < MAX_RT_MATERIAL_TEXTURES) {
                    float2 hit_uv = fetch_interpolated_uv(normal_sources, hoi, hpid, hbary);
                    constexpr sampler bc_sampler(coord::normalized, address::repeat, filter::linear);
                    hit_albedo = material_textures[hsrc.base_color_tex_index].sample(bc_sampler, hit_uv).rgb;
                }
                float4 mr = gi_materials[hoi].metallic_roughness;
                float hit_metallic = mr.x;
                float hit_roughness = mr.y;
                float3 hit_pos = rr.origin + rr.direction * hit_dist;
                float3 hit_n = fetch_interpolated_normal(normal_sources, hoi, hpid, hbary);
                // Raster-parity diffuse term (irradiance approximation): sample
                // roughest mip for near-isotropic diffuse contribution.
                const float RT_REFL_HIT_ENV_DIFFUSE_ROUGHNESS = 1.0;
                float3 hit_diffuse_env = refl_env_sample(prefiltered_env, hit_n, RT_REFL_HIT_ENV_DIFFUSE_ROUGHNESS);
                // Compute F0 for the hit surface (Schlick approximation: 0.04 dielectric base).
                const float RT_REFL_HIT_DIELECTRIC_F0 = 0.04;
                float3 hit_f0 = mix(float3(RT_REFL_HIT_DIELECTRIC_F0), hit_albedo, hit_metallic);
                // Raster-parity specular term: one-bounce specular continuation along
                // the reflection direction, at the hit surface's roughness.
                float3 refl_dir = reflect(-normalize(hit_pos - float3(p.camera_pos)), hit_n);
                float3 hit_specular_env = refl_env_sample(prefiltered_env, refl_dir, hit_roughness);
                // Sun-bounce term — multi-caster fix: sums every sun caster
                // (kind==0), same discipline as the GI gather's bounce above.
                float3 sun_bounce_term = sun_bounce_at_hit(
                    accel, normal_sources, material_textures, p, n_casters,
                    hit_pos, hit_n, hit_albedo, bias_eps, tid, 500u);
                // Full raster-parity shading: emissive + diffuse-env + specular-env + sun-bounce.
                traced = hit_emissive + hit_albedo * hit_diffuse_env + hit_f0 * hit_specular_env + sun_bounce_term;
            } else {
                // RD4: miss returns the env at the ray's actual (possibly
                // GGX-perturbed) direction, roughness mip.
                traced = refl_env_sample(prefiltered_env, rdir, roughness);
            }
            // RD7's band: blend traced -> env across [max_roughness,
            // max_roughness + band] so the cutoff is continuous, not a
            // visible edge (Q2's approved BRDF-domain split).
            float band_t = saturate((roughness - p.refl_max_roughness) / max(p.refl_rough_band, 1e-4));
            out_refl.write(float4(mix(traced, env, band_t), hit_dist), tid);
        }
    } else {
        // Reflections off this frame (or the primary ray missed — no
        // material to read): no valid value. `.a = -1` (BUG-88m) so the
        // raster keeps its prefiltered-env IBL at these texels (the
        // primary-miss case covers Mask holes: the depth prepass writes
        // depth where the RT primary ray alpha-tests the triangle away).
        out_refl.write(float4(0, 0, 0, -1.0), tid);
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
    // RT-R1 (section 9.3): reflection-radiance textures — upsampled with the
    // SAME depth+normal edge-stopped weights (R2 adds roughness-aware
    // filtering; v1 rides the shared chain).
    texture2d<float>                lo_refl   [[texture(7)]],
    texture2d<float, access::write> hi_refl   [[texture(8)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.gbuffer_size.x || tid.y >= p.gbuffer_size.y) return;
    float d = depth_tex.read(tid, 0);
    if (d >= 1.0 - 1e-6) {
        hi_sv.write(float4(1, 1, 1, 1), tid);
        hi_irr.write(float4(p.ambient_color, 0), tid);
        hi_n.write(float4(0, 1, 0, -1.0), tid);
        // BUG-88m: `.a = -1` must survive the half->full chain — Blend
        // fragments shade at these "void" texels and fs_pbr's
        // substitution gate falls back to prefiltered env only on < 0.
        hi_refl.write(float4(0, 0, 0, -1.0), tid);
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
    float4 acc_sv = 0.0; float3 acc_irr = 0.0; float3 acc_n = 0.0; float3 acc_refl = 0.0; float wsum = 0.0;
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
        acc_sv += lo_sv.read(uint2(q)) * w;
        acc_irr += lo_irr.read(uint2(q)).rgb * w;
        acc_n += qn * w;
        acc_refl += lo_refl.read(uint2(q)).rgb * w;
        wsum += w;
    }
    hi_sv.write(acc_sv / wsum, tid);
    hi_irr.write(float4(acc_irr / wsum, 0), tid);
    float3 n_avg = acc_n / wsum;
    float n_len = length(n_avg);
    // RT-T2-C: object ids never blend — carry the nearest tap's id (the
    // same tap already trusted as the edge-stop reference normal).
    hi_n.write(float4(n_len > 1e-4 ? n_avg / n_len : float3(0, 1, 0), ref_n4.w), tid);
    // RT-R1: reflection radiance blends like irradiance; the hit distance
    // in `.a` never blends (R2's reprojection needs ONE surface's
    // distance) — carry the nearest tap's, same discipline as the object
    // id above.
    hi_refl.write(float4(acc_refl / wsum, lo_refl.read(uint2(nearest_lo)).a), tid);
}

// RT-T1-D (RAYTRACING_DESIGN.md section 8 Tier-1 item 3, BUG-312): CPU mirror
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
    uint  obj_count;
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
// - DEPTH: raw NDC-z, same discipline as `upsample_shadow`'s guide
//   (shared across all channels: surface continuity is surface-based).
// - NORMAL: cosine power against the center texel's own normal (shared).
// - LUMA/VARIANCE (irradiance/mask): SVGF's key trick — the luma edge-
//   stop's sigma SCALES with sqrt(this texel's temporally-accumulated
//   variance) (read from `moments_read`, RT-T1-D's moment-tracking
//   addition to `accumulate_irradiance`...): a converged texel rejects
//   differing taps sharply; a noisy texel tolerates more difference.
// - LUMA (reflection channel): its OWN roughness-narrowed luma stop —
//   the refl channel's luma is guided by `gi_materials[oid].metallic_
//   roughness.y`, not the AO/GI variance. Shiny (low roughness) = narrow
//   sigma = crisp mirror image; rough = wide sigma = heavy blur (its
//   reflections are already lobe-diffuse). Irradiance/mask channels are
//   unchanged by this weight (the shared `w` governs them; the refl
//   channel has its own `w_refl`).
kernel void atrous_filter(
    constant AtrousParams&           p            [[buffer(1)]],
    device GiMaterial*            gi_materials [[buffer(2)]],
    depth2d<float>                   depth_tex    [[texture(0)]],
    texture2d<float>                 moments_read [[texture(1)]],
    texture2d<float>                 src_sv       [[texture(2)]],
    texture2d<float, access::write>  dst_sv       [[texture(3)]],
    texture2d<float>                 src_irr      [[texture(4)]],
    texture2d<float, access::write>  dst_irr      [[texture(5)]],
    texture2d<float>                 src_n        [[texture(6)]],
    texture2d<float, access::write>  dst_n        [[texture(7)]],
    // RT-R2: reflection-radiance textures — filtered with the same depth
    // and normal edge-stops as the AO/GI channels, but its OWN
    // roughness-narrowed luma stop (rough surface = wide sigma = blur
    // harder; shiny surface = narrow sigma = preserve crisp mirror image).
    texture2d<float>                 src_refl     [[texture(8)]],
    texture2d<float, access::write>  dst_refl     [[texture(9)]],
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
        dst_refl.write(src_refl.read(tid), tid);
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
    // RT-R2: reflection-channel luma edge-stop — narrows as roughness
    // falls: shiny surfaces reject differing taps sharply (the mirror
    // image stays crisp), rough surfaces blur wide (their reflections
    // are already lobe-diffuse). Range 0.02-0.5 for both, untuned —
    // tuning is Peter's look.
    const float ATROUS_REFL_LUMA_SIGMA_SHINY = 0.05;
    const float ATROUS_REFL_LUMA_SIGMA_ROUGH = 0.3;
    // Roughness at which the refl sigma reaches its wide end. Range 0.3-0.7.
    const float ATROUS_REFL_SIGMA_ROUGHNESS_REF = 0.5;
    float luma_sigma = max(ATROUS_LUMA_SIGMA_SCALE * sqrt(center_var), ATROUS_LUMA_SIGMA_FLOOR);
    // RT-R2: center-texel roughness from the material table, via the
    // object id in `src_n.w` (same convention accumulate_irradiance uses).
    float center_rough = 1.0;
    if (center_n4.w >= 0.0) {
        uint oid = uint(center_n4.w + 0.5);
        if (oid < p.obj_count) { center_rough = gi_materials[oid].metallic_roughness.y; }
    }
    float refl_luma_sigma = mix(ATROUS_REFL_LUMA_SIGMA_SHINY, ATROUS_REFL_LUMA_SIGMA_ROUGH,
                                clamp(center_rough / ATROUS_REFL_SIGMA_ROUGHNESS_REF, 0.0, 1.0));
    float center_refl_luma = luma(src_refl.read(tid).rgb);
    float wsum_refl = 1.0; // center tap weight 1, same convention as wsum
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
    // Multi-caster fix: `src_sv`/`dst_sv` now carry up to 4 independent
    // caster-slot visibility channels (.rgba), not 2 (.rg — vis+AO, AO
    // since moved into irradiance) — reading/writing only `.rg` here would
    // silently zero slots 2/3 on every à-trous pass, outside this file's
    // stated edit list but a direct correctness consequence of the format
    // change above (not a judgment call — same treatment as
    // `upsample_shadow`).
    float4 acc_sv = src_sv.read(tid);
    float3 acc_refl = src_refl.read(tid).rgb;
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
        acc_sv += src_sv.read(uq) * w;
        // RT-R2: the refl channel's own weight — shared depth/normal stops,
        // its own roughness-narrowed luma stop on the REFLECTION signal.
        float3 qrefl = src_refl.read(uq).rgb;
        float w_refl = w_depth * w_normal * exp(-fabs(luma(qrefl) - center_refl_luma) / refl_luma_sigma);
        acc_refl += qrefl * w_refl;
        wsum_refl += w_refl;
        wsum += w;
    }
    dst_irr.write(float4(acc_irr / wsum, 0), tid);
    dst_sv.write(acc_sv / wsum, tid);
    // RT-T2-C: `.w` = object id, passed through untouched (never blended).
    dst_n.write(float4(center_n, center_n4.w), tid);
    // RT-R2: reflection radiance filters with roughness-narrowed luma
    // stop (its own `wsum_refl`); hit distance in `.a` passes through
    // untouched (never blended — R2's reprojection needs one surface's
    // distance).
    dst_refl.write(float4(acc_refl / wsum_refl, src_refl.read(tid).a), tid);
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
// BUG-dx6w: variance clip (Salvi-style) — clamp reprojected specular
// history to mean ± RT_REFL_CLAMP_GAMMA·stddev of the CURRENT frame's
// 3x3 reflection neighborhood. The specular path has no depth test (a
// virtual image's depth never matches the surface), so stale history
// that passes the normal test previously decayed only at the blend
// rate — the camera-sweep trail Peter rejected (D-61 verdict). At a
// noisy texel the box widens with the noise, so converged history
// survives exactly where there is noise worth amortizing; at a flat
// texel the box collapses and history snaps to current, which is
// harmless (nothing to amortize).
//
// BUG-axe9: the box is built in Reinhard-mapped (Karis) space, not raw
// linear HDR — an emissive texel at intensity 10+ next to a black one
// inflates linear sigma so much that the box widens to swallow stale
// bright history over black, producing a residual streak on fast sweeps
// and bright-to-black transitions (Peter's verdict, 2026-07-29). Mapping
// `t(c) = c / (1 + luma(c))` compresses HDR range before computing
// mean/sigma, so one hot texel no longer dominates the box; the clamped
// mapped value is inverted back with `c = t / (1 - luma(t))` (valid since
// luma(t) < 1 by construction — `min(..., 0.999)` guards the invert
// against float error at the asymptote).
inline float3 clamp_refl_history(float3 hist,
                                 texture2d<float> hi_refl,
                                 uint2 tid, uint2 size) {
    float3 m1 = float3(0.0);
    float3 m2 = float3(0.0);
    for (int dy = -1; dy <= 1; ++dy) {
        for (int dx = -1; dx <= 1; ++dx) {
            int2 t = clamp(int2(tid) + int2(dx, dy), int2(0), int2(size) - 1);
            float3 c = hi_refl.read(uint2(t)).rgb;
            float3 tc = c / (1.0 + luma(c));
            m1 += tc;
            m2 += tc * tc;
        }
    }
    m1 /= 9.0;
    m2 /= 9.0;
    float3 sigma = sqrt(max(m2 - m1 * m1, float3(0.0)));
    float3 mapped_hist = hist / (1.0 + luma(hist));
    float3 clamped = clamp(mapped_hist, m1 - RT_REFL_CLAMP_GAMMA * sigma,
                                        m1 + RT_REFL_CLAMP_GAMMA * sigma);
    float inv_denom = 1.0 - min(luma(clamped), 0.999);
    return clamped / inv_denom;
}

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
    // RT-R2 (RD6): reflection channel — current-frame filtered reflections
    // (`.a` = hit distance), specular history ping-pong, and the material
    // table (roughness source for the reprojection blend, Step 2).
    texture2d<float>                     hi_refl             [[texture(11)]],
    texture2d<float>                     refl_history_read   [[texture(12)]],
    texture2d<float, access::write>      refl_history_write  [[texture(13)]],
    device GiMaterial*                 gi_materials        [[buffer(3)]],
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
        refl_history_write.write(hi_refl.read(tid), tid);
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
    // NORMAL_REJECT_COS_THRESHOLD: cosine of the angle between
    // the reprojected history's normal and THIS frame's normal
    // carried back into that frame's object orientation
    // (`cur_normal_prev`, BUG-322) — 0.9 (~26 degrees) rejects
    // a silhouette/edge texel whose reprojection lands on a
    // different face while tolerating the same surface's normal
    // drifting slightly under one frame of motion. Comparing in
    // ONE consistent orientation is what makes the threshold
    // mean "different surface" rather than "the object turned".
    // Hoisted to function scope: used by both the irradiance
    // validity test and the reflection reprojection block (RT-R2).
    const float NORMAL_REJECT_COS_THRESHOLD = 0.9;
    // RT-R2 (RD6): hoisted declarations for the virtual-hit-point
    // reprojection — visible to both the irradiance validity block
    // and the reflection block that follows.
    float3 wp = float3(0.0); bool have_wp = false;
    // BUG-322: the normal must be carried into the previous frame's
    // object orientation before comparing against `normal_history`
    // (which stores world-space normals). Comparing raw fails the
    // validity test by exactly the object's rotation — history rejected
    // every frame, all temporal amortization lost (the helmet shimmer).
    float3 cur_normal_prev = cur_normal;
    bool oid_ok = false; float4x4 obj_m = float4x4(0); uint oid = 0;
    if (cur_depth < 1.0 - 1e-6) {
        float2 uv = (float2(tid) + 0.5) / float2(p.size);
        float4 clip = float4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, cur_depth, 1.0);
        float4 wh = p.inv_view_proj * clip;
        wp = wh.xyz / wh.w;
        have_wp = true;
        if (cur_n4.w >= 0.0) {
            oid = uint(cur_n4.w + 0.5);
            if (oid < p.obj_count) {
                obj_m = obj_motion[oid];
                oid_ok = true;
                wp = (obj_m * float4(wp, 1.0)).xyz;
                // Rotation/scale block only — a normal is a direction, so
                // the translation column must not apply. Non-uniform
                // scale would strictly want the inverse-transpose, but
                // this matrix is `prev_model * inverse(model)`: for the
                // rigid and uniformly-scaled transforms scene objects
                // carry it is already a similarity, where the plain 3x3
                // preserves direction exactly. Normalized below, so any
                // uniform scale factor drops out.
                float3x3 r = float3x3(obj_m[0].xyz, obj_m[1].xyz, obj_m[2].xyz);
                float3 n = r * cur_normal;
                float len = length(n);
                cur_normal_prev = len > 1e-6 ? n / len : cur_normal;
            }
        }

        float4 prev_clip = p.prev_view_proj * float4(wp, 1.0);
        if (prev_clip.w > 1e-6) {
            float3 prev_ndc = prev_clip.xyz / prev_clip.w;
            float2 prev_uv = float2(prev_ndc.x * 0.5 + 0.5, 0.5 - prev_ndc.y * 0.5);
            if (all(prev_uv >= 0.0) && all(prev_uv <= 1.0) && prev_ndc.z >= 0.0 && prev_ndc.z <= 1.0) {
                // Per-tap validated bilinear history resample (BUG-ukg): 2x2
                // footprint, each tap validated independently (depth + normal),
                // invalid taps get zero weight, valid taps renormalized;
                // all-invalid = full reject. A single nearest tap accepted a
                // neighboring texel's CONTENT under fractional camera
                // reprojection — the camera-motion smear. Exact self-
                // reprojection lands fr=(0,0), weight 1 on the own tap, so
                // static scenes are byte-identical.
                // DEPTH_REJECT_THRESHOLD: raw NDC-z units, directly comparable
                // without linearizing (same discipline as `upsample_shadow`'s
                // depth guide); 5e-3 rejects a different surface while
                // tolerating one surface's NDC-z noise across a frame.
                const float DEPTH_REJECT_THRESHOLD = 5e-3;
                float2 pf = prev_uv * float2(p.size) - 0.5;
                int2 base = int2(floor(pf));
                float2 fr = pf - float2(base);
                float w[4] = { (1.0-fr.x)*(1.0-fr.y), fr.x*(1.0-fr.y), (1.0-fr.x)*fr.y, fr.x*fr.y };
                int2 offs[4] = { int2(0,0), int2(1,0), int2(0,1), int2(1,1) };
                float wsum = 0.0; float3 hsum = float3(0.0); float2 msum = float2(0.0);
                for (int i = 0; i < 4; ++i) {
                    int2 t = clamp(base + offs[i], int2(0), int2(p.size) - 1);
                    uint2 tt = uint2(t);
                    bool depth_ok  = fabs(depth_history_read.read(tt).r - prev_ndc.z) < DEPTH_REJECT_THRESHOLD;
                    bool normal_ok = dot(normalize(normal_history_read.read(tt).xyz), cur_normal_prev) > NORMAL_REJECT_COS_THRESHOLD;
                    if (depth_ok && normal_ok) {
                        wsum += w[i];
                        hsum += w[i] * history_read.read(tt).xyz;
                        msum += w[i] * moments_read.read(tt).rg;
                    }
                }
                if (wsum > 1e-4) {
                    blended = mix(hsum / wsum, cur.xyz, p.alpha);
                    valid = true;
                    moment1 = mix(msum.x / wsum, cur_luma, p.alpha);
                    moment2 = mix(msum.y / wsum, cur_luma * cur_luma, p.alpha);
                }
            }
        }
    }
    history_write.write(valid ? float4(blended, 0) : cur, tid);
    depth_history_write.write(float4(cur_depth, 0, 0, 0), tid);
    normal_history_write.write(float4(cur_normal, 0), tid);
    moments_write.write(float4(moment1, moment2, 0, 0), tid);
    // RT-R2 (RD6): specular history through the virtual hit point.
    // No depth test — the virtual image's depth never equals the surface
    // depth stored in history; a depth test rejects all mirror history by
    // construction. Validity = validity-alpha + the shared normal test;
    // disocclusion ghosting is bounded by 1/RT_REFL_ACCUM_ALPHA (Peter's
    // look owns that verdict, D19/D20). The SURFACE object's motion
    // carries the virtual point — reflected-object motion is an accepted
    // v1 residual.
    float4 cur_refl = hi_refl.read(tid);
    float3 refl_write = cur_refl.rgb;
    if (cur_refl.a >= 0.0 && have_wp) {
        float rough = 1.0;
        if (oid_ok) { rough = gi_materials[oid].metallic_roughness.y; }
        float3 V = normalize(p.camera_pos - wp);
        // The virtual image is wp − hit_dist·V, NOT wp + hit_dist·R:
        // mirroring the hit point q across the tangent plane gives
        // q' = wp + d·R − 2d(R·n)n, and R = −V + 2(V·n)n collapses it to
        // q' = wp − d·V (exact for planar mirrors; the roughness lerp
        // covers the GGX-perturbed breakdown). wp + d·R is the REAL hit
        // point — reprojecting that only works against a scene-color
        // history; with our own refl history it reads the hit surface's
        // reflection channel (wrong content) and lands off-screen in
        // practice (no blend ever — found by the R2 scene gate, D-62).
        float3 vwp = cur_refl.a > 0.0 ? wp - cur_refl.a * V : wp;
        float bt = clamp(rough / RT_REFL_VIRTUAL_REPROJ_ROUGHNESS_BLEND, 0.0, 1.0);
        float3 rp = mix(vwp, wp, bt);
        if (oid_ok) { rp = (obj_m * float4(rp, 1.0)).xyz; }
        float4 rclip = p.prev_view_proj * float4(rp, 1.0);
        if (rclip.w > 1e-6) {
            float3 rndc = rclip.xyz / rclip.w;
            float2 ruv = float2(rndc.x * 0.5 + 0.5, 0.5 - rndc.y * 0.5);
            if (all(ruv >= 0.0) && all(ruv <= 1.0) && rndc.z >= 0.0 && rndc.z <= 1.0) {
                // Per-tap validated bilinear history resample: 2x2 footprint,
                // each tap validated via normal test only (no depth test — see
                // contract comment above); invalid taps get zero weight;
                // all-invalid = full reject.
                float2 pf = ruv * float2(p.size) - 0.5;
                int2 base = int2(floor(pf));
                float2 fr = pf - float2(base);
                float w[4] = { (1.0-fr.x)*(1.0-fr.y), fr.x*(1.0-fr.y), (1.0-fr.x)*fr.y, fr.x*fr.y };
                int2 offs[4] = { int2(0,0), int2(1,0), int2(0,1), int2(1,1) };
                float wsum = 0.0; float3 rsum = float3(0.0);
                for (int i = 0; i < 4; ++i) {
                    int2 t = clamp(base + offs[i], int2(0), int2(p.size) - 1);
                    uint2 tt = uint2(t);
                    bool normal_ok = dot(normalize(normal_history_read.read(tt).xyz), cur_normal_prev) > NORMAL_REJECT_COS_THRESHOLD;
                    if (normal_ok) {
                        wsum += w[i];
                        rsum += w[i] * refl_history_read.read(tt).rgb;
                    }
                }
                if (wsum > 1e-4) {
                    refl_write = mix(clamp_refl_history(rsum / wsum, hi_refl, tid, p.size), cur_refl.rgb, RT_REFL_ACCUM_ALPHA);
                }
            }
        }
    }
    refl_history_write.write(float4(refl_write, cur_refl.a), tid);
}

// RT-T1-B value-level test surface ONLY (`docs/RAYTRACING_DESIGN.md` section 8
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
    device RtNormalSource*         normal_sources [[buffer(0)]],
    constant DebugFetchNormalParams& p              [[buffer(1)]],
    device packed_float3*            out_normal     [[buffer(2)]],
    uint tid [[thread_position_in_grid]])
{
    if (tid != 0u) return;
    float3 n = fetch_interpolated_normal(normal_sources, p.instance_id, p.primitive_id, float2(p.bary));
    out_normal[0] = packed_float3(n);
}

// BUG-dx6w value-test-only surface, mirroring the RT-T1-B
// `debug_fetch_interpolated_normal` precedent above — exercises the EXACT
// SAME `clamp_refl_history` helper `accumulate_irradiance` calls
// internally, against a caller-supplied 3x3 neighborhood texture and
// history value, no accumulation pass involved. Not part of the production
// dispatch path (never called by `render_scene.rs`).
kernel void debug_clamp_refl_history(
    texture2d<float>              hi_refl [[texture(0)]],
    constant packed_float3&        history [[buffer(0)]],
    device packed_float3*          out     [[buffer(1)]],
    uint tid [[thread_position_in_grid]])
{
    if (tid != 0u) return;
    float3 r = clamp_refl_history(float3(history), hi_refl, uint2(1, 1), uint2(3, 3));
    out[0] = packed_float3(r);
}
"#;

/// One shadow-casting light's ray-tracing params — the per-caster payload
/// of [`ShadowRayParams::casters`]. Field order/packing mirrors the MSL
/// `RtCasterParams` exactly (P0 section 5.1 kernel lesson).
///
/// `kind` 0 = sun (`dir_or_pos` = normalized direction FROM the surface
/// TOWARD the sun, `cone_or_size` = cone half-angle radians); `kind` 1 =
/// point (`dir_or_pos` = world-space light position, `cone_or_size` =
/// world-units light diameter, `0.0` = hard shadows). `color` is
/// premultiplied color×intensity, same convention as `render_scene.rs`'s
/// `Light::color`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RtCasterParams {
    pub dir_or_pos: [f32; 3],
    pub cone_or_size: f32,
    pub color: [f32; 3],
    pub kind: u32,
}

const _: () = assert!(std::mem::size_of::<RtCasterParams>() == 32);

impl RtCasterParams {
    pub const ZERO: Self = Self {
        dir_or_pos: [0.0; 3],
        cone_or_size: 0.0,
        color: [0.0; 3],
        kind: 0,
    };

    pub fn new(dir_or_pos: [f32; 3], cone_or_size: f32, color: [f32; 3], kind: u32) -> Self {
        Self {
            dir_or_pos,
            cone_or_size,
            color,
            kind,
        }
    }
}

/// CPU mirror of `ShadowRayParams` above — field order and packing MUST
/// match exactly (P0 section 5.1 kernel lesson: `packed_float3` in MSL == dense
/// `[f32; 3]` here, no padding).
///
/// RAYTRACING_DESIGN.md section 5.2 P2 extended this in place (same struct, same
/// binding(1) slot, same single half-res dispatch — D11/D16's "P2 joins
/// the SAME half-res dispatch and SAME upsample" seam, not a parallel
/// pass): `ao_radius`/`ao_spp` drive the added AO-ray gather, `ambient_color`
/// is the demodulated-irradiance term's flat-env input (no albedo folded
/// in here — that happens once, downstream, in `render_scene.wgsl`'s
/// shading step, per D3's "accumulate lighting separated from albedo").
///
/// Per-caster shadow support (multi-caster fix): `sun_dir`/`sun_cone`/
/// `sun_color` (single-caster-only) replaced with `casters`/`caster_count`
/// — up to [`MAX_RT_CASTERS`] independently-traced casters, one visibility
/// channel per slot in `trace_shadow_rays`'s `out_sv` output.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ShadowRayParams {
    pub shadow_spp: u32,
    pub frame_index: u32,
    pub trace_size: [u32; 2],
    pub gbuffer_size: [u32; 2],
    /// World-space max AO ray distance (RT-P2). 0 samples (`ao_spp == 0`)
    /// skips the AO gather entirely.
    pub ao_radius: f32,
    /// AO rays per pixel (RT-P2 half-res dispatch).
    pub ao_spp: u32,
    /// RT-P3: one-bounce GI gather rays/pixel (emissive-hit + sun-bounce).
    /// 0 skips the gather entirely (same discipline as `ao_spp == 0`).
    pub gi_spp: u32,
    /// Number of valid entries in `casters` (0..=[`MAX_RT_CASTERS`]). Slots
    /// at/beyond this count are ignored by the kernel and read back as
    /// visibility 1.0 (unshadowed).
    pub caster_count: u32,
    pub casters: [RtCasterParams; MAX_RT_CASTERS],
    /// Flat ambient/env color (scene `atmosphere.ambient_tint` scaled by
    /// a named constant — RAYTRACING_DESIGN.md section 5.2 P2's "denoiser/
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
    /// RT-R1 (RAYTRACING section 9.3, RD7/RD8): reflection-ray config. `refl_spp`
    /// = reflection rays/pixel (1 in v1; 0 disables the branch — inert in
    /// T3, the kernel reads these in T5). `refl_max_roughness` =
    /// RT_REFLECTION_MAX_ROUGHNESS (0.6 starting, RD7 BRDF-domain split);
    /// `refl_rough_band` = the blend-band width. `_pad_refl` pads the block
    /// to 16B — with `casters` sized as it is, `inv_view_proj` lands on a
    /// 16-byte boundary (208) without any extra alignment padding; see the
    /// offset assert below.
    pub refl_spp: u32,
    pub refl_max_roughness: f32,
    pub refl_rough_band: f32,
    _pad_refl: u32,
    /// Column-major, matches `render_scene.rs`'s `mat4_inverse` output.
    pub inv_view_proj: [[f32; 4]; 4],
}

/// Fixed per-dispatch shadow-caster slot count — mirrors `render_scene.rs`'s
/// `MAX_SHADOW_CASTING_LIGHTS` (both are 4; no compiler-enforced link
/// between the two crates, same manual-sync discipline this file already
/// uses for other cross-crate constants).
pub const MAX_RT_CASTERS: usize = 4;

impl ShadowRayParams {
    /// Construct with the alignment padding zeroed. `casters` may contain
    /// up to [`MAX_RT_CASTERS`] entries; extras beyond that are ignored and
    /// `caster_count` is clamped to the slice's (capped) length.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        casters: &[RtCasterParams],
        shadow_spp: u32,
        frame_index: u32,
        trace_size: [u32; 2],
        gbuffer_size: [u32; 2],
        ao_radius: f32,
        ao_spp: u32,
        gi_spp: u32,
        ambient_color: [f32; 3],
        camera_pos: [f32; 3],
        inv_view_proj: [[f32; 4]; 4],
        refl_spp: u32,
        refl_max_roughness: f32,
        refl_rough_band: f32,
    ) -> Self {
        let caster_count = casters.len().min(MAX_RT_CASTERS) as u32;
        let mut caster_arr = [RtCasterParams::ZERO; MAX_RT_CASTERS];
        for (slot, c) in caster_arr.iter_mut().zip(casters.iter()) {
            *slot = *c;
        }
        Self {
            shadow_spp,
            frame_index,
            trace_size,
            gbuffer_size,
            ao_radius,
            ao_spp,
            gi_spp,
            caster_count,
            casters: caster_arr,
            ambient_color,
            camera_pos,
            refl_spp,
            refl_max_roughness,
            refl_rough_band,
            _pad_refl: 0,
            inv_view_proj,
        }
    }
}

/// CPU mirror of the MSL `GiMaterial` struct — RT-P3's per-instance
/// emissive/albedo table for the GI gather's emissive-hit + sun-bounce
/// terms. Field order and packing MUST match exactly (P0 section 5.1 kernel
/// lesson).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GiMaterial {
    pub albedo: [f32; 3],
    _pad0: f32,
    pub emissive: [f32; 3],
    _pad1: f32,
    /// RT-R1: x = metallic, y = roughness — read straight off
    /// `d.uniforms.pbr_metallic_roughness` (render_scene.rs:332), the SAME
    /// resolved factors `fs_pbr` shades with. z/w reserved.
    pub metallic_roughness: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<GiMaterial>() == 48);

impl GiMaterial {
    pub fn new(albedo: [f32; 3], emissive: [f32; 3], metallic_roughness: [f32; 4]) -> Self {
        Self {
            albedo,
            _pad0: 0.0,
            emissive,
            _pad1: 0.0,
            metallic_roughness,
        }
    }
}

// RT-D3/RT-P2 alignment gotcha (see `ShadowRayParams::refl_spp` block's doc
// comment): this is the regression guard a GPU test alone wouldn't localize
// as clearly — if `inv_view_proj`'s offset ever drifts from its required
// 16-byte-aligned value (a field reordered/resized above it), this fails at
// compile time instead of silently reading garbage on the GPU.
const _: () = assert!(std::mem::offset_of!(ShadowRayParams, inv_view_proj) == 208);
const _: () = assert!(std::mem::size_of::<ShadowRayParams>() == 272);

/// RT-T1-B (RAYTRACING_DESIGN.md section 8 Tier-1 item 2): per-object bindless
/// indirection for real vertex-normal interpolation in the RT trace kernel
/// — one entry per object, SAME order as the `objects` slice `build_accel`
/// was called with (so `hit.instance_id` at any ray hit indexes this
/// directly, identical convention to [`GiMaterial`]). `vertex_base_addr` is
/// `MTLBuffer::gpuAddress()` (via [`GpuBuffer::gpu_address`]) PLUS the
/// object's `vertex_offset` already folded in — the kernel reads
/// `vertex_base_addr + vertex_index * vertex_stride + normal_offset` as a
/// raw `packed_float3`. Metal documents that binding an acceleration
/// structure makes its transitively-referenced resources resident — but
/// BUG-jddy proved that insufficient in practice: static scenes lost
/// GI/reflections until `dispatch_compute_with_accel` explicitly
/// `useResource`-declared the TLAS, every BLAS, and the instance buffer.
/// Treat that explicit declaration as the contract, not the doc claim.
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
    /// RT-T2-A (RAYTRACING_DESIGN.md section 8.2 Tier-2 item 4): extends this SAME
    /// bindless table (D21's brief) rather than a parallel one — see the
    /// MSL mirror's doc comment for the field-by-field extension.
    pub uv_offset: u32,
    pub alpha_mask: u32,
    pub alpha_cutoff: f32,
    /// Index into `trace_shadow_rays`'s fixed `material_textures` array;
    /// `>= MAX_RT_MATERIAL_TEXTURES` means "no texture bound" (degrades to
    /// always-pass — see `ensure_normal_sources`).
    pub alpha_tex_index: u32,
    /// Raster-parity reflections (RAYTRACING_DESIGN.md section 9.6): base-color texture
    /// index for hit-point material sampling; `>= MAX_RT_MATERIAL_TEXTURES` means
    /// "no texture bound" (flat gi_materials albedo is the fallback).
    pub base_color_tex_index: u32,
    /// Textured roughness (R3) (RAYTRACING_DESIGN.md section 9.6): metallic-roughness
    /// texture index for the reflection lobe's primary-hit sampling;
    /// `>= MAX_RT_MATERIAL_TEXTURES` means "no texture bound" (flat
    /// `GiMaterial::metallic_roughness` factor is the fallback).
    pub mr_tex_index: u32,
    /// Explicit pad — this struct leads with a `u64` (align-8); every
    /// field after it must keep the whole struct's size a multiple of 8,
    /// same discipline the RT-T2-A extension already established.
    pub _pad: u32,
}

const _: () = assert!(std::mem::size_of::<RtNormalSource>() == 80);

/// Fixed texture-argument-table slot count for per-object material textures
/// (alpha-mask + base-color; roughness/metallic/normals consume this same cap) —
/// MUST match the embedded MSL's `#define MAX_RT_MATERIAL_TEXTURES` (manual-sync
/// discipline, same as every other CPU/GPU struct mirror in this file).
/// Raster-parity reflections raised this from 4 to 64 to headroom the AMG GT3
/// hero asset (39 unique textures wired across all materials; this cap covers
/// that plus growth). Raise when a hero scene's RT-caster set needs more; cost
/// is one more fixed texture-array binding (4 bytes/table-entry GPU, negligible CPU).
pub const MAX_RT_MATERIAL_TEXTURES: usize = 64;
/// Sentinel tex_index meaning "no texture bound" — degrades to factor fallback.
pub const RT_MATERIAL_TEX_INDEX_NONE: u32 = u32::MAX;

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
    let mut material_textures: Vec<&'a GpuTexture> = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        let alpha_tex_index = if obj.alpha_mask {
            match obj.base_color_texture {
                Some(tex) if material_textures.len() < MAX_RT_MATERIAL_TEXTURES => {
                    // Check if this texture is already bound
                    let idx = material_textures.iter().position(|&t| std::ptr::eq(t, tex))
                        .unwrap_or_else(|| {
                            material_textures.push(tex);
                            material_textures.len() - 1
                        });
                    idx as u32
                }
                Some(_) => {
                    log::warn!("RT alpha-mask texture table full ({} bound, {} cap) — object {} degraded to always-pass",
                        material_textures.len(), MAX_RT_MATERIAL_TEXTURES, i);
                    RT_MATERIAL_TEX_INDEX_NONE
                }
                None => RT_MATERIAL_TEX_INDEX_NONE,
            }
        } else {
            RT_MATERIAL_TEX_INDEX_NONE
        };
        let base_color_tex_index = match obj.base_color_texture {
            Some(tex) if material_textures.len() < MAX_RT_MATERIAL_TEXTURES => {
                // Check if this texture is already bound (deduplicate)
                let idx = material_textures.iter().position(|&t| std::ptr::eq(t, tex))
                    .unwrap_or_else(|| {
                        material_textures.push(tex);
                        material_textures.len() - 1
                    });
                idx as u32
            }
            Some(_) => {
                log::warn!("RT material-texture table full ({} bound, {} cap) — object {} base-color degraded to flat albedo",
                    material_textures.len(), MAX_RT_MATERIAL_TEXTURES, i);
                RT_MATERIAL_TEX_INDEX_NONE
            }
            None => RT_MATERIAL_TEX_INDEX_NONE,
        };
        // Textured roughness (R3) (RAYTRACING_DESIGN.md section 9.6): SAME dedupe-
        // into-`material_textures`, cap-check, and log-warn-on-full pattern
        // as `base_color_tex_index` above — rides the one general
        // material-texture cap Raster-parity reflections widened, no
        // separate table.
        let mr_tex_index = match obj.mr_texture {
            Some(tex) if material_textures.len() < MAX_RT_MATERIAL_TEXTURES => {
                let idx = material_textures.iter().position(|&t| std::ptr::eq(t, tex))
                    .unwrap_or_else(|| {
                        material_textures.push(tex);
                        material_textures.len() - 1
                    });
                idx as u32
            }
            Some(_) => {
                log::warn!("RT material-texture table full ({} bound, {} cap) — object {} MR map degraded to flat metallic_roughness factor",
                    material_textures.len(), MAX_RT_MATERIAL_TEXTURES, i);
                RT_MATERIAL_TEX_INDEX_NONE
            }
            None => RT_MATERIAL_TEX_INDEX_NONE,
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
            base_color_tex_index,
            mr_tex_index,
            _pad: 0,
        };
        unsafe {
            std::ptr::write_unaligned(ptr.add(i * std::mem::size_of::<RtNormalSource>()) as *mut _, src);
        }
    }
    material_textures
}

/// CPU mirror of the MSL `AccumulateParams` struct backing
/// `accumulate_irradiance` — RAYTRACING_DESIGN.md section 5.2 P2/D3's temporal-
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
    /// RT-T2-C (object motion): number of entries in the `obj_motion`
    /// buffer; a per-pixel object id at or beyond this count reprojects
    /// camera-only (identity object motion).
    pub obj_count: u32,
    /// RT-R2 (RD6): camera world position for the virtual-hit-point
    /// reprojection (12 bytes, same layout as the three `u32` pads it
    /// replaces — keeps `inv_view_proj`/`prev_view_proj` at the same
    /// 16-byte-aligned offsets).
    pub camera_pos: [f32; 3],
    /// RT-T1-C (BUG-311): current-frame inverse view-proj, for
    /// reconstructing this texel's world position from `depth_tex` — SAME
    /// matrix `ShadowRayParams::inv_view_proj` already carries this frame.
    pub inv_view_proj: [[f32; 4]; 4],
    /// RT-T1-C (BUG-311): PREVIOUS frame's view-proj, for reprojecting the
    /// reconstructed world position to locate/validate the history sample.
    /// Already threaded through `RenderScene` for MetalFX
    /// (RAYTRACING_DESIGN.md section 8 Tier-1 item 1); no new CPU-side matrix.
    pub prev_view_proj: [[f32; 4]; 4],
}

// `size`(8) + `alpha`(4) + `reset`(4) + `obj_count`(4) + camera_pos(12)
// = 32 bytes — a multiple of 16, so both `float4x4`s that follow land on
// a 16-byte boundary (RT-R2's camera_pos replaces the old u32 pads at
// the same offset).
// Asserted directly rather than re-derived, same discipline as the
// `ShadowRayParams` guard above.
const _: () = assert!(std::mem::offset_of!(AccumulateParams, camera_pos) == 20);
const _: () = assert!(std::mem::offset_of!(AccumulateParams, inv_view_proj) == 32);
const _: () = assert!(std::mem::offset_of!(AccumulateParams, prev_view_proj) == 96);
const _: () = assert!(std::mem::size_of::<AccumulateParams>() == 160);

impl AccumulateParams {
    pub fn new(
        size: [u32; 2],
        alpha: f32,
        reset: bool,
        obj_count: u32,
        camera_pos: [f32; 3],
        inv_view_proj: [[f32; 4]; 4],
        prev_view_proj: [[f32; 4]; 4],
    ) -> Self {
        Self {
            size,
            alpha,
            reset: reset as u32,
            obj_count,
            camera_pos,
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
    /// RT-R2: number of objects in the `gi_materials` table — used by the
    /// kernel to bounds-check the roughness lookup for the refl-channel
    /// luma edge-stop.
    pub obj_count: u32,
}

const _: () = assert!(std::mem::size_of::<AtrousParams>() == 20);

impl AtrousParams {
    pub fn new(size: [u32; 2], step: u32, history_valid: bool, obj_count: u32) -> Self {
        Self {
            size,
            step,
            history_valid: history_valid as u32,
            obj_count,
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
        out_refl: &GpuTexture,
        // RT-R1 (section 9.3 RD4): prefiltered-specular env mip chain — the
        // reflection ray's miss radiance. Always bound (dummy when the
        // scene has no env chain).
        prefiltered_env: &GpuTexture,
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
        lo_refl: &GpuTexture,
        hi_refl: &GpuTexture,
        label: &str,
    );

    /// RT-T1-D (RAYTRACING_DESIGN.md section 8 Tier-1 item 3, BUG-312): one
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
        gi_materials: &GpuBuffer,
        depth_tex: &GpuTexture,
        moments_read: &GpuTexture,
        src_sv: &GpuTexture,
        dst_sv: &GpuTexture,
        src_irr: &GpuTexture,
        dst_irr: &GpuTexture,
        src_n: &GpuTexture,
        dst_n: &GpuTexture,
        src_refl: &GpuTexture,
        dst_refl: &GpuTexture,
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
        // RT-R2 (RD6): reflection channel — current-frame filtered reflections
        // (`.a` = hit distance), specular history ping-pong, and the material
        // table (roughness source for the reprojection blend, Step 2).
        hi_refl: &GpuTexture,
        refl_history_read: &GpuTexture,
        refl_history_write: &GpuTexture,
        gi_materials: &GpuBuffer,
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
    /// BUG-dx6w value-test-only surface (`debug_clamp_refl_history`'s only
    /// caller) — see the MSL `debug_clamp_refl_history` kernel's doc
    /// comment. Always compiled (tiny kernel, negligible cost); never
    /// dispatched by the production `render_scene.rs` path.
    debug_clamp_refl_history_pipeline: GpuComputePipeline,
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

        // Raster-parity reflections: the material-texture table is
        // MAX_RT_MATERIAL_TEXTURES wide, so the slot list is built, not a
        // literal — the R1 incident below happened TWICE (T3's original
        // miss, then the 64-wide table move left out_refl/prefiltered_env
        // at the OLD indices 8/9 while the MSL moved to 68/69 — writes went
        // to a dummy, the mirror probe read zeros). Computed from the cap
        // so a future cap change can't strand them again.
        let mut trace_slots: Vec<(u32, SlotKind)> = vec![
            (1, SlotKind::Buffer),
            (2, SlotKind::Buffer), // RT-P3: gi_materials, MSL [[buffer(2)]]
            (3, SlotKind::Buffer), // RT-T1-B: normal_sources, MSL [[buffer(3)]]
            (0, SlotKind::Texture),
            (1, SlotKind::Texture),
            (2, SlotKind::Texture),
            (3, SlotKind::Texture), // RT-T1-C: out_n, MSL [[texture(3)]]
        ];
        // material_textures[MAX_RT_MATERIAL_TEXTURES], MSL [[texture(4)]] —
        // occupies MAX_RT_MATERIAL_TEXTURES consecutive slots starting at 4.
        trace_slots.extend((4..4 + MAX_RT_MATERIAL_TEXTURES as u32).map(|i| (i, SlotKind::Texture)));
        // RT-R1 (section 9.3): out_refl, MSL [[texture(68)]]. MISSED by the
        // T3 plumbing (slot maps weren't extended with the kernel
        // signatures) — the reflection block's writes went nowhere
        // and the chain read zeros; caught by the R1 mirror probe.
        trace_slots.push((4 + MAX_RT_MATERIAL_TEXTURES as u32, SlotKind::Texture));
        // RT-R1: prefiltered_env, MSL [[texture(69)]] — miss-branch
        // radiance source.
        trace_slots.push((5 + MAX_RT_MATERIAL_TEXTURES as u32, SlotKind::Texture));
        let trace_pipeline = compile_pipeline(
            device,
            &library,
            "trace_shadow_rays",
            identity_slot_map(&trace_slots),
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
                // RT-R1 (section 9.3): lo_refl / hi_refl — see the trace pipeline's
                // slot-map note (T3 missed these too).
                (7, SlotKind::Texture),
                (8, SlotKind::Texture),
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
                // RT-R1 (section 9.3): src_refl / dst_refl — see the trace
                // pipeline's slot-map note (T3 missed these too).
                (8, SlotKind::Texture),
                (9, SlotKind::Texture),
                // RT-R2: gi_materials — roughness source for the refl luma
                // stop. Signatures and slot maps change together (R1 incident
                // class).
                (2, SlotKind::Buffer),
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
                // RT-R2 (RD6): hi_refl / refl history pair / gi_materials —
                // the R1 slot-map incident class; signatures and slot maps
                // change together.
                (11, SlotKind::Texture),
                (12, SlotKind::Texture),
                (13, SlotKind::Texture),
                (3, SlotKind::Buffer),
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

        let debug_clamp_refl_history_pipeline = compile_pipeline(
            device,
            &library,
            "debug_clamp_refl_history",
            identity_slot_map(&[
                (0, SlotKind::Texture),
                (0, SlotKind::Buffer),
                (1, SlotKind::Buffer),
            ]),
        );

        let dummy_alpha_tex = create_dummy_alpha_texture(device);

        Self {
            trace_pipeline,
            upsample_pipeline,
            atrous_pipeline,
            accumulate_pipeline,
            debug_fetch_normal_pipeline,
            debug_clamp_refl_history_pipeline,
            dummy_alpha_tex,
        }
    }

    /// RT-T1-B value-test-only entry point (`docs/RAYTRACING_DESIGN.md` section 8
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

    /// BUG-dx6w value-test-only entry point — dispatches the SAME
    /// `clamp_refl_history` MSL helper `accumulate_irradiance` uses
    /// internally, against a caller-supplied 3x3 `hi_refl` neighborhood
    /// (row-major, `neighborhood[0]` = top-left) and a history value. No
    /// accumulation pass, no ray tracing/RNG involved. Synchronous (commits
    /// and waits) — test-only call pattern, never used on a hot path.
    pub fn debug_clamp_refl_history(
        &self,
        device: &GpuDevice,
        neighborhood: &[[f32; 4]; 9],
        history: [f32; 3],
    ) -> [f32; 3] {
        let neighborhood_tex = device.create_texture(&GpuTextureDesc {
            width: 3,
            height: 3,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "bug-dx6w-debug-clamp-refl-history-neighborhood",
            mip_levels: 1,
        });
        let neighborhood_bytes: Vec<u8> = neighborhood
            .iter()
            .flat_map(|texel| texel.iter().flat_map(|c| c.to_le_bytes()))
            .collect();
        device.upload_texture(&neighborhood_tex, &neighborhood_bytes);

        let history_buffer = device.create_buffer_shared(16); // packed_float3, rounded up
        let history_ptr = history_buffer
            .mapped_ptr()
            .expect("debug history buffer must be CPU-mapped");
        unsafe {
            std::ptr::copy_nonoverlapping(history.as_ptr(), history_ptr as *mut f32, 3);
        }
        let out_buffer = device.create_buffer_shared(16); // packed_float3, rounded up
        out_buffer.zero_fill();

        let cb = device
            .raw_queue()
            .commandBuffer()
            .expect("Failed to acquire command buffer for BUG-dx6w debug dispatch");
        let enc: Retained<ProtocolObject<dyn MTLComputeCommandEncoder>> = cb
            .computeCommandEncoder()
            .expect("computeCommandEncoder failed");
        unsafe {
            enc.setComputePipelineState(&self.debug_clamp_refl_history_pipeline.state);
            enc.setTexture_atIndex(Some(&neighborhood_tex.raw), 0);
            enc.setBuffer_offset_atIndex(Some(history_buffer.raw()), 0, 0);
            enc.setBuffer_offset_atIndex(Some(out_buffer.raw()), 0, 1);
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
        out_refl: &GpuTexture,
        prefiltered_env: &GpuTexture,
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
        // RT-T2-A / Raster-parity reflections: fill all MAX_RT_MATERIAL_TEXTURES
        // argument-table slots — real textures first (caller-supplied order matches
        // `RtNormalSource::alpha_tex_index`/`base_color_tex_index`), the 1x1 dummy
        // for the rest (Metal requires every slot a compiled kernel references
        // bound to a valid resource).
        for i in 0..MAX_RT_MATERIAL_TEXTURES {
            let tex = alpha_textures.get(i).copied().unwrap_or(&self.dummy_alpha_tex);
            bindings.push(GpuBinding::Texture {
                binding: 4 + i as u32,
                texture: tex,
            });
        }
        // RT-R1 (section 9.3): out_refl at [[texture(68)]] — free (material_textures
        // occupy 4..68, i.e. 4 + 64). Computed from the cap (see the slot-map
        // note in `new` — hard-coded 8/9 here was the second slot-map miss).
        bindings.push(GpuBinding::Texture {
            binding: 4 + MAX_RT_MATERIAL_TEXTURES as u32,
            texture: out_refl,
        });
        // RT-R1 (section 9.3 RD4): prefiltered env chain at [[texture(69)]] — the
        // reflection miss branch's radiance source.
        bindings.push(GpuBinding::Texture {
            binding: 5 + MAX_RT_MATERIAL_TEXTURES as u32,
            texture: prefiltered_env,
        });
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
        lo_refl: &GpuTexture,
        hi_refl: &GpuTexture,
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
                // RT-R1 (section 9.3): reflection-radiance textures — bind-only, inert until T5.
                GpuBinding::Texture {
                    binding: 7,
                    texture: lo_refl,
                },
                GpuBinding::Texture {
                    binding: 8,
                    texture: hi_refl,
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
        gi_materials: &GpuBuffer,
        depth_tex: &GpuTexture,
        moments_read: &GpuTexture,
        src_sv: &GpuTexture,
        dst_sv: &GpuTexture,
        src_irr: &GpuTexture,
        dst_irr: &GpuTexture,
        src_n: &GpuTexture,
        dst_n: &GpuTexture,
        src_refl: &GpuTexture,
        dst_refl: &GpuTexture,
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
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: gi_materials,
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
                // RT-R1 (section 9.3): reflection-radiance textures — bind-only, inert until T5.
                GpuBinding::Texture {
                    binding: 8,
                    texture: src_refl,
                },
                GpuBinding::Texture {
                    binding: 9,
                    texture: dst_refl,
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
        // RT-R2 (RD6): reflection channel — current-frame filtered reflections
        // (`.a` = hit distance), specular history ping-pong, and the material
        // table (roughness source for the reprojection blend, Step 2).
        hi_refl: &GpuTexture,
        refl_history_read: &GpuTexture,
        refl_history_write: &GpuTexture,
        gi_materials: &GpuBuffer,
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
                // RT-R2 (RD6): hi_refl / refl history pair / gi_materials —
                // the R1 slot-map incident class; signatures and slot maps
                // change together.
                GpuBinding::Texture {
                    binding: 11,
                    texture: hi_refl,
                },
                GpuBinding::Texture {
                    binding: 12,
                    texture: refl_history_read,
                },
                GpuBinding::Texture {
                    binding: 13,
                    texture: refl_history_write,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: gi_materials,
                    offset: 0,
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
    // Compile-time offset (not a hand-counted magic number) — survives any
    // future `ShadowRayParams` field reordering/resizing without drifting.
    let offset = std::mem::offset_of!(ShadowRayParams, gbuffer_size);
    unsafe {
        let p = ptr.add(offset) as *const u32;
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
