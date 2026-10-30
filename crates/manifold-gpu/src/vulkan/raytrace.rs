//! Ray tracing on Vulkan (`VK_KHR_acceleration_structure` and
//! `VK_KHR_ray_query`) is P7. Until then the shared data types and the
//! `ShadowRayTracer` seam exist, `GpuDevice::supports_ray_tracing` is false,
//! and the tracer and acceleration structure can never be built: callers
//! check the capability first and use raster shadow maps.

// The never-built types' methods take parameters they cannot reach.
#![allow(unused_variables)]

use std::convert::Infallible;

pub use crate::raytrace_types::*;
use crate::*;

/// Nothing traces until P7, so there are no tiles to make conservative.
pub fn request_conservative_trace_tiles() {}

pub struct GpuShadowRayTracer {
    never: Infallible,
}

/// The scene reads the same public fields as on Metal.
pub struct RtAccel {
    pub ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub emissive_table: Option<EmissiveLightTable>,
    never: Infallible,
}

/// The emissive-triangle light table the lighting kernels bind.
pub struct EmissiveLightTable {
    pub triangles: GpuBuffer,
    pub aliases: GpuBuffer,
    pub stats: GpuBuffer,
    _never: Infallible,
}

/// Boxed so dropping a plan frees something, as on Metal, where callers drop
/// a plan to release it before planning again.
pub struct RtAccelPlan {
    never: Box<Infallible>,
}

impl GpuShadowRayTracer {
    /// Populate the device-global RT pipeline set at startup
    /// After this, tracer construction
    /// compiles nothing.
    pub fn prewarm(device: &GpuDevice) {
        // Nothing to compile until ray tracing exists (P7).
    }

    pub fn new(device: &GpuDevice) -> Self {
        panic!("ray tracing is unavailable until vulkan P7; check GpuDevice::supports_ray_tracing")
    }

    /// A tracer whose candidate walks test up to `cap` candidates per ray
    /// instead of [`RT_WALK_MAX_CANDIDATES`].
    #[cfg(feature = "gpu-proofs")]
    pub fn with_walk_cap_for_proof(device: &GpuDevice, cap: u32) -> Self {
        panic!("ray tracing is unavailable until vulkan P7; check GpuDevice::supports_ray_tracing")
    }

    /// Start the next trace from the smallest tiles.
    pub fn restart_trace_tiles(&self) {
        match self.never {}
    }

    /// Read back any finished trace buffers now instead of at the next
    /// dispatch, and report what has been measured.
    pub fn poll_trace_timing(&self) -> TraceTimingReport {
        match self.never {}
    }

    /// P4a (§5.1): the shared 16-byte zero stats buffer every
    /// `dispatch_shadow_rays`/`firefly_clamp` caller binds when the scene
    /// has no emissive table (entry_count 0 — the sampler skips; the
    /// firefly floor reduces to its fixed minimum).
    pub fn zero_emissive_stats(&self) -> &GpuBuffer {
        match self.never {}
    }

    pub fn set_secondary_lighting(
        &mut self,
        lights: &GpuBuffer,
        irradiance: &GpuTexture,
        brdf_lut: &GpuTexture,
        prefiltered_sheen: &GpuTexture,
    ) {
        match self.never {}
    }

    /// RT-T1-B value-test-only entry point (`docs/RAYTRACING_DESIGN.md` section 8
    /// Tier-1 item 2's gate) — dispatches the SAME `fetch_interpolated_normal`
    /// MSL helper `trace_shadow_rays` uses internally, against caller-
    /// supplied `(instance_id, primitive_id, barycentric)` inputs, no ray
    /// tracing/RNG involved.
    pub fn debug_fetch_interpolated_normal(
        &self,
        device: &GpuDevice,
        normal_sources: &GpuBuffer,
        instance_id: u32,
        primitive_id: u32,
        bary: [f32; 2],
        // The SAME slot-row base the
        // production kernel's params carry (object count N) — the debug
        // surface must exercise the production slot-row indexing, not the
        // canonical rows.
        slot_row_base: u32,
    ) -> [f32; 3] {
        match self.never {}
    }

    /// Value-test-only entry
    /// point — traces caller-supplied world-space rays through the SAME
    /// acceleration structure, slot-row indexing and candidate-hit walk the
    /// production kernels use, writing one [`DebugRayQueryHit`] per ray.
    pub fn debug_ray_query(
        &self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        accel: &RtAccel,
        normal_sources: &GpuBuffer,
        rays: &[DebugRayQueryRay],
        material_textures: Option<&[&GpuTexture]>,
        slot_row_base: u32,
        seed_base: u32,
    ) -> GpuBuffer {
        match self.never {}
    }

    /// BUG-dx6w value-test-only entry point — dispatches the SAME
    /// `clamp_refl_history` MSL helper `accumulate_irradiance` uses
    /// internally, against a caller-supplied 3x3 `hi_refl` neighborhood
    /// (row-major, `neighborhood[0]` = top-left) and a history value.
    pub fn debug_clamp_refl_history(
        &self,
        device: &GpuDevice,
        neighborhood: &[[f32; 4]; 9],
        history: [f32; 3],
    ) -> [f32; 3] {
        match self.never {}
    }

    /// RT-Stage-3 P1 (BUG-mkgh) value-test-only entry point — dispatches the
    /// SAME `firefly_clamp_center` MSL helper the production `firefly_clamp`
    /// kernel calls, against a caller-supplied 3x3 `color` neighborhood
    /// (row-major, `color[0]` = top-left) and a matching 3x3 `depth`
    /// neighborhood (`depth[i] <= 0.0` = reversed-Z void, read from the
    /// center's (1,1) texel).
    pub fn debug_firefly_clamp(
        &self,
        device: &GpuDevice,
        color: &[[f32; 4]; 9],
        depth: &[f32; 9],
        gain: f32,
        floor: f32,
        // P4a (§5.1): the mean power riding the GPU stats buffer — the
        // kernel's floor is max(floor, mean_power); 0.0 reduces to the
        // fixed minimum, the pre-P4a behavior with a black scene.
        mean_power: f32,
    ) -> [f32; 3] {
        match self.never {}
    }

    /// RT-Stage-3 P3 value-test-only surface (`debug_atrous_post`'s only
    /// caller) — exercises the EXACT SAME `atrous_post_center` MSL helper
    /// the production `atrous_post` kernel calls, against a caller-supplied
    /// 3x3 neighborhood (depth, normal, moments, src_irr).
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
        match self.never {}
    }
}

impl RtAccel {
    pub fn check_topology(
        &self,
        objects: &[RtObjectGeometry<'_>],
    ) -> Result<(), RtTopologyMismatch> {
        match self.never {}
    }
}

impl RtAccelPlan {
    /// Bytes prepare will allocate BEYOND what the resident set already
    /// holds — BLAS/TLAS storage, both scratch kinds, descriptor/instance
    /// tables, emissive workspaces, lifetime pins.
    pub fn additional_peak_bytes(&self) -> u64 {
        match *self.never {}
    }
}

impl ShadowRayTracer for GpuShadowRayTracer {
    type Accel = RtAccel;

    fn plan_accel(
        &self,
        device: &GpuDevice,
        resident: Option<&Self::Accel>,
        objects: &[RtObjectGeometry<'_>],
        materials: &[GiMaterial],
    ) -> Result<RtAccelPlan, RtAccelError> {
        match self.never {}
    }

    fn prepare_accel(
        &self,
        device: &GpuDevice,
        resident: &mut Option<Self::Accel>,
        plan: RtAccelPlan,
    ) -> Result<(), RtAccelError> {
        match self.never {}
    }

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
    ) -> Result<RtAccelUpdate, RtAccelError> {
        match self.never {}
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_shadow_rays(
        &self,
        encoder: &mut GpuEncoder,
        device: &GpuDevice,
        accel: &Self::Accel,
        emissive_stats: &GpuBuffer,
        params: &ShadowRayParams,
        casters: &GpuBuffer,
        gi_materials: &GpuBuffer,
        normal_sources: &GpuBuffer,
        current_objects: &[RtObjectGeometry<'_>],
        alpha_textures: &[&GpuTexture],
        depth_tex: &GpuTexture,
        out_sv: &GpuTexture,
        out_svt: &GpuTexture,
        out_irr: &GpuTexture,
        out_n: &GpuTexture,
        out_refl: &GpuTexture,
        prefiltered_env: &GpuTexture,
        emissive_triangles: &GpuBuffer,
        emissive_aliases: &GpuBuffer,
        has_translucency: bool,
        label: &str,
    ) {
        match self.never {}
    }

    #[allow(clippy::too_many_arguments)]
    fn upsample_shadow(
        &self,
        encoder: &mut GpuEncoder,
        params: &ShadowRayParams,
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
        lo_svt: &GpuTexture,
        hi_svt: &GpuTexture,
        label: &str,
    ) {
        match self.never {}
    }

    #[allow(clippy::too_many_arguments)]
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
        match self.never {}
    }

    fn firefly_clamp(
        &self,
        encoder: &mut GpuEncoder,
        emissive_stats: &GpuBuffer,
        params: &FireflyClampParams,
        _params_buffer: &GpuBuffer,
        depth_tex: &GpuTexture,
        src: &GpuTexture,
        dst: &GpuTexture,
        label: &str,
    ) {
        match self.never {}
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
        match self.never {}
    }

    #[allow(clippy::too_many_arguments)]
    fn accumulate_irradiance(
        &self,
        encoder: &mut GpuEncoder,
        params: &AccumulateParams,
        _params_buffer: &GpuBuffer,
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
        hi_refl: &GpuTexture,
        refl_history_read: &GpuTexture,
        refl_history_write: &GpuTexture,
        gi_materials: &GpuBuffer,
        hi_sv: &GpuTexture,
        sv_history_read: &GpuTexture,
        sv_history_write: &GpuTexture,
        sv_m1_read: &GpuTexture,
        sv_m1_write: &GpuTexture,
        sv_m2_read: &GpuTexture,
        sv_m2_write: &GpuTexture,
        sv_hold_read: &GpuTexture,
        sv_hold_write: &GpuTexture,
        hi_svt: &GpuTexture,
        svt_history_read: &GpuTexture,
        svt_history_write: &GpuTexture,
        label: &str,
    ) {
        match self.never {}
    }
}

pub fn encode_inline_copy(
    device: &GpuDevice,
    encoder: &mut crate::GpuEncoder,
    dst: &GpuBuffer,
    dst_offset: u64,
    bytes: &[u8],
) {
    panic!("ray tracing is unavailable until vulkan P7; check GpuDevice::supports_ray_tracing")
}

pub fn ensure_normal_sources<'a>(
    slot: &mut Option<GpuBuffer>,
    capacity: &mut usize,
    device: &GpuDevice,
    objects: &[RtObjectGeometry<'a>],
) -> RtMaterialTextures<'a> {
    panic!("ray tracing is unavailable until vulkan P7; check GpuDevice::supports_ray_tracing")
}

pub fn ensure_normal_sources_snapshot<'a>(
    scratch: &mut Option<GpuBuffer>,
    scratch_capacity: &mut usize,
    destination: &mut Option<GpuBuffer>,
    destination_capacity: &mut usize,
    device: &GpuDevice,
    encoder: &mut crate::GpuEncoder,
    objects: &[RtObjectGeometry<'a>],
) -> RtMaterialTextures<'a> {
    panic!("ray tracing is unavailable until vulkan P7; check GpuDevice::supports_ray_tracing")
}
