//! The ray-tracing backend seam. Each backend's `raytrace` module
//! implements it on its `GpuShadowRayTracer`.

use super::*;
use crate::raytrace::RtAccelPlan;
use crate::{GpuBuffer, GpuDevice, GpuEncoder, GpuTexture};

/// Hardware ray-tracing seam for the RAYTRACING_DESIGN.md hard-shadow-ray
/// pass. Metal ray queries implement this now (`GpuShadowRayTracer`);
/// Vulkan `VK_KHR_ray_query` fits the same method shape when the Vulkan
/// backend builds it (D9, P7) — no method here assumes a Metal-specific call
/// order beyond "build once, dispatch many, refit only for deforming
/// geometry".
pub trait ShadowRayTracer {
    /// Backend-specific resident acceleration structure handle.
    type Accel;

    /// CPU-only sizing for one scene's acceleration state
    /// Descriptor-based sizes plus the
    /// admission byte count ([`RtAccelPlan::additional_peak_bytes`]), which
    /// the renderer hands to `admit_candidate_bytes` before
    /// [`Self::prepare_accel`]. Never inspects vertex contents, allocates,
    /// or encodes.
    fn plan_accel(
        &self,
        device: &GpuDevice,
        resident: Option<&Self::Accel>,
        objects: &[RtObjectGeometry<'_>],
    ) -> Result<RtAccelPlan, RtAccelError>;

    /// Allocate/reuse capacity for a plan — never commits a GPU command.
    /// A structural replacement is prepared atomically and swapped in; an
    /// allocation failure leaves the old resident valid (§4.1).
    fn prepare_accel(
        &self,
        device: &GpuDevice,
        resident: &mut Option<Self::Accel>,
        plan: RtAccelPlan,
    ) -> Result<(), RtAccelError>;

    /// Encode a current-frame update onto the caller's encoder (§4.2):
    /// instance descriptors → changed BLAS builds → TLAS → emissive
    /// preparation, ordered after the frame's geometry writes and before
    /// the trace dispatch. No allocation, no commit, no CPU wait.
    /// Position-only changes refit resident BLAS storage; connectivity
    /// changes rebuild it. Both update TLAS bounds before tracing.
    /// Design amendment: `device` is threaded explicitly — §4.1's sketch
    /// omits it, but the descriptor-build pipeline and the (pre-P4a)
    /// emissive table are device-held and neither the encoder nor the
    /// tracer owns one.
    #[allow(clippy::too_many_arguments)]
    fn encode_accel_update(
        &self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        accel: &mut Self::Accel,
        objects: &[RtObjectGeometry<'_>],
        changes: &[RtGeometryChange],
        materials: &[GiMaterial],
        instance_data_changed: bool,
        emissive_data_changed: bool,
    ) -> Result<RtAccelUpdate, RtAccelError>;

    /// Dispatch the half-res shadow/AO-ray pass (RT-D3; RT-P2 widens this
    /// SAME dispatch to add the AO gather + demodulated-irradiance term —
    /// D16's seam note, not a parallel pass; RT-P3 widens it again with the
    /// emissive/sun-bounce GI gather, reading `gi_materials` — one entry
    /// per object, SAME order as the `objects` slice the accel was
    /// planned/prepared from, so `instance_id` at a GI ray hit indexes it
    /// directly) —
    /// ray origins + bias normal reconstructed in-kernel from `depth_tex`
    /// (the full-res opaque-depth prepass) + `params.inv_view_proj` — no
    /// world-pos/normal G-buffer target. Writes per-caster visibility to
    /// `out_sv` (a layer array, four casters per layer) and demodulated
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
        // The GPU-written 16-byte
        // emissive stats buffer — the table's when one exists, the tracer's
        // zero-stats buffer otherwise. Immediately after `accel`, per the
        // design's signature amendment.
        emissive_stats: &GpuBuffer,
        // §4.3 (P5): params ride the command buffer as inline bytes
        // (setBytes snapshots at encode) — no CPU-mapped params buffer a
        // later frame's encode could tear under an in-flight trace.
        params: &ShadowRayParams,
        // The buffer holding the caster table at `params.caster_data_addr`;
        // the kernel reads it by address, so it is declared to Metal here.
        casters: &GpuBuffer,
        gi_materials: &GpuBuffer,
        normal_sources: &GpuBuffer,
        current_objects: &[RtObjectGeometry<'_>],
        alpha_textures: &[&GpuTexture],
        depth_tex: &GpuTexture,
        out_sv: &GpuTexture,
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
        // zero-filled and sized to the larger typed element (96-byte
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
    /// validity test needs). The
    /// visibility textures are layer arrays; every layer is upsampled.
    #[allow(clippy::too_many_arguments)]
    fn upsample_shadow(
        &self,
        encoder: &mut GpuEncoder,
        // §4.3 (P5): the same inline-bytes snapshot as the trace dispatch —
        // `params.gbuffer_size` also drives the dispatch grid (this replaces
        // the shared params buffer and its read-back sizing hack).
        params: &ShadowRayParams,
        // Retained for compatibility; params are snapshotted inline below.
        _params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        lo_sv: &GpuTexture,
        hi_sv: &GpuTexture,
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
    /// the initial full-res `src_*` set. The
    /// visibility layers all filter with the same edge-stops.
    #[allow(clippy::too_many_arguments)]
    fn atrous_pass(
        &self,
        encoder: &mut GpuEncoder,
        params: &AtrousParams,
        // Retained for compatibility; params are snapshotted inline below.
        _params_buffer: &GpuBuffer,
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
        // P4a (§5.1): the GPU emissive stats buffer — the kernel computes
        // max(floor, stats.mean_power) itself, so the floor stays the fixed
        // minimum and the mean power is never CPU-stale.
        emissive_stats: &GpuBuffer,
        params: &FireflyClampParams,
        // Retained for compatibility; params are snapshotted inline below.
        _params_buffer: &GpuBuffer,
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
        // Retained for compatibility; params are snapshotted inline below.
        _params_buffer: &GpuBuffer,
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
        // Retained for compatibility; params are snapshotted inline below.
        _params_buffer: &GpuBuffer,
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
        // SV-ACCUM: shadow-visibility layer arrays (four casters per layer) — current
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
        // RT-TL-C (section 16 TL5/TL8): sun-transmission tint channel —
        // same flip clock, same weights/reset as irradiance (not sv sigma-gate).
        hi_svt: &GpuTexture,
        svt_history_read: &GpuTexture,
        svt_history_write: &GpuTexture,
        label: &str,
    );
}
