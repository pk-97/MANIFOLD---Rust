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

// BUG-xmsx driver split: the 4.8k-line file carved into sibling modules.
// The root re-exports the full historical `raytrace::*` surface so callers
// are untouched.
mod accel;
pub use accel::*;
mod emissive;
mod params;
pub use params::*;
pub use emissive::*;
mod tracer;
pub use tracer::*;
