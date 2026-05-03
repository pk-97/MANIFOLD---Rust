//! Resource backend abstraction.
//!
//! A [`Backend`] sits below the executor and decides what a [`Slot`] *physically*
//! is: a `GpuTexture` in production, or an opaque integer in tests.
//!
//! Step 5 (this commit) introduces the trait and the [`MockBackend`] — the
//! same slot-tracking logic that lived on `ResourcePool` in step 4, now
//! reachable via dynamic dispatch. The executor takes a `Box<dyn Backend>`
//! at construction; tests use `MockBackend`, future production code will
//! use a `MetalBackend` that wraps `manifold_gpu::RenderTargetPool`.
//!
//! The trait is intentionally narrow. Typed resource accessors
//! (`texture_2d`, `texture_3d`, `scalar`) land in step 6 alongside the real
//! `MetalBackend` so the trait surface and its first non-trivial
//! implementation are designed together.

use ahash::AHashMap;
use manifold_gpu::GpuTexture;

use crate::node_graph::bindings::Slot;
use crate::node_graph::execution_plan::ResourceId;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::ports::PortType;

/// Abstracts physical resource allocation behind the slot-based runtime.
pub trait Backend: Send {
    /// Acquire a slot for `id` of the given [`PortType`]. Backends are
    /// expected to recycle freed slots of the same type before allocating
    /// fresh ones — that's where pool reuse happens.
    fn acquire(&mut self, id: ResourceId, ty: PortType) -> Slot;

    /// Release `id`'s slot back to the per-type free pool. Idempotent —
    /// releasing an already-released id is a no-op.
    fn release(&mut self, id: ResourceId, ty: PortType);

    /// Currently bound slot for `id`, or `None` if unbound.
    fn slot_for(&self, id: ResourceId) -> Option<Slot>;

    /// High-water mark — total physical slots ever allocated. Useful for
    /// asserting that resource recycling actually happens in tests, and as
    /// a baseline pool-size hint for production backends.
    fn slot_count(&self) -> u32;

    /// Drop all bindings and free pools. Slot count (high-water mark) is
    /// retained so subsequent allocations don't reuse slots across the
    /// boundary.
    fn clear(&mut self);

    /// Real `GpuTexture` bound to a slot, if this backend tracks textures
    /// and the slot was allocated as a `Texture2D`.
    ///
    /// Mock backends return `None`. A real backend
    /// ([`MetalBackend`](crate::node_graph::MetalBackend)) returns the
    /// `&GpuTexture` an EffectNode's evaluate needs to dispatch GPU work.
    fn texture_2d(&self, slot: Slot) -> Option<&GpuTexture>;

    /// Scalar value bound to a slot. Set by upstream nodes that produce
    /// scalar outputs (e.g. an audio-level → bloom-intensity wire).
    /// Mock backends return `None`; real backends look up an inline value.
    fn scalar(&self, slot: Slot) -> Option<ParamValue>;
}

/// In-memory backend with no real GPU resources. Tracks slot identity and
/// per-type recycling — same logic as step 4's `ResourcePool`, now behind
/// the [`Backend`] trait.
///
/// Used by every test in the `node_graph` module. Production code uses a
/// future `MetalBackend` that wraps `manifold_gpu::RenderTargetPool`.
pub struct MockBackend {
    free_by_type: AHashMap<PortType, Vec<Slot>>,
    bound: AHashMap<ResourceId, Slot>,
    next_slot: u32,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            free_by_type: AHashMap::default(),
            bound: AHashMap::default(),
            next_slot: 0,
        }
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for MockBackend {
    fn acquire(&mut self, id: ResourceId, ty: PortType) -> Slot {
        // Idempotent on existing bindings — mirrors `MetalBackend`.
        if let Some(&slot) = self.bound.get(&id) {
            return slot;
        }
        let pool = self.free_by_type.entry(ty).or_default();
        let slot = pool.pop().unwrap_or_else(|| {
            let s = Slot(self.next_slot);
            self.next_slot += 1;
            s
        });
        self.bound.insert(id, slot);
        slot
    }

    fn release(&mut self, id: ResourceId, ty: PortType) {
        if let Some(slot) = self.bound.remove(&id) {
            self.free_by_type.entry(ty).or_default().push(slot);
        }
    }

    fn slot_for(&self, id: ResourceId) -> Option<Slot> {
        self.bound.get(&id).copied()
    }

    fn slot_count(&self) -> u32 {
        self.next_slot
    }

    fn clear(&mut self) {
        self.bound.clear();
        self.free_by_type.clear();
    }

    fn texture_2d(&self, _slot: Slot) -> Option<&GpuTexture> {
        // Mock backend has no real GPU resources. Real EffectNode
        // implementations that dispatch GPU work require a backend that
        // returns Some here (see `MetalBackend`). Tests that only exercise
        // graph mechanics don't call this.
        None
    }

    fn scalar(&self, _slot: Slot) -> Option<ParamValue> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::ScalarType;

    #[test]
    fn reuses_freed_slot_of_matching_type() {
        let mut b = MockBackend::new();
        let s0 = b.acquire(ResourceId(0), PortType::Texture2D);
        b.release(ResourceId(0), PortType::Texture2D);
        let s1 = b.acquire(ResourceId(1), PortType::Texture2D);
        assert_eq!(s0, s1);
        assert_eq!(b.slot_count(), 1);
    }

    #[test]
    fn does_not_cross_type_boundaries() {
        let mut b = MockBackend::new();
        b.acquire(ResourceId(0), PortType::Texture2D);
        b.release(ResourceId(0), PortType::Texture2D);
        let s = b.acquire(ResourceId(1), PortType::Texture3D);
        assert_eq!(s.0, 1);
        assert_eq!(b.slot_count(), 2);
    }

    #[test]
    fn scalar_subtypes_pool_independently() {
        // F32 and Vec3 scalars live in different physical-buffer kinds even
        // though they're both Scalar-flavoured.
        let mut b = MockBackend::new();
        b.acquire(ResourceId(0), PortType::Scalar(ScalarType::F32));
        b.release(ResourceId(0), PortType::Scalar(ScalarType::F32));
        let s = b.acquire(ResourceId(1), PortType::Scalar(ScalarType::Vec3));
        assert_eq!(s.0, 1);
        assert_eq!(b.slot_count(), 2);
    }

    #[test]
    fn clear_resets_bindings_but_preserves_high_water_mark() {
        let mut b = MockBackend::new();
        b.acquire(ResourceId(0), PortType::Texture2D);
        b.acquire(ResourceId(1), PortType::Texture2D);
        b.clear();
        assert!(b.slot_for(ResourceId(0)).is_none());
        // After clear, the next acquire allocates a fresh slot rather than
        // reusing — the cleared bindings don't repopulate the free pool.
        let s = b.acquire(ResourceId(2), PortType::Texture2D);
        assert_eq!(s.0, 2);
    }
}
