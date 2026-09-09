//! Acceleration structures and topology: per-object BLAS + one instance
//! TLAS (RtAccel), the topology model and check, the BLAS/descriptor build
//! encoders, the instance buffer, completion pinning, and the per-frame
//! normal-source/material texture tables. Split out of `raytrace.rs`
//! (BUG-xmsx driver split); see that file for the module map.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSArray;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLAccelerationStructure, MTLAccelerationStructureCommandEncoder,
    MTLAccelerationStructureGeometryDescriptor,
    MTLAccelerationStructureInstanceDescriptor, MTLAccelerationStructureInstanceOptions,
    MTLAccelerationStructureTriangleGeometryDescriptor, MTLAccelerationStructureUsage,
    MTLAttributeFormat, MTLBuffer, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLComputeCommandEncoder, MTLDevice, MTLInstanceAccelerationStructureDescriptor,
    MTLIndexType, MTLPackedFloat3, MTLPackedFloat4x3,
    MTLPrimitiveAccelerationStructureDescriptor, MTLResourceUsage, MTLSize,
};

use super::super::device::GpuDevice;
use super::super::types::{GpuBuffer, GpuTexture};
use super::{
    EmissiveLightTable, GiMaterial, MAX_RT_MATERIAL_TEXTURES, RT_MATERIAL_TEX_INDEX_NONE, RtInstanceBuildObj,
    RT_INSTANCE_TRANSFORM_BYTES, RtNormalSource, SHADOW_WORKGROUP, build_emissive_table,
    effective_instance_slots, write_instance_obj_params,
};

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
        super::super::device::retire_on_queue(&self.queue, pins, "RT accel retire");
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

pub(crate) fn validate_instance_source_address(instances_addr: u64, source_address: Option<u64>) -> Result<(), &'static str> {
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
pub(crate) fn blas_geometry_opaque(alpha_mask: bool) -> bool {
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
    if super::super::gpu_fault::diagnostics_enabled() {
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
    if super::super::gpu_fault::diagnostics_enabled() {
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
                    super::super::gpu_fault::log_error_diagnostics(&err, label);
                    (err.code() as i64, err.localizedDescription().to_string())
                },
            };
            super::super::gpu_fault::record_fault(&desc);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
