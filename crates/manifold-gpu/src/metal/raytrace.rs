//! RAYTRACING_DESIGN.md P1 — Metal ray-query acceleration structures and
//! the hard-shadow-ray dispatch kernel.
//!
//! Ports `tools/rt_prototype/src/accel.rs` (acceleration-structure
//! build/refit) and the shadow-only slice of
//! `tools/rt_prototype/shaders/rt_trace.metal`'s `trace_lighting` +
//! `upsample_lighting` kernels (AO/GI sampling is P2/P3 scope — dropped
//! here, not ported). `ShadowRayTracer` is the D9 backend seam: all data
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

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSArray;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLAccelerationStructure, MTLAccelerationStructureCommandEncoder,
    MTLAccelerationStructureGeometryDescriptor,
    MTLAccelerationStructureTriangleGeometryDescriptor, MTLAccelerationStructureUsage,
    MTLAttributeFormat, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLCompileOptions,
    MTLComputePipelineState, MTLDevice, MTLIndexType, MTLLanguageVersion, MTLLibrary,
    MTLPrimitiveAccelerationStructureDescriptor,
};

use super::device::GpuDevice;
use super::types::{GpuBuffer, GpuComputePipeline, GpuTexture};
use super::{GpuEncoder, Slot, SlotKind, SlotMap};
use crate::types::GpuBinding;

// ─── Acceleration structure (ports accel.rs) ───────────────────────────

/// A built acceleration structure over one triangle mesh, kept resident for
/// the lifetime of an RT-enabled scene (built once at scene load — never
/// mid-frame, RAYTRACING_DESIGN.md P1 performer-gesture gate).
pub struct RtAccel {
    pub(crate) structure: Retained<ProtocolObject<dyn MTLAccelerationStructure>>,
    descriptor: Retained<MTLPrimitiveAccelerationStructureDescriptor>,
    /// Scratch buffer for `refit`. The `build()` scratch buffer is only
    /// read by the GPU during `build_accel`, which commits+waits, so it
    /// does not need to outlive that call — only the refit scratch is
    /// kept alive here (matches accel.rs).
    refit_scratch: GpuBuffer,
}

fn make_descriptor(
    vertex_buffer: &GpuBuffer,
    index_buffer: &GpuBuffer,
    triangle_count: u32,
) -> Retained<MTLPrimitiveAccelerationStructureDescriptor> {
    let tri_desc = MTLAccelerationStructureTriangleGeometryDescriptor::descriptor();
    tri_desc.setVertexBuffer(Some(vertex_buffer.raw()));
    tri_desc.setVertexFormat(MTLAttributeFormat::Float3);
    tri_desc.setVertexStride(12);
    tri_desc.setIndexBuffer(Some(index_buffer.raw()));
    tri_desc.setIndexType(MTLIndexType::UInt32);
    tri_desc.setTriangleCount(triangle_count as usize);
    tri_desc.setOpaque(true);
    let geom: Retained<MTLAccelerationStructureGeometryDescriptor> = tri_desc.into_super();
    let array = NSArray::from_retained_slice(&[geom]);
    let prim_desc = MTLPrimitiveAccelerationStructureDescriptor::descriptor();
    prim_desc.setGeometryDescriptors(Some(&array));
    prim_desc.setUsage(MTLAccelerationStructureUsage::Refit);
    prim_desc
}

/// Build a fresh acceleration structure over `vertex_buffer`
/// (`packed_float3`, stride 12) / `index_buffer` (u32 triples).
pub(crate) fn build_accel(
    device: &GpuDevice,
    vertex_buffer: &GpuBuffer,
    index_buffer: &GpuBuffer,
    triangle_count: u32,
) -> RtAccel {
    let descriptor = make_descriptor(vertex_buffer, index_buffer, triangle_count);
    let raw_device = device.raw_device();
    let sizes = raw_device.accelerationStructureSizesWithDescriptor(&descriptor);
    let structure = raw_device
        .newAccelerationStructureWithSize(sizes.accelerationStructureSize)
        .expect("newAccelerationStructureWithSize failed");
    let scratch = device.create_buffer(sizes.buildScratchBufferSize.max(16) as u64);
    let refit_scratch = device.create_buffer(sizes.refitScratchBufferSize.max(16) as u64);

    // One-off build pass at scene load — bypasses `GpuEncoder` (its
    // `EncoderState` has no acceleration-structure variant; adding one for
    // a scene-load-only, once-per-scene call would be a new encoder mode
    // for no per-frame benefit) and goes straight to the queue, exactly as
    // `accel.rs` does.
    let cb = device
        .raw_queue()
        .commandBuffer()
        .expect("Failed to acquire command buffer for RT AS build");
    let enc = cb
        .accelerationStructureCommandEncoder()
        .expect("accelerationStructureCommandEncoder failed");
    enc.buildAccelerationStructure_descriptor_scratchBuffer_scratchBufferOffset(
        &structure,
        &descriptor,
        scratch.raw(),
        0,
    );
    enc.endEncoding();
    cb.commit();
    unsafe { cb.waitUntilCompleted() };

    RtAccel {
        structure,
        descriptor,
        refit_scratch,
    }
}

/// Refit `accel` in place against an already GPU-side-modified vertex
/// buffer (deforming meshes — P0 measured ~12-16ms/frame at 1.43M tris;
/// static hero scenes never call this).
pub(crate) fn refit_accel(device: &GpuDevice, accel: &RtAccel) {
    let cb = device
        .raw_queue()
        .commandBuffer()
        .expect("Failed to acquire command buffer for RT AS refit");
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
    cb.commit();
    unsafe { cb.waitUntilCompleted() };
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

// Dispatch: trace_size grid. Inputs sampled at the matching full-res texel.
// g_wpos: rgba32f, xyz = world pos, w = view dist (<=0 = void background).
// g_nrm: rgba16f, xyz = world normal. Output (trace_size): r = sun
// visibility [0,1].
kernel void trace_shadow_rays(
    primitive_acceleration_structure accel   [[buffer(0)]],
    constant ShadowRayParams&        p       [[buffer(1)]],
    texture2d<float>                 g_wpos  [[texture(0)]],
    texture2d<float>                 g_nrm   [[texture(1)]],
    texture2d<float, access::write>  out_sv  [[texture(2)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.trace_size.x || tid.y >= p.trace_size.y) return;
    uint2 gpix = uint2((float2(tid) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size));
    gpix = min(gpix, p.gbuffer_size - 1);

    float4 wp = g_wpos.read(gpix);
    if (wp.w <= 0.0) {
        out_sv.write(float4(1, 0, 0, 0), tid);
        return;
    }
    float3 n = normalize(g_nrm.read(gpix).xyz);
    float3 origin = wp.xyz + n * 1e-3;

    intersector<triangle_data> shadow_i;
    shadow_i.assume_geometry_type(geometry_type::triangle);
    shadow_i.force_opacity(forced_opacity::opaque);
    shadow_i.accept_any_intersection(true);

    ray r;
    r.origin = origin;
    r.min_distance = 0.0;
    r.max_distance = INFINITY;

    uint spp = max(p.shadow_spp, 1u);
    float vis = 0.0;
    for (uint s = 0; s < spp; s++) {
        r.direction = cone_sample(p.sun_dir, p.sun_cone, rand2(tid, p.frame_index, s));
        if (shadow_i.intersect(r, accel).type == intersection_type::none) vis += 1.0;
    }
    vis /= float(spp);
    out_sv.write(float4(vis, 0, 0, 0), tid);
}

// Depth-aware joint bilateral upsample: half-res sun-visibility -> full
// res. Guides: full-res depth (g_wpos.w) + normal, resampled at each
// low-res tap's mapped full-res texel (numerically identical to caching
// trace-time depth in a spare channel, as the prototype's `upsample_
// lighting` does via `lo_gi.a` — no `gi` buffer exists in the shadow-only
// slice, so this resamples `g_wpos` directly instead).
kernel void upsample_shadow(
    constant ShadowRayParams&       p      [[buffer(1)]],
    texture2d<float>                g_wpos [[texture(0)]],
    texture2d<float>                g_nrm  [[texture(1)]],
    texture2d<float>                lo_sv  [[texture(2)]],
    texture2d<float, access::write> hi_sv  [[texture(3)]],
    uint2 tid [[thread_position_in_grid]])
{
    if (tid.x >= p.gbuffer_size.x || tid.y >= p.gbuffer_size.y) return;
    float4 wp = g_wpos.read(tid);
    if (wp.w <= 0.0) { hi_sv.write(float4(1, 0, 0, 0), tid); return; }
    float3 n = normalize(g_nrm.read(tid).xyz);

    float2 lo_uv = (float2(tid) + 0.5) / float2(p.gbuffer_size) * float2(p.trace_size);
    int2 lo_c = int2(lo_uv - 0.5);
    float acc = 0.0; float wsum = 0.0;
    for (int dy = 0; dy <= 1; dy++)
    for (int dx = 0; dx <= 1; dx++) {
        int2 q = clamp(lo_c + int2(dx, dy), int2(0), int2(p.trace_size) - 1);
        uint2 gq = min(uint2((float2(q) + 0.5) / float2(p.trace_size) * float2(p.gbuffer_size)), p.gbuffer_size - 1);
        float4 qwp = g_wpos.read(gq);
        float2 f = saturate(1.0 - fabs(lo_uv - 0.5 - float2(q)));
        float w_bilin = f.x * f.y;
        float w_depth = exp(-fabs(qwp.w - wp.w) / max(wp.w * 0.02, 1e-4));
        float w_nrm = pow(saturate(dot(n, normalize(g_nrm.read(gq).xyz))), 8.0);
        float w = max(w_bilin * w_depth * w_nrm, 1e-5);
        acc += lo_sv.read(uint2(q)).r * w;
        wsum += w;
    }
    hi_sv.write(float4(acc / wsum, 0, 0, 0), tid);
}
"#;

/// CPU mirror of `ShadowRayParams` above — field order and packing MUST
/// match exactly (P0 §5.1 kernel lesson: `packed_float3` in MSL == dense
/// `[f32; 3]` here, no padding).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ShadowRayParams {
    pub sun_dir: [f32; 3],
    pub sun_cone: f32,
    pub shadow_spp: u32,
    pub frame_index: u32,
    pub trace_size: [u32; 2],
    pub gbuffer_size: [u32; 2],
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

    /// Build a fresh acceleration structure over a triangle mesh. Call
    /// once at scene load for an RT-enabled scene; never mid-frame.
    fn build_accel(
        &self,
        device: &GpuDevice,
        vertex_buffer: &GpuBuffer,
        index_buffer: &GpuBuffer,
        triangle_count: u32,
    ) -> Self::Accel;

    /// Refit `accel` in place against updated vertex data (deforming
    /// meshes only — static hero scenes never call this).
    fn refit_accel(&self, device: &GpuDevice, accel: &Self::Accel);

    /// Dispatch the half-res hard-shadow-ray pass: reads the full-res
    /// world-position (`g_wpos`, rgba32f, w = view dist, <=0 = void) and
    /// normal (`g_nrm`) G-buffer textures, writes sun-visibility to
    /// `out_sv` at `params.trace_size`.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_shadow_rays(
        &self,
        encoder: &mut GpuEncoder,
        accel: &Self::Accel,
        params: &ShadowRayParams,
        params_buffer: &GpuBuffer,
        g_wpos: &GpuTexture,
        g_nrm: &GpuTexture,
        out_sv: &GpuTexture,
        label: &str,
    );

    /// Depth-aware bilateral upsample of the half-res `lo_sv` term to
    /// full G-buffer resolution `hi_sv`.
    #[allow(clippy::too_many_arguments)]
    fn upsample_shadow(
        &self,
        encoder: &mut GpuEncoder,
        params_buffer: &GpuBuffer,
        g_wpos: &GpuTexture,
        g_nrm: &GpuTexture,
        lo_sv: &GpuTexture,
        hi_sv: &GpuTexture,
        label: &str,
    );
}

/// Metal implementation of [`ShadowRayTracer`] — ray queries via
/// `metal_raytracing`, compiled once and kept resident (mirrors the
/// pipeline-cache pattern `GpuDevice` already uses for the WGSL path).
pub struct MetalShadowRayTracer {
    trace_pipeline: GpuComputePipeline,
    upsample_pipeline: GpuComputePipeline,
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
                (0, SlotKind::Texture),
                (1, SlotKind::Texture),
                (2, SlotKind::Texture),
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
            ]),
        );

        Self {
            trace_pipeline,
            upsample_pipeline,
        }
    }
}

impl ShadowRayTracer for MetalShadowRayTracer {
    type Accel = RtAccel;

    fn build_accel(
        &self,
        device: &GpuDevice,
        vertex_buffer: &GpuBuffer,
        index_buffer: &GpuBuffer,
        triangle_count: u32,
    ) -> Self::Accel {
        build_accel(device, vertex_buffer, index_buffer, triangle_count)
    }

    fn refit_accel(&self, device: &GpuDevice, accel: &Self::Accel) {
        refit_accel(device, accel);
    }

    fn dispatch_shadow_rays(
        &self,
        encoder: &mut GpuEncoder,
        accel: &Self::Accel,
        params: &ShadowRayParams,
        params_buffer: &GpuBuffer,
        g_wpos: &GpuTexture,
        g_nrm: &GpuTexture,
        out_sv: &GpuTexture,
        label: &str,
    ) {
        params_buffer.upload(bytemuck_bytes(params));
        let groups = dispatch_groups_2d(params.trace_size, SHADOW_WORKGROUP);
        encoder.dispatch_compute_with_accel(
            &self.trace_pipeline,
            0,
            accel,
            &[
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: params_buffer,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 0,
                    texture: g_wpos,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: g_nrm,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: out_sv,
                },
            ],
            groups,
            label,
        );
    }

    fn upsample_shadow(
        &self,
        encoder: &mut GpuEncoder,
        params_buffer: &GpuBuffer,
        g_wpos: &GpuTexture,
        g_nrm: &GpuTexture,
        lo_sv: &GpuTexture,
        hi_sv: &GpuTexture,
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
                    texture: g_wpos,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: g_nrm,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: lo_sv,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: hi_sv,
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
