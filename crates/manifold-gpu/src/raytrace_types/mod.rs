//! Ray-tracing data shared by every backend: the kernel parameter mirrors,
//! material tables, per-object geometry records, update counters, errors
//! and debug-ray records. Each backend's `raytrace` module re-exports these
//! beside its tracer, so callers name them the same way on every backend.

use arrayvec::ArrayVec;

use crate::{GpuBuffer, GpuTexture};

mod params;
pub use params::*;
mod seam;
pub use seam::ShadowRayTracer;

/// Fixed-capacity texture list returned by the normal-source table builder.
/// The cap is part of the shader ABI, so this keeps the hot per-frame list on
/// the stack while preserving the existing bounded-table degradation rules.
pub type RtMaterialTextures<'a> = ArrayVec<&'a GpuTexture, MAX_RT_MATERIAL_TEXTURES>;

/// One object's geometry + world transform for [`ShadowRayTracer::plan_accel`]/
/// `prepare_accel`/`encode_accel_update`. `transform` is manifold's own
/// column-major `[[f32; 4]; 4]` convention (matches `render_scene.rs`'s
/// `model_matrix`) — the same layout `render_scene.wgsl`'s `Uniforms.model`
/// already uses. `vertex_buffer`/`vertex_stride`/`vertex_offset` read
/// straight from an existing interleaved vertex buffer (e.g.
/// `render_scene.rs`'s `MeshVertex`, stride 80, position at offset 0) —
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
    /// The renderer's instance space: every slot's world matrix is
    /// `transform · T_instance · instance_inner` (an unwired object's T is
    /// the identity). [`RtObjectGeometry::NO_INNER`] in Object space.
    pub instance_inner: [[f32; 4]; 4],
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
    /// no map wired renders exactly as before this feature. The product is
    /// consumed at primary and secondary reflection-hit shading sites.
    pub mr_texture: Option<&'a GpuTexture>,
    /// BUG-wytp (rt-reflections-are-normal-map-blind): this object's tangent-
    /// space normal map (glTF packing: R/G = tangent-space X/Y, B = Z),
    /// sampled at the PRIMARY hit's interpolated UV to perturb the shading
    /// normal feeding the reflection lobe's R and the AO/GI cosine-hemisphere
    /// gather. `None` degrades to the barycentric vertex normal (pre-BUG-wytp
    /// behavior). Applied at primary and secondary hit shading sites.
    pub normal_texture: Option<&'a GpuTexture>,
    /// BUG-1gqt: this object's emissive texture, sampled at the ray hit's
    /// interpolated UV (with `emissive_uv_m/t` applied) and multiplied by
    /// the flat `GiMaterial::emissive` factor — the trace-path mirror of
    /// the raster's `resolve_emissive`. `None` = factor alone
    /// (pre-feature behavior). Consumed at every emissive-hit shading
    /// site (the GI gather's emissive term and the reflection hit's).
    pub emissive_texture: Option<&'a GpuTexture>,
    /// Extension material textures in the 15-map ABI order. The first three
    /// retain anisotropy/specular-weight/specular-color order; the remaining
    /// eleven follow raster map indices 5, 6, 7, 8, 10, 11, 12, 15, 16, 17,
    /// and 18, followed by AO's canonical raster index 3. They share the
    /// bounded deduplicated table used by the core maps and are indexed in
    /// `RtNormalSource::extra_tex_indices`.
    pub extra_material_textures: [Option<&'a GpuTexture>; 15],
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
    /// Bindless GPU address of this object's
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
    /// The buffer backing `instances_addr` — the
    /// BUG-84fv lifetime handle. The descriptor-build kernel reads instance
    /// values through the raw address, which no binding declares, so the
    /// build/refit pins this handle through its async completion exactly
    /// like `vertex_buffer` above. `Some` iff `instances_addr != 0`;
    /// `instances_addr` must be this buffer's `gpu_address()`.
    pub instances_buffer: Option<&'a GpuBuffer>,
    /// Per-object instance-slot CAPACITY
    /// (rigid topology — a change rebuilds the accel). 0 or 1 = unwired /
    /// identity (the D7 fast path). The raster's live count is in-band
    /// (`pos_scale.w == 0` = dead slot), so capacity is all the CPU needs.
    pub instance_slots: u32,
    /// Per-vertex appearance-weight
    /// buffer (`f32` per vertex, vertex-index addressed — indexed meshes
    /// included), `None` = unwired (weight 1). The checked weight count is
    /// the mesh vertex count: a wired buffer with fewer floats than
    /// `(vertex_buffer.size - vertex_offset) / vertex_stride` is a
    /// structured `RtAccelError::InvalidGeometry` at plan/encode. Read
    /// bindless by the trace/emissive kernels through
    /// `RtNormalSource::appearance_weights_addr`; pinned in the accel's
    /// geometry set exactly like `vertex_buffer`. Buffer identity is
    /// topology (same discipline as `index_buffer`).
    pub appearance_weights: Option<&'a GpuBuffer>,
    /// P4b: per-object appearance gain — the raster's `u.appearance.x`.
    /// `!= 1.0` (with or without weights) puts the object in the
    /// descriptor-nonopaque class so the candidate walkers can apply the
    /// coverage test; a fractional-to-fractional gain change rewrites only
    /// the normal-source table row (rebuilt every RT-ready frame).
    pub appearance_gain: f32,
    /// KHR_texture_transform fold for the base-color map, in the raster's
    /// `apply_uv_transform` convention: `(m00, m01, m10, m11, tx, ty)`.
    pub base_color_uv_transform: [f32; 6],
    /// KHR_texture_transform fold for the metallic-roughness map.
    pub mr_uv_transform: [f32; 6],
    /// KHR_texture_transform fold for the tangent-space normal map.
    pub normal_uv_transform: [f32; 6],
    /// Raster normal-map scale applied to the decoded tangent-space XY.
    pub normal_scale: f32,
    /// Base-color alpha factor used by alpha-mask candidate tests.
    pub base_color_alpha: f32,
    /// Byte offset of the per-vertex tangent (`float4`, xyz + authored
    /// handedness). `u32::MAX` means the mesh has no authored tangent.
    pub tangent_offset: u32,
    pub material_attributes: params::RtMaterialAttributes,
}

impl RtObjectGeometry<'_> {
    /// The Object-space `instance_inner`: the identity.
    pub const NO_INNER: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
}

/// Per-object geometry change class for [`encode_accel_update`] — the
/// caller's dirty decision from its revision metadata (§4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtGeometryChange {
    /// No geometry or descriptor change — no BLAS work.
    Reuse,
    /// Normals/UVs/appearance-only change — tables/history refresh, no
    /// BLAS work while the descriptor opacity class holds.
    Attributes,
    /// Positions moved, topology stable. Refits the existing BLAS in place.
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
    InvalidGeometry {
        object: usize,
        reason: String,
    },
    /// The resident set cannot express the requested change — the caller
    /// must run plan/prepare first (object count, capacity, or a Rebuild
    /// whose new shape exceeds the resident storage).
    NeedsPreparation,
    Allocation {
        bytes: u64,
        resource: &'static str,
    },
    Encode(&'static str),
}

/// One caller-supplied world-space ray
/// for [`GpuShadowRayTracer::debug_ray_query`]. Layout mirrors the MSL
/// `DebugRayQueryRay` (packed_float3 pairs + two scalars).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DebugRayQueryRay {
    pub origin: [f32; 3],
    pub direction: [f32; 3],
    pub min_distance: f32,
    pub max_distance: f32,
}

/// ID sentinel written for a miss (MSL `DEBUG_RAY_INVALID`).
pub const DEBUG_RAY_INVALID: u32 = u32::MAX;

/// One committed-hit record from
/// [`GpuShadowRayTracer::debug_ray_query`]. Layout mirrors the MSL
/// `DebugRayQueryHit` exactly. P4b (§5.2): `coverage`/`brightness` report
/// the accepted hit's appearance evaluation (`clamp(gain*weight, 0, 1)` /
/// `max(gain*weight, 1)` — render_scene.wgsl:889's `apply_appearance`);
/// both 1.0 when the object has no appearance wired, both 0 on a miss.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DebugRayQueryHit {
    pub hit: u32,
    pub object_id: u32,
    pub instance_id: u32,
    pub primitive_id: u32,
    pub distance: f32,
    pub bary: [f32; 2],
    pub coverage: f32,
    pub brightness: f32,
    pub normal: [f32; 3],
    /// Candidates the walk tested, at most [`RT_WALK_MAX_CANDIDATES`].
    pub candidates: u32,
    pub uv: [f32; 2],
    pub pad2: [f32; 2],
}

/// RS-B: GPU-side emissive triangle entry — world-space positions of the
/// three vertices plus per-vertex UVs and the owning object index for
/// gi_materials/normal_sources lookups (RS-C). `packed_float3` discipline:
/// `[f32; 3]` + explicit pad (P0 section 5.1 kernel lesson). P4a: written by
/// the `emissive_gather` kernel, never by the CPU. P4b (§5.2): grew the
/// per-corner APPEARANCE WEIGHTS (`w0/w1/w2`, 1.0 when unwired) — explicit
/// emitter samples have not passed the hit test, so the trace kernel
/// multiplies `coverage * brightness` at the sampled barycentrics from
/// these corners and the normal-source row's current gain.
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
    /// The TLAS descriptor slot of THIS entry's
    /// copy. In instanced mode the entries stay LOCAL-space and the kernel
    /// composes world positions from `emissive_descriptors[descriptor_index]`;
    /// the D7 fast path sets this to the object index (descriptor_index ==
    /// object_index there) and never reads it.
    pub descriptor_index: u32,
    /// P4b: per-corner appearance weights (vertex-order, post index
    /// resolution), 1.0 when the object has no weights wired.
    pub w0: f32,
    pub w1: f32,
    pub w2: f32,
    _pad3: f32,
}

const _: () = assert!(std::mem::size_of::<EmissiveTriangleGpu>() == 96);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtTopologyMismatchCategory {
    ObjectCount,
    DescriptorMode,
    Vertex,
    Index,
    NormalUv,
    InstanceSlots,
    NonOpaque,
    AppearanceWeights,
    SlotOverflow,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtTopologyMismatch {
    pub object: usize,
    pub category: RtTopologyMismatchCategory,
}

/// What the tracer has measured, for proofs and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TraceTimingReport {
    /// Trace command buffers whose GPU time has been read back.
    pub measured_buffers: u64,
    /// Longest measured trace buffer, seconds, over the tracer's life.
    pub longest_buffer_seconds: f64,
    /// Trace command buffers the most recent dispatch committed.
    pub last_dispatch_buffers: u32,
    /// Pixel budget the most recent dispatch planned with.
    pub last_pixels_per_tile: u64,
}
