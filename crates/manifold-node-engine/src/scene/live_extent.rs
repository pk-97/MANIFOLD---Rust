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

impl LiveExtent {
    /// What a kernel anchored on this array binds: the counts word and its
    /// multiplier.
    pub fn counts(&self) -> LiveCounts {
        LiveCounts {
            counts: self.counts.clone(),
            offset: self.offset,
            per_item: self.per_item.max(1),
        }
    }
}

/// The live count a live-guarded kernel reads for its anchor array: bind
/// `counts` at byte `offset`, and write `per_item` to the kernel's
/// `live_per_item` uniform. `per_item == 0` means the anchor has no extent:
/// `counts` is then any buffer (the anchor array itself) and the kernel skips
/// the live clamp.
#[derive(Clone)]
pub struct LiveCounts {
    pub counts: GpuBuffer,
    pub offset: u64,
    pub per_item: u32,
}

/// An array output that publishes a [`LiveExtent`] on every run. The freeze
/// compiler reads these to know which fused regions anchor on a GPU-counted
/// array; a type lists each port it publishes.
pub struct LiveExtentOutput {
    pub type_id: &'static str,
    pub port: &'static str,
}
inventory::collect!(LiveExtentOutput);

/// Whether nodes of `type_id` publish a live extent on `port`.
pub fn publishes_live_extent(type_id: &str, port: &str) -> bool {
    inventory::iter::<LiveExtentOutput>
        .into_iter()
        .any(|entry| entry.type_id == type_id && entry.port == port)
}
