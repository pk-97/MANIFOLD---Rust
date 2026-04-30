//! Per-step resource bindings exposed to an [`EffectNode`] during `evaluate`.
//!
//! The runtime hands each node two views — [`NodeInputs`] for ports it reads
//! and [`NodeOutputs`] for ports it writes. Both expose the bound [`Slot`] for
//! a port; the slot is an opaque handle the runtime maps onto a physical
//! resource (a GPU texture, a scalar value) via its pool.
//!
//! Step 4 (this commit) returns slot IDs only. Step 5 will add typed
//! accessors that resolve a slot to a `&GpuTexture` / `&mut GpuTexture` /
//! scalar value via the runtime backend.

/// Opaque physical-buffer index handed out by the runtime's resource pool.
///
/// Two [`crate::node_graph::ResourceId`]s with compatible
/// [`crate::node_graph::PortType`]s may share the same slot if their
/// lifetimes don't overlap (resource recycling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Slot(pub u32);

/// Read-only view of an [`EffectNode`](crate::node_graph::EffectNode)'s
/// input port bindings for one frame.
#[derive(Debug, Clone, Copy)]
pub struct NodeInputs<'a> {
    bindings: &'a [(&'static str, Slot)],
}

impl<'a> NodeInputs<'a> {
    pub(crate) fn new(bindings: &'a [(&'static str, Slot)]) -> Self {
        Self { bindings }
    }

    /// Slot bound to the named input port, or `None` if the port is optional
    /// and unwired.
    pub fn slot(&self, port: &str) -> Option<Slot> {
        self.bindings
            .iter()
            .find(|(name, _)| *name == port)
            .map(|(_, slot)| *slot)
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
/// Step 4 returns only slot IDs (the node has no actual texture to write to
/// at the mock layer). Step 5 will add typed accessors for real GPU textures.
#[derive(Debug, Clone, Copy)]
pub struct NodeOutputs<'a> {
    bindings: &'a [(&'static str, Slot)],
}

impl<'a> NodeOutputs<'a> {
    pub(crate) fn new(bindings: &'a [(&'static str, Slot)]) -> Self {
        Self { bindings }
    }

    pub fn slot(&self, port: &str) -> Option<Slot> {
        self.bindings
            .iter()
            .find(|(name, _)| *name == port)
            .map(|(_, slot)| *slot)
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
