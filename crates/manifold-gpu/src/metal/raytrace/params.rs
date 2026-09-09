//! CPU mirrors of the MSL kernel param structs (ShadowRayParams,
//! AccumulateParams, the atrous/firefly/atrous-post params), the caster
//! and material tables (RtCasterParams, GiMaterial, RtNormalSource), and the
//! instance-descriptor mirrors with their writers. Size/offset asserts stay
//! beside the struct they pin (BUG-xmsx driver split); see `raytrace.rs` for
//! the module map.

use super::RtObjectGeometry;
use objc2_metal::MTLAccelerationStructureInstanceDescriptor;

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
pub(crate) struct RtTraceDiagnostics {
    pub(crate) enabled: u32,
    pub(crate) state: u32,
    pub(crate) invalid_count: u32,
    pub(crate) first_stage: u32,
    pub(crate) first_pixel: u32,
    _pad: u32,
    pub(crate) raw_origin: [f32; 3],
    pub(crate) raw_direction: [f32; 3],
    pub(crate) raw_min_distance: f32,
    pub(crate) raw_max_distance: f32,
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
pub(crate) const RT_INSTANCE_TRANSFORM_BYTES: usize = 32;

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
pub(crate) struct RtInstanceBuildObj {
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
pub(crate) fn effective_instance_slots(obj: &RtObjectGeometry) -> u32 {
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
pub(crate) fn write_instance_obj_params(ptr: *mut u8, objects: &[RtObjectGeometry]) {
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

pub(crate) fn atrous_params_bytes(params: &AtrousParams) -> &[u8] {
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

pub(crate) fn firefly_clamp_params_bytes(params: &FireflyClampParams) -> &[u8] {
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

pub(crate) fn atrous_post_params_bytes(params: &AtrousPostParams) -> &[u8] {
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
