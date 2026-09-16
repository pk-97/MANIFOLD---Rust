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
    MTLIndexType, MTLPackedFloat3, MTLPackedFloat4x3, MTLAccelerationStructureSizes,
    MTLPrimitiveAccelerationStructureDescriptor, MTLResourceUsage, MTLSize,
};

use super::super::device::GpuDevice;
use super::super::types::{GpuBuffer, GpuTexture};
use super::{
    EmissiveLightTable, GiMaterial, MAX_RT_MATERIAL_TEXTURES, RT_MATERIAL_TEX_INDEX_NONE, RtInstanceBuildObj,
    RT_INSTANCE_TRANSFORM_BYTES, RtNormalSource, SHADOW_WORKGROUP, build_emissive_table,
    effective_instance_slots, refit_emissive_table, write_instance_obj_params,
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

/// One object's LOCAL-space bottom-level acceleration structure. The Blas
/// retains everything a caller-ordered update needs (SCENE_MODIFIER_RT_
/// DESIGN.md §4.2): the primitive + triangle descriptors (a stable-topology
/// rebuild re-encodes into the same structure/storage), the structure
/// itself, and BOTH scratch buffers, each sized from Metal's reported
/// requirements at plan time.
pub(crate) struct Blas {
    pub(crate) structure: Retained<ProtocolObject<dyn MTLAccelerationStructure>>,
    pub(crate) descriptor: Retained<MTLPrimitiveAccelerationStructureDescriptor>,
    /// Retained per the §4.2 contract (every Blas retains its triangle
    /// descriptor); the primitive descriptor's geometry array already owns
    /// it, so nothing reads this field directly today.
    #[allow(dead_code, reason = "§4.2 retention contract; un-suppress when a rebuild/refit path reads the triangle descriptor directly")]
    pub(crate) tri: Retained<MTLAccelerationStructureTriangleGeometryDescriptor>,
    pub(crate) build_scratch: GpuBuffer,
    pub(crate) refit_scratch: GpuBuffer,
    /// False from preparation until the first successful encode — an
    /// unbuilt Blas always builds regardless of the requested change class
    /// (design §4.1).
    pub(crate) built: bool,
}

/// The resident RT scene: N per-object BLAS instanced into one TLAS via
/// `transform`. Built once (scene load / topology change — dirty-checked
/// by the caller, e.g. render_scene.rs's existing shadow-map cache-key
/// idiom); kept resident across frames (RAYTRACING_DESIGN.md P1
/// performer-gesture gate — never built mid-frame).
pub struct RtAccel {
    pub(crate) structure: Retained<ProtocolObject<dyn MTLAccelerationStructure>>,
    descriptor: Retained<MTLInstanceAccelerationStructureDescriptor>,
    /// TLAS build scratch — retained (SCENE_MODIFIER_RT_DESIGN.md §4.2:
    /// both scratch kinds are resident, sized from Metal's requirements).
    build_scratch: GpuBuffer,
    refit_scratch: GpuBuffer,
    /// True from preparation/replacement until the first encode builds the
    /// TLAS — membership/order/capacity changes are a TLAS build, not a
    /// refit (§4.2 dirty rule 1).
    pub(crate) tlas_needs_build: bool,
    /// Immutable resident-handle set packaged at preparation/replacement
    /// (§4.3): completion callbacks clone this existing `Arc` — no new
    /// mutex, no per-frame pin assembly. Readiness belongs to the resource
    /// set: a superseded callback flips only ITS set's flag.
    pub(crate) pins: Arc<AccelPins>,
    /// Kept alive: the TLAS descriptor's `instancedAccelerationStructures`
    /// array holds retained references to each BLAS regardless, but owning
    /// them here too makes a future per-BLAS refit (deforming mesh) a
    /// simple field access instead of an NSArray walk. pub(crate):
    /// encoder.rs's dispatch useResource coverage (BUG-jddy arm 5).
    pub(crate) blas: Vec<Blas>,
    /// CPU-writable instance-descriptor buffer (transform per object).
    /// Retained here so `encode_accel_update` can rewrite transforms in
    /// place on a transform/mask-only update.
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
    /// BUG-308/RT-D4: accel updates are async — encoded on the caller's
    /// command buffer (`encode_accel_update`), never committed or waited
    /// here — set `true` by that buffer's completion handler once the GPU
    /// has actually finished building/refitting. `render_scene.rs`
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

/// One object's geometry + world transform for [`ShadowRayTracer::plan_accel`]/
/// `prepare_accel`/`encode_accel_update`. `transform` is manifold's own
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
/// structure encoder (BUG-308/RT-D4 — see `encode_accel_update`'s doc
/// comment for the no-stall history).
/// why this is no longer its own command buffer). Returns the built
/// `Blas` handle (valid to reference immediately — Metal resolves the
/// GPU-side build asynchronously) plus the scratch buffer, which the
/// caller must keep alive until the ENCLOSING command buffer's completion
/// handler fires (the GPU reads it for the duration of the build).
/// Build one object's BLAS descriptors (triangle + primitive) from its
/// geometry — pure descriptor construction, no allocation, no encoding.
/// Shared by plan-time sizing and encode-time builds (SCENE_MODIFIER_RT_
/// DESIGN.md §4.1: plan and encode must describe the SAME structure).
fn blas_descriptors(
    obj: &RtObjectGeometry,
) -> (
    Retained<MTLAccelerationStructureTriangleGeometryDescriptor>,
    Retained<MTLPrimitiveAccelerationStructureDescriptor>,
) {
    if std::env::var("MANIFOLD_PROBE_RT_ACCEL").is_ok() {
        eprintln!("MANIFOLD_PROBE_RT_ACCEL: blas_descriptors triangle_count={}, vertex_buffer_size={}, vertex_stride={}, vertex_offset={}",
            obj.triangle_count, obj.vertex_buffer.size(), obj.vertex_stride, obj.vertex_offset);
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
    let geom: Retained<MTLAccelerationStructureGeometryDescriptor> = tri_desc.clone().into_super();
    let array = NSArray::from_retained_slice(&[geom]);
    let descriptor = MTLPrimitiveAccelerationStructureDescriptor::descriptor();
    descriptor.setGeometryDescriptors(Some(&array));
    descriptor.setUsage(MTLAccelerationStructureUsage::Refit);
    (tri_desc, descriptor)
}

/// Metal's reported sizes for one BLAS descriptor set.
fn blas_sizes(device: &GpuDevice, descriptor: &MTLPrimitiveAccelerationStructureDescriptor) -> MTLAccelerationStructureSizes {
    let sizes = device.raw_device().accelerationStructureSizesWithDescriptor(descriptor);
    if crate::metal::device::alloc_log_enabled() {
        eprintln!(
            "[gpu-alloc] blas-plan struct={} build_scratch={} refit_scratch={}",
            sizes.accelerationStructureSize, sizes.buildScratchBufferSize, sizes.refitScratchBufferSize
        );
        crate::metal::device::alloc_log_backtrace();
    }
    sizes
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
/// no CPU readback). Called by `encode_accel_update` ahead of the TLAS
/// build/refit on the same command buffer.
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

// ─── P3: caller-ordered acceleration (SCENE_MODIFIER_RT_DESIGN.md §4) ─────
//
// The accel lifecycle is three explicit steps on the CALLER's command
// stream: plan (CPU-only sizing) → prepare (allocate/reuse, no GPU
// commands) → encode (ordered BLAS/TLAS/emissive work on the caller's
// `GpuEncoder`). This replaces the independent `build_accel`/`refit_accel`
// pair that committed its own command buffer mid-frame.

/// Per-object geometry change class for [`encode_accel_update`] — the
/// caller's dirty decision from its revision metadata (§4.1). Until P6,
/// `Refit` intentionally executes the rebuild branch and reports
/// `blas_builds`; P6 replaces that branch, not the caller policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtGeometryChange {
    /// No geometry or descriptor change — no BLAS work.
    Reuse,
    /// Normals/UVs/appearance-only change — tables/history refresh, no
    /// BLAS work while the descriptor opacity class holds.
    Attributes,
    /// Positions moved, topology stable. Executes as a rebuild until P6.
    Refit,
    /// Topology/cut/alpha-class change — rebuild the affected BLAS.
    Rebuild,
}

/// What one [`encode_accel_update`] call actually encoded (§4.1) — the
/// acceptance counters (`rt_dynamic_selective_updates_and_idle`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtAccelUpdate {
    pub blas_builds: u32,
    pub blas_refits: u32,
    pub tlas_builds: u32,
    pub tlas_refits: u32,
    pub emissive_refreshes: u32,
}

/// Structured accel lifecycle failure (§4.1) — no partial publication:
/// every variant is returned BEFORE any GPU work is encoded.
#[derive(Debug)]
pub enum RtAccelError {
    InvalidGeometry { object: usize, reason: String },
    /// The resident set cannot express the requested change — the caller
    /// must run plan/prepare first (object count, capacity, or a Rebuild
    /// whose new shape exceeds the resident storage).
    NeedsPreparation,
    Allocation { bytes: u64, resource: &'static str },
    Encode(&'static str),
}

/// Immutable resident-handle pin set for one prepared `RtAccel` resource
/// generation (§4.3): packaged once at preparation/replacement, cloned by
/// completion callbacks — no per-frame pin assembly, no mutex. Fields are
/// never read on the CPU; the set exists to be HELD until the consuming
/// command buffer completes (drop releases the pins).
#[allow(dead_code, reason = "fields are held for completion-pin lifetime only; un-suppress when a debug inspector or validator reads them")]
pub(crate) struct AccelPins {
    tlas: Retained<ProtocolObject<dyn MTLAccelerationStructure>>,
    blas: Vec<Retained<ProtocolObject<dyn MTLAccelerationStructure>>>,
    blas_scratch: Vec<Retained<ProtocolObject<dyn MTLBuffer>>>,
    tlas_build_scratch: Retained<ProtocolObject<dyn MTLBuffer>>,
    tlas_refit_scratch: Retained<ProtocolObject<dyn MTLBuffer>>,
    instance_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    instance_obj_params: Option<Retained<ProtocolObject<dyn MTLBuffer>>>,
    geometry_buffers: Vec<Retained<ProtocolObject<dyn MTLBuffer>>>,
}
/// Send/Sync: pinned handles are HELD, never touched, until the completion
/// handler drops them on a Metal-owned callback thread; Metal retain/release
/// is thread-safe — the same nominal gap as `CompletionPins`.
unsafe impl Send for AccelPins {}
unsafe impl Sync for AccelPins {}

/// One object's planned BLAS: descriptors + Metal-reported sizes. The
/// descriptors are retained so prepare allocates against EXACTLY what plan
/// sized and encode builds against the same descriptor set.
struct BlasPlan {
    tri: Retained<MTLAccelerationStructureTriangleGeometryDescriptor>,
    descriptor: Retained<MTLPrimitiveAccelerationStructureDescriptor>,
    struct_bytes: u64,
    build_scratch: u64,
    refit_scratch: u64,
}

/// CPU-only sizing result for one scene's acceleration update (§4.1).
/// Owns the prepared descriptors and sizes; `additional_peak_bytes` is the
/// admission number the renderer hands to `admit_candidate_bytes` before
/// [`prepare_accel`]. Never inspects vertex contents.
pub struct RtAccelPlan {
    blas: Vec<BlasPlan>,
    tlas_descriptor: Retained<MTLInstanceAccelerationStructureDescriptor>,
    tlas_struct_bytes: u64,
    tlas_build_scratch: u64,
    tlas_refit_scratch: u64,
    topology: Vec<RtGeometryTopology>,
    instanced: bool,
    total_slots: u32,
    geometry_buffers: Vec<Retained<ProtocolObject<dyn MTLBuffer>>>,
    additional_peak: u64,
}

impl RtAccelPlan {
    /// Bytes prepare will allocate BEYOND what the resident set already
    /// holds — BLAS/TLAS storage, both scratch kinds, descriptor/instance
    /// tables, emissive workspaces, lifetime pins. A full structural
    /// replacement charges the overlap (old set stays live until the swap).
    pub fn additional_peak_bytes(&self) -> u64 {
        self.additional_peak
    }
}

/// Validate one object's geometry record — plan-time structural checks
/// only, never vertex contents (§4.1). The errors here used to be log-only
/// diagnostics (`[RT-DIAG]`); the caller-ordered seam makes them hard
/// preparation failures instead of encoding known-bad descriptors.
fn validate_object_geometry(index: usize, o: &RtObjectGeometry) -> Result<(), RtAccelError> {
    if o.triangle_count == 0 {
        return Err(RtAccelError::InvalidGeometry { object: index, reason: "zero triangles".into() });
    }
    if o.vertex_stride < 12 {
        return Err(RtAccelError::InvalidGeometry { object: index, reason: format!("vertex stride {} < 12 (float3 position)", o.vertex_stride) });
    }
    match o.index_buffer {
        None => {
            let needed = u64::from(o.triangle_count)
                .checked_mul(3)
                .and_then(|v| v.checked_mul(u64::from(o.vertex_stride)))
                .and_then(|v| v.checked_add(u64::from(o.vertex_offset)))
                .ok_or_else(|| RtAccelError::InvalidGeometry { object: index, reason: "flat vertex byte count overflows".into() })?;
            if needed > o.vertex_buffer.size() {
                return Err(RtAccelError::InvalidGeometry { object: index, reason: format!("flat geometry needs {needed} bytes, buffer has {}", o.vertex_buffer.size()) });
            }
        }
        Some(ib) => {
            let needed = u64::from(o.triangle_count)
                .checked_mul(3)
                .and_then(|v| v.checked_mul(4))
                .ok_or_else(|| RtAccelError::InvalidGeometry { object: index, reason: "index byte count overflows".into() })?;
            if needed > ib.size() {
                return Err(RtAccelError::InvalidGeometry { object: index, reason: format!("index read needs {needed} bytes, buffer has {}", ib.size()) });
            }
        }
    }
    Ok(())
}

/// The worst-case emissive table charge (§4.1): the 4096-entry cap over
/// the existing 80-byte triangle + 8-byte alias records, plus the CPU
/// local-space vertex mirror (3 × float3 per entry). P4a's GPU candidate/
/// sort workspaces refine this from actual triangle counts; P3 charges the
/// cap unconditionally so admission never under-counts a scene that gains
/// emission without a topology edit (§5.1's zero→positive requirement).
fn emissive_peak_bytes() -> u64 {
    const TRI: u64 = 80;
    const ALIAS: u64 = 8;
    const CPU_VERTS: u64 = 3 * 12;
    u64::from(super::emissive::MAX_RT_EMISSIVE_TRIANGLES) * (TRI + ALIAS + CPU_VERTS)
}

/// Storage-compatibility for a stable-shape rebuild (§4.2): structure and
/// scratch are reusable when the size-bearing fields match — buffer
/// IDENTITY may move (the descriptor is retargeted), counts/strides/
/// layout/opacity class may not. `vertex`/`index` identity keys are
/// deliberately absent from this comparison.
fn blas_storage_compatible(resident: &RtGeometryTopology, current: &RtGeometryTopology) -> bool {
    resident.vertex_offset == current.vertex_offset
        && resident.vertex_stride == current.vertex_stride
        && resident.triangle_count == current.triangle_count
        && resident.index.is_some() == current.index.is_some()
        && resident.normal_offset == current.normal_offset
        && resident.uv_offset == current.uv_offset
        && resident.instance_slots == current.instance_slots
        && resident.wired == current.wired
        && resident.alpha_mask == current.alpha_mask
}

/// The tracer's cached TLAS-sizing probe: a 1-triangle BLAS STRUCTURE
/// (allocated, never built — sizing needs handles, not contents, so no GPU
/// command is ever encoded for it). plan_accel replicates this handle to
/// fill the TLAS query descriptor's instance array; Metal's sizes depend
/// on instance count and usage flags, not BLAS identity.
///
/// Send/Sync: the handle is only ever READ (cloned into a sizing
/// descriptor's array) from the content thread; Metal retain/release is
/// thread-safe, so the marker gap is nominal — same discipline as
/// `CompletionPins`.
pub(crate) struct ProbeBlas(
    #[allow(dead_code, reason = "the vertex buffer backs the probe descriptor's address; never read directly")]
    pub(crate) GpuBuffer,
    pub(crate) Retained<ProtocolObject<dyn MTLAccelerationStructure>>,
);
unsafe impl Send for ProbeBlas {}
unsafe impl Sync for ProbeBlas {}

pub(crate) fn tlas_probe_structure(
    device: &GpuDevice,
) -> ProbeBlas {
    let verts = device.create_buffer(3 * 12);
    let obj = RtObjectGeometry {
        vertex_buffer: &verts,
        vertex_stride: 12,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: 1,
        transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]],
        normal_offset: 0,
        uv_offset: 0,
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
        instance_slots: 0,
    };
    let (_tri, descriptor) = blas_descriptors(&obj);
    let sizes = blas_sizes(device, &descriptor);
    let structure = device
        .raw_device()
        .newAccelerationStructureWithSize(sizes.accelerationStructureSize)
        .expect("TLAS probe structure allocation failed");
    ProbeBlas(verts, structure)
}

/// CPU-only sizing for one scene's acceleration state (§4.1). Builds the
/// descriptors Metal will see, queries its reported sizes, and accounts
/// every byte prepare would allocate. No allocation, no GPU command, no
/// vertex-content reads.
///
/// TLAS sizing needs a non-empty `instancedAccelerationStructures` array
/// on the query descriptor; `tlas_probe` supplies a throwaway structure
/// handle to replicate (sizes depend on instance COUNT and usage flags,
/// not BLAS identity — the rt_dynamic_ordering gate asserts planned bytes
/// match prepared reality). Callers without a resident set pass the
/// tracer's cached probe; see the trait impl.
pub(crate) fn plan_accel(
    device: &GpuDevice,
    resident: Option<&RtAccel>,
    objects: &[RtObjectGeometry],
    tlas_probe: &Retained<ProtocolObject<dyn MTLAccelerationStructure>>,
) -> Result<RtAccelPlan, RtAccelError> {
    for (i, o) in objects.iter().enumerate() {
        validate_object_geometry(i, o)?;
    }
    let topology: Vec<_> = objects.iter().map(RtGeometryTopology::from_geometry).collect();
    let instanced = objects.iter().any(|o| o.instances_addr != 0);
    let total_slots: u32 = topology.iter().map(|t| t.instance_slots).sum::<u32>().max(1);

    let mut blas = Vec::with_capacity(objects.len());
    for o in objects {
        let (tri, descriptor) = blas_descriptors(o);
        let sizes = blas_sizes(device, &descriptor);
        blas.push(BlasPlan {
            tri,
            descriptor,
            struct_bytes: sizes.accelerationStructureSize as u64,
            build_scratch: sizes.buildScratchBufferSize.max(16) as u64,
            refit_scratch: sizes.refitScratchBufferSize.max(16) as u64,
        });
    }

    let tlas_descriptor = MTLInstanceAccelerationStructureDescriptor::descriptor();
    tlas_descriptor.setInstanceCount(total_slots as usize);
    let probe_array = NSArray::from_retained_slice(&vec![
        tlas_probe.clone();
        total_slots as usize
    ]);
    tlas_descriptor.setInstancedAccelerationStructures(Some(&probe_array));
    tlas_descriptor.setUsage(MTLAccelerationStructureUsage::Refit);
    let tlas_sizes = device.raw_device().accelerationStructureSizesWithDescriptor(&tlas_descriptor);
    if crate::metal::device::alloc_log_enabled() {
        eprintln!(
            "[gpu-alloc] tlas-plan instances={} struct={} build_scratch={} refit_scratch={}",
            total_slots, tlas_sizes.accelerationStructureSize,
            tlas_sizes.buildScratchBufferSize, tlas_sizes.refitScratchBufferSize
        );
    }

    let geometry_buffers: Vec<Retained<ProtocolObject<dyn MTLBuffer>>> = objects
        .iter()
        .flat_map(|o| {
            let mut v = vec![o.vertex_buffer.raw.clone()];
            if let Some(ib) = o.index_buffer {
                v.push(ib.raw.clone());
            }
            v
        })
        .collect();

    // Reuse accounting: a resident set covers its bytes only when the plan
    // is byte-identical in topology INCLUDING buffer identity — an identity
    // move is a full replacement (the pin set and descriptors reference the
    // old buffers; §4.3 pins are packaged at preparation/replacement only).
    let reusable = resident.is_some_and(|acc| {
        acc.instanced == instanced
            && acc.instance_slot_total == total_slots
            && acc.topology == topology
    });

    let descriptor_bytes = u64::from(total_slots) * std::mem::size_of::<MTLAccelerationStructureInstanceDescriptor>() as u64;
    let obj_params_bytes = if instanced {
        objects.len() as u64 * std::mem::size_of::<RtInstanceBuildObj>() as u64
    } else {
        0
    };
    let additional_peak = if reusable {
        0
    } else {
        blas.iter().map(|b| b.struct_bytes + b.build_scratch + b.refit_scratch).sum::<u64>()
            + tlas_sizes.accelerationStructureSize as u64
            + (tlas_sizes.buildScratchBufferSize.max(16)) as u64
            + (tlas_sizes.refitScratchBufferSize.max(16)) as u64
            + descriptor_bytes
            + obj_params_bytes
            + emissive_peak_bytes()
    };

    Ok(RtAccelPlan {
        blas,
        tlas_descriptor,
        tlas_struct_bytes: tlas_sizes.accelerationStructureSize as u64,
        tlas_build_scratch: tlas_sizes.buildScratchBufferSize.max(16) as u64,
        tlas_refit_scratch: tlas_sizes.refitScratchBufferSize.max(16) as u64,
        topology,
        instanced,
        total_slots,
        geometry_buffers,
        additional_peak,
    })
}

/// Allocate or reuse capacity for a plan (§4.1) — never commits a GPU
/// command. A storage-compatible resident set is kept as-is; anything
/// else is prepared as a fresh `RtAccel` and swapped atomically, so an
/// allocation failure leaves the old resident untouched and valid
/// (`rt_dynamic_admission_is_atomic`). The replaced set self-retires
/// through `RtAccel::drop` → `retire_on_queue`.
pub(crate) fn prepare_accel(
    device: &GpuDevice,
    resident: &mut Option<RtAccel>,
    plan: RtAccelPlan,
) -> Result<(), RtAccelError> {
    if let Some(acc) = resident.as_ref() {
        // Full topology equality INCLUDING buffer identity — anything less
        // is a replacement (pins and descriptors reference the resident
        // buffers; §4.3 packages pins at preparation/replacement only).
        let reusable = acc.instanced == plan.instanced
            && acc.instance_slot_total == plan.total_slots
            && acc.topology == plan.topology;
        if reusable {
            return Ok(());
        }
    }

    let raw_device = device.raw_device();
    let mut blas_out = Vec::with_capacity(plan.blas.len());
    for b in &plan.blas {
        let structure = raw_device
            .newAccelerationStructureWithSize(b.struct_bytes as usize)
            .ok_or(RtAccelError::Allocation { bytes: b.struct_bytes, resource: "BLAS structure" })?;
        blas_out.push(Blas {
            structure,
            descriptor: b.descriptor.clone(),
            tri: b.tri.clone(),
            build_scratch: device.create_buffer(b.build_scratch),
            refit_scratch: device.create_buffer(b.refit_scratch),
            built: false,
        });
    }
    let tlas_structure = raw_device
        .newAccelerationStructureWithSize(plan.tlas_struct_bytes as usize)
        .ok_or(RtAccelError::Allocation { bytes: plan.tlas_struct_bytes, resource: "TLAS structure" })?;
    let tlas_build_scratch = device.create_buffer(plan.tlas_build_scratch);
    let tlas_refit_scratch = device.create_buffer(plan.tlas_refit_scratch);

    let instance_buffer = if plan.instanced {
        device.create_buffer(
            u64::from(plan.total_slots) * std::mem::size_of::<MTLAccelerationStructureInstanceDescriptor>() as u64,
        )
    } else {
        // Non-instanced fast path: CPU-mapped descriptors (existing D7
        // behavior — write_instance/transforms ride the mapped buffer).
        build_instance_buffer_from_plan(device, &plan)
    };
    let instance_obj_params = if plan.instanced {
        Some(device.create_buffer_shared(
            (plan.topology.len() * std::mem::size_of::<RtInstanceBuildObj>()) as u64,
        ))
    } else {
        None
    };

    let pins = Arc::new(AccelPins {
        tlas: tlas_structure.clone(),
        blas: blas_out.iter().map(|b| b.structure.clone()).collect(),
        blas_scratch: blas_out
            .iter()
            .flat_map(|b| [b.build_scratch.raw.clone(), b.refit_scratch.raw.clone()])
            .collect(),
        tlas_build_scratch: tlas_build_scratch.raw.clone(),
        tlas_refit_scratch: tlas_refit_scratch.raw.clone(),
        instance_buffer: instance_buffer.raw.clone(),
        instance_obj_params: instance_obj_params.as_ref().map(|b| b.raw.clone()),
        geometry_buffers: plan.geometry_buffers.clone(),
    });

    *resident = Some(RtAccel {
        structure: tlas_structure,
        descriptor: plan.tlas_descriptor.clone(),
        build_scratch: tlas_build_scratch,
        refit_scratch: tlas_refit_scratch,
        tlas_needs_build: true,
        pins,
        blas: blas_out,
        instance_buffer,
        instanced: plan.instanced,
        instance_slot_total: plan.total_slots,
        topology: plan.topology,
        instance_obj_params,
        geometry_buffers: plan.geometry_buffers,
        ready: Arc::new(AtomicBool::new(false)),
        emissive_table: None,
        queue: device.clone_queue(),
    });
    Ok(())
}

/// Non-instanced instance-buffer allocation from a plan — zeroed identity
/// descriptors; encode rewrites transforms/masks from the current objects
/// every update (same content as the old `build_instance_buffer`).
fn build_instance_buffer_from_plan(device: &GpuDevice, plan: &RtAccelPlan) -> GpuBuffer {
    let stride = std::mem::size_of::<MTLAccelerationStructureInstanceDescriptor>();
    let buf = device.create_buffer_shared((stride * plan.topology.len().max(1)) as u64);
    let ptr = buf.mapped_ptr().expect("RT instance-descriptor buffer must be CPU-mapped");
    for i in 0..plan.topology.len() {
        let desc = MTLAccelerationStructureInstanceDescriptor {
            transformationMatrix: to_packed_4x3([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]]),
            options: MTLAccelerationStructureInstanceOptions::None,
            mask: 0,
            intersectionFunctionTableOffset: 0,
            accelerationStructureIndex: i as u32,
        };
        unsafe {
            std::ptr::write_unaligned(ptr.add(i * stride) as *mut _, desc);
        }
    }
    buf
}

/// Encode a current-frame acceleration update onto the caller's encoder
/// (§4.1/§4.2): instance descriptors → changed BLAS builds → TLAS →
/// emissive preparation, ordered after everything the caller already
/// encoded (the modifier writes this frame) and before the trace dispatch
/// the caller encodes later. No allocation, no command-buffer commit, no
/// CPU wait — a successful encode permits a later trace on the SAME
/// encoder; `ready` completion remains for warmup/diagnostics only.
///
/// Design amendment (P3 landing): `device` is threaded explicitly — §4.1's
/// sketch omits it, but the descriptor-build pipeline and the P3 emissive
/// path are device-held and neither `GpuEncoder` nor the tracer owns one.
pub(crate) fn encode_accel_update(
    device: &GpuDevice,
    encoder: &mut crate::metal::encoder::GpuEncoder,
    accel: &mut RtAccel,
    objects: &[RtObjectGeometry],
    changes: &[RtGeometryChange],
    materials: &[GiMaterial],
    instance_data_changed: bool,
    emissive_data_changed: bool,
) -> Result<RtAccelUpdate, RtAccelError> {
    // Validate EVERYTHING before encoding anything (§4.1).
    if changes.len() != objects.len() {
        return Err(RtAccelError::Encode("changes/objects length mismatch"));
    }
    if objects.len() != accel.blas.len() {
        return Err(RtAccelError::NeedsPreparation);
    }
    let mut current_topology: Vec<RtGeometryTopology> = Vec::with_capacity(objects.len());
    for (i, o) in objects.iter().enumerate() {
        validate_object_geometry(i, o)?;
        current_topology.push(RtGeometryTopology::from_geometry(o));
    }
    let slot_total: u32 = current_topology.iter().map(|t| t.instance_slots).sum();
    if accel.instanced != objects.iter().any(|o| o.instances_addr != 0)
        || slot_total != accel.instance_slot_total
    {
        return Err(RtAccelError::NeedsPreparation);
    }

    // Per-object action. An unbuilt BLAS always builds. Refit runs the
    // rebuild branch until P6 (§4.1). Rebuild reuses resident storage when
    // the shape is compatible; a shape change needs prepare first.
    enum Action { None, Build }
    let mut actions = Vec::with_capacity(objects.len());
    for (i, change) in changes.iter().enumerate() {
        let action = if !accel.blas[i].built {
            Action::Build
        } else {
            // Buffer-identity moved: the resident descriptors still reference
            // the OLD buffers, and the resident pin set holds them. Any
            // identity move needs prepare (full replacement retargets and
            // repins) — never encode against stale handles.
            let identity_moved = accel.topology[i].vertex != current_topology[i].vertex
                || accel.topology[i].index != current_topology[i].index;
            if identity_moved {
                return Err(RtAccelError::NeedsPreparation);
            }
            match change {
                RtGeometryChange::Reuse | RtGeometryChange::Attributes => Action::None,
                RtGeometryChange::Refit | RtGeometryChange::Rebuild => {
                    if blas_storage_compatible(&accel.topology[i], &current_topology[i]) {
                        Action::Build
                    } else {
                        return Err(RtAccelError::NeedsPreparation);
                    }
                }
            }
        };
        actions.push(action);
    }
    let blas_builds = actions.iter().filter(|a| matches!(a, Action::Build)).count() as u32;
    let tlas_build = accel.tlas_needs_build;
    let tlas_refit = !tlas_build && (blas_builds > 0 || instance_data_changed);
    if blas_builds == 0 && !tlas_build && !tlas_refit && !emissive_data_changed {
        return Ok(RtAccelUpdate::default());
    }

    accel.ready.store(false, Ordering::Release);
    let mut update = RtAccelUpdate::default();

    // Instance descriptors: instanced mode re-dispatches the GPU builder
    // (GPU-private buffer; the params rewrite is CPU-mapped obj params),
    // fast path rewrites the mapped descriptor buffer — both BEFORE the
    // AS encoder on the same command buffer (sequential encoders run in
    // creation order). Snapshot discipline: this CPU write happens at
    // encode time from the caller's current `objects` — §4.3.
    let cb = encoder.raw_cmd_buf();
    if accel.instanced {
        let obj_params = accel
            .instance_obj_params
            .as_ref()
            .expect("instanced accel carries instance build params");
        write_instance_obj_params(
            obj_params.mapped_ptr().expect("RT instance build-params buffer must be CPU-mapped"),
            objects,
        );
        encode_descriptor_build(device, cb, objects, &accel.instance_buffer, obj_params, accel.topology.iter().map(|t| t.instance_slots).max().unwrap_or(1));
    } else if instance_data_changed || blas_builds > 0 || tlas_build {
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
    }

    let enc = cb
        .accelerationStructureCommandEncoder()
        .expect("accelerationStructureCommandEncoder failed");
    unsafe { enc.setLabel(Some(&NSString::from_str("RT accel update"))) };

    // BUG-84fv declaration discipline: every resource the AS commands
    // reach through raw addresses is useResource-declared on this encoder
    // AND pinned through completion via the resident set's Arc.
    unsafe {
        for geo in &accel.geometry_buffers {
            let () = msg_send![&*enc, useResource: &**geo, usage: MTLResourceUsage::Read];
        }
        let () = msg_send![&*enc, useResource: accel.instance_buffer.raw(), usage: MTLResourceUsage::Read];
        let () = msg_send![&*enc, useResource: &*accel.structure, usage: MTLResourceUsage::Read | MTLResourceUsage::Write];
    }

    if blas_builds > 0 {
        for (i, action) in actions.iter().enumerate() {
            if !matches!(action, Action::Build) {
                continue;
            }
            let blas = &mut accel.blas[i];
            unsafe {
                let () = msg_send![&*enc, useResource: &*blas.structure, usage: MTLResourceUsage::Read | MTLResourceUsage::Write];
            }
            enc.buildAccelerationStructure_descriptor_scratchBuffer_scratchBufferOffset(
                &blas.structure,
                &blas.descriptor,
                blas.build_scratch.raw(),
                0,
            );
            blas.built = true;
        }
        update.blas_builds = blas_builds;
    }

    if tlas_build || tlas_refit {
        let blas_structures: Vec<Retained<ProtocolObject<dyn MTLAccelerationStructure>>> =
            accel.blas.iter().map(|b| b.structure.clone()).collect();
        unsafe {
            for b in &blas_structures {
                let () = msg_send![&*enc, useResource: &**b, usage: MTLResourceUsage::Read];
            }
        }
        if tlas_build {
            // The retained descriptor's BLAS array names the structures the
            // TLAS instances — refresh it to the resident set (a fresh
            // prepare cloned the plan's probe array).
            accel.descriptor.setInstancedAccelerationStructures(Some(&NSArray::from_retained_slice(&blas_structures)));
            accel.descriptor.setInstanceCount(accel.instance_slot_total.max(1) as usize);
            unsafe {
                accel.descriptor.setInstanceDescriptorBuffer(Some(accel.instance_buffer.raw()));
            }
            enc.buildAccelerationStructure_descriptor_scratchBuffer_scratchBufferOffset(
                &accel.structure,
                &accel.descriptor,
                accel.build_scratch.raw(),
                0,
            );
            accel.tlas_needs_build = false;
            update.tlas_builds = 1;
        } else {
            unsafe {
                enc.refitAccelerationStructure_descriptor_destination_scratchBuffer_scratchBufferOffset(
                    &accel.structure,
                    &accel.descriptor,
                    Some(&accel.structure),
                    Some(accel.refit_scratch.raw()),
                    0,
                );
            }
            update.tlas_refits = 1;
        }
    }
    enc.endEncoding();

    // Pins ride the CALLER's command buffer completion — they must survive
    // graph teardown before the caller commits (§4.3), which attaching
    // here (pre-commit) guarantees. Readiness belongs to this resource
    // set's own flag; a superseded set's callback can no longer flip the
    // live one.
    add_ready_completion_handler(
        cb,
        "RT accel update",
        Arc::clone(&accel.ready),
        CompletionPins(Arc::clone(&accel.pins)),
    );

    // Emissive preparation stays on the existing CPU-table paths until P4a
    // swaps in `encode_emissive_table`: a BLAS build or a material/emission
    // change rebuilds the table; a transform/instance-only update refits
    // its world-space positions (the old refit_accel behavior).
    if blas_builds > 0 || emissive_data_changed {
        accel.emissive_table = build_emissive_table(device, objects, materials);
        update.emissive_refreshes = 1;
    } else if let Some(ref table) = accel.emissive_table {
        refit_emissive_table(table, objects);
        update.emissive_refreshes = 1;
    }

    accel.topology = current_topology;
    Ok(update)
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
/// indirection table from the SAME `objects` slice `plan_accel`/
/// `encode_accel_update` use — same "grow, never shrink-then-reallocate every
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
