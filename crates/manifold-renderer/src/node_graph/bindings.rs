//! Per-step resource bindings exposed to an [`EffectNode`] during `evaluate`.
//!
//! The runtime hands each node two views — [`NodeInputs`] for ports it reads
//! and [`NodeOutputs`] for ports it writes. Each view exposes:
//!
//! - **slot lookup** ([`NodeInputs::slot`] / [`NodeOutputs::slot`]) — the
//!   abstract `Slot` the runtime allocated for this port. Stable across
//!   backends; useful for introspection and tests.
//! - **typed lookup** ([`NodeInputs::texture_2d`], [`NodeInputs::scalar`],
//!   etc.) — resolves the slot to a real GPU resource via the [`Backend`].
//!   Real EffectNode implementations use these to get a `&GpuTexture` they
//!   can bind in shader dispatches. With a mock backend the typed lookups
//!   return `None`, which is fine for tests that don't dispatch GPU work.

use manifold_gpu::GpuTexture;

use crate::node_graph::backend::Backend;
use crate::node_graph::parameters::ParamValue;

/// Opaque physical-buffer index handed out by the runtime's resource pool.
///
/// Two [`crate::node_graph::ResourceId`]s with compatible
/// [`crate::node_graph::PortType`]s may share the same slot if their
/// lifetimes don't overlap (resource recycling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Slot(pub u32);

/// Read-only view of an [`EffectNode`](crate::node_graph::EffectNode)'s
/// input port bindings for one frame.
#[derive(Clone, Copy)]
pub struct NodeInputs<'a> {
    bindings: &'a [(&'static str, Slot)],
    backend: &'a dyn Backend,
}

impl<'a> NodeInputs<'a> {
    pub(crate) fn new(bindings: &'a [(&'static str, Slot)], backend: &'a dyn Backend) -> Self {
        Self { bindings, backend }
    }

    /// Slot bound to the named input port, or `None` if the port is optional
    /// and unwired.
    pub fn slot(&self, port: &str) -> Option<Slot> {
        self.bindings
            .iter()
            .find(|(name, _)| *name == port)
            .map(|(_, slot)| *slot)
    }

    /// `&GpuTexture` bound to the named input port. `None` if unwired,
    /// or if the backend doesn't track textures (mock).
    ///
    /// The returned reference is tied to the backend's lifetime (`'a`),
    /// not to a temporary borrow of `self`. This lets a node keep input
    /// texture refs in locals while it later borrows the encoder
    /// mutably from the same context.
    pub fn texture_2d(&self, port: &str) -> Option<&'a GpuTexture> {
        self.backend.texture_2d(self.slot(port)?)
    }

    /// Scalar value bound to the named input port (when wired through a
    /// scalar output upstream).
    pub fn scalar(&self, port: &str) -> Option<ParamValue> {
        self.backend.scalar(self.slot(port)?)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, Slot)> + '_ {
        self.bindings.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// View of an [`EffectNode`](crate::node_graph::EffectNode)'s output port
/// bindings for one frame.
///
/// Texture writes happen through the backend's shared mutable state
/// (Metal's `MTLTexture` is interior-mutable via the GPU command
/// buffer), so the backend reference here can stay shared. Scalar
/// writes, however, need to land in the backend's CPU-side scalar
/// map — and the backend can't be borrowed mutably here without
/// fighting the `NodeInputs` borrow active in the same evaluate call.
/// The scratch buffer pattern threads writes out through
/// [`Self::set_scalar`]: nodes push, the executor drains and applies
/// them via [`Backend::set_scalar`] after `evaluate` returns. Synchronous
/// — downstream readers in the same frame see the value.
pub struct NodeOutputs<'a> {
    bindings: &'a [(&'static str, Slot)],
    backend: &'a dyn Backend,
    /// Per-step scratch the executor hands to every node so scalar
    /// writes can be drained back into the backend after `evaluate`.
    pending_scalar_writes: &'a mut Vec<(Slot, ParamValue)>,
}

impl<'a> NodeOutputs<'a> {
    pub(crate) fn new(
        bindings: &'a [(&'static str, Slot)],
        backend: &'a dyn Backend,
        pending_scalar_writes: &'a mut Vec<(Slot, ParamValue)>,
    ) -> Self {
        Self {
            bindings,
            backend,
            pending_scalar_writes,
        }
    }

    pub fn slot(&self, port: &str) -> Option<Slot> {
        self.bindings
            .iter()
            .find(|(name, _)| *name == port)
            .map(|(_, slot)| *slot)
    }

    /// `&GpuTexture` an EffectNode should *write to* for the named output
    /// port. The encoder uses this as the render-target / storage-texture
    /// binding when dispatching the node's shader.
    ///
    /// The returned reference is tied to the backend's lifetime (`'a`),
    /// matching `NodeInputs::texture_2d` so a node can hold both input
    /// and output texture refs in locals across the encoder's mutable
    /// borrow.
    pub fn texture_2d(&self, port: &str) -> Option<&'a GpuTexture> {
        self.backend.texture_2d(self.slot(port)?)
    }

    /// Queue a scalar write to the named output port. The executor
    /// applies the write through [`Backend::set_scalar`] after the
    /// node's `evaluate` returns; downstream readers in the same
    /// frame see the value via [`NodeInputs::scalar`]. A no-op when
    /// `port` isn't a declared output on this node (debug-builds
    /// could assert; production silently drops).
    pub fn set_scalar(&mut self, port: &str, value: ParamValue) {
        if let Some(slot) = self.slot(port) {
            self.pending_scalar_writes.push((slot, value));
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, Slot)> + '_ {
        self.bindings.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}
