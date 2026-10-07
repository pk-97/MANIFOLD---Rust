//! The live length of an array whose element count only the GPU knows
//! (GPU_FLUID_SURFACE_DESIGN.md P6b). The producer publishes it with its array
//! output; consumers that draw or trace the array use it instead of the
//! array's capacity. Elements past the live count are zero, so a consumer that
//! reads the whole array stays correct, only slower.

use manifold_gpu::GpuBuffer;

#[derive(Clone)]
pub struct LiveExtent {
    /// A u32 at byte `offset` in `counts`, written on the GPU earlier in the
    /// same command buffer: the number of live items. Live elements are
    /// items × `per_item` (triangles × 3 for a triangle list).
    pub counts: GpuBuffer,
    pub offset: u64,
    pub per_item: u32,
    /// A CPU upper bound on live elements, never above the array's capacity,
    /// for passes that need a CPU count (Metal acceleration-structure builds).
    /// Elements between the live count and the bound are zero. Consumers
    /// clamp the GPU count to it.
    pub bound: u32,
}
