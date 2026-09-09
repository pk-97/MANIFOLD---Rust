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
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSArray;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLAccelerationStructure, MTLAccelerationStructureCommandEncoder,
    MTLAccelerationStructureGeometryDescriptor, MTLAccelerationStructureInstanceDescriptor,
    MTLAccelerationStructureInstanceOptions, MTLAccelerationStructureTriangleGeometryDescriptor,
    MTLAccelerationStructureUsage, MTLAttributeFormat, MTLBuffer, MTLCommandBuffer, MTLCommandBufferStatus,
    MTLCommandEncoder,
    MTLCommandQueue, MTLCompileOptions, MTLComputeCommandEncoder, MTLComputePipelineState,
    MTLDataType, MTLDevice, MTLFunctionConstantValues,
    MTLInstanceAccelerationStructureDescriptor, MTLIndexType, MTLLanguageVersion,
    MTLLibrary, MTLPackedFloat3, MTLPackedFloat4x3, MTLPrimitiveAccelerationStructureDescriptor,
    MTLResourceUsage, MTLSize,
};

use manifold_foundation::cold_touch::{ColdTouchKind, record_cold_touch};

use super::device::GpuDevice;
use super::types::{GpuBuffer, GpuComputePipeline, GpuTexture};
use super::{GpuEncoder, Slot, SlotKind, SlotMap};
use crate::types::{GpuBinding, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};
use crate::trace_planner::{TraceRegion, DEFAULT_TRACE_WORK_LIMITS, estimate_trace_query_units_per_pixel, plan_trace_regions};

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
    ///
    /// RT_INSTANCING_DESIGN.md D1: in the INSTANCED mode (any object wired
    /// with `instances_addr != 0` at build — P1.5: 1-capacity included,
    /// its TRS is GPU-side) this is the GPU-private buffer
    /// the descriptor-build kernel writes and the TLAS build/refit consumes
    /// — the TLAS descriptor's `instanceDescriptorBuffer` points at it in
    /// BOTH modes, so encoder.rs's existing declaration covers it.
    pub(crate) instance_buffer: GpuBuffer,
    /// RT_INSTANCING_DESIGN.md D1/D7: true when the accel was built with
    /// the GPU descriptor-build path (any object had `instances_addr != 0`
    /// at build — P1.5: 1-capacity included). Refit then re-dispatches the descriptor
    /// kernel + TLAS refit in one command buffer instead of rewriting
    /// `instance_buffer` from the CPU (it is GPU-private in this mode).
    pub(crate) instanced: bool,
    /// RT_INSTANCING_DESIGN.md D9/P0.5: Σ effective instance slots at
    /// BUILD time. Refit asserts the running Σ equals this — a capacity
    /// shrink with an unchanged object count would otherwise leave stale
    /// descriptors beyond the rewritten range (the count assert alone
    /// can't see it). Rigid topology: a real change rebuilds instead.
    pub(crate) instance_slot_total: u32,
    pub(crate) topology: Vec<RtGeometryTopology>,
    /// RT_INSTANCING_DESIGN.md D1: CPU-mapped per-object descriptor-build
    /// params (model matrix, `instances_addr`, slot base/count, cast mask) —
    /// rewritten every instanced build/refit from the CURRENT `objects`
    /// slice (buffer identity deliberately does NOT ride the topo key; D9),
    /// consumed by the descriptor-build kernel on the GPU. `None` in the
    /// D7 fast path.
    pub(crate) instance_obj_params: Option<GpuBuffer>,
    /// Retained handles to every object's vertex (and index) buffers as
    /// built. The trace kernels read these through RAW GPU ADDRESSES
    /// (`RtNormalSource.vertex_base_addr`) — an indirect reach no binding
    /// declares, exactly the BUG-jddy reclamation class: under memory
    /// pressure the driver may reclaim a resource no submitted command
    /// declares usage on (BUG-84fv audit). Retaining them here pins
    /// lifetime to the accel's; encoder.rs's accel dispatch declares
    /// useResource on each per trace dispatch. Buffer-identity changes
    /// are a topology change (refit contract) and rebuild the accel, so
    /// these never go stale across a refit.
    pub(crate) geometry_buffers: Vec<Retained<ProtocolObject<dyn MTLBuffer>>>,
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
    /// RS-B: emissive-triangle light table built alongside the accel from the
    /// same `objects` slice — `None` when the scene has no emissive geometry.
    /// GPU buffers for the kernel's alias-draw + point-sample step (RS-C);
    /// CPU-side local-space vertices for refit alongside the TLAS.
    pub emissive_table: Option<EmissiveLightTable>,
    /// Queue clone for `Drop`'s self-retire (see the Drop impl below).
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RtGeometryTopology { vertex: usize, vertex_offset: u32, vertex_stride: u32, triangle_count: u32, index: Option<usize>, normal_offset: u32, uv_offset: u32, instance_slots: u32, wired: bool, alpha_mask: bool }
impl RtGeometryTopology { fn from_geometry(o: &RtObjectGeometry<'_>) -> Self { Self { vertex: o.vertex_buffer.identity_key(), vertex_offset: o.vertex_offset, vertex_stride: o.vertex_stride, triangle_count: o.triangle_count, index: o.index_buffer.map(|b| b.identity_key()), normal_offset: o.normal_offset, uv_offset: o.uv_offset, instance_slots: effective_instance_slots(o), wired: o.instances_addr != 0, alpha_mask: o.alpha_mask } } }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum RtTopologyMismatchCategory { ObjectCount, DescriptorMode, Vertex, Index, NormalUv, InstanceSlots, AlphaMask, SlotOverflow }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct RtTopologyMismatch { pub object: usize, pub category: RtTopologyMismatchCategory }
fn check_topology_records<I: ExactSizeIterator<Item = RtGeometryTopology>>(resident: &[RtGeometryTopology], resident_instanced: bool, resident_slots: u32, current: I) -> Result<(), RtTopologyMismatch> {
    if resident.len() != current.len() { return Err(RtTopologyMismatch { object: resident.len().min(current.len()), category: RtTopologyMismatchCategory::ObjectCount }); }
    let mut slots = 0u32; let mut instanced = false;
    for (i, (a, b)) in resident.iter().zip(current).enumerate() {
        instanced |= b.wired; slots = slots.checked_add(b.instance_slots).ok_or(RtTopologyMismatch { object: i, category: RtTopologyMismatchCategory::SlotOverflow })?;
        if a.vertex != b.vertex || a.vertex_offset != b.vertex_offset || a.vertex_stride != b.vertex_stride || a.triangle_count != b.triangle_count { return Err(RtTopologyMismatch { object: i, category: RtTopologyMismatchCategory::Vertex }); }
        if a.index != b.index { return Err(RtTopologyMismatch { object: i, category: RtTopologyMismatchCategory::Index }); }
        if a.normal_offset != b.normal_offset || a.uv_offset != b.uv_offset { return Err(RtTopologyMismatch { object: i, category: RtTopologyMismatchCategory::NormalUv }); }
        if a.instance_slots != b.instance_slots || a.wired != b.wired { return Err(RtTopologyMismatch { object: i, category: RtTopologyMismatchCategory::InstanceSlots }); }
        if a.alpha_mask != b.alpha_mask { return Err(RtTopologyMismatch { object: i, category: RtTopologyMismatchCategory::AlphaMask }); }
    }
    if instanced != resident_instanced { return Err(RtTopologyMismatch { object: 0, category: RtTopologyMismatchCategory::DescriptorMode }); }
    if slots != resident_slots { return Err(RtTopologyMismatch { object: resident.len(), category: RtTopologyMismatchCategory::InstanceSlots }); }
    Ok(())
}
impl RtAccel { pub fn check_topology(&self, objects: &[RtObjectGeometry<'_>]) -> Result<(), RtTopologyMismatch> { check_topology_records(&self.topology, self.instanced, self.instance_slot_total, objects.iter().map(RtGeometryTopology::from_geometry)) } }

/// BUG-84fv class, root fix: an RtAccel must NEVER free its Metal objects
/// while a previously-committed command buffer could still reference them
/// (prior frames' Generators traces reach the TLAS/BLAS/instance/geometry
/// through raw GPU addresses; encode pacing lets the CPU run frames ahead
/// of GPU completion, and Metal's binding retention is proven insufficient
/// for this path — BUG-jddy). Drop therefore retires clones of every
/// Metal handle through `retire_on_queue`'s empty-buffer completion pin:
/// the actual deallocations happen only after everything committed before
/// the drop has finished on the GPU. Covers every scenario uniformly —
/// rebuild swap, superseded build/refit, primitive teardown (clip cut,
/// preset swap, project close) — because the protection lives in the
/// object's death, not in each call site.
impl Drop for RtAccel {
    fn drop(&mut self) {
        let pins = (
            self.structure.clone(),
            self.descriptor.clone(),
            self.refit_scratch.raw.clone(),
            self.blas.iter().map(|b| b.structure.clone()).collect::<Vec<_>>(),
            self.instance_buffer.raw.clone(),
            self.instance_obj_params.as_ref().map(|p| p.raw.clone()),
            self.geometry_buffers.clone(),
            self.emissive_table.as_ref().map(|t| (t.triangles.raw.clone(), t.aliases.raw.clone())),
        );
        super::device::retire_on_queue(&self.queue, pins, "RT accel retire");
    }
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
/// `render_scene.rs`'s `MeshVertex`, stride 64, position at offset 0) —
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
    /// RT-TL-B (RAYTRACING_DESIGN.md section 16 TL6): this object's material
    /// carries a nonzero `translucency` factor — it must ALSO leave the
    /// hardware opaque fast path, or the kernel's `walk_with_transmission`
    /// never sees solid-but-thin surfaces as candidates. Read fresh at every
    /// BLAS build and folded into the topo dirty key (render_scene.rs), so a
    /// live 0→nonzero card flip triggers a bounded async rebuild — the same
    /// D17 gesture as toggling RT itself.
    pub translucent: bool,
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
    /// BUG-wytp (rt-reflections-are-normal-map-blind): this object's tangent-
    /// space normal map (glTF packing: R/G = tangent-space X/Y, B = Z),
    /// sampled at the PRIMARY hit's interpolated UV to perturb the shading
    /// normal feeding the reflection lobe's R and the AO/GI cosine-hemisphere
    /// gather. `None` degrades to the barycentric vertex normal (pre-BUG-wytp
    /// behavior). Consumed ONLY at the primary hit; secondary/extension-ray
    /// hit shading keeps vertex normals.
    pub normal_texture: Option<&'a GpuTexture>,
    /// BUG-1gqt: this object's emissive texture, sampled at the ray hit's
    /// interpolated UV (with `emissive_uv_m/t` applied) and multiplied by
    /// the flat `GiMaterial::emissive` factor — the trace-path mirror of
    /// the raster's `resolve_emissive`. `None` = factor alone
    /// (pre-feature behavior). Consumed at every emissive-hit shading
    /// site (the GI gather's emissive term and the reflection hit's).
    pub emissive_texture: Option<&'a GpuTexture>,
    /// BUG-1gqt: KHR_texture_transform fold for the emissive map, in the
    /// raster's `apply_uv_transform` convention:
    /// `uv' = (m[0]*u + m[1]*v + t[0], m[2]*u + m[3]*v + t[1])`.
    /// Identity when the material declares no transform.
    pub emissive_uv_m: [f32; 4],
    pub emissive_uv_t: [f32; 2],
    /// Per-object shadow-cast toggle (`node.scene_object`'s `cast_shadows`
    /// param, threaded through `render_scene.rs`'s `ObjectDraw`). `false`
    /// clears `RT_MASK_SHADOW_CASTER` from this instance's mask (see
    /// [`build_instance_buffer`]) — it still carries `RT_MASK_VISIBLE`, so
    /// it stays hit by every query EXCEPT the shadow/sun-bounce rays that
    /// mask against `RT_MASK_SHADOW_CASTER` alone.
    pub cast_shadows: bool,
    /// RT_INSTANCING_DESIGN.md D1/D7: bindless GPU address of this object's
    /// `Array<InstanceTransform>` wire (mesh_common.rs's 32-byte
    /// `InstanceTransform` layout), or 0 when unwired. Wired with
    /// wired with `instance_slots > 1` puts the accel on the GPU
    /// descriptor-build path (one TLAS slot per instance slot, composed
    /// `model · T_instance` in-kernel per D4; P1.5: any WIRED capacity, 1
    /// included — a wired 1-capacity buffer's TRS lives GPU-side, which
    /// the CPU identity-descriptor path can never represent); 0 keeps the
    /// object at a single identity-slot descriptor, exactly as before this
    /// field existed.
    pub instances_addr: u64,
    /// RT_INSTANCING_DESIGN.md: the buffer backing `instances_addr` — the
    /// BUG-84fv lifetime handle. The descriptor-build kernel reads instance
    /// values through the raw address, which no binding declares, so the
    /// build/refit pins this handle through its async completion exactly
    /// like `vertex_buffer` above. `Some` iff `instances_addr != 0`;
    /// `instances_addr` must be this buffer's `gpu_address()`.
    pub instances_buffer: Option<&'a GpuBuffer>,
    /// RT_INSTANCING_DESIGN.md D2/D9: per-object instance-slot CAPACITY
    /// (rigid topology — a change rebuilds the accel). 0 or 1 = unwired /
    /// identity (the D7 fast path). The raster's live count is in-band
    /// (`pos_scale.w == 0` = dead slot), so capacity is all the CPU needs.
    pub instance_slots: u32,
}

fn validate_instance_source_address(instances_addr: u64, source_address: Option<u64>) -> Result<(), &'static str> {
    if instances_addr == 0 {
        return Ok(());
    }
    let Some(source_address) = source_address else {
        return Err("RT wired instance source buffer is missing");
    };
    if instances_addr != source_address {
        return Err("RT instance address does not match its current source buffer");
    }
    Ok(())
}

/// RT instance mask bits (`MTLAccelerationStructureInstanceDescriptor::mask`,
/// matched by `intersection_query::reset`'s mask argument). Every instance
/// carries `RT_MASK_VISIBLE`; `RT_MASK_SHADOW_CASTER` is additionally set
/// only when the object's `cast_shadows` is on. Manual-sync discipline: kept
/// in lockstep with the MSL `constant uint` pair of the same name in
/// `SHADOW_RAYS_MSL` below.
pub const RT_MASK_VISIBLE: u32 = 0x01;
pub const RT_MASK_SHADOW_CASTER: u32 = 0x02;

/// RT-T2-A / RT-TL-B (I-TL6): the BLAS hardware-opacity decision, one place.
/// Opaque (hardware early-out) only when the object is neither alpha-masked
/// nor translucent — both flags mean the kernel's candidate walks must see
/// this object's triangles to reject/attenuate them manually.
fn blas_geometry_opaque(alpha_mask: bool) -> bool {
    !alpha_mask
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
    // Q2 probe: log BLAS build sizes
    if std::env::var("MANIFOLD_PROBE_RT_ACCEL").is_ok() {
        eprintln!("MANIFOLD_PROBE_RT_ACCEL: encode_blas_build triangle_count={}, vertex_buffer_size={}, vertex_stride={}, vertex_offset={}",
            obj.triangle_count, obj.vertex_buffer.size(), obj.vertex_stride, obj.vertex_offset);
    }
    if super::gpu_fault::diagnostics_enabled() {
        let flat_bytes = u64::from(obj.triangle_count).checked_mul(3)
            .and_then(|v| v.checked_mul(u64::from(obj.vertex_stride)))
            .and_then(|v| v.checked_add(u64::from(obj.vertex_offset)));
        let bounds = if obj.index_buffer.is_none() {
            flat_bytes.map(|needed| needed <= obj.vertex_buffer.size())
        } else { None };
        log::info!("[RT-DIAG] BLAS triangles={} vertex_bytes={} stride={} offset={} indexed={} flat_vertex_bounds={bounds:?} instance_slots={}",
            obj.triangle_count, obj.vertex_buffer.size(), obj.vertex_stride, obj.vertex_offset,
            obj.index_buffer.is_some(), effective_instance_slots(obj));
        if bounds == Some(false) { log::error!("[RT-DIAG] invalid flat geometry bounds before AS build"); }
    }
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
    // RT-TL-B (section 16 TL6): translucent objects leave the fast path too —
    // `walk_with_transmission` needs them delivered as candidates.
    tri_desc.setOpaque(blas_geometry_opaque(obj.alpha_mask));
    let geom: Retained<MTLAccelerationStructureGeometryDescriptor> = tri_desc.into_super();
    let array = NSArray::from_retained_slice(&[geom]);
    let descriptor = MTLPrimitiveAccelerationStructureDescriptor::descriptor();
    descriptor.setGeometryDescriptors(Some(&array));
    descriptor.setUsage(MTLAccelerationStructureUsage::Refit);

    let raw_device = device.raw_device();
    let sizes = raw_device.accelerationStructureSizesWithDescriptor(&descriptor);
    if crate::metal::device::alloc_log_enabled() {
        eprintln!(
            "[gpu-alloc] blas tris={} struct={} scratch={}",
            obj.triangle_count, sizes.accelerationStructureSize, sizes.buildScratchBufferSize
        );
        crate::metal::device::alloc_log_backtrace();
    }
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

/// RT_INSTANCING_DESIGN.md D1: encode the descriptor-build compute dispatch
/// onto an already-open command buffer, ahead of the TLAS build/refit on
/// the SAME buffer (sequential encoders execute in creation order — the
/// GPU ordering the design requires; INV-RTI6: values move GPU→GPU only,
/// no CPU readback). Shared by `build_accel` (before the TLAS build) and
/// `refit_accel` (before the TLAS refit, one command buffer for both).
///
/// BUG-jddy discipline: the kernel reads each wired `instances` buffer
/// through a RAW GPU address (`instances_addr`) no binding declares, so
/// the dispatch `useResource`-declares every wired source buffer on the
/// compute encoder (same class as the trace kernels' vertex buffers).
fn encode_descriptor_build(
    device: &GpuDevice,
    cb: &ProtocolObject<dyn MTLCommandBuffer>,
    objects: &[RtObjectGeometry],
    descriptor_buffer: &GpuBuffer,
    obj_params: &GpuBuffer,
    max_slots: u32,
) {
    // INV-RTI4: exactly one line per descriptor-build dispatch, so a
    // stray per-frame rebuild is visible in MANIFOLD_PROBE_RT_ACCEL logs.
    if std::env::var("MANIFOLD_PROBE_RT_ACCEL").is_ok() {
        let total_slots: u32 = objects.iter().map(|o| effective_instance_slots(o)).sum();
        eprintln!(
            "MANIFOLD_PROBE_RT_ACCEL: descriptor-build dispatch objects={} total_slots={} max_slots={}",
            objects.len(), total_slots, max_slots
        );
    }
    let enc: Retained<ProtocolObject<dyn MTLComputeCommandEncoder>> = cb
        .computeCommandEncoder()
        .expect("computeCommandEncoder failed");
    unsafe { enc.setLabel(Some(&NSString::from_str("RT descriptor build compute"))) };
    let pipeline = &device.rt_pipelines().descriptor_build_pipeline;
    unsafe {
        enc.setComputePipelineState(&pipeline.state);
        enc.setBuffer_offset_atIndex(Some(descriptor_buffer.raw()), 0, 0);
        enc.setBuffer_offset_atIndex(Some(obj_params.raw()), 0, 1);
        for o in objects {
            if let Some(src) = o.instances_buffer {
                let () = msg_send![&*enc, useResource: &*src.raw, usage: MTLResourceUsage::Read];
            }
        }
        enc.dispatchThreadgroups_threadsPerThreadgroup(
            MTLSize {
                width: (max_slots as usize).div_ceil(SHADOW_WORKGROUP[0] as usize),
                height: objects.len().max(1),
                depth: 1,
            },
            MTLSize {
                width: SHADOW_WORKGROUP[0] as usize,
                height: 1,
                depth: 1,
            },
        );
    }
    enc.endEncoding();
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
///
/// The accel this build REPLACES needs no handoff: `RtAccel`'s `Drop`
/// self-retires through `retire_on_queue` (the root fix — see the Drop
/// impl), so a plain swap/drop/teardown is always safe regardless of
/// caller.
pub(crate) fn build_accel(device: &GpuDevice, objects: &[RtObjectGeometry], gi_materials: &[GiMaterial]) -> RtAccel {
    // RT_INSTANCING_DESIGN.md D7 + P1.5: the GPU descriptor-build path
    // serves ALL objects uniformly when ANY object is wired (instances_addr
    // != 0) — a wired 1-capacity buffer's TRS is GPU-side and the CPU path
    // can only place an identity descriptor at model. Otherwise today's
    // CPU per-object path, byte-identical.
    let instanced = objects.iter().any(|o| o.instances_addr != 0);
    let topology: Vec<_> = objects.iter().map(RtGeometryTopology::from_geometry).collect();
    let slot_total_raw: usize = topology.iter().map(|o| o.instance_slots as usize).sum();
    let total_slots = slot_total_raw.max(1);
    if super::gpu_fault::diagnostics_enabled() {
        log::info!("[RT-DIAG] AS build objects={} instances={total_slots} instanced={instanced}", objects.len());
    }
    let max_slots: u32 = objects.iter().map(effective_instance_slots).max().unwrap_or(1);

    let cb = device.new_command_buffer("RT accel build");

    // D1: in instanced mode the descriptor-build kernel runs FIRST on this
    // same command buffer (sequential encoders are GPU-ordered ahead of
    // the TLAS build below). `instance_buffer` is the buffer the TLAS
    // descriptor references in BOTH modes — GPU-private here (the CPU
    // never authors descriptors in instanced mode), CPU-mapped shared on
    // the fast path.
    let mut instance_obj_params: Option<GpuBuffer> = None;
    let instance_buffer = if instanced {
        let obj_params = device.create_buffer_shared(
            (objects.len() * std::mem::size_of::<RtInstanceBuildObj>()) as u64,
        );
        write_instance_obj_params(
            obj_params
                .mapped_ptr()
                .expect("RT instance build-params buffer must be CPU-mapped"),
            objects,
        );
        let descriptor_buffer =
            device.create_buffer((total_slots * std::mem::size_of::<MTLAccelerationStructureInstanceDescriptor>()) as u64);
        encode_descriptor_build(device, &cb, objects, &descriptor_buffer, &obj_params, max_slots);
        instance_obj_params = Some(obj_params);
        descriptor_buffer
    } else {
        build_instance_buffer(device, objects)
    };

    let enc = cb
        .accelerationStructureCommandEncoder()
        .expect("accelerationStructureCommandEncoder failed");
    unsafe { enc.setLabel(Some(&NSString::from_str("RT BLAS build"))) };

    // BUG-84fv hardening: pin the geometry buffers up front. The BLAS
    // builds below read them through descriptor raw addresses, so they
    // need a usage declaration on this encoder (BUG-jddy reclamation
    // class) AND a CPU-side keep-alive until this command buffer
    // completes — a teardown dropping the RtAccel mid-build must not
    // unpin what the GPU is still reading.
    let mut geometry_buffers = Vec::with_capacity(objects.len() * 2);
    for o in objects {
        geometry_buffers.push(o.vertex_buffer.raw.clone());
        if let Some(ib) = o.index_buffer {
            geometry_buffers.push(ib.raw.clone());
        }
    }
    unsafe {
        for geo in &geometry_buffers {
            let () = msg_send![&*enc, useResource: &**geo, usage: MTLResourceUsage::Read];
        }
    }

    let mut blas = Vec::with_capacity(objects.len());
    let mut blas_scratch = Vec::with_capacity(objects.len());
    for o in objects {
        let (b, scratch) = encode_blas_build(device, &enc, o);
        blas.push(b);
        blas_scratch.push(scratch);
    }
    let blas_structures: Vec<Retained<ProtocolObject<dyn MTLAccelerationStructure>>> =
        blas.iter().map(|b| b.structure.clone()).collect();
    // The TLAS build reads the instance buffer through the descriptor —
    // same raw-address class as the geometry buffers above.
    unsafe {
        let () = msg_send![&*enc, useResource: instance_buffer.raw(), usage: MTLResourceUsage::Read];
    }

    let tlas_instance_count = if instanced { total_slots } else { objects.len() };
    let descriptor = MTLInstanceAccelerationStructureDescriptor::descriptor();
    descriptor.setInstanceCount(tlas_instance_count);
    unsafe {
        descriptor.setInstanceDescriptorBuffer(Some(instance_buffer.raw()));
    }
    descriptor.setInstancedAccelerationStructures(Some(&NSArray::from_retained_slice(&blas_structures)));
    descriptor.setUsage(MTLAccelerationStructureUsage::Refit);

    let raw_device = device.raw_device();
    let sizes = raw_device.accelerationStructureSizesWithDescriptor(&descriptor);
    if crate::metal::device::alloc_log_enabled() {
        eprintln!(
            "[gpu-alloc] tlas instances={} struct={} build_scratch={} refit_scratch={}",
            tlas_instance_count, sizes.accelerationStructureSize,
            sizes.buildScratchBufferSize, sizes.refitScratchBufferSize
        );
        crate::metal::device::alloc_log_backtrace();
    }
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
    // BUG-84fv: pin the wired `instances` source buffers (read through raw
    // addresses by the descriptor-build kernel) until the build completes
    // — the same lifetime hazard as the geometry buffers above. Empty on
    // the D7 fast path.
    let instance_source_pins: Vec<Retained<ProtocolObject<dyn MTLBuffer>>> = objects
        .iter()
        .filter_map(|o| o.instances_buffer.map(|b| b.raw.clone()))
        .collect();
    add_ready_completion_handler(
        &cb,
        "RT accel build",
        Arc::clone(&ready),
        CompletionPins((
            blas_scratch,
            build_scratch,
            geometry_buffers.clone(),
            blas_structures.clone(),
            instance_buffer.raw.clone(),
            instance_source_pins,
        )),
    );
    cb.commit();

    // RS-B: build the emissive light table from the same objects + material
    // arrays. None when no object has non-black emissive (zero triangles).
    let emissive_table = build_emissive_table(device, objects, gi_materials);

    RtAccel {
        structure,
        descriptor,
        refit_scratch,
        blas,
        instance_buffer,
        instanced,
        instance_slot_total: slot_total_raw as u32,
        topology,
        instance_obj_params,
        geometry_buffers,
        ready,
        queue: device.clone_queue(),
        emissive_table,
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
/// Resource pins handed to an async command buffer's completion handler:
/// held until the GPU finishes, then dropped on a Metal-owned callback
/// thread. Metal retain/release is thread-safe and nothing else touches
/// the pinned objects from that thread, so the Send gap is nominal.
struct CompletionPins<T>(T);
unsafe impl<T> Send for CompletionPins<T> {}

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
                Some(err) => {
                    super::gpu_fault::log_error_diagnostics(&err, label);
                    (err.code() as i64, err.localizedDescription().to_string())
                },
            };
            super::gpu_fault::record_fault(&desc);
            log::error!("[GPU] Command buffer '{label}' error (code={code}): {desc}");
            // BUG-84fv: a failed build/refit must NOT flip ready — tracing
            // a structure the GPU faulted while writing sends hardware
            // traversal into garbage and can hang the device. Leaving
            // ready false keeps the caller on the raster fallback; the
            // error above is the loud signal.
            return;
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
pub(crate) fn refit_accel(device: &GpuDevice, accel: &RtAccel, objects: &[RtObjectGeometry]) -> Result<(), RtTopologyMismatch> {
    accel.check_topology(objects)?;
    debug_assert_eq!(
        objects.len(),
        accel.blas.len(),
        "refit_accel called with a different object COUNT than build_accel built — the BLAS \
         list (and instance buffer) don't match; call build_accel again instead (topology change)"
    );
    // RT_INSTANCING_DESIGN.md P0.5: the count assert above can't see a
    // per-object CAPACITY shrink (Σ slots changes, count doesn't) — a
    // stale tail of descriptors would survive the refit. Capacity is
    // rigid topology; a real change must rebuild.
    let refit_slot_total: u32 = objects.iter().map(effective_instance_slots).sum();
    debug_assert_eq!(
        refit_slot_total,
        accel.instance_slot_total,
        "refit_accel called with a different Σ instance slots than build_accel built — \
         capacity change is a topology change; call build_accel again"
    );

    // RT_INSTANCING_DESIGN.md D9: instanced refit = descriptor-build
    // dispatch + TLAS refit in ONE command buffer (GPU-ordered). The
    // descriptor buffer is GPU-private, so the CPU-mapped-tear discipline
    // of the fast path (below) does not apply — the params rewrite is the
    // per-object build-params buffer, not the descriptors themselves.
    // Transforms, masks, and the wired `instances_addr` are read fresh
    // from the CURRENT objects each refit (buffer identity rides no key;
    // the kernel reads whatever buffer is wired this frame).
    if accel.instanced {
        let obj_params = accel
            .instance_obj_params
            .as_ref()
            .expect("instanced accel carries instance build params");
        write_instance_obj_params(
            obj_params
                .mapped_ptr()
                .expect("RT instance build-params buffer must be CPU-mapped"),
            objects,
        );
        let max_slots: u32 = objects.iter().map(effective_instance_slots).max().unwrap_or(1);

        accel.ready.store(false, Ordering::Release);
        let cb = device.new_command_buffer("RT TLAS instanced refit");
        encode_descriptor_build(device, &cb, objects, &accel.instance_buffer, obj_params, max_slots);
        let enc = cb
            .accelerationStructureCommandEncoder()
            .expect("accelerationStructureCommandEncoder failed");
        unsafe { enc.setLabel(Some(&NSString::from_str("RT TLAS instanced refit"))) };
        // BUG-84fv hardening: same declaration set as the fast path below,
        // plus the wired instances sources for the descriptor kernel's
        // raw-address reads (declared on the compute encoder inside
        // `encode_descriptor_build`).
        unsafe {
            let () = msg_send![&*enc, useResource: &*accel.structure, usage: MTLResourceUsage::Read | MTLResourceUsage::Write];
            for b in &accel.blas {
                let () = msg_send![&*enc, useResource: &*b.structure, usage: MTLResourceUsage::Read];
            }
            let () = msg_send![&*enc, useResource: accel.instance_buffer.raw(), usage: MTLResourceUsage::Read];
        }
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
        let blas_keep: Vec<Retained<ProtocolObject<dyn MTLAccelerationStructure>>> =
            accel.blas.iter().map(|b| b.structure.clone()).collect();
        let instance_source_pins: Vec<Retained<ProtocolObject<dyn MTLBuffer>>> = objects
            .iter()
            .filter_map(|o| o.instances_buffer.map(|b| b.raw.clone()))
            .collect();
        add_ready_completion_handler(
            &cb,
            "RT TLAS instanced refit",
            Arc::clone(&accel.ready),
            CompletionPins((
                blas_keep,
                accel.instance_buffer.raw.clone(),
                accel.structure.clone(),
                accel.refit_scratch.raw.clone(),
                obj_params.raw.clone(),
                instance_source_pins,
            )),
        );
        cb.commit();
        return Ok(());
    }

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
    let cb = device.new_command_buffer("RT TLAS refit");
    let enc = cb
        .accelerationStructureCommandEncoder()
        .expect("accelerationStructureCommandEncoder failed");
    unsafe { enc.setLabel(Some(&NSString::from_str("RT TLAS refit"))) };
    // BUG-84fv hardening: the refit reads the BLAS list + instance buffer
    // through the descriptor (raw addresses) — declare usage on this
    // encoder (BUG-jddy reclamation class) and pin them through completion
    // so a teardown dropping the RtAccel mid-refit can't unpin them.
    unsafe {
        let () = msg_send![&*enc, useResource: &*accel.structure, usage: MTLResourceUsage::Read | MTLResourceUsage::Write];
        for b in &accel.blas {
            let () = msg_send![&*enc, useResource: &*b.structure, usage: MTLResourceUsage::Read];
        }
        let () = msg_send![&*enc, useResource: accel.instance_buffer.raw(), usage: MTLResourceUsage::Read];
    }
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
    let blas_keep: Vec<Retained<ProtocolObject<dyn MTLAccelerationStructure>>> =
        accel.blas.iter().map(|b| b.structure.clone()).collect();
    add_ready_completion_handler(
        &cb,
        "RT TLAS refit",
        Arc::clone(&accel.ready),
        CompletionPins((
            blas_keep,
            accel.instance_buffer.raw.clone(),
            accel.structure.clone(),
            // The refit scratch is GPU-written for the refit's whole async
            // duration; the RtAccel (its owner) can be replaced or torn
            // down while this buffer is in flight.
            accel.refit_scratch.raw.clone(),
        )),
    );
    cb.commit();
    Ok(())
}

// ─── Raw MSL kernels (shadow-only slice of rt_trace.metal) ────────────

/// Shadow-only trim of the prototype's `TraceParams`/`trace_lighting` +
/// `upsample_lighting` kernels. AO (`ao_spp`) and one-bounce GI
/// (`gi_spp`, `Material`/`mat_index` buffers) are P2/P3 scope — dropped,
/// not ported. `packed_float3` is mandatory (P0 section 5.1 kernel lesson):
/// bare MSL `float3` is sizeof 16 and desyncs from `#[repr(C)] [f32; 3]`.
const SHADOW_RAYS_MSL: &str = include_str!("shadow_rays.msl");

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

#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct RtTraceDiagnostics {
    enabled: u32,
    state: u32,
    invalid_count: u32,
    first_stage: u32,
    first_pixel: u32,
    _pad: u32,
    raw_origin: [f32; 3],
    raw_direction: [f32; 3],
    raw_min_distance: f32,
    raw_max_distance: f32,
}



/// CPU mirror of `ShadowRayParams` above — field order and packing MUST
/// match exactly (P0 section 5.1 kernel lesson: `packed_float3` in MSL == dense
/// `[f32; 3]` here, no padding).
///
/// RAYTRACING_DESIGN.md section 5.2 P2 extended this in place (same struct, same
/// binding(1) slot, same single half-res dispatch — D11/D16's "P2 joins
/// the SAME half-res dispatch and SAME upsample" seam, not a parallel
/// pass): `ao_radius`/`ao_spp` drive the added AO-ray gather. ED2 (section
/// 14.2) DELETED `ambient_color`: the flat ambient term no longer enters
/// the kernel — the gather's output is `rgb = env+GI, a = ao`, and the flat
/// ambient is recomposed consumer-side in `render_scene.wgsl`'s
/// `rt_or_flat_ambient` (no albedo folded in here — that happens once,
/// downstream, per D3's "accumulate lighting separated from albedo").
///
/// Per-caster shadow support (multi-caster fix): `sun_dir`/`sun_cone`/
/// `sun_color` (single-caster-only) replaced with `casters`/`caster_count`
/// — up to [`MAX_RT_CASTERS`] independently-traced casters, one visibility
/// channel per slot in `trace_shadow_rays`'s `out_sv` output.
/// RT quality A3a: Split-dispatch control.
/// The single `trace_shadow_rays` kernel now runs twice with different spp masks:
/// - Mask dispatch: shadow_spp > 0, ao_spp=0, gi_spp=0, refl_spp=0 → writes out_sv only
/// - Lighting dispatch: shadow_spp=0, ao_spp > 0 and/or gi_spp > 0 and/or refl_spp > 0 → writes out_irr, out_refl, out_n
///
/// Each dispatch carries its own trace_size; spp=0 gates kernel writes to leave textures untouched.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
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
    /// `refl_rough_band` = the blend-band width. `_pad_refl` is 8 bytes:
    /// MSL lays out `uint _pad_refl[2]` (8 bytes) plus the 4 bytes of
    /// `emissive_table_mean_power` + 4 bytes of `emissive_table_count` = 16
    /// bytes total, the alignment `float4x4 inv_view_proj` demands.
    pub refl_spp: u32,
    pub refl_max_roughness: f32,
    pub refl_rough_band: f32,
    /// RS-B (RAYTRACING_DESIGN.md section 15.2 RS8): per-sample firefly cap
    /// anchor — the emissive table's CPU-computed mean power. Must precede
    /// `_pad_refl` to match MSL field order (emissive fields BEFORE padding).
    pub emissive_table_mean_power: f32,
    /// RS-C: number of valid entries in the emissive table/alias buffers.
    /// 0 = no emissive geometry — the sampler kernel block is skipped.
    pub emissive_table_count: u32,
    /// RS-C: total world-space area (in world units²) of all emissive
    /// triangles in the table. The RIS estimator's geometry weight uses
    /// this as the PDF correction factor for uniform area sampling.
    pub emissive_table_total_area: f32,
    /// RT-TL-C (RAYTRACING_DESIGN.md section 16 TL5): index of the designated
    /// sun caster whose rgb transmission tint fills `out_svt`;
    /// SVT_SLOT_NONE = no designated sun.
    pub svt_slot: u32,
    /// Column-major, matches `render_scene.rs`'s `mat4_inverse` output.
    pub inv_view_proj: [[f32; 4]; 4],
    /// RT_INSTANCING_DESIGN.md D11: row-base of the per-slot table rows in
    /// the shared `normal_sources`/`gi_materials` buffers — canonical
    /// per-object rows at `[0, N)`, per-slot rows at `[N, N+Σ)`. The kernel
    /// adds this to every `instance_id`-indexed read. `new()` defaults 0
    /// (canonical rows — valid for unwired scenes, where slot rows are
    /// identical copies); production sets it to the object count via
    /// [`ShadowRayParams::with_slot_row_base`].
    pub slot_row_base: u32,
    /// RT_INSTANCING_DESIGN.md D8: non-zero when the emissive table
    /// entries are LOCAL-space (instanced mode — the kernel composes each
    /// entry's world triangle from the TLAS descriptor buffer and weights
    /// by per-entry true world area). Zero = the D7 fast path (entries
    /// world-space, the pre-instancing data path). `new()` defaults 0;
    /// production sets it from `EmissiveLightTable::entries_are_local`.
    pub emissive_entries_are_local: u32,
    /// Alignment pad to the 16-byte multiple the MSL mirror rounds to
    /// (float4x4 member): 400 + 4 + 4 + 8 = 416.
    pub _pad_slot: [u32; 2],
}

/// Fixed per-dispatch shadow-caster slot count — mirrors the embedded MSL
/// `MAX_RT_CASTERS` at `raytrace.rs` (metal) `:565` (both are 8; no
/// compiler-enforced link between the two, same manual-sync discipline this
/// file already uses for other cross-constant constants). RS-A (caster cap
/// 4 -> 8): doubled from 4; the MSL mirror must stay in sync.
pub const MAX_RT_CASTERS: usize = 8;

/// RT-TL-C (section 16 TL5): sentinel for no designated sun caster —
/// `out_svt` reads white (1,1,1) everywhere and fs_pbr keeps the luma channel.
pub const SVT_SLOT_NONE: u32 = u32::MAX;

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
        camera_pos: [f32; 3],
        inv_view_proj: [[f32; 4]; 4],
        refl_spp: u32,
        refl_max_roughness: f32,
        refl_rough_band: f32,
        emissive_table_mean_power: f32,
        emissive_table_count: u32,
        emissive_table_total_area: f32,
        svt_slot: u32,
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
            camera_pos,
            refl_spp,
            refl_max_roughness,
            refl_rough_band,
            emissive_table_mean_power,
            emissive_table_count,
            emissive_table_total_area,
            svt_slot,
            inv_view_proj,
            slot_row_base: 0,
            emissive_entries_are_local: 0,
            _pad_slot: [0; 2],
        }
    }

    /// RT_INSTANCING_DESIGN.md D11: production passes the object count N so
    /// `instance_id`-indexed table reads land in the per-slot rows
    /// `[N, N+Σ)`. Rides the params (not a new binding) — the SAME
    /// discipline as `AccumulateParams`' flag setters.
    pub fn with_slot_row_base(mut self, base: u32) -> Self {
        self.slot_row_base = base;
        self
    }

    /// RT_INSTANCING_DESIGN.md D8: set when the emissive table entries are
    /// local-space (instanced mode) — the kernel composes world positions
    /// from the TLAS descriptor buffer instead of reading them from the
    /// entries.
    pub fn with_emissive_entries_local(mut self, local: bool) -> Self {
        self.emissive_entries_are_local = local as u32;
        self
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
    /// RT-TL-B (RAYTRACING_DESIGN.md section 16 TL4/TL7): x = thin-surface
    /// diffuse-transmission factor, populated from the SAME
    /// `diffuse_transmission_params` uniform the raster forward term reads.
    /// 0 = opaque to shadow-class rays (pre-feature behavior). yzw reserved.
    pub translucency: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<GiMaterial>() == 64);

impl GiMaterial {
    pub fn new(
        albedo: [f32; 3],
        emissive: [f32; 3],
        metallic_roughness: [f32; 4],
        translucency: [f32; 4],
    ) -> Self {
        Self {
            albedo,
            _pad0: 0.0,
            emissive,
            _pad1: 0.0,
            metallic_roughness,
            translucency,
        }
    }
}

// RS-B (RAYTRACING_DESIGN.md section 15.3): per-triangle emissive light table
// cap — power-rank truncated. Mirrors the embedded MSL `MAX_RT_EMISSIVE_TRIANGLES`
// at the MSL source above (same manual-sync discipline as `MAX_RT_CASTERS`).
pub const MAX_RT_EMISSIVE_TRIANGLES: u32 = 4096;

/// RS-B: GPU-side emissive triangle entry — world-space positions of the
/// three vertices plus per-vertex UVs and the owning object index for
/// gi_materials/normal_sources lookups (RS-C). `packed_float3` discipline:
/// `[f32; 3]` + explicit pad (P0 section 5.1 kernel lesson).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EmissiveTriangleGpu {
    pub v0: [f32; 3],
    _pad0: f32,
    pub v1: [f32; 3],
    _pad1: f32,
    pub v2: [f32; 3],
    _pad2: f32,
    pub uv0: [f32; 2],
    pub uv1: [f32; 2],
    pub uv2: [f32; 2],
    pub object_index: u32,
    /// RT_INSTANCING_DESIGN.md D8: the TLAS descriptor slot of THIS entry's
    /// copy. In instanced mode the entries stay LOCAL-space and the kernel
    /// composes world positions from `emissive_descriptors[descriptor_index]`;
    /// the D7 fast path sets this to the object index (descriptor_index ==
    /// object_index there) and never reads it. Replaces the old `_pad_obj`
    /// word — size stays 80 bytes.
    pub descriptor_index: u32,
}

const _: () = assert!(std::mem::size_of::<EmissiveTriangleGpu>() == 80);

/// RS-B: GPU-side alias-table entry — `prob` is the probability of selecting
/// the entry's own triangle; when the draw fails the self-probability, `alias`
/// names the alternative entry index.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EmissiveAliasEntry {
    pub prob: f32,
    pub alias: u32,
}

const _: () = assert!(std::mem::size_of::<EmissiveAliasEntry>() == 8);

/// RS-B: CPU-side per-triangle storage for refit — local-space vertex
/// positions survive across transforms so `refit_emissive_table` can
/// recompute world-space positions from the new object transform.
/// consumed by `build_emissive_table`/`refit_emissive_table` via
/// `EmissiveLightTable.local_triangles`.
#[allow(dead_code, reason = "stored in EmissiveLightTable.local_triangles, consumed by build_/refit_emissive_table")]
#[derive(Clone, Debug)]
struct EmissiveTriangleCpu {
    v0_local: [f32; 3],
    v1_local: [f32; 3],
    v2_local: [f32; 3],
    uv0: [f32; 2],
    uv1: [f32; 2],
    uv2: [f32; 2],
    /// Index into the `objects` slice this triangle came from (for refit).
    object_index: u32,
}

/// RS-B (section 15.3): the resident emissive-geometry light table — GPU
/// buffers for the kernel's sampling step (RS-C) and CPU-side local-space
/// data for refit alongside the TLAS. Built once at accel-registration time
/// (D17 async discipline applies — table lands with the accel-ready flag);
/// refit re-transforms positions when object transforms change.
pub struct EmissiveLightTable {
    /// GPU buffer of [`EmissiveTriangleGpu`] entries (world-space positions
    /// in the D7 fast path, LOCAL-space in instanced mode — see
    /// [`Self::entries_are_local`]).
    pub triangles: GpuBuffer,
    /// GPU buffer of [`EmissiveAliasEntry`] entries.
    pub aliases: GpuBuffer,
    /// Valid entry count (0..=[`MAX_RT_EMISSIVE_TRIANGLES`]).
    pub entry_count: u32,
    /// Arithmetic mean of per-entry power (area × build-time emissive luma)
    /// — the firefly cap anchor in [`ShadowRayParams::emissive_table_mean_power`].
    pub mean_power: f32,
    /// RS-C: total area of all table entries. D8 derivation: in instanced
    /// mode the RIS weight uses each entry's TRUE world area computed
    /// in-kernel (exact under per-slot scales), so this aggregate is NOT
    /// consumed by the weight — it carries the LOCAL-space aggregate
    /// (Σ entry local areas, per-slot duplication included) for ABI
    /// stability and diagnostics. Fast path: Σ world areas (the weight
    /// normalizer, unchanged).
    pub total_area: f32,
    /// RT_INSTANCING_DESIGN.md D8: true when entries are LOCAL-space and
    /// the kernel composes world positions from the TLAS descriptor
    /// buffer (instanced mode). False = the D7 fast path (world entries).
    pub entries_are_local: bool,
    /// CPU-side local-space vertices for refit (one per entry, same order).
    /// Fast path only: `refit_emissive_table` re-transforms them into
    /// world space. Instanced mode: never consumed (local entries never
    /// change under transform animation — the kernel reads the composed
    /// world matrix from the descriptor buffer, which `refit_accel`
    /// refreshes), kept so the fast path shape stays one code path.
    local_triangles: Vec<EmissiveTriangleCpu>,
}

/// RS-B: build the emissive-triangle light table from `objects` (SAME slice
/// the accel was built from) and the parallel `gi_materials` array. Returns
/// `None` when no object has non-black emissive (zero triangles).
///
/// Algorithm: for each object whose `gi_materials[i].emissive` luma > 0,
/// iterate its indexed/non-indexed triangles via the CPU-mapped vertex
/// buffer; compute local-space area and power (area × emissive luma).
/// RT_INSTANCING_DESIGN.md D8: the candidate set spans (object, SLOT)
/// pairs — each emissive triangle is duplicated per effective instance
/// slot (same local power; the alias is a LOCAL-power proposal), and every
/// entry carries its slot's TLAS descriptor index. The entry cap spans the
/// expanded set: power-rank truncated to [`MAX_RT_EMISSIVE_TRIANGLES`].
///
/// D8 upload: instanced mode (any object wired — the SAME rule as the
/// accel's mode trigger) keeps entries LOCAL-space; the kernel composes
/// world positions from the TLAS descriptor buffer, so transform animation
/// reaches the light table through the accel's own refit, with no CPU
/// rewrite. The D7 fast path uploads world-space entries (today's bytes).
///
/// D8 RIS-weighting derivation (the kernel side is the arbiter proof's
/// oracle): the alias proposal is p(entry) ∝ local_power, duplicated per
/// slot, so the proposal mass of an object's triangles scales with its
/// slot count — a copy is proposed as often as it emits. The weight must
/// then carry the sampled entry's TRUE world area: for a uniform scene
/// (one luma, rigid slots) E[contrib] = Σ_slots Σ_tris (A_local·lum/W)·lum·
/// cosθ·cos_emit·A_world(t,s)/dist², which equals the true direct light
/// only with the per-entry world area in the weight — a global scalar
/// normalizer would bake the slot scale into the bias. Hence the kernel
/// computes ½|(w1−w0)×(w2−w0)| from the composed world edges per sample.
/// `total_area` accordingly keeps its fast-path meaning (Σ world areas,
/// the global weight normalizer) and becomes the LOCAL aggregate in
/// instanced mode, where the weight no longer consumes it. `mean_power`
/// stays the mean over the FINAL expanded candidate set — the firefly
/// anchor.
///
/// The caller owns the returned table (pass to `refit_emissive_table` on
/// transform change; keep alive as long as the accel lives).
pub fn build_emissive_table(
    device: &GpuDevice,
    objects: &[RtObjectGeometry],
    gi_materials: &[GiMaterial],
) -> Option<EmissiveLightTable> {
    if gi_materials.is_empty() { return None; }
    assert_eq!(objects.len(), gi_materials.len(), "RT emissive table requires one material row per RT object");
    // D8: same mode rule as `build_accel` (any WIRED object, P1.5: a wired
    // 1-capacity buffer is GPU-path too) — the table's slot set must equal
    // the accel's slot set exactly, or entries would name descriptors that
    // don't exist.
    let instanced = objects.iter().any(|o| o.instances_addr != 0);

    // Phase 1: collect per-(triangle, slot) candidates (local vertices,
    // object index, descriptor slot index, local power).
    struct Candidate {
        v0: [f32; 3],
        v1: [f32; 3],
        v2: [f32; 3],
        uv0: [f32; 2],
        uv1: [f32; 2],
        uv2: [f32; 2],
        obj_index: u32,
        /// D8: the TLAS descriptor slot of this entry's copy (the kernel's
        /// world-composition index). Fast path: equals the object index.
        desc_index: u32,
        power: f32,
    }
    let mut candidates: Vec<Candidate> = Vec::new();

    // Slot bases under the SAME object-major addressing the descriptor
    // kernel and `write_instance_obj_params` use — one running sum.
    let mut slot_base = 0u32;

    for (oi, obj) in objects.iter().enumerate() {
        let obj_slots = effective_instance_slots(obj);
        let object_slot_base = slot_base;
        slot_base = slot_base.checked_add(obj_slots).expect("RT emissive slot base overflow");
        let emissive_luma = luma(gi_materials[oi].emissive);
        if emissive_luma <= 0.0 {
            continue;
        }
        let Some(ptr) = obj.vertex_buffer.mapped_ptr() else {
            log::warn!(
                "RT emissive table: object {} vertex buffer is not CPU-mapped — \
                 its emissive triangles are absent from the light table",
                oi
            );
            continue;
        };
        let stride = obj.vertex_stride as usize;
        let offset = obj.vertex_offset as usize;
        let tri_count = obj.triangle_count as usize;

        // Read position from vertex i (offset 0 = position for MeshVertex convention).
        let pos_at = |vi: usize| -> [f32; 3] {
            let byte_offset = offset + vi * stride;
            unsafe {
                let p: *const [f32; 3] = ptr.add(byte_offset) as *const [f32; 3];
                *p
            }
        };

        // Compute per-triangle local-space area and power.
        let index_at = |vi: usize| -> u32 {
            if let Some(ib) = obj.index_buffer {
                if let Some(ib_ptr) = ib.mapped_ptr() {
                    unsafe { *(ib_ptr.add(vi * 4) as *const u32) }
                } else {
                    vi as u32
                }
            } else {
                vi as u32
            }
        };

        for ti in 0..tri_count {
            let i0 = index_at(ti * 3) as usize;
            let i1 = index_at(ti * 3 + 1) as usize;
            let i2 = index_at(ti * 3 + 2) as usize;
            let v0 = pos_at(i0);
            let v1 = pos_at(i1);
            let v2 = pos_at(i2);

            let area = triangle_area(v0, v1, v2);
            if area <= 0.0 {
                continue;
            }
            let power = area * emissive_luma;
            // RS-C: also capture per-vertex UVs for emissive-map sampling.
            let uv_offset = obj.uv_offset as usize;
            let uv_at = |vi: usize| -> [f32; 2] {
                let byte_offset = offset + vi * stride + uv_offset;
                unsafe { *(ptr.add(byte_offset) as *const [f32; 2]) }
            };
            let uv0 = uv_at(i0);
            let uv1 = uv_at(i1);
            let uv2 = uv_at(i2);
            // D8: one candidate per (triangle, slot) — the local power is
            // identical across an object's slots (rigid copies), so the
            // duplication lives in the proposal mass, not the power value.
            for s in 0..obj_slots {
                candidates.push(Candidate {
                    v0,
                    v1,
                    v2,
                    uv0,
                    uv1,
                    uv2,
                    obj_index: oi as u32,
                    desc_index: object_slot_base + s,
                    power,
                });
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // Phase 2: power-rank truncate to MAX_RT_EMISSIVE_TRIANGLES.
    if candidates.len() > MAX_RT_EMISSIVE_TRIANGLES as usize {
        candidates.select_nth_unstable_by(
            MAX_RT_EMISSIVE_TRIANGLES as usize,
            |a, b| b.power.partial_cmp(&a.power).unwrap_or(std::cmp::Ordering::Equal),
        );
        candidates.truncate(MAX_RT_EMISSIVE_TRIANGLES as usize);
    }

    let entry_count = candidates.len() as u32;
    let total_power: f32 = candidates.iter().map(|c| c.power).sum();
    let mean_power = total_power / entry_count as f32;
    // RS-C: total world-space area computed below during upload.
    let mut total_area: f32 = 0.0;

    // Phase 3: build alias table from the power distribution.
    let weights: Vec<f32> = candidates.iter().map(|c| c.power).collect();
    let (probs, aliases) = build_alias_table(&weights);

    // Phase 4: upload to GPU buffers.
    let triangles_bytes = (entry_count as usize * std::mem::size_of::<EmissiveTriangleGpu>()) as u64;
    let aliases_bytes = (entry_count as usize * std::mem::size_of::<EmissiveAliasEntry>()) as u64;
    let tri_buf = device.create_buffer_shared(triangles_bytes.max(1));
    let alias_buf = device.create_buffer_shared(aliases_bytes.max(1));

    {
        let tri_ptr = tri_buf
            .mapped_ptr()
            .expect("emissive triangle buffer must be shared");
        let alias_ptr = alias_buf
            .mapped_ptr()
            .expect("emissive alias buffer must be shared");
        let mut local_triangles: Vec<EmissiveTriangleCpu> =
            Vec::with_capacity(entry_count as usize);
        let model = |oi: u32| -> [[f32; 4]; 4] {
            objects.get(oi as usize).map(|o| o.transform).unwrap_or([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ])
        };

        for (i, c) in candidates.iter().enumerate() {
            // D8: instanced entries stay LOCAL-space (the kernel composes
            // world from the descriptor buffer); the fast path uploads
            // world-space positions exactly as before.
            let (e0, e1, e2) = if instanced {
                total_area += triangle_area(c.v0, c.v1, c.v2);
                (c.v0, c.v1, c.v2)
            } else {
                let m = model(c.obj_index);
                let w0 = transform_point(&m, c.v0);
                let w1 = transform_point(&m, c.v1);
                let w2 = transform_point(&m, c.v2);
                total_area += triangle_area(w0, w1, w2);
                (w0, w1, w2)
            };

            unsafe {
                let tri_dst = tri_ptr.add(i * std::mem::size_of::<EmissiveTriangleGpu>())
                    as *mut EmissiveTriangleGpu;
                std::ptr::write_unaligned(
                    tri_dst,
                    EmissiveTriangleGpu {
                        v0: e0,
                        _pad0: 0.0,
                        v1: e1,
                        _pad1: 0.0,
                        v2: e2,
                        _pad2: 0.0,
                        uv0: c.uv0,
                        uv1: c.uv1,
                        uv2: c.uv2,
                        object_index: c.obj_index,
                        descriptor_index: c.desc_index,
                    },
                );

                let alias_dst = alias_ptr.add(i * std::mem::size_of::<EmissiveAliasEntry>())
                    as *mut EmissiveAliasEntry;
                std::ptr::write_unaligned(
                    alias_dst,
                    EmissiveAliasEntry {
                        prob: probs[i],
                        alias: aliases[i],
                    },
                );
            }

            local_triangles.push(EmissiveTriangleCpu {
                v0_local: c.v0,
                v1_local: c.v1,
                v2_local: c.v2,
                uv0: c.uv0,
                uv1: c.uv1,
                uv2: c.uv2,
                object_index: c.obj_index,
            });
        }

        Some(EmissiveLightTable {
            triangles: tri_buf,
            aliases: alias_buf,
            entry_count,
            mean_power,
            total_area,
            entries_are_local: instanced,
            local_triangles,
        })
    }
}

/// RS-B: refit the emissive table's world-space positions from the stored
/// local-space vertices and the objects' current transforms. Same call-site
/// discipline as [`refit_accel`] — called when topology is stable and
/// transforms changed.
///
/// RT_INSTANCING_DESIGN.md D8: INSTANCED MODE IS A NO-OP — the entries stay
/// LOCAL-space forever, and the kernel composes their world positions from
/// the TLAS descriptor buffer, which `refit_accel` refreshes on the same
/// cadence. A transform animation therefore reaches the light table
/// through the accel's own refit with no CPU rewrite here. (This CPU
/// re-transform stays for the D7 fast path, whose entries are world-space.)
pub fn refit_emissive_table(table: &EmissiveLightTable, objects: &[RtObjectGeometry]) {
    if table.entries_are_local {
        return;
    }
    let Some(ptr) = table.triangles.mapped_ptr() else {
        return;
    };
    for (i, local) in table.local_triangles.iter().enumerate() {
        let m = objects
            .get(local.object_index as usize)
            .map(|o| o.transform)
            .unwrap_or([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]);
        let w0 = transform_point(&m, local.v0_local);
        let w1 = transform_point(&m, local.v1_local);
        let w2 = transform_point(&m, local.v2_local);
        unsafe {
            let dst = ptr.add(i * std::mem::size_of::<EmissiveTriangleGpu>())
                as *mut EmissiveTriangleGpu;
            (*dst).v0 = w0;
            (*dst).v1 = w1;
            (*dst).v2 = w2;
        }
    }
}

/// Luminance of a linear-HDR RGB triple (Rec.709 weights, same convention
/// the kernel's `luma()` MSL helper uses).
fn luma(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// Area of a triangle from its three vertices (half the cross-product
/// magnitude of two edge vectors).
fn triangle_area(v0: [f32; 3], v1: [f32; 3], v2: [f32; 3]) -> f32 {
    let e1 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
    let e2 = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
    let cross = [
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ];
    let mag2 = cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2];
    if mag2 <= 0.0 {
        return 0.0;
    }
    0.5 * mag2.sqrt()
}

/// Transform a point by a column-major 4×4 matrix (position only — w=1,
/// no projective divide). Same convention as `render_scene.wgsl`'s
/// `(M * vec4(p, 1.0)).xyz`.
fn transform_point(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}

/// Build a discrete alias table from a slice of non-negative weights (the
/// "alias method" — Walker 1974 / Vose 1991, O(n)). Returns `(probs, aliases)`
/// where `probs[i]` is the self-selection probability and `aliases[i]` is the
/// alternate index.
///
/// At sample time: draw `u ~ U(0,1)`, `j = floor(u * n)`, `u' = u * n - j`;
/// if `u' < probs[j]` pick `j`, else pick `aliases[j]`. `probs[j]` is stored
/// as `prob * n` (the scaled probability) so the comparison is direct.
fn build_alias_table(weights: &[f32]) -> (Vec<f32>, Vec<u32>) {
    let n = weights.len();
    if n == 0 {
        return (vec![], vec![]);
    }
    let total: f32 = weights.iter().sum();
    if total <= 0.0 {
        // Degenerate: all zero weights — uniform probabilities, alias to self.
        return (vec![1.0; n], (0..n as u32).collect());
    }
    let inv_total = 1.0 / total;
    let n_f = n as f32;
    let avg = 1.0 / n_f;

    let mut probs: Vec<f32> = weights.iter().map(|w| w * inv_total).collect();
    let mut aliases: Vec<u32> = (0..n as u32).collect();

    let mut small: Vec<usize> = Vec::new();
    let mut large: Vec<usize> = Vec::new();

    for (i, &p) in probs.iter().enumerate() {
        if p < avg {
            small.push(i);
        } else {
            large.push(i);
        }
    }

    while let (Some(&s), Some(&l)) = (small.last(), large.last()) {
        probs[s] *= n_f; // scaled probability: p * n
        aliases[s] = l as u32;
        probs[l] = (probs[l] + probs[s] / n_f) - avg; // remaining excess
        small.pop();
        if probs[l] < avg {
            large.pop();
            small.push(l);
        }
    }

    // Remaining entries (rounding) — set prob=1 (always self).
    for &s in &small {
        probs[s] = 1.0;
        aliases[s] = s as u32;
    }
    for &l in &large {
        probs[l] = 1.0;
        aliases[l] = l as u32;
    }

    (probs, aliases)
}

// RT-D3/RT-P2 alignment gotcha (see `ShadowRayParams::refl_spp` block's doc
// comment): this is the regression guard a GPU test alone wouldn't localize
// as clearly — if `inv_view_proj`'s offset ever drifts from its required
// 16-byte-aligned value (a field reordered/resized above it), this fails at
// compile time instead of silently reading garbage on the GPU.
// RS-A (caster cap 4 -> 8): casters grew from 4×32=128B to 8×32=256B;
// inv_view_proj offset and total size recomputed. 336 % 16 == 0.
// RT_INSTANCING_DESIGN.md D11: slot_row_base appended after inv_view_proj
// (offset 400) with 12 bytes of pad — 416 total, a 16-byte multiple on the
// MSL side (float4x4 member alignment) matching this side's 416 exactly.
// D8: the first pad word became emissive_entries_are_local (offset 404).
const _: () = assert!(std::mem::offset_of!(ShadowRayParams, inv_view_proj) == 336);
const _: () = assert!(std::mem::offset_of!(ShadowRayParams, slot_row_base) == 400);
const _: () = assert!(std::mem::offset_of!(ShadowRayParams, emissive_entries_are_local) == 404);
const _: () = assert!(std::mem::size_of::<ShadowRayParams>() == 416);

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
    /// BUG-wytp (rt-reflections-are-normal-map-blind): normal-map texture
    /// index for PRIMARY-hit shading — the perturbed normal feeds both the
    /// reflection lobe's R and the AO/GI cosine-hemisphere gather. `>=
    /// MAX_RT_MATERIAL_TEXTURES` means "no texture bound" (the barycentric
    /// vertex normal stands — pre-BUG-wytp behavior). Populated from the
    /// material's normal-map wiring in `render_scene.rs`, same place/shape
    /// as `mr_tex_index`. Secondary/extension-ray hit shading keeps vertex
    /// normals.
    pub normal_tex_index: u32,
    /// BUG-1gqt: emissive-map texture index for hit-sample emission
    /// (GI gather + reflection-hit shading); `>= MAX_RT_MATERIAL_TEXTURES`
    /// means "no texture bound" (flat `GiMaterial::emissive` factor alone —
    /// pre-BUG-1gqt behavior).
    pub emissive_tex_index: u32,
    /// BUG-1gqt: KHR_texture_transform fold for the emissive map, applied
    /// at the hit sample — the raster's `apply_uv_transform` convention
    /// (see `RtObjectGeometry::emissive_uv_m`). Scalar fields, not a
    /// packed float4: the MSL mirror must match byte-for-byte and a
    /// `float4` would 16-align against this offset.
    pub emissive_uv_m: [f32; 4],
    pub emissive_uv_t: [f32; 2],
    /// RT_INSTANCING_DESIGN.md D6: the owning OBJECT index of this per-slot
    /// row — `trace_shadow_rays` packs it into `out_n.w` at the primary hit
    /// so `accumulate_irradiance`'s `obj_motion` lookup stays per-object.
    /// For unwired scenes `object_index == instance_id` (one row per
    /// object), byte-identical to the pre-instancing id.
    pub object_index: u32,
    /// RT_INSTANCING_DESIGN.md D3: bindless address of THIS slot's
    /// `InstanceTransform` (the renderer's 32-byte layout,
    /// mesh_common.rs:96 — mirrored manually in the MSL; manifold-gpu
    /// cannot depend on manifold-renderer), or 0 when unwired. When
    /// nonzero, `fetch_world_normal` applies the D3 fold
    /// (`rot · (n · msign)`) to the local normal BEFORE the row's
    /// `normal_matrix`, bit-parity with render_scene.wgsl's `vs_main`.
    /// Declared AFTER `object_index` so the u64 lands on its natural
    /// 8-byte alignment at offset 112 — an earlier slot would push the
    /// struct past 120 bytes (the MSL mirror declares the same order).
    pub instance_addr: u64,
}

const _: () = assert!(std::mem::size_of::<RtNormalSource>() == 120);
// RT_INSTANCING_DESIGN.md D3: the consumed `_pad2` words become
// object_index (108) + instance_addr (112) — asserted, not hand-counted.
const _: () = assert!(std::mem::offset_of!(RtNormalSource, object_index) == 108);
const _: () = assert!(std::mem::offset_of!(RtNormalSource, instance_addr) == 112);

/// RT_INSTANCING_DESIGN.md D1/P0: manual mirror of the renderer's
/// `generators::mesh_common::InstanceTransform` (32 bytes,
/// `pos_scale` xyz position + w uniform scale, `rot_pad` xyz XYZ Euler +
/// w mirror marker) — manifold-gpu cannot depend on manifold-renderer, so
/// the layout is mirrored by hand and tied to the MSL `RtInstanceTransform`
/// below by the same manual-sync discipline as every other CPU/GPU mirror
/// in this file. Only the SIZE is machine-checked (the renderer asserts
/// its own copy is 32); the descriptor-build kernel addresses slots as
/// `instances_addr + slot * 32`.
const RT_INSTANCE_TRANSFORM_BYTES: usize = 32;

/// RT_INSTANCING_DESIGN.md D1/P0: Rust mirror of the embedded-MSL
/// `RtAsInstanceDescriptor` — itself the field-for-field mirror of
/// `MTLAccelerationStructureInstanceDescriptor` the descriptor-build kernel
/// writes. The CPU never authors descriptors in instanced mode (the buffer
/// is GPU-private), so this struct exists ONLY to tie the two layouts
/// together at compile time: its size/offsets must equal the objc2
/// binding's, and the MSL mirror must equal this. Do NOT hardcode 64 —
/// assert against the binding (per the design's P0 brief) so an
/// objc2-metal layout change fails loudly here instead of mis-striding the
/// GPU writes.
#[repr(C)]
struct RtAsInstanceDescriptorMirror {
    transformation_matrix: [[f32; 3]; 4],
    options: u32,
    mask: u32,
    intersection_function_table_offset: u32,
    acceleration_structure_index: u32,
}

const _: () = assert!(
    std::mem::size_of::<RtAsInstanceDescriptorMirror>()
        == std::mem::size_of::<MTLAccelerationStructureInstanceDescriptor>()
);
const _: () = assert!(
    std::mem::offset_of!(RtAsInstanceDescriptorMirror, mask)
        == std::mem::offset_of!(MTLAccelerationStructureInstanceDescriptor, mask)
);
const _: () = assert!(
    std::mem::offset_of!(RtAsInstanceDescriptorMirror, acceleration_structure_index)
        == std::mem::offset_of!(
            MTLAccelerationStructureInstanceDescriptor,
            accelerationStructureIndex
        )
);

/// RT_INSTANCING_DESIGN.md D1/P0: Rust mirror of the embedded-MSL
/// `RtInstanceBuildObj` — one entry per object, rewritten every instanced
/// build/refit from the current `objects` slice (transforms, wired
/// instance address, slot base/count, cast mask) and consumed by the
/// descriptor-build kernel. Layout notes (mirrored field-for-field in the
/// MSL): `model` fills the first 64 bytes, `instances_addr` lands at 64
/// (8-aligned), the three `u32`s pack to 84, and the explicit pad brings
/// the stride to 96 — MSL rounds the same struct to 96 (its `float2` pad
/// requires 8-byte alignment, landing at 88). Asserted, not hand-counted.
#[repr(C)]
#[derive(Clone, Copy)]
struct RtInstanceBuildObj {
    model: [[f32; 4]; 4],
    instances_addr: u64,
    slot_base: u32,
    slot_count: u32,
    cast_shadows: u32,
    _pad: [f32; 2],
}

const _: () = assert!(std::mem::size_of::<RtInstanceBuildObj>() == 96);
const _: () = assert!(std::mem::offset_of!(RtInstanceBuildObj, instances_addr) == 64);
const _: () = assert!(std::mem::offset_of!(RtInstanceBuildObj, slot_base) == 72);
const _: () = assert!(std::mem::offset_of!(RtInstanceBuildObj, slot_count) == 76);
const _: () = assert!(std::mem::offset_of!(RtInstanceBuildObj, cast_shadows) == 80);

/// RT_INSTANCING_DESIGN.md D7 + P1.5: an object occupies `instance_slots`
/// TLAS slots when wired (its TRS lives GPU-side); an UNWIRED object
/// collapses to the single identity slot of the D7 fast path. P1.5: the
/// plural-capacity qualifier is gone — a wired 1-capacity buffer still
/// carries a GPU-side TRS the CPU identity descriptor can never
/// represent, so wired-at-all means slot-count = max(1, capacity).
fn effective_instance_slots(obj: &RtObjectGeometry) -> u32 {
    if obj.instances_addr != 0 {
        obj.instance_slots.max(1)
    } else {
        1
    }
}

/// CPU-side rewrite of the instanced descriptor-build params from the
/// CURRENT `objects` slice — the same slice discipline as
/// `build_instance_buffer`/`refit_accel` (slot bases recomputed
/// object-major; capacity is topology, so a refit's bases always match the
/// build's). `ptr` is the mapped base of the `instance_obj_params` buffer.
fn write_instance_obj_params(ptr: *mut u8, objects: &[RtObjectGeometry]) {
    let stride = std::mem::size_of::<RtInstanceBuildObj>();
    let mut base = 0u32;
    for (i, obj) in objects.iter().enumerate() {
        let slots = effective_instance_slots(obj);
        let params = RtInstanceBuildObj {
            model: obj.transform,
            instances_addr: obj.instances_addr,
            slot_base: base,
            slot_count: slots,
            cast_shadows: obj.cast_shadows as u32,
            _pad: [0.0; 2],
        };
        // SAFETY: `ptr` is the buffer's mapped base, sized
        // `objects.len() * stride` by the caller; per-element unaligned
        // writes match the file's other CPU-mirror writes.
        unsafe {
            std::ptr::write_unaligned(ptr.add(i * stride) as *mut RtInstanceBuildObj, params);
        }
        base += slots;
    }
}

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

/// (Re)allocate-if-needed + rewrite in place the [`RtNormalSource`]
/// indirection table from the SAME `objects` slice `build_accel`/
/// `refit_accel` use — same "grow, never shrink-then-reallocate every
/// frame" idiom as `render_scene.rs`'s `ensure_rt_gi_materials`; rewritten
/// every RT-ready frame (cheap: N small POD structs, same cadence as that
/// file's `gi_materials_data` rebuild). Never requires a GPU readback of
/// the actual vertex data itself — the bindless address does that lookup
/// on the GPU, at ray-hit time.
///
/// RT_INSTANCING_DESIGN.md D11 (supersedes D3's object-major duplication):
/// ONE table — canonical per-object rows at `[0, N)`, per-slot rows at
/// `[N, N + Σ)`. Object-indexed kernel readers (the `n4.w` roughness
/// lookups in `atrous_filter`/`accumulate_irradiance`, the RS-C emissive
/// sampler) read canonical rows through the un-offset pointer, UNCHANGED;
/// `instance_id`-indexed readers add `N` via
/// `ShadowRayParams::slot_row_base` (the kernel's offset pointers). Slot
/// rows duplicate the object's fields with `object_index` naming the owner
/// (packed into out_n.w at the primary hit, D6) and `instance_addr`
/// pointing at THIS slot's `InstanceTransform` (0 = unwired, so unwired
/// slot rows are identical to their canonical row). When every object has
/// ≤ 1 slot the slot region is a verbatim copy of the canonical region —
/// byte-identical values whichever region a reader lands in (INV-RTI3).
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
    let object_count = objects.len();
    let slot_total: usize = objects
        .iter()
        .map(|o| effective_instance_slots(o) as usize)
        .sum();
    // D11: canonical rows [0, N) + slot rows [N, N+Σ) in one buffer.
    let needed = (object_count + slot_total).max(1);
    if slot.is_none() || *capacity < needed {
        *slot = Some(device.create_buffer_shared((needed * std::mem::size_of::<RtNormalSource>()) as u64));
        *capacity = needed;
    }
    let buf = slot.as_ref().expect("just ensured above");
    let ptr = buf
        .mapped_ptr()
        .expect("RT normal-source buffer must be CPU-mapped");
    let mut material_textures: Vec<&'a GpuTexture> = Vec::new();
    // D11: slot-region cursor — object i's slots occupy [base_i, base_i +
    // cap_i) within [N, N+Σ), matching the TLAS slot addressing.
    let mut slot_row = 0usize;
    for (i, obj) in objects.iter().enumerate() {
        let slots = effective_instance_slots(obj);
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
        // BUG-wytp (rt-reflections-are-normal-map-blind): SAME dedupe-into-
        // `material_textures`, cap-check, and log-warn-on-full pattern as
        // `mr_tex_index` above — the normal map rides the one general
        // material-texture cap, no separate table.
        let normal_tex_index = match obj.normal_texture {
            Some(tex) if material_textures.len() < MAX_RT_MATERIAL_TEXTURES => {
                let idx = material_textures.iter().position(|&t| std::ptr::eq(t, tex))
                    .unwrap_or_else(|| {
                        material_textures.push(tex);
                        material_textures.len() - 1
                    });
                idx as u32
            }
            Some(_) => {
                log::warn!("RT material-texture table full ({} bound, {} cap) — object {} normal map degraded to vertex normal",
                    material_textures.len(), MAX_RT_MATERIAL_TEXTURES, i);
                RT_MATERIAL_TEX_INDEX_NONE
            }
            None => RT_MATERIAL_TEX_INDEX_NONE,
        };
        // BUG-1gqt (rt-trace-ignores-emissive-texture): SAME dedupe-into-
        // `material_textures`, cap-check, and log-warn-on-full pattern as
        // `normal_tex_index` above — the emissive map rides the one general
        // material-texture cap, no separate table.
        let emissive_tex_index = match obj.emissive_texture {
            Some(tex) if material_textures.len() < MAX_RT_MATERIAL_TEXTURES => {
                let idx = material_textures.iter().position(|&t| std::ptr::eq(t, tex))
                    .unwrap_or_else(|| {
                        material_textures.push(tex);
                        material_textures.len() - 1
                    });
                idx as u32
            }
            Some(_) => {
                log::warn!("RT material-texture table full ({} bound, {} cap) — object {} emissive map degraded to flat emissive factor",
                    material_textures.len(), MAX_RT_MATERIAL_TEXTURES, i);
                RT_MATERIAL_TEX_INDEX_NONE
            }
            None => RT_MATERIAL_TEX_INDEX_NONE,
        };
        let mut src = RtNormalSource {
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
            normal_tex_index,
            emissive_tex_index,
            emissive_uv_m: obj.emissive_uv_m,
            emissive_uv_t: obj.emissive_uv_t,
            object_index: i as u32,
            // Canonical row: unwired by definition — object-indexed readers
            // must never see an instance fold.
            instance_addr: 0,
        };
        // D11: canonical row at [0, N).
        unsafe {
            std::ptr::write_unaligned(ptr.add(i * std::mem::size_of::<RtNormalSource>()) as *mut _, src);
        }
        // D11: slot rows at [N, N+Σ) duplicate the row; only instance_addr
        // varies (object_index already names this object).
        for s in 0..slots {
            src.instance_addr = if obj.instances_addr != 0 {
                obj.instances_addr + s as u64 * RT_INSTANCE_TRANSFORM_BYTES as u64
            } else {
                0
            };
            unsafe {
                std::ptr::write_unaligned(ptr.add((object_count + slot_row) * std::mem::size_of::<RtNormalSource>()) as *mut _, src);
            }
            slot_row += 1;
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
    /// Camera-motion magnitude this frame: radians of view-direction turn
    /// plus a weighted translation term (see `RenderScene`'s computation).
    /// 0 on a held camera — the kernel's change gates then behave
    /// byte-identically to before this field existed. Under motion the
    /// gates' bands widen (they cannot tell a real lighting change from
    /// motion-induced content change, and snapped every frame — the
    /// snap→rebuild→retrip cycle was the camera-rotation boil); the CPU
    /// lighting key still snaps, so real cues keep landing mid-gesture.
    pub cam_motion: f32,
    /// Padding to the 16-byte matrix alignment (MSL pads identically).
    pub _cam_motion_pad: [f32; 3],
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
// + cam_motion(4) + pad(12) = 48 bytes — a multiple of 16, so both
// `float4x4`s that follow land on a 16-byte boundary.
// Asserted directly rather than re-derived, same discipline as the
// `ShadowRayParams` guard above.
const _: () = assert!(std::mem::offset_of!(AccumulateParams, camera_pos) == 20);
const _: () = assert!(std::mem::offset_of!(AccumulateParams, cam_motion) == 32);
const _: () = assert!(std::mem::offset_of!(AccumulateParams, inv_view_proj) == 48);
const _: () = assert!(std::mem::offset_of!(AccumulateParams, prev_view_proj) == 112);
const _: () = assert!(std::mem::size_of::<AccumulateParams>() == 176);

impl AccumulateParams {
    pub fn new(
        size: [u32; 2],
        alpha: f32,
        reset: bool,
        obj_count: u32,
        camera_pos: [f32; 3],
        cam_motion: f32,
        inv_view_proj: [[f32; 4]; 4],
        prev_view_proj: [[f32; 4]; 4],
    ) -> Self {
        Self {
            size,
            alpha,
            reset: reset as u32,
            obj_count,
            camera_pos,
            cam_motion,
            _cam_motion_pad: [0.0; 3],
            inv_view_proj,
            prev_view_proj,
        }
    }

    /// Tell the accumulator the CPU knows a light changed this frame, so it
    /// collapses its per-texel history length and the cue lands instead of
    /// averaging in.
    ///
    /// Why a flag and not a per-pixel heuristic: the per-texel gates in the
    /// kernel compare this frame against the tracked spread, which works only
    /// when the changed term is a large enough share of the channel. A sun
    /// intensity move is a SMALL share of a buffer dominated by the ambient
    /// term, so it slipped under the gate and faded over the whole window,
    /// while an env move — the dominant term — snapped. Peter found exactly
    /// that split. The engine already knows a light param changed, so it says
    /// so; the gates stay for what the CPU cannot see (an emissive object
    /// animated from inside the graph).
    ///
    /// Rides `reset`'s spare bits rather than a new field: `AccumulateParams`
    /// is sized so both `float4x4`s land 16-byte aligned, and a new `u32`
    /// would break that (the alignment guard above is deliberate).
    /// Bit 0 = full reset, bit 1 = lighting changed.
    pub fn with_lighting_changed(mut self, changed: bool) -> Self {
        if changed {
            self.reset |= ACCUM_FLAG_LIGHTING_CHANGED;
        }
        self
    }

    /// Tell the accumulator a gesture is in progress (two consecutive frames
    /// of lighting-key change). The irradiance and reflection channels hold
    /// n=2 for the gesture's duration so the output stays soft-and-current
    /// instead of trailing. RAYTRACING_DESIGN.md section 10 addendum.
    pub fn with_gesture(mut self, gesture: bool) -> Self {
        if gesture {
            self.reset |= ACCUM_FLAG_GESTURE;
        }
        self
    }

    /// Tell the accumulator the geometry-only sub-key changed this frame
    /// (caster positions/directions/cone/kind moved, or the designated svt
    /// slot changed). Drives svt's own snap decision. Section 10 addendum.
    pub fn with_geo_changed(mut self, changed: bool) -> Self {
        if changed {
            self.reset |= ACCUM_FLAG_GEO_CHANGED;
        }
        self
    }

    /// Two consecutive frames of geometry-sub-key change — a geometry
    /// gesture. Folded into the sv/sv2 trip condition so shadow channels
    /// take the cue directly. Section 10 addendum.
    pub fn with_geo_gesture(mut self, gesture: bool) -> Self {
        if gesture {
            self.reset |= ACCUM_FLAG_GEO_GESTURE;
        }
        self
    }

    /// Tell the accumulator the MetalFX denoiser consumes this frame's
    /// beauty (RAYTRACING_DESIGN.md section 17.7 DN-L): the kernel drops
    /// every temporal history cap to near-raw (n ≤ 4) so the network's
    /// own history replaces ours.
    pub fn with_denoise_near_raw(mut self, active: bool) -> Self {
        if active {
            self.reset |= ACCUM_FLAG_DENOISE_NEAR_RAW;
        }
        self
    }
}

/// `AccumulateParams::reset` bit 1 — see `with_lighting_changed`. Bit 0 is the
/// original full-reset meaning, so a plain `reset: true` is still `1`.
pub const ACCUM_FLAG_LIGHTING_CHANGED: u32 = 2;
/// Bit 2 — two consecutive frames of lighting-key change (a gesture in
/// progress). The irradiance and reflection channels hold n=2 for the
/// gesture's duration so the output stays soft-and-current instead of
/// trailing. Rides `reset`'s spare bits per the section 10 addendum.
pub const ACCUM_FLAG_GESTURE: u32 = 4;
/// Bit 3 — the geometry-only sub-key changed this frame (caster position/
/// direction/cone/kind moved, or the designated svt slot changed). Drives
/// svt's own decision: tint snaps on geometry cues.
pub const ACCUM_FLAG_GEO_CHANGED: u32 = 8;
/// Bit 4 — two consecutive frames of geometry-sub-key change. Folded into
/// the sv/sv2 trip condition so shadow channels take the cue directly.
pub const ACCUM_FLAG_GEO_GESTURE: u32 = 16;
/// Bit 5 — the MetalFX denoiser consumes this frame's beauty
/// (RAYTRACING_DESIGN.md section 17.7 DN-L): drop every temporal history
/// cap to near-raw (n ≤ 4, alpha floor 0.25) so the network's own history
/// replaces ours — feeding it pre-smoothed frames is double temporal
/// filtering (Peter's fused-path look rejection, confound 1).
pub const ACCUM_FLAG_DENOISE_NEAR_RAW: u32 = 32;

/// CPU mirror of the MSL `AtrousParams` struct backing `atrous_filter`
/// (RT-T1-D, BUG-312). MSL uint2 requires eight-byte alignment, including
/// tail padding; keep that padding explicit and initialized on the CPU.
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
    _pad: u32,
}

const _: () = assert!(std::mem::size_of::<AtrousParams>() == 24);

impl AtrousParams {
    pub fn new(size: [u32; 2], step: u32, history_valid: bool, obj_count: u32) -> Self {
        Self {
            size,
            step,
            history_valid: history_valid as u32,
            obj_count,
            _pad: 0,
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

/// CPU mirror of the MSL `FireflyClampParams` struct backing `firefly_clamp`
/// (RT-Stage-3 P1, BUG-mkgh). Plain POD — `uint2 size` then two `f32`s, no
/// alignment surprises.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FireflyClampParams {
    pub size: [u32; 2],
    /// `FIREFLY_MEDIAN_GAIN` — how many medians above the local median a
    /// texel's luma may reach before it is clamped. Committed 8.0.
    pub gain: f32,
    /// `FIREFLY_ABS_FLOOR` = `max(4.0, emissive_table_mean_power)` — the
    /// absolute luma floor the threshold never dips below, so an isolated
    /// legit small emitter isn't hard-ceilinged at `gain * 1.0` luma.
    pub floor: f32,
}

const _: () = assert!(std::mem::size_of::<FireflyClampParams>() == 16);

impl FireflyClampParams {
    pub fn new(size: [u32; 2], gain: f32, floor: f32) -> Self {
        Self { size, gain, floor }
    }
}

fn firefly_clamp_params_bytes(params: &FireflyClampParams) -> &[u8] {
    // SAFETY: `FireflyClampParams` is `#[repr(C)]`, all-POD (two u32 + two
    // f32), no padding, no interior pointers — same discipline as
    // `atrous_params_bytes`.
    unsafe {
        std::slice::from_raw_parts(
            (params as *const FireflyClampParams) as *const u8,
            std::mem::size_of::<FireflyClampParams>(),
        )
    }
}

/// CPU mirror of the MSL `AtrousPostParams` struct backing `atrous_post`
/// (RT-Stage-3 P3, BUG-eytk). Plain POD — `uint2 size` + `uint step` +
/// `float strength`, 16 bytes. Same discipline as `AtrousParams`/`FireflyClampParams`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AtrousPostParams {
    pub size: [u32; 2],
    /// Dilation step in texels (1, 2, 4, 8 — same convention as AtrousParams.step).
    pub step: u32,
    /// Blend strength: 0.0 = no filtering, 1.0 = full replace.
    pub strength: f32,
}

const _: () = assert!(std::mem::size_of::<AtrousPostParams>() == 16);

impl AtrousPostParams {
    pub fn new(size: [u32; 2], step: u32, strength: f32) -> Self {
        Self { size, step, strength }
    }
}

fn atrous_post_params_bytes(params: &AtrousPostParams) -> &[u8] {
    // SAFETY: `AtrousPostParams` is `#[repr(C)]`, all-POD (two u32 + one
    // u32 + one f32 = 16 bytes), no padding, no interior pointers — same
    // discipline as `atrous_params_bytes`.
    unsafe {
        std::slice::from_raw_parts(
            (params as *const AtrousPostParams) as *const u8,
            std::mem::size_of::<AtrousPostParams>(),
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
    compile_pipeline_with_constants(device, library, entry, slot_map, None)
}

/// Compile a compute PSO with optional function constants.
/// `constants` is `None` for the default (all false) path; when present,
/// the values are baked into the PSO at compile time (dead-code elimination).
fn compile_pipeline_with_constants(
    device: &GpuDevice,
    library: &ProtocolObject<dyn MTLLibrary>,
    entry: &str,
    slot_map: SlotMap,
    constants: Option<&MTLFunctionConstantValues>,
) -> GpuComputePipeline {
    // COMPILE_CONTRACT_DESIGN D1: the MSL path records cold touches too —
    // the WGSL-path-only counter left this whole file invisible to the
    // no-compiles-during-playback gate.
    record_cold_touch(ColdTouchKind::PipelineCompile);
    let name = NSString::from_str(entry);
    let func = match constants {
        Some(cv) => library
            .newFunctionWithName_constantValues_error(&name, cv)
            .unwrap_or_else(|e| {
                panic!(
                    "RT kernel entry point '{entry}' with constants: {}",
                    e.localizedDescription()
                )
            }),
        None => library
            .newFunctionWithName(&name)
            .unwrap_or_else(|| panic!("RT kernel entry point '{entry}' not found")),
    };
    let state: Retained<ProtocolObject<dyn MTLComputePipelineState>> = device
        .raw_device()
        .newComputePipelineStateWithFunction_error(&func)
        .unwrap_or_else(|e| panic!("{entry}: compute PSO error: {}", e.localizedDescription()));
    let workgroup_product = SHADOW_WORKGROUP[0] as usize * SHADOW_WORKGROUP[1] as usize * SHADOW_WORKGROUP[2] as usize;
    assert!(workgroup_product <= state.maxTotalThreadsPerThreadgroup(), "RT pipeline {entry} workgroup exceeds device limit");
    log::info!("[RT] pipeline {entry}: workgroup={SHADOW_WORKGROUP:?} max_threads={} execution_width={} static_threadgroup_memory={}", state.maxTotalThreadsPerThreadgroup(), state.threadExecutionWidth(), state.staticThreadgroupMemoryLength());
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
    /// RS-B: `gi_materials` is the per-object material table (SAME order
    /// as `objects`) — consumed to build the emissive-triangle light table.
    fn build_accel(&self, device: &GpuDevice, objects: &[RtObjectGeometry], gi_materials: &[GiMaterial]) -> Self::Accel;

    /// Refit `accel`'s instance transforms in place from `objects` — cheap
    /// (TLAS-only update), used when objects move but the object SET and
    /// each object's topology are unchanged (mirrors `objects.len()` and
    /// vertex/index buffer identity against what `accel` was built from —
    /// caller's dirty-check, e.g. render_scene.rs's shadow-map cache-key
    /// idiom). A topology change calls `build_accel` again instead.
    /// RS-B: also refits the emissive light table's world-space positions
    /// when the accel carries one.
    fn refit_accel(&self, device: &GpuDevice, accel: &Self::Accel, objects: &[RtObjectGeometry]) -> Result<(), RtTopologyMismatch>;

    /// Dispatch the half-res shadow/AO-ray pass (RT-D3; RT-P2 widens this
    /// SAME dispatch to add the AO gather + demodulated-irradiance term —
    /// D16's seam note, not a parallel pass; RT-P3 widens it again with the
    /// emissive/sun-bounce GI gather, reading `gi_materials` — one entry
    /// per object, SAME order as the `objects` slice `build_accel` was
    /// called with, so `instance_id` at a GI ray hit indexes it directly):
    /// ray origins + bias normal reconstructed in-kernel from `depth_tex`
    /// (the full-res opaque-depth prepass) + `params.inv_view_proj` — no
    /// world-pos/normal G-buffer target. Writes per-caster visibility to
    /// `out_sv` (slots 0-3) + `out_sv2` (slots 4-7, RS-A) and demodulated
    /// irradiance (now including the GI gather) to `out_irr`, all at
    /// `params.trace_size`.
    /// `current_objects` must be the identical ordered slice used to build
    /// `normal_sources`; wired instance sources are declared from this slice.
    /// RT-T1-B: `normal_sources` is the per-object [`RtNormalSource`] bindless table (built via
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
        device: &GpuDevice,
        accel: &Self::Accel,
        params: &ShadowRayParams,
        params_buffer: &GpuBuffer,
        gi_materials: &GpuBuffer,
        normal_sources: &GpuBuffer,
        current_objects: &[RtObjectGeometry<'_>],
        alpha_textures: &[&GpuTexture],
        depth_tex: &GpuTexture,
        out_sv: &GpuTexture,
        out_sv2: &GpuTexture,
        // RT-TL-C (section 16 TL5): rgb sun-transmission tint for the
        // designated sun caster — half-res trace output, rides the chain.
        out_svt: &GpuTexture,
        out_irr: &GpuTexture,
        out_n: &GpuTexture,
        out_refl: &GpuTexture,
        // RT-R1 (section 9.3 RD4): prefiltered-specular env mip chain — the
        // reflection ray's miss radiance. Always bound (dummy when the
        // scene has no env chain).
        prefiltered_env: &GpuTexture,
        // RS-C: emissive-triangle light table + alias table buffers, built
        // CPU-side at accel registration. Empty-scene fallbacks are
        // zero-filled and sized to the larger typed element (80-byte
        // triangle / 8-byte alias); entry_count=0 skips the kernel block.
        emissive_triangles: &GpuBuffer,
        emissive_aliases: &GpuBuffer,
        // RT-TL-B cost recovery (RAYTRACING_DESIGN.md section 16.4): selects
        // between the binary pipeline (walk_with_alpha_test, pre-TL-B codegen)
        // and the translucent pipeline (walk_with_transmission) at dispatch time.
        has_translucency: bool,
        label: &str,
    );

    /// Depth-aware bilateral upsample of the half-res `lo_sv`/`lo_irr`/
    /// `lo_n` terms to full G-buffer resolution `hi_sv`/`hi_irr`/`hi_n`
    /// (RT-D3's "D11 trivial pass"; RT-P2 widened the SAME upsample to
    /// also carry irradiance; RT-T1-C widens it once more to carry the
    /// primary-hit vertex normal `accumulate_irradiance`'s reprojection
    /// validity test needs). RS-A (caster cap 4 -> 8): `lo_sv2`/`hi_sv2`
    /// carry the second shadow-visibility quad (caster slots 4-7), same
    /// half->full bilateral upsample as the first.
    #[allow(clippy::too_many_arguments)]
    fn upsample_shadow(
        &self,
        encoder: &mut GpuEncoder,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        lo_sv: &GpuTexture,
        hi_sv: &GpuTexture,
        lo_sv2: &GpuTexture,
        hi_sv2: &GpuTexture,
        lo_irr: &GpuTexture,
        hi_irr: &GpuTexture,
        lo_n: &GpuTexture,
        hi_n: &GpuTexture,
        lo_refl: &GpuTexture,
        hi_refl: &GpuTexture,
        // RT-TL-C (section 16 TL5): sun-transmission tint — MASK-class,
        // same half->full bilateral upsample as sv.
        lo_svt: &GpuTexture,
        hi_svt: &GpuTexture,
        label: &str,
    );

    /// RT-T1-D (RAYTRACING_DESIGN.md section 8 Tier-1 item 3, BUG-312): one
    /// dilated edge-aware à-trous pass, full-res to full-res, guided by
    /// `depth_tex` + `src_n`'s own normal + `moments_read`'s variance
    /// (one-frame-lagged, from the LAST `accumulate_irradiance` call —
    /// same lag convention as the depth/normal history reads). Called
    /// `ATROUS_ITERATIONS`-1 times by the caller with an increasing
    /// `step` (1, 2, ...), after `upsample_shadow` has already produced
    /// the initial full-res `src_*` set. RS-A: `src_sv2`/`dst_sv2` filter
    /// the second shadow-visibility quad independently with the same
    /// depth+normal edge-stops.
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
        src_sv2: &GpuTexture,
        dst_sv2: &GpuTexture,
        src_irr: &GpuTexture,
        dst_irr: &GpuTexture,
        src_n: &GpuTexture,
        dst_n: &GpuTexture,
        src_refl: &GpuTexture,
        dst_refl: &GpuTexture,
        // RT-TL-C (section 16 TL5): sun-transmission tint — MASK-class,
        // same depth + normal edge-stops as sv.
        src_svt: &GpuTexture,
        dst_svt: &GpuTexture,
        label: &str,
    );

    /// RT-Stage-3 P1 (BUG-mkgh): pre-blur firefly clamp — one full-res pass
    /// that caps each texel's luma to `gain * max(neighborhood median,
    /// floor)` (void and sub-3-neighbor texels pass through untouched), so
    /// downstream DoF/bloom blurs don't smear RT fireflies into bokeh blobs.
    /// `src` and `dst` are always distinct textures (the caller resolves the
    /// forward pass into a dedicated `rt_firefly_scratch`, then clamps
    /// scratch→`dst` — never aliased, or the 3x3 read would race the write).
    fn firefly_clamp(
        &self,
        encoder: &mut GpuEncoder,
        params: &FireflyClampParams,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        src: &GpuTexture,
        dst: &GpuTexture,
        label: &str,
    );

    /// RT-Stage-3 P3 (BUG-eytk): post-accumulation à-trous spatial filter on
    /// the demodulated irradiance. Smooths temporal accumulation's residual
    /// Monte-Carlo noise using variance + spatial-spread guided bilateral
    /// weights. Writes ONLY `dst_irr` — never touches moments or history
    /// (I2: the filter never teaches the accumulator).
    fn atrous_post_pass(
        &self,
        encoder: &mut GpuEncoder,
        params: &AtrousPostParams,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        normal_tex: &GpuTexture,
        moments_read: &GpuTexture,
        src_irr: &GpuTexture,
        dst_irr: &GpuTexture,
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
        // SV-ACCUM: shadow-visibility channel (4 caster slots, 0-3) — current
        // frame post-atrous mask + its own ping-pong history pair, same
        // flip clock as the irradiance/reflection pairs.
        hi_sv: &GpuTexture,
        sv_history_read: &GpuTexture,
        sv_history_write: &GpuTexture,
        // SV-ACCUM moments: per-channel first/second visibility moments
        // (two ping-pong pairs, same clock) — the sigma the sv change gate
        // needs to tell penumbra boil from a real shadow-edge crossing.
        sv_m1_read: &GpuTexture,
        sv_m1_write: &GpuTexture,
        sv_m2_read: &GpuTexture,
        sv_m2_write: &GpuTexture,
        // SV-ACCUM snap-hold countdown pair (`.x`, same clock) — a gate
        // trip holds the n=2 snap for 4 frames because the straddling
        // moments deaden the sigma gate right after a crossing.
        sv_hold_read: &GpuTexture,
        sv_hold_write: &GpuTexture,
        // RS-A (caster cap 4 -> 8): second shadow-visibility channel
        // (caster slots 4-7) — same SV-ACCUM pipeline (running-mean blend
        // with its own sigma-gate + snap-hold), same flip clock, fully
        // independent from the first channel so a penumbra boil in an
        // upper-slot caster never trips the non-boiling lower slots.
        hi_sv2: &GpuTexture,
        sv2_history_read: &GpuTexture,
        sv2_history_write: &GpuTexture,
        sv2_m1_read: &GpuTexture,
        sv2_m1_write: &GpuTexture,
        sv2_m2_read: &GpuTexture,
        sv2_m2_write: &GpuTexture,
        sv2_hold_read: &GpuTexture,
        sv2_hold_write: &GpuTexture,
        // RT-TL-C (section 16 TL5/TL8): sun-transmission tint channel —
        // same flip clock, same weights/reset as irradiance (not sv sigma-gate).
        hi_svt: &GpuTexture,
        svt_history_read: &GpuTexture,
        svt_history_write: &GpuTexture,
        label: &str,
    );
}

/// Metal implementation of [`ShadowRayTracer`] — ray queries via
/// `metal_raytracing`, compiled once and kept resident (mirrors the
/// pipeline-cache pattern `GpuDevice` already uses for the WGSL path).
// Fixed diagnostic slots. Each slot is exclusively owned until its completion
// callback finishes reading; no CPU/GPU readback race and no per-frame buffers.
struct TraceDiagnosticPool {
    buffers: [GpuBuffer; 8],
    busy: [AtomicBool; 8],
    disabled: GpuBuffer,
}

const TRACE_TRANSLUCENCY_CONSTANT_INDEX: usize = 100;
const TRACE_PASS_CONSTANT_INDEX: usize = 101;
const MAX_RT_REFLECTION_SPP: u32 = 32;

fn trace_region_bytes(region: &TraceRegion) -> &[u8] {
    const _: () = assert!(std::mem::size_of::<TraceRegion>() == 16);
    // repr(C), four initialized u32s, and no padding.
    unsafe { std::slice::from_raw_parts((region as *const TraceRegion).cast(), 16) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
enum TracePass {
    Shadow = 0,
    Diffuse = 1,
    Reflection = 2,
}

impl TracePass {
    const ALL: [Self; 3] = [Self::Shadow, Self::Diffuse, Self::Reflection];
    fn enabled(self, params: &ShadowRayParams) -> bool {
        match self {
            Self::Shadow => params.shadow_spp > 0,
            Self::Diffuse => params.ao_spp > 0 || params.gi_spp > 0,
            Self::Reflection => params.refl_spp > 0,
        }
    }
    fn pipeline_label(self, translucent: bool) -> &'static str {
        match (self, translucent) {
            (Self::Shadow, false) => "RT shadow binary",
            (Self::Shadow, true) => "RT shadow translucent",
            (Self::Diffuse, false) => "RT AO+GI binary",
            (Self::Diffuse, true) => "RT AO+GI translucent",
            (Self::Reflection, false) => "RT reflection binary",
            (Self::Reflection, true) => "RT reflection translucent",
        }
    }
}

pub struct MetalShadowRayTracer {
    /// Fixed slots retain their first incident; callbacks hold the pool alive.
    rt_diagnostics: Arc<TraceDiagnosticPool>,
    /// RT-TL-B cost recovery (RAYTRACING_DESIGN.md section 16.4): trace pipeline
    /// for translucent scenes — `HAS_TRANSLUCENCY` baked to true (walk_with_transmission
    /// in sv caster loop + sun_bounce_at_hit).
    trace_pipelines: [[GpuComputePipeline; 2]; 3],
    /// RT-TL-B cost recovery (section 16.4): trace pipeline for binary (no-translucency)
    /// scenes — `HAS_TRANSLUCENCY` baked to false (walk_with_alpha_test, pre-TL-B
    /// codegen byte-for-byte in the sv caster loop).
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
    /// RT-Stage-3 P1 (BUG-mkgh): the pre-blur firefly clamp pipeline.
    firefly_clamp_pipeline: GpuComputePipeline,
    /// RT-Stage-3 P1 value-test-only surface (`debug_firefly_clamp`'s only
    /// caller) — see the MSL `debug_firefly_clamp` kernel's doc comment.
    debug_firefly_clamp_pipeline: GpuComputePipeline,
    /// RT-Stage-3 P3 (BUG-eytk): the post-accumulation à-trous filter pipeline.
    atrous_post_pipeline: GpuComputePipeline,
    /// RT-Stage-3 P3 value-test-only surface (`debug_atrous_post`'s only
    /// caller) — see the MSL `debug_atrous_post` kernel's doc comment.
    debug_atrous_post_pipeline: GpuComputePipeline,
    /// RT-T2-A: 1x1 fully-opaque texture bound into every one of
    /// `trace_shadow_rays`'s `alpha_textures` slots that this frame's
    /// `dispatch_shadow_rays` call doesn't supply a real texture for —
    /// Metal requires a valid resource bound at every argument-table index
    /// a compiled kernel references, even one `sample_candidate_alpha`
    /// (MSL) never actually indexes at runtime.
    dummy_alpha_tex: GpuTexture,
}

/// COMPILE_CONTRACT_DESIGN D3: the RT pipeline set is device-global code —
/// one MSL library and its PSOs, compiled once per process behind
/// [`GpuDevice::rt_pipelines`] (OnceLock; the MSL source is a per-build
/// constant, so the OnceLock IS the source-hash cache). Tracer instances
/// own data (accels, buffers, textures), never code.
#[derive(Clone)]
pub struct RtPipelines {
    pub trace_pipelines: [[GpuComputePipeline; 2]; 3],
    pub upsample_pipeline: GpuComputePipeline,
    pub atrous_pipeline: GpuComputePipeline,
    pub accumulate_pipeline: GpuComputePipeline,
    pub debug_fetch_normal_pipeline: GpuComputePipeline,
    pub debug_clamp_refl_history_pipeline: GpuComputePipeline,
    pub firefly_clamp_pipeline: GpuComputePipeline,
    pub debug_firefly_clamp_pipeline: GpuComputePipeline,
    pub atrous_post_pipeline: GpuComputePipeline,
    pub debug_atrous_post_pipeline: GpuComputePipeline,
    /// RT_INSTANCING_DESIGN.md D1/P0: the TLAS descriptor-build kernel —
    /// dispatched ahead of the TLAS build/refit on the same command buffer
    /// in instanced mode (never on the D7 fast path).
    pub descriptor_build_pipeline: GpuComputePipeline,
}

impl RtPipelines {
    pub(crate) fn compile(device: &GpuDevice) -> Self {
        // One MSL library compile per populate (the PSO compiles record
        // themselves inside compile_pipeline_with_constants).
        record_cold_touch(ColdTouchKind::PipelineCompile);
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
            (4, SlotKind::Buffer), // RS-C: emissive_triangles, MSL [[buffer(4)]]
            (5, SlotKind::Buffer), // RS-C: emissive_aliases, MSL [[buffer(5)]]
            (6, SlotKind::Buffer), // D8: instance descriptors, MSL [[buffer(6)]]
            (7, SlotKind::Buffer), // diagnostics record, MSL [[buffer(7)]]
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
        // RS-A (caster cap 4 -> 8): out_sv2, MSL [[texture(70)]].
        trace_slots.push((6 + MAX_RT_MATERIAL_TEXTURES as u32, SlotKind::Texture));
        // RT-TL-C: out_svt, MSL [[texture(71)]].
        trace_slots.push((7 + MAX_RT_MATERIAL_TEXTURES as u32, SlotKind::Texture));
        trace_slots.push((8, SlotKind::Buffer));
        // Three trace passes crossed with binary/translucent ray semantics.
        // Both constants are supplied before compilation; SPP is unchanged.
        let trace_slot_map = identity_slot_map(&trace_slots);
        let trace_pipelines = TracePass::ALL.map(|pass| [false, true].map(|translucent| {
            let cv = unsafe { MTLFunctionConstantValues::init(MTLFunctionConstantValues::alloc()) };
            let has: u8 = u8::from(translucent);
            let value = pass as u32;
            unsafe {
                cv.setConstantValue_type_atIndex(core::ptr::NonNull::from(&has).cast(), MTLDataType::Bool, TRACE_TRANSLUCENCY_CONSTANT_INDEX);
                cv.setConstantValue_type_atIndex(core::ptr::NonNull::from(&value).cast(), MTLDataType::UInt, TRACE_PASS_CONSTANT_INDEX);
            }
            let mut pipeline = compile_pipeline_with_constants(device, &library, "trace_shadow_rays", trace_slot_map.clone(), Some(&cv));
            pipeline.label = pass.pipeline_label(translucent).into();
            pipeline
        }));
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
                // RS-A (caster cap 4 -> 8): lo_sv2/hi_sv2, MSL [[texture(9)]]/[[texture(10)]].
                (9, SlotKind::Texture),
                (10, SlotKind::Texture),
                // RT-TL-C: lo_svt/hi_svt, MSL [[texture(11)]]/[[texture(12)]].
                (11, SlotKind::Texture),
                (12, SlotKind::Texture),
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
                // RS-A (caster cap 4 -> 8): src_sv2/dst_sv2, MSL [[texture(11)]]/[[texture(12)]].
                (11, SlotKind::Texture),
                (12, SlotKind::Texture),
                // RT-TL-C: src_svt/dst_svt, MSL [[texture(13)]]/[[texture(14)]].
                (13, SlotKind::Texture),
                (14, SlotKind::Texture),
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
                // SV-ACCUM: hi_sv / sv history pair — same incident class,
                // same rule.
                (14, SlotKind::Texture),
                (15, SlotKind::Texture),
                (16, SlotKind::Texture),
                // SV-ACCUM moments (m1/m2 pairs).
                (17, SlotKind::Texture),
                (18, SlotKind::Texture),
                (19, SlotKind::Texture),
                (20, SlotKind::Texture),
                // SV-ACCUM snap-hold countdown pair.
                (21, SlotKind::Texture),
                (22, SlotKind::Texture),
                // RS-A (caster cap 4 -> 8): sv2 channel — full SV-ACCUM
                // pipeline at [[texture(23)]] through [[texture(31)]].
                (23, SlotKind::Texture),
                (24, SlotKind::Texture),
                (25, SlotKind::Texture),
                (26, SlotKind::Texture),
                (27, SlotKind::Texture),
                (28, SlotKind::Texture),
                (29, SlotKind::Texture),
                (30, SlotKind::Texture),
                (31, SlotKind::Texture),
                // RT-TL-C: hi_svt at [[texture(32)]] and svt history at
                // [[texture(33)]]/[[texture(34)]].
                (32, SlotKind::Texture),
                (33, SlotKind::Texture),
                (34, SlotKind::Texture),
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

        // RT-Stage-3 P1 (BUG-mkgh): firefly clamp — params buffer(1), depth
        // texture(0), src texture(1), dst texture(2). Signatures and slot
        // maps change together.
        let firefly_clamp_pipeline = compile_pipeline(
            device,
            &library,
            "firefly_clamp",
            identity_slot_map(&[
                (1, SlotKind::Buffer),
                (0, SlotKind::Texture),
                (1, SlotKind::Texture),
                (2, SlotKind::Texture),
            ]),
        );

        let debug_firefly_clamp_pipeline = compile_pipeline(
            device,
            &library,
            "debug_firefly_clamp",
            identity_slot_map(&[
                (0, SlotKind::Buffer),
                (0, SlotKind::Texture),
                (1, SlotKind::Texture),
                (1, SlotKind::Buffer),
            ]),
        );

        // RT-Stage-3 P3 (BUG-eytk): post-accumulation à-trous filter —
        // params buffer(1), depth texture(0), normal texture(1), moments(2),
        // src_irr(3), dst_irr(write, 4). Signatures and slot maps change
        // together.
        let atrous_post_pipeline = compile_pipeline(
            device,
            &library,
            "atrous_post",
            identity_slot_map(&[
                (1, SlotKind::Buffer),
                (0, SlotKind::Texture),
                (1, SlotKind::Texture),
                (2, SlotKind::Texture),
                (3, SlotKind::Texture),
                (4, SlotKind::Texture),
            ]),
        );

        let debug_atrous_post_pipeline = compile_pipeline(
            device,
            &library,
            "debug_atrous_post",
            identity_slot_map(&[
                (0, SlotKind::Buffer),
                (0, SlotKind::Texture),
                (1, SlotKind::Texture),
                (2, SlotKind::Texture),
                (3, SlotKind::Texture),
                (1, SlotKind::Buffer),
            ]),
        );

        // RT_INSTANCING_DESIGN.md D1/P0: descriptor-build kernel —
        // descriptors out at [[buffer(0)]], per-object build params at
        // [[buffer(1)]]. Signatures and slot maps change together (the R1
        // slot-map incident class).
        let descriptor_build_pipeline = compile_pipeline(
            device,
            &library,
            "build_instance_descriptors",
            identity_slot_map(&[
                (0, SlotKind::Buffer),
                (1, SlotKind::Buffer),
            ]),
        );

        Self {
            trace_pipelines,
            upsample_pipeline,
            atrous_pipeline,
            accumulate_pipeline,
            debug_fetch_normal_pipeline,
            debug_clamp_refl_history_pipeline,
            firefly_clamp_pipeline,
            debug_firefly_clamp_pipeline,
            atrous_post_pipeline,
            debug_atrous_post_pipeline,
            descriptor_build_pipeline,
        }
    }
}

impl MetalShadowRayTracer {
    /// Populate the device-global RT pipeline set at startup
    /// (COMPILE_CONTRACT_DESIGN P1) — after this, tracer construction
    /// compiles nothing.
    pub fn prewarm(device: &GpuDevice) {
        device.rt_pipelines();
    }

    pub fn new(device: &GpuDevice) -> Self {
        // COMPILE_CONTRACT_DESIGN D3: code is device-global (compiled once
        // per process); the tracer instance owns only data.
        let p = device.rt_pipelines();
        let dummy_alpha_tex = create_dummy_alpha_texture(device);
        let enabled = super::gpu_fault::diagnostics_enabled();
        if enabled {
            log::info!("[RT-DIAG] stages:0=primary,1=shadow/sun,2=AO,3=GI,4=emissive-shadow,5=reflection; invalid rays replaced only in diagnostic mode; records retain first incident per fixed slot");
        }
        let make_record = |enabled: bool| {
            let buffer = device.create_buffer_shared(std::mem::size_of::<RtTraceDiagnostics>() as u64);
            let ptr = buffer.mapped_ptr().expect("shared RT diagnostics");
            unsafe {
                std::ptr::write_bytes(ptr, 0, std::mem::size_of::<RtTraceDiagnostics>());
                (*ptr.cast::<RtTraceDiagnostics>()).enabled = enabled as u32;
            }
            buffer
        };
        let rt_diagnostics = Arc::new(TraceDiagnosticPool {
            buffers: std::array::from_fn(|_| make_record(enabled)),
            busy: std::array::from_fn(|_| AtomicBool::new(false)),
            disabled: make_record(false),
        });

        Self {
            trace_pipelines: p.trace_pipelines.clone(),
            upsample_pipeline: p.upsample_pipeline.clone(),
            atrous_pipeline: p.atrous_pipeline.clone(),
            accumulate_pipeline: p.accumulate_pipeline.clone(),
            debug_fetch_normal_pipeline: p.debug_fetch_normal_pipeline.clone(),
            debug_clamp_refl_history_pipeline: p.debug_clamp_refl_history_pipeline.clone(),
            firefly_clamp_pipeline: p.firefly_clamp_pipeline.clone(),
            debug_firefly_clamp_pipeline: p.debug_firefly_clamp_pipeline.clone(),
            atrous_post_pipeline: p.atrous_post_pipeline.clone(),
            debug_atrous_post_pipeline: p.debug_atrous_post_pipeline.clone(),
            dummy_alpha_tex,
            rt_diagnostics,
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
        // RT_INSTANCING_DESIGN.md D11: the SAME slot-row base the
        // production kernel's params carry (object count N) — the debug
        // surface must exercise the production slot-row indexing, not the
        // canonical rows.
        slot_row_base: u32,
    ) -> [f32; 3] {
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct DebugFetchNormalParams {
            instance_id: u32,
            primitive_id: u32,
            bary: [f32; 2],
            slot_row_base: u32,
        }
        const _: () = assert!(std::mem::size_of::<DebugFetchNormalParams>() == 20);
        let params = DebugFetchNormalParams {
            instance_id,
            primitive_id,
            bary,
            slot_row_base,
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
        unsafe { cb.setLabel(Some(&NSString::from_str("RT-T1-B debug fetch normal"))) };
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
        unsafe { cb.setLabel(Some(&NSString::from_str("BUG-dx6w debug clamp refl history"))) };
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

    /// RT-Stage-3 P1 (BUG-mkgh) value-test-only entry point — dispatches the
    /// SAME `firefly_clamp_center` MSL helper the production `firefly_clamp`
    /// kernel calls, against a caller-supplied 3x3 `color` neighborhood
    /// (row-major, `color[0]` = top-left) and a matching 3x3 `depth`
    /// neighborhood (`depth[i] >= 1.0 - 1e-6` = void, read from the center's
    /// (1,1) texel). Depth uploads as R32Float (Depth32Float has no
    /// CPU-upload path) and the debug kernel reads it via the
    /// `read_firefly_depth` `texture2d<float>` overload — the same scalar
    /// depth value the production `depth2d<float>` path sees. No ray
    /// tracing, no full-res pass; synchronous (commits and waits) —
    /// test-only call pattern, never used on a hot path.
    pub fn debug_firefly_clamp(
        &self,
        device: &GpuDevice,
        color: &[[f32; 4]; 9],
        depth: &[f32; 9],
        gain: f32,
        floor: f32,
    ) -> [f32; 3] {
        let color_tex = device.create_texture(&GpuTextureDesc {
            width: 3,
            height: 3,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "bug-mkgh-debug-firefly-color",
            mip_levels: 1,
        });
        let color_bytes: Vec<u8> = color
            .iter()
            .flat_map(|texel| texel.iter().flat_map(|c| c.to_le_bytes()))
            .collect();
        device.upload_texture(&color_tex, &color_bytes);

        let depth_tex = device.create_texture(&GpuTextureDesc {
            width: 3,
            height: 3,
            depth: 1,
            format: GpuTextureFormat::R32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "bug-mkgh-debug-firefly-depth",
            mip_levels: 1,
        });
        let depth_bytes: Vec<u8> = depth.iter().flat_map(|d| d.to_le_bytes()).collect();
        device.upload_texture(&depth_tex, &depth_bytes);

        let params = FireflyClampParams::new([3, 3], gain, floor);
        let params_buffer = device.create_buffer_shared(16); // FireflyClampParams
        let params_ptr = params_buffer
            .mapped_ptr()
            .expect("debug firefly params buffer must be CPU-mapped");
        unsafe {
            std::ptr::write_unaligned(params_ptr as *mut FireflyClampParams, params);
        }
        let out_buffer = device.create_buffer_shared(16); // packed_float3, rounded up
        out_buffer.zero_fill();

        let cb = device
            .raw_queue()
            .commandBuffer()
            .expect("Failed to acquire command buffer for BUG-mkgh debug dispatch");
        unsafe { cb.setLabel(Some(&NSString::from_str("BUG-mkgh debug firefly clamp"))) };
        let enc: Retained<ProtocolObject<dyn MTLComputeCommandEncoder>> = cb
            .computeCommandEncoder()
            .expect("computeCommandEncoder failed");
        unsafe {
            enc.setComputePipelineState(&self.debug_firefly_clamp_pipeline.state);
            enc.setBuffer_offset_atIndex(Some(params_buffer.raw()), 0, 0);
            enc.setTexture_atIndex(Some(&depth_tex.raw), 0);
            enc.setTexture_atIndex(Some(&color_tex.raw), 1);
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

    /// RT-Stage-3 P3 value-test-only surface (`debug_atrous_post`'s only
    /// caller) — exercises the EXACT SAME `atrous_post_center` MSL helper
    /// the production `atrous_post` kernel calls, against a caller-supplied
    /// 3x3 neighborhood (depth, normal, moments, src_irr). Center is (1,1),
    /// step=1 (the 3x3 dilated read at step 1 reads the full 3x3 — exactly
    /// like `debug_firefly_clamp`'s pattern). No ray tracing, no full-res
    /// pass — synchronous (commits and waits) — test-only call pattern,
    /// never used on a hot path.
    ///
    /// Returns `[r, g, b, a]` — the a channel matters here because the
    /// kernel's `.a` passthrough is an invariant (I2: accumulated AO
    /// unchanged).
    pub fn debug_atrous_post(
        &self,
        device: &GpuDevice,
        depth: &[f32; 9],
        normal: &[[f32; 4]; 9],
        moments: &[[f32; 4]; 9],
        src_irr: &[[f32; 4]; 9],
        step: u32,
        strength: f32,
    ) -> [f32; 4] {
        let depth_tex = device.create_texture(&GpuTextureDesc {
            width: 3,
            height: 3,
            depth: 1,
            format: GpuTextureFormat::R32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "bug-eytk-debug-atrous-depth",
            mip_levels: 1,
        });
        let depth_bytes: Vec<u8> = depth.iter().flat_map(|d| d.to_le_bytes()).collect();
        device.upload_texture(&depth_tex, &depth_bytes);

        let normal_tex = device.create_texture(&GpuTextureDesc {
            width: 3,
            height: 3,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "bug-eytk-debug-atrous-normal",
            mip_levels: 1,
        });
        let normal_bytes: Vec<u8> = normal
            .iter()
            .flat_map(|texel| texel.iter().flat_map(|c| c.to_le_bytes()))
            .collect();
        device.upload_texture(&normal_tex, &normal_bytes);

        let moments_tex = device.create_texture(&GpuTextureDesc {
            width: 3,
            height: 3,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "bug-eytk-debug-atrous-moments",
            mip_levels: 1,
        });
        let moments_bytes: Vec<u8> = moments
            .iter()
            .flat_map(|texel| texel.iter().flat_map(|c| c.to_le_bytes()))
            .collect();
        device.upload_texture(&moments_tex, &moments_bytes);

        let src_tex = device.create_texture(&GpuTextureDesc {
            width: 3,
            height: 3,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "bug-eytk-debug-atrous-src",
            mip_levels: 1,
        });
        let src_bytes: Vec<u8> = src_irr
            .iter()
            .flat_map(|texel| texel.iter().flat_map(|c| c.to_le_bytes()))
            .collect();
        device.upload_texture(&src_tex, &src_bytes);

        let params = AtrousPostParams::new([3, 3], step, strength);
        let params_buffer = device.create_buffer_shared(20); // AtrousPostParams
        let params_ptr = params_buffer
            .mapped_ptr()
            .expect("debug atrous post params buffer must be CPU-mapped");
        unsafe {
            std::ptr::write_unaligned(params_ptr as *mut AtrousPostParams, params);
        }
        let out_buffer = device.create_buffer_shared(16); // packed_float4
        out_buffer.zero_fill();

        let cb = device
            .raw_queue()
            .commandBuffer()
            .expect("Failed to acquire command buffer for BUG-eytk debug dispatch");
        unsafe { cb.setLabel(Some(&NSString::from_str("BUG-eytk debug atrous post"))) };
        let enc: Retained<ProtocolObject<dyn MTLComputeCommandEncoder>> = cb
            .computeCommandEncoder()
            .expect("computeCommandEncoder failed");
        unsafe {
            enc.setComputePipelineState(&self.debug_atrous_post_pipeline.state);
            enc.setBuffer_offset_atIndex(Some(params_buffer.raw()), 0, 0);
            enc.setTexture_atIndex(Some(&depth_tex.raw), 0);
            enc.setTexture_atIndex(Some(&normal_tex.raw), 1);
            enc.setTexture_atIndex(Some(&moments_tex.raw), 2);
            enc.setTexture_atIndex(Some(&src_tex.raw), 3);
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
        let mut result = [0.0f32; 4];
        unsafe {
            std::ptr::copy_nonoverlapping(out_ptr as *const f32, result.as_mut_ptr(), 4);
        }
        result
    }
}

impl ShadowRayTracer for MetalShadowRayTracer {
    type Accel = RtAccel;

    fn build_accel(&self, device: &GpuDevice, objects: &[RtObjectGeometry], gi_materials: &[GiMaterial]) -> Self::Accel {
        build_accel(device, objects, gi_materials)
    }

    fn refit_accel(&self, device: &GpuDevice, accel: &Self::Accel, objects: &[RtObjectGeometry]) -> Result<(), RtTopologyMismatch> {
        refit_accel(device, accel, objects)?;
        // RS-B: refit the emissive light table's world-space positions.
        if let Some(ref table) = accel.emissive_table {
            refit_emissive_table(table, objects);
        }
        Ok(())
    }

    fn dispatch_shadow_rays(
        &self,
        encoder: &mut GpuEncoder,
        device: &GpuDevice,
        accel: &Self::Accel,
        params: &ShadowRayParams,
        params_buffer: &GpuBuffer,
        gi_materials: &GpuBuffer,
        normal_sources: &GpuBuffer,
        current_objects: &[RtObjectGeometry<'_>],
        alpha_textures: &[&GpuTexture],
        depth_tex: &GpuTexture,
        out_sv: &GpuTexture,
        out_sv2: &GpuTexture,
        out_svt: &GpuTexture,
        out_irr: &GpuTexture,
        out_n: &GpuTexture,
        out_refl: &GpuTexture,
        prefiltered_env: &GpuTexture,
        emissive_triangles: &GpuBuffer,
        emissive_aliases: &GpuBuffer,
        // RT-TL-B cost recovery (RAYTRACING_DESIGN.md section 16.4): selects
        // between the binary pipeline (walk_with_alpha_test, pre-TL-B codegen)
        // and the translucent pipeline (walk_with_transmission) at dispatch time.
        has_translucency: bool,
        label: &str,
    ) {
        for object in current_objects {
            validate_instance_source_address(
                object.instances_addr,
                object.instances_buffer.map(GpuBuffer::gpu_address),
            ).unwrap_or_else(|message| panic!("{message}"));
        }
        assert!(params.refl_spp <= MAX_RT_REFLECTION_SPP,
            "reflection spp exceeds the supported quality ladder maximum");
        params_buffer.upload(bytemuck_bytes(params));
        let diagnostic_slot = if super::gpu_fault::diagnostics_enabled() {
            self.rt_diagnostics.busy.iter().position(|busy|
                busy.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok())
        } else { None };
        let diagnostic_callback = if let Some(slot) = diagnostic_slot {
            let pool = Arc::clone(&self.rt_diagnostics);
            let frame = params.frame_index;
            log::info!("[RT-DIAG] trace frame={frame} size={:?} shadow_spp={} ao_spp={} gi_spp={} reflection_spp={}",
                params.trace_size, params.shadow_spp, params.ao_spp, params.gi_spp, params.refl_spp);
            let block = block2::RcBlock::new(move |cb: std::ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                let cb = unsafe { cb.as_ref() };
                if unsafe { cb.status() } == MTLCommandBufferStatus::Completed {
                    // This slot cannot be submitted again until we release it.
                    // Metal completion makes all writes visible, including
                    // relaxed-atomic publication within this completed buffer.
                    let ptr = pool.buffers[slot].mapped_ptr().expect("shared diagnostic slot");
                    let d = unsafe { ptr.cast::<RtTraceDiagnostics>().read_unaligned() };
                    if d.state == 2 {
                        log::error!("[RT-DIAG] frame={frame} slot={slot} first_invalid_in_slot stage={} pixel={} origin={:?} direction={:?} min={} max={} diagnostic_ray_replaced=true", d.first_stage, d.first_pixel, d.raw_origin, d.raw_direction, d.raw_min_distance, d.raw_max_distance);
                    } else {
                        log::info!("[RT-DIAG] frame={frame} slot={slot} record_state={} (0=no invalid recorded,1=incomplete)", d.state);
                    }
                } else {
                    log::error!("[RT-DIAG] frame={frame} slot={slot} validation=unavailable command did not complete");
                }
                pool.busy[slot].store(false, Ordering::Release);
                super::gpu_fault::complete_submission();
            });
            Some(block)
        } else {
            if super::gpu_fault::diagnostics_enabled() {
                log::warn!("[RT-DIAG] validation=unavailable all diagnostic slots in flight");
            }
            None
        };
        let diagnostic_buffer = diagnostic_slot.map(|slot| &self.rt_diagnostics.buffers[slot])
            .unwrap_or(&self.rt_diagnostics.disabled);
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
            GpuBinding::Buffer {
                binding: 4,
                buffer: emissive_triangles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: emissive_aliases,
                offset: 0,
            },
            // RT_INSTANCING_DESIGN.md D8: the TLAS instance-descriptor
            // buffer — the kernel composes local-space emissive entries to
            // world through it in instanced mode. Always bound (the D7 fast
            // path never reads it; encoder.rs's accel dispatch already
            // declares useResource on this buffer).
            GpuBinding::Buffer {
                binding: 6,
                buffer: &accel.instance_buffer,
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
        bindings.push(GpuBinding::Buffer { binding: 7, buffer: diagnostic_buffer, offset: 0 });
        // RT-R1 (section 9.3 RD4): prefiltered env chain at [[texture(69)]] — the
        // reflection miss branch's radiance source.
        bindings.push(GpuBinding::Texture {
            binding: 5 + MAX_RT_MATERIAL_TEXTURES as u32,
            texture: prefiltered_env,
        });
        // RS-A (caster cap 4 -> 8): out_sv2 at [[texture(70)]] — the first slot
        // after prefiltered_env (69).
        bindings.push(GpuBinding::Texture {
            binding: 6 + MAX_RT_MATERIAL_TEXTURES as u32,
            texture: out_sv2,
        });
        // RT-TL-C: out_svt at [[texture(71)]] — the slot after out_sv2 (70).
        bindings.push(GpuBinding::Texture {
            binding: 7 + MAX_RT_MATERIAL_TEXTURES as u32,
            texture: out_svt,
        });
        let caster_count = params.caster_count.min(MAX_RT_CASTERS as u32);
        let sun_count = params.casters[..caster_count as usize].iter()
            .filter(|caster| caster.kind == 0).count() as u32;
        let query_units = estimate_trace_query_units_per_pixel(
            caster_count, sun_count, params.shadow_spp, params.ao_spp,
            params.gi_spp, params.refl_spp, params.emissive_table_count != 0,
        ).expect("validated RT quality must have a finite query estimate");
        let mut regions = plan_trace_regions(
            params.trace_size[0], params.trace_size[1],
            SHADOW_WORKGROUP[0], SHADOW_WORKGROUP[1], query_units,
            DEFAULT_TRACE_WORK_LIMITS,
        ).expect("validated RT trace dimensions must produce a tile plan").peekable();
        while let Some(region) = regions.next() {
            for pass in TracePass::ALL {
                if !pass.enabled(params) { continue; }
                let pipeline = &self.trace_pipelines[pass as usize][usize::from(has_translucency)];
                let diagnostic_label = super::gpu_fault::diagnostics_enabled().then(|| format!(
                    "{label} {} tile origin={},{} extent={}x{}", pipeline.label,
                    region.origin[0], region.origin[1], region.extent[0], region.extent[1],
                ));
                encoder.dispatch_compute_with_accel(
                    pipeline, 0, accel, &bindings,
                    current_objects.iter()
                        .filter(|object| object.instances_addr != 0)
                        .map(|object| object.instances_buffer
                            .expect("validated RT wired instance source buffer")),
                    Some((8, trace_region_bytes(&region))),
                    dispatch_groups_2d(region.extent, SHADOW_WORKGROUP),
                    diagnostic_label.as_deref().unwrap_or(&pipeline.label),
                );
            }
            if regions.peek().is_some() {
                encoder.commit_and_continue(device);
            }
        }
        // The pool slot covers all regions. Only final-buffer completion may
        // read/release it; earlier regions are ordered on the same queue.
        if let Some(block) = diagnostic_callback {
            super::gpu_fault::begin_submission();
            unsafe { encoder.cmd_buf.addCompletedHandler(block2::RcBlock::as_ptr(&block)); }
        }
    }

    fn upsample_shadow(
        &self,
        encoder: &mut GpuEncoder,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        lo_sv: &GpuTexture,
        hi_sv: &GpuTexture,
        lo_sv2: &GpuTexture,
        hi_sv2: &GpuTexture,
        lo_irr: &GpuTexture,
        hi_irr: &GpuTexture,
        lo_n: &GpuTexture,
        hi_n: &GpuTexture,
        lo_refl: &GpuTexture,
        hi_refl: &GpuTexture,
        lo_svt: &GpuTexture,
        hi_svt: &GpuTexture,
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
                // RS-A (caster cap 4 -> 8): second shadow-visibility quad.
                GpuBinding::Texture {
                    binding: 9,
                    texture: lo_sv2,
                },
                GpuBinding::Texture {
                    binding: 10,
                    texture: hi_sv2,
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
                // RT-TL-C: lo_svt/hi_svt at [[texture(11)]]/[[texture(12)]].
                GpuBinding::Texture {
                    binding: 11,
                    texture: lo_svt,
                },
                GpuBinding::Texture {
                    binding: 12,
                    texture: hi_svt,
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
        _params_buffer: &GpuBuffer,
        gi_materials: &GpuBuffer,
        depth_tex: &GpuTexture,
        moments_read: &GpuTexture,
        src_sv: &GpuTexture,
        dst_sv: &GpuTexture,
        src_sv2: &GpuTexture,
        dst_sv2: &GpuTexture,
        src_irr: &GpuTexture,
        dst_irr: &GpuTexture,
        src_n: &GpuTexture,
        dst_n: &GpuTexture,
        src_refl: &GpuTexture,
        dst_refl: &GpuTexture,
        src_svt: &GpuTexture,
        dst_svt: &GpuTexture,
        label: &str,
    ) {
        // Each dispatch owns a parameter snapshot; later passes may use a different step.
        let groups = dispatch_groups_2d(params.size, SHADOW_WORKGROUP);
        encoder.dispatch_compute(
            &self.atrous_pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 1,
                    data: atrous_params_bytes(params),
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
                // RS-A (caster cap 4 -> 8): second shadow-visibility quad.
                GpuBinding::Texture {
                    binding: 11,
                    texture: src_sv2,
                },
                GpuBinding::Texture {
                    binding: 12,
                    texture: dst_sv2,
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
                // RT-TL-C: src_svt/dst_svt at [[texture(13)]]/[[texture(14)]].
                GpuBinding::Texture {
                    binding: 13,
                    texture: src_svt,
                },
                GpuBinding::Texture {
                    binding: 14,
                    texture: dst_svt,
                },
            ],
            groups,
            label,
        );
    }

    fn firefly_clamp(
        &self,
        encoder: &mut GpuEncoder,
        params: &FireflyClampParams,
        params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        src: &GpuTexture,
        dst: &GpuTexture,
        label: &str,
    ) {
        params_buffer.upload(firefly_clamp_params_bytes(params));
        let groups = dispatch_groups_2d(params.size, SHADOW_WORKGROUP);
        encoder.dispatch_compute(
            &self.firefly_clamp_pipeline,
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
                    texture: src,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: dst,
                },
            ],
            groups,
            label,
        );
    }

    fn atrous_post_pass(
        &self,
        encoder: &mut GpuEncoder,
        params: &AtrousPostParams,
        _params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        normal_tex: &GpuTexture,
        moments_read: &GpuTexture,
        src_irr: &GpuTexture,
        dst_irr: &GpuTexture,
        label: &str,
    ) {
        // Each dispatch owns a parameter snapshot; later passes may use a different step.
        let groups = dispatch_groups_2d(params.size, SHADOW_WORKGROUP);
        encoder.dispatch_compute(
            &self.atrous_post_pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 1,
                    data: atrous_post_params_bytes(params),
                },
                GpuBinding::Texture {
                    binding: 0,
                    texture: depth_tex,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: normal_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: moments_read,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: src_irr,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: dst_irr,
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
        // SV-ACCUM: shadow-visibility channel (caster slots 0-3).
        hi_sv: &GpuTexture,
        sv_history_read: &GpuTexture,
        sv_history_write: &GpuTexture,
        // SV-ACCUM moments: per-channel first/second visibility moments.
        sv_m1_read: &GpuTexture,
        sv_m1_write: &GpuTexture,
        sv_m2_read: &GpuTexture,
        sv_m2_write: &GpuTexture,
        // SV-ACCUM snap-hold countdown pair (`.x`).
        sv_hold_read: &GpuTexture,
        sv_hold_write: &GpuTexture,
        // RS-A (caster cap 4 -> 8): second shadow-visibility channel
        // (slots 4-7) — independent SV-ACCUM pipeline.
        hi_sv2: &GpuTexture,
        sv2_history_read: &GpuTexture,
        sv2_history_write: &GpuTexture,
        sv2_m1_read: &GpuTexture,
        sv2_m1_write: &GpuTexture,
        sv2_m2_read: &GpuTexture,
        sv2_m2_write: &GpuTexture,
        sv2_hold_read: &GpuTexture,
        sv2_hold_write: &GpuTexture,
        // RT-TL-C (section 16 TL5/TL8): sun-transmission tint channel —
        // same flip clock, same weights/reset as irradiance.
        hi_svt: &GpuTexture,
        svt_history_read: &GpuTexture,
        svt_history_write: &GpuTexture,
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
                // RT-R2 (RD6): hi_refl / refl history pair / gi_materials.
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
                // SV-ACCUM: hi_sv / sv history pair.
                GpuBinding::Texture {
                    binding: 14,
                    texture: hi_sv,
                },
                GpuBinding::Texture {
                    binding: 15,
                    texture: sv_history_read,
                },
                GpuBinding::Texture {
                    binding: 16,
                    texture: sv_history_write,
                },
                // SV-ACCUM moments (m1/m2 pairs).
                GpuBinding::Texture {
                    binding: 17,
                    texture: sv_m1_read,
                },
                GpuBinding::Texture {
                    binding: 18,
                    texture: sv_m1_write,
                },
                GpuBinding::Texture {
                    binding: 19,
                    texture: sv_m2_read,
                },
                GpuBinding::Texture {
                    binding: 20,
                    texture: sv_m2_write,
                },
                // SV-ACCUM snap-hold countdown pair.
                GpuBinding::Texture {
                    binding: 21,
                    texture: sv_hold_read,
                },
                GpuBinding::Texture {
                    binding: 22,
                    texture: sv_hold_write,
                },
                // RS-A (caster cap 4 -> 8): sv2 channel — full SV-ACCUM
                // pipeline, independent sigma-gate per quad.
                GpuBinding::Texture {
                    binding: 23,
                    texture: hi_sv2,
                },
                GpuBinding::Texture {
                    binding: 24,
                    texture: sv2_history_read,
                },
                GpuBinding::Texture {
                    binding: 25,
                    texture: sv2_history_write,
                },
                GpuBinding::Texture {
                    binding: 26,
                    texture: sv2_m1_read,
                },
                GpuBinding::Texture {
                    binding: 27,
                    texture: sv2_m1_write,
                },
                GpuBinding::Texture {
                    binding: 28,
                    texture: sv2_m2_read,
                },
                GpuBinding::Texture {
                    binding: 29,
                    texture: sv2_m2_write,
                },
                GpuBinding::Texture {
                    binding: 30,
                    texture: sv2_hold_read,
                },
                GpuBinding::Texture {
                    binding: 31,
                    texture: sv2_hold_write,
                },
                // RT-TL-C: hi_svt at [[texture(32)]] and svt history pair
                // at [[texture(33)]]/[[texture(34)]].
                GpuBinding::Texture {
                    binding: 32,
                    texture: hi_svt,
                },
                GpuBinding::Texture {
                    binding: 33,
                    texture: svt_history_read,
                },
                GpuBinding::Texture {
                    binding: 34,
                    texture: svt_history_write,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "gpu-proofs")]
    fn test_rgba_texture(device: &GpuDevice, label: &str, values: &[[f32; 4]; 81]) -> GpuTexture {
        let tex = device.create_texture(&GpuTextureDesc { width: 9, height: 9, depth: 1,
            format: GpuTextureFormat::Rgba32Float, dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
            label, mip_levels: 1 });
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes())).collect();
        device.upload_texture(&tex, &bytes);
        tex
    }

    #[cfg(feature = "gpu-proofs")]
    fn test_depth_texture(device: &GpuDevice) -> GpuTexture {
        let tex = device.create_texture(&GpuTextureDesc { width: 9, height: 9, depth: 1,
            format: GpuTextureFormat::R32Float, dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "f7f1-depth", mip_levels: 1 });
        device.upload_texture(&tex, &vec![0.5f32.to_le_bytes(); 81].into_iter().flatten().collect::<Vec<_>>());
        tex
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn bug_f7f1_atrous_passes_snapshot_parameters() {
        let device = GpuDevice::new();
        let tracer = MetalShadowRayTracer::new(&device);
        let depth = test_depth_texture(&device);
        let normal = test_rgba_texture(&device, "f7f1-normal", &[[0.0, 0.0, 1.0, -1.0]; 81]);
        let moments = test_rgba_texture(&device, "f7f1-moments", &[[0.0, 1.0, 1.0, 1.0]; 81]);
        // Bright immediate neighbors, black center and distant neighbors:
        // step=1 filters the center; step=4 leaves it black. A linear ramp
        // would be symmetric and could hide the overwritten step.
        let src = std::array::from_fn(|i| {
            let x = i % 9;
            let y = i / 9;
            let v = if (3..=5).contains(&x) && (3..=5).contains(&y) && i != 40 { 1.0 } else { 0.0 };
            [v, v, v, 1.0]
        });
        let src_tex = test_rgba_texture(&device, "f7f1-src", &src);
        let params_buffer = device.create_buffer_shared(24);
        let materials = device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64);
        materials.zero_fill();
        let read = |tex: &GpuTexture| {
            let buf = device.create_buffer_shared(9 * 9 * 16);
            let mut rb = device.create_encoder("f7f1-readback");
            rb.copy_texture_to_buffer(tex, &buf, 9, 9, 9 * 16);
            rb.try_commit_and_wait_completed().expect("denoiser readback");
            unsafe { buf.mapped_ptr().unwrap().cast::<[f32; 4]>().add(40).read_unaligned() }
        };
        for post in [false, true] {
            // Two batched outputs, then the same two passes submitted one at
            // a time as references. Each regular pass needs six output textures.
            let outputs: [[GpuTexture; 6]; 4] = std::array::from_fn(|_| {
                std::array::from_fn(|_| test_rgba_texture(&device, "f7f1-output", &[[0.0; 4]; 81]))
            });
            let dispatch = |enc: &mut GpuEncoder, step: u32, out: &[GpuTexture; 6]| {
                if post {
                    tracer.atrous_post_pass(enc, &AtrousPostParams::new([9, 9], step, 1.0),
                        &params_buffer, &depth, &normal, &moments, &src_tex, &out[2], "f7f1-post");
                } else {
                    tracer.atrous_pass(enc, &AtrousParams::new([9, 9], step, true, 0),
                        &params_buffer, &materials, &depth, &moments,
                        &src_tex, &out[0], &src_tex, &out[1], &src_tex, &out[2],
                        &normal, &out[3], &src_tex, &out[4], &src_tex, &out[5], "f7f1-atrous");
                }
            };
            let mut enc = device.create_encoder("f7f1-batched");
            dispatch(&mut enc, 1, &outputs[0]);
            dispatch(&mut enc, 4, &outputs[1]);
            enc.try_commit_and_wait_completed().expect("batched denoiser passes");
            for (i, step) in [1, 4].into_iter().enumerate() {
                let mut enc = device.create_encoder("f7f1-individual-reference");
                dispatch(&mut enc, step, &outputs[i + 2]);
                enc.try_commit_and_wait_completed().expect("reference denoiser pass");
            }
            let values: [[f32; 4]; 4] = std::array::from_fn(|i| read(&outputs[i][2]));
            assert!((values[2][0] - values[3][0]).abs() > 0.05, "fixture does not distinguish steps: post={post} {values:?}");
            for i in 0..2 {
                for c in 0..4 {
                    assert!(values[i][c].is_finite());
                    assert!((values[i][c] - values[i + 2][c]).abs() < 1e-5,
                        "batched parameters changed: post={post} pass={i} channel={c} {values:?}");
                }
            }
        }
    }

    #[test]
    fn wired_instance_source_address_contract() {
        assert!(validate_instance_source_address(0, None).is_ok());
        assert!(validate_instance_source_address(0, Some(7)).is_ok());
        assert_eq!(validate_instance_source_address(7, None),
            Err("RT wired instance source buffer is missing"));
        assert_eq!(validate_instance_source_address(7, Some(8)),
            Err("RT instance address does not match its current source buffer"));
        assert!(validate_instance_source_address(7, Some(7)).is_ok());

        // Equal-capacity replacement is valid only when the current source's
        // identity is the address carried by the object; stale identity fails.
        let old = 0x1000;
        let replacement = 0x2000;
        assert!(validate_instance_source_address(replacement, Some(replacement)).is_ok());
        assert!(validate_instance_source_address(old, Some(replacement)).is_err());

        // Multiple objects may share one source buffer; each declaration is
        // independently valid and the dispatch iterator may contain duplicates.
        for address in [replacement, replacement] {
            assert!(validate_instance_source_address(address, Some(replacement)).is_ok());
        }
    }

    #[test]
    fn trace_specialization_selection_preserves_params() {
        // ShadowRayParams is repr(C) numeric POD; zero is valid for every field.
        let mut params: ShadowRayParams = unsafe { std::mem::zeroed() };
        for mask in 0u32..16 {
            params.shadow_spp = if mask & 1 != 0 { 8 } else { 0 };
            params.ao_spp = if mask & 2 != 0 { 16 } else { 0 };
            params.gi_spp = if mask & 4 != 0 { 16 } else { 0 };
            params.refl_spp = if mask & 8 != 0 { 32 } else { 0 };
            let before = bytemuck_bytes(&params).to_vec();
            let actual: Vec<_> = TracePass::ALL.into_iter().filter(|p| p.enabled(&params)).collect();
            let expected: Vec<_> = [
                (mask & 1 != 0).then_some(TracePass::Shadow),
                (mask & 6 != 0).then_some(TracePass::Diffuse),
                (mask & 8 != 0).then_some(TracePass::Reflection),
            ].into_iter().flatten().collect();
            assert_eq!(actual, expected);
            assert_eq!(bytemuck_bytes(&params), before);
        }
    }

    #[test]
    fn trace_specialization_constants_and_region_abi() {
        assert_eq!(TRACE_TRANSLUCENCY_CONSTANT_INDEX, 100);
        assert_eq!(TRACE_PASS_CONSTANT_INDEX, 101);
        assert_eq!(MAX_RT_REFLECTION_SPP, 32);
        let mut labels = std::collections::HashSet::new();
        for (index, pass) in TracePass::ALL.into_iter().enumerate() {
            assert_eq!(pass as usize, index);
            for translucent in [false, true] { assert!(labels.insert(pass.pipeline_label(translucent))); }
        }
        assert_eq!(labels.len(), 6);
        let region = TraceRegion { origin: [3, 5], extent: [7, 11] };
        let expected: Vec<_> = [3u32, 5, 7, 11].into_iter().flat_map(u32::to_ne_bytes).collect();
        assert_eq!(trace_region_bytes(&region), expected);
        assert_eq!(std::mem::offset_of!(TraceRegion, extent), 8);
    }

    #[test]
    fn trace_specialization_scheduler_keeps_parent_boundaries() {
        let source = include_str!("raytrace.rs");
        let implementation = source.split_once("impl ShadowRayTracer for MetalShadowRayTracer").unwrap().1;
        let dispatch = msl_block(implementation, "fn dispatch_shadow_rays(");
        let regions = msl_block(dispatch, "while let Some(region) = regions.next()");
        let passes = msl_block(regions, "for pass in TracePass::ALL");
        assert!(passes.contains("pass.enabled(params)"));
        assert!(passes.contains("Some((8, trace_region_bytes(&region)))"));
        assert!(!passes.contains("commit_and_continue"));
        assert!(msl_block(regions, "if regions.peek().is_some()").contains("commit_and_continue(device)"));
        assert_eq!(dispatch.matches("params_buffer.upload").count(), 1);
        assert_eq!(dispatch.matches("addCompletedHandler").count(), 1);
        assert!(dispatch.find("addCompletedHandler").unwrap() > dispatch.find("commit_and_continue(device)").unwrap());
        assert!(!dispatch.contains("None,\n            groups"));
    }

    fn msl_block<'a>(source: &'a str, marker: &str) -> &'a str {
        let tail = source.split_once(marker).expect(marker).1;
        let start = tail.find('{').expect("opening brace");
        let mut depth = 0;
        for (index, byte) in tail.bytes().enumerate().skip(start) {
            match byte {
                b'{' => depth += 1,
                b'}' => { depth -= 1; if depth == 0 { return &tail[start + 1..index]; } }
                _ => {}
            }
        }
        panic!("unclosed MSL block {marker}");
    }

    // Source contracts verify ownership and gates, not generated machine code.
    #[test]
    fn trace_specialization_source_ownership_and_global_pixels() {
        let kernel = msl_block(SHADOW_RAYS_MSL, "kernel void trace_shadow_rays(");
        for declaration in [
            "constant bool HAS_TRANSLUCENCY [[function_constant(100)]];",
            "constant uint TRACE_PASS [[function_constant(101)]];",
            "constant uint TRACE_SHADOW = 0u;", "constant uint TRACE_DIFFUSE = 1u;",
            "constant uint TRACE_REFLECTION = 2u;", "#define MAX_RT_REFL_SPP 32u",
        ] { assert!(SHADOW_RAYS_MSL.contains(declaration)); }
        assert!(kernel.contains("uint2 tid = trace_region.xy + local_tid;"));
        assert!(kernel.contains("local_tid.x >= trace_region.z || local_tid.y >= trace_region.w"));
        assert!(kernel.contains("bool owns_normal = do_diffuse ||"));
        assert!(kernel.contains("(do_reflection && p.ao_spp == 0u && p.gi_spp == 0u)"));
        assert!(kernel.contains("bool clears_reflection = do_diffuse && p.refl_spp == 0u;"));
        let void = msl_block(kernel, "if (!valid)");
        assert!(msl_block(void, "if (do_shadow").contains("out_svt.write"));
        assert!(msl_block(void, "if (do_diffuse").contains("out_irr.write"));
        assert!(msl_block(void, "if (do_reflection || clears_reflection)").contains("-1.0"));
        assert_eq!(kernel.matches("if (owns_normal)").count(), 2);
        assert!(msl_block(kernel, "if (do_diffuse || do_reflection)").contains("primary_q.reset"));
        assert!(msl_block(kernel, "if (do_diffuse && p.ao_spp").contains("ao_q.reset"));
        let gi = msl_block(kernel, "if (do_diffuse && p.gi_spp");
        assert!(gi.contains("gi_q.reset") && gi.contains("em_q.reset"));
        let reflection = msl_block(kernel, "if (do_reflection)");
        assert!(reflection.contains("refl_q.reset") && reflection.contains("uint rspp = p.refl_spp;"));
        assert!(reflection.contains("out_refl.write(float4(0, 0, 0, -1.0), tid);"));
        for forbidden in ["out_n.write", "out_irr.write", "out_sv.write"] { assert!(!reflection.contains(forbidden)); }
        assert!(msl_block(kernel, "else if (clears_reflection)").contains("out_refl.write"));
        for forbidden in ["runtime_pass", "active_pass", "fused_lighting", "[[buffer(9)]]"] {
            assert!(!SHADOW_RAYS_MSL.contains(forbidden));
        }
    }

    fn topo(id: usize, slots: u32, wired: bool) -> RtGeometryTopology {
        RtGeometryTopology { vertex: id, vertex_offset: 4, vertex_stride: 32, triangle_count: 3, index: Some(id + 100), normal_offset: 12, uv_offset: 24, instance_slots: slots, wired, alpha_mask: false }
    }

    #[test]
    fn topology_records_cover_order_layout_slots_and_global_wiring() {
        let resident = [topo(1, 1, false), topo(2, 3, false)];
        assert!(check_topology_records(&resident, false, 4, resident.into_iter()).is_ok());
        assert!(check_topology_records(&resident, false, 4, [topo(2, 3, false), topo(1, 1, false)].into_iter()).is_err());
        assert!(check_topology_records(&resident, false, 4, [topo(1, 2, false), topo(2, 2, false)].into_iter()).is_err());
        assert!(check_topology_records(&resident, false, 4, [RtGeometryTopology { alpha_mask: true, ..resident[0] }, resident[1]].into_iter()).is_err());
        let mixed = [topo(1, 2, true), topo(2, 1, false)];
        assert!(check_topology_records(&mixed, true, 3, mixed.into_iter()).is_ok());
        assert!(check_topology_records(&mixed, false, 3, mixed.into_iter()).is_err());
        assert!(check_topology_records(&[topo(1, u32::MAX, false)], false, u32::MAX, [topo(1, u32::MAX, false), topo(2, 1, false)].into_iter()).is_err());
    }

    use super::blas_geometry_opaque;
    use super::{GpuDevice, MetalShadowRayTracer, SHADOW_RAYS_MSL};
    use manifold_foundation::cold_touch::{ColdTouchKind, cold_touch_count};

    /// Executes the production normal-frame helper under production MSL options.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn normal_map_degenerate_frame_stays_finite() {
        use super::*;
        let device = GpuDevice::new();
        let source = format!("{}\n{}", SHADOW_RAYS_MSL, r#"
        kernel void normal_guard_probe(device const float4* input [[buffer(0)]],
            device float4* output [[buffer(1)]], uint2 tid [[thread_position_in_grid]]) {
            if (tid.y != 0 || tid.x >= 8) return;
            uint base = tid.x * 4;
            output[tid.x] = float4(rt_perturb_frame(input[base].xyz,
                input[base+1].xyz, input[base+2].xyz, input[base+3].xyz), 1);
        }
        "#);
        let opts = MTLCompileOptions::init(MTLCompileOptions::alloc());
        opts.setLanguageVersion(MTLLanguageVersion::Version3_1);
        let library = device.raw_device()
            .newLibraryWithSource_options_error(&NSString::from_str(&source), Some(&opts))
            .expect("normal guard MSL compile");
        let pipeline = compile_pipeline(&device, &library, "normal_guard_probe",
            identity_slot_map(&[(0, SlotKind::Buffer), (1, SlotKind::Buffer)]));
        let n = [0.0_f32, 0.0, 1.0, 0.0];
        let t = [1.0, 0.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0, 0.0];
        let mapped = [1.0, 1.0, 1.0, 0.0];
        let cases = [
            [n, t, b, mapped],
            [n, n, b, mapped], // tangent collapses after projection
            [n, t, t, mapped], // bitangent collapses after projection
            [n, [f32::INFINITY, 0.0, 0.0, 0.0], b, mapped],
            [n, [f32::NAN, 0.0, 0.0, 0.0], b, mapped],
            [n, t, b, [0.0; 4]],
            [n, t, b, [f32::NAN, 0.0, 1.0, 0.0]],
            [n, [1e-8, 0.0, 0.0, 0.0], b, mapped],
        ];
        let input = device.create_buffer_shared(std::mem::size_of_val(&cases) as u64);
        let output = device.create_buffer_shared((cases.len() * 16) as u64);
        unsafe {
            std::ptr::copy_nonoverlapping(cases.as_ptr().cast::<u8>(),
                input.mapped_ptr().expect("shared input"), std::mem::size_of_val(&cases));
        }
        let mut enc = device.create_encoder("normal guard regression");
        enc.dispatch_compute(&pipeline, &[
            GpuBinding::Buffer { binding: 0, buffer: &input, offset: 0 },
            GpuBinding::Buffer { binding: 1, buffer: &output, offset: 0 },
        ], [1, 1, 1], "normal guard regression");
        enc.try_commit_and_wait_completed().expect("normal guard dispatch");
        let actual = unsafe { std::slice::from_raw_parts(
            output.mapped_ptr().expect("shared output").cast::<[f32; 4]>(), cases.len()) };
        for (i, result) in actual.iter().enumerate() {
            let expected = if i == 0 { [1.0 / 3.0_f32.sqrt(); 3] } else { [0.0, 0.0, 1.0] };
            for channel in 0..3 {
                assert!(result[channel].is_finite(), "case {i}: {result:?}");
                assert!((result[channel] - expected[channel]).abs() < 1e-5,
                    "case {i}: {result:?}, expected {expected:?}");
            }
        }
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn trace_diagnostics_records_invalid_ray() {
        use super::*;
        let device = GpuDevice::new();
        let source = format!("{}\n{}", SHADOW_RAYS_MSL, r#"
        kernel void diag_probe(device RtTraceDiagnostics* d [[buffer(0)]],
            device float4* out [[buffer(1)]], uint2 tid [[thread_position_in_grid]]) {
            if (tid.x != 0 || tid.y != 0) return;
            ray r; r.origin=float3(0); r.direction=float3(0,1,0);
            r.min_distance=0; r.max_distance=INFINITY;
            bool valid = rt_validate_ray(r, 0, uint2(0), d);
            r.direction.x = as_type<float>(0x7fc00000u);
            rt_sanitize_ray(r, 3, uint2(17,29), d);
            out[0]=float4(r.direction, valid ? 1.0 : 0.0);
            // A later failure must not overwrite the original incident.
            r.min_distance=-1;
            rt_sanitize_ray(r, 5, uint2(99), d);
        }
        "#);
        let opts = MTLCompileOptions::init(MTLCompileOptions::alloc());
        opts.setLanguageVersion(MTLLanguageVersion::Version3_1);
        let library = device.raw_device()
            .newLibraryWithSource_options_error(&NSString::from_str(&source), Some(&opts))
            .expect("trace diagnostics MSL compile");
        let pipeline = compile_pipeline(&device, &library, "diag_probe",
            identity_slot_map(&[(0, SlotKind::Buffer), (1, SlotKind::Buffer)]));
        let record = device.create_buffer_shared(std::mem::size_of::<RtTraceDiagnostics>() as u64);
        let ptr = record.mapped_ptr().unwrap();
        unsafe {
            std::ptr::write_bytes(ptr, 0, std::mem::size_of::<RtTraceDiagnostics>());
            (*ptr.cast::<RtTraceDiagnostics>()).enabled = 1;
        }
        let out = device.create_buffer_shared(16);
        let mut enc = device.create_encoder("trace diagnostics regression");
        enc.dispatch_compute(&pipeline, &[
            GpuBinding::Buffer { binding: 0, buffer: &record, offset: 0 },
            GpuBinding::Buffer { binding: 1, buffer: &out, offset: 0 },
        ], [1,1,1], "trace diagnostics regression");
        enc.try_commit_and_wait_completed().expect("trace diagnostic dispatch");
        let d = unsafe { ptr.cast::<RtTraceDiagnostics>().read_unaligned() };
        assert_eq!(d.state, 2);
        assert_eq!(d.first_stage, 3);
        assert_eq!(d.first_pixel, 29 * 65536 + 17);
        assert!(d.raw_direction[0].is_nan());
        let result = unsafe { out.mapped_ptr().unwrap().cast::<[f32;4]>().read_unaligned() };
        assert_eq!(result, [0.0,1.0,0.0,1.0]);
    }

    /// I-TL6 (RAYTRACING_DESIGN.md section 16.5): BLAS opacity tracks
    /// translucency — the hardware fast path is kept only for objects the
    /// kernel's candidate walks never need to see.
    #[test]
    fn blas_opacity_tracks_alpha_mask_only() {
        assert!(blas_geometry_opaque(false));
        assert!(!blas_geometry_opaque(true));
        assert_eq!(blas_geometry_opaque(false), blas_geometry_opaque(false));
        assert_eq!(SHADOW_RAYS_MSL.matches("force_opacity(forced_opacity::non_opaque)").count(), 2);
    }

    #[test]
    fn retained_rt_source_contracts_are_present() {
        assert!(SHADOW_RAYS_MSL.contains("constant bool HAS_TRANSLUCENCY [[function_constant(100)]];"));
        assert_eq!(SHADOW_RAYS_MSL.matches("force_opacity(forced_opacity::non_opaque)").count(), 2);
        assert!(SHADOW_RAYS_MSL.contains("MAX_RT_REFL_SPP"));
    }

    /// COMPILE_CONTRACT_DESIGN INV2: code is device-global — a second tracer
    /// construction (the fresh-RenderScene trigger: chain rebuild/eviction)
    /// compiles nothing. Pre-hoist this failed: each `new` compiled the MSL
    /// library + 7 PSOs.
    #[test]
    fn tracer_reconstruction_compiles_nothing() {
        let device = GpuDevice::new();
        let _t1 = MetalShadowRayTracer::new(&device);
        let before = cold_touch_count(ColdTouchKind::PipelineCompile);
        let _t2 = MetalShadowRayTracer::new(&device);
        assert_eq!(
            before,
            cold_touch_count(ColdTouchKind::PipelineCompile),
            "second tracer construction compiled pipelines — PSOs must be device-global"
        );
    }
}
