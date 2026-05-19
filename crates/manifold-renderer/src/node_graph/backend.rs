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
use manifold_gpu::{GpuBuffer, GpuTexture};

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

    /// `GpuBuffer` bound to a slot, if this backend tracks Array resources
    /// and the slot was pre-bound as an [`PortType::Array`]. Mock backends
    /// return `None`. The buffer's capacity (max items) was set at
    /// pre-bind time by the chain-build code reading the producing
    /// primitive's `max_capacity` param; primitives observe both the
    /// buffer and the dynamic active-count via the runtime context.
    fn array_buffer(&self, _slot: Slot) -> Option<&GpuBuffer> {
        None
    }

    /// Write a scalar value into a slot. The runtime invokes this after
    /// a control-rate node's `evaluate` returns, draining the
    /// per-step scratch buffer populated via
    /// [`NodeOutputs::set_scalar`](crate::node_graph::NodeOutputs::set_scalar).
    /// Downstream nodes read the value through
    /// [`Backend::scalar`] in the same frame — control wires evaluate
    /// synchronously in topological order.
    fn set_scalar(&mut self, slot: Slot, value: ParamValue);

    /// Backend-specific downcast hook. Default implementation returns
    /// `None`. Real backends override to expose themselves for
    /// implementation-specific calls (e.g., the chain's swap-based
    /// dispatch reaches through this to call
    /// [`MetalBackend::swap_texture_2d`](crate::node_graph::MetalBackend::swap_texture_2d)).
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Install the texture currently bound to `src_slot` into `dst_slot`
    /// as a transient borrowed override. Used by the runtime when a node
    /// declares itself a no-op for the frame via
    /// [`EffectNode::skip_passthrough`] — zero GPU work, just an atomic
    /// retain bump on the underlying texture.
    ///
    /// Downstream nodes that read `dst_slot` see the source texture
    /// without any compute / blit dispatch.
    ///
    /// The override is tracked separately from host-installed borrows
    /// (those persist across frames; skip-aliases must clear) and is
    /// wiped by [`Self::clear_skip_aliases`] at the start of each frame.
    ///
    /// Default: no-op (mock backends).
    fn alias_2d(&mut self, _src_slot: Slot, _dst_slot: Slot) -> bool {
        false
    }

    /// Clear every transient slot alias installed by [`Self::alias_2d`].
    /// Called by the executor at the start of each frame so a skip-frame's
    /// alias doesn't shadow a subsequent non-skip-frame's real write.
    /// Host-installed borrows (e.g. the chain source slot's per-frame
    /// `replace_texture_2d`) are untouched.
    ///
    /// Default: no-op (mock backends).
    fn clear_skip_aliases(&mut self) {}
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
    /// Scalar values written via [`Backend::set_scalar`]. Mock has no
    /// real GPU resources, but the scalar map is needed so tests can
    /// observe control-wire dataflow without a Metal device.
    scalars: AHashMap<Slot, ParamValue>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            free_by_type: AHashMap::default(),
            bound: AHashMap::default(),
            next_slot: 0,
            scalars: AHashMap::default(),
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

    fn scalar(&self, slot: Slot) -> Option<ParamValue> {
        self.scalars.get(&slot).copied()
    }

    fn set_scalar(&mut self, slot: Slot, value: ParamValue) {
        self.scalars.insert(slot, value);
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
