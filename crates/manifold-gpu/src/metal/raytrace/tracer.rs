//! The D9 backend seam: `ShadowRayTracer` trait, `MetalShadowRayTracer`,
//! `RtPipelines`, the raw-MSL kernel compile path, and the trace/upsample/
//! atrous/accumulate dispatch implementations plus their tests. Split out
//! of `raytrace.rs` (BUG-xmsx driver split); see that file for the map.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder, MTLCommandQueue,
    MTLCompileOptions, MTLComputeCommandEncoder, MTLComputePipelineState, MTLDataType,
    MTLDevice, MTLFunctionConstantValues, MTLLanguageVersion, MTLLibrary, MTLSize,
};

use manifold_foundation::cold_touch::{ColdTouchKind, record_cold_touch};

use super::super::device::GpuDevice;
use super::super::types::{GpuBuffer, GpuComputePipeline, GpuTexture};
use super::super::{GpuEncoder, Slot, SlotKind, SlotMap};
use super::*;
use crate::types::{GpuBinding, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};
use crate::trace_planner::{TraceRegion, DEFAULT_TRACE_WORK_LIMITS, estimate_trace_query_units_per_pixel, plan_trace_regions};

// ─── Raw MSL kernels (shadow-only slice of rt_trace.metal) ────────────

/// Shadow-only trim of the prototype's `TraceParams`/`trace_lighting` +
/// `upsample_lighting` kernels. AO (`ao_spp`) and one-bounce GI
/// (`gi_spp`, `Material`/`mat_index` buffers) are P2/P3 scope — dropped,
/// not ported. `packed_float3` is mandatory (P0 section 5.1 kernel lesson):
/// bare MSL `float3` is sizeof 16 and desyncs from `#[repr(C)] [f32; 3]`.
const SHADOW_RAYS_MSL: &str = include_str!("../shadow_rays.msl");

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

pub(crate) const SHADOW_WORKGROUP: [u32; 3] = [8, 8, 1];

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
        let enabled = super::super::gpu_fault::diagnostics_enabled();
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
        let diagnostic_slot = if super::super::gpu_fault::diagnostics_enabled() {
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
                super::super::gpu_fault::complete_submission();
            });
            Some(block)
        } else {
            if super::super::gpu_fault::diagnostics_enabled() {
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
                let diagnostic_label = super::super::gpu_fault::diagnostics_enabled().then(|| format!(
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
            super::super::gpu_fault::begin_submission();
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
        let source = include_str!("tracer.rs");
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


    use super::super::blas_geometry_opaque;
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
