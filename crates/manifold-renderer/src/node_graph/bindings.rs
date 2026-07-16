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

use manifold_gpu::{GpuBuffer, GpuTexture};

use crate::node_graph::backend::Backend;
use crate::node_graph::camera::Camera;
use crate::node_graph::light::Light;
use crate::node_graph::material::Material;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::atmosphere::Atmosphere;
use crate::node_graph::transform::Transform;

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
    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D5 — per-physical-slot write
    /// generation counters, indexed by `Slot.0`, owned by the [`Executor`]
    /// (see `Executor::slot_generations`). Threaded through so
    /// [`Self::slot_generation`] can resolve a port name to its current
    /// generation without the executor itself being reachable from a node's
    /// `evaluate`.
    ///
    /// [`Executor`]: crate::node_graph::execution::Executor
    generations: &'a [u64],
}

impl<'a> NodeInputs<'a> {
    pub(crate) fn new(
        bindings: &'a [(&'static str, Slot)],
        backend: &'a dyn Backend,
        generations: &'a [u64],
    ) -> Self {
        Self { bindings, backend, generations }
    }

    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D5 — write generation of the
    /// physical slot currently bound to `port`, or `None` if the port is
    /// unwired. The counter bumps every frame the producing step's output
    /// slot is committed, UNLESS that step declared its outputs unchanged
    /// this frame (`ctx.mark_outputs_unchanged()`) — so an unchanging
    /// generation number across frames is a reliable "this slot's content
    /// has not changed since the last time a consumer observed this exact
    /// number" signal, usable as a cache-key component (D6). Never compare
    /// this alone across executor lifetimes without also folding in the
    /// executor's `rebuild_epoch` — see D6's rationale.
    pub fn slot_generation(&self, port: &str) -> Option<u64> {
        let slot = self.slot(port)?;
        self.generations.get(slot.0 as usize).copied()
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

    /// 3D `&GpuTexture` bound to the named [`PortType::Texture3D`]
    /// input port. `None` if unwired or the backend doesn't track 3D
    /// textures (mock). The volume was pre-bound by the chain build at
    /// dimensions sized for the producing primitive's volume-resolution
    /// param.
    pub fn texture_3d(&self, port: &str) -> Option<&'a GpuTexture> {
        self.backend.texture_3d(self.slot(port)?)
    }

    /// Scalar value bound to the named input port (when wired through a
    /// scalar output upstream).
    pub fn scalar(&self, port: &str) -> Option<ParamValue> {
        self.backend.scalar(self.slot(port)?)
    }

    /// `&GpuBuffer` bound to the named [`PortType::Array`] input port.
    /// `None` if unwired or if the backend doesn't track Array
    /// resources (mock backends). The buffer was sized by the chain
    /// build at `(item_size × max_capacity)` bytes; primitives read
    /// items 0..active_count from it. Active-count plumbing lands in
    /// Phase A.7 alongside the particle primitives that need it.
    pub fn array(&self, port: &str) -> Option<&'a GpuBuffer> {
        self.backend.array_buffer(self.slot(port)?)
    }

    /// [`Camera`] bound to the named [`PortType::Camera`] input port.
    /// `None` if unwired. Camera wires are CPU-only structs, set by
    /// the producing camera primitive's `set_camera` write and drained
    /// by the executor into the backend's per-slot map before the
    /// consumer runs.
    pub fn camera(&self, port: &str) -> Option<Camera> {
        self.backend.camera(self.slot(port)?)
    }

    /// [`Light`] bound to the named [`PortType::Light`] input port.
    /// `None` if unwired. Light wires are CPU-only structs with the same
    /// drain shape as `Camera` — produced by `node.light`, consumed by
    /// shading atoms and shadow-aware mesh renderers.
    pub fn light(&self, port: &str) -> Option<Light> {
        self.backend.light(self.slot(port)?)
    }

    /// [`Material`] bound to the named [`PortType::Material`] input port.
    /// `None` if unwired — but the bundled 3D mesh renderers TREAT an
    /// unwired material as a structured error (per the Material design
    /// doc's "no silent fallbacks" rule), so consumers should check for
    /// `None` and emit `ctx.error(...)` rather than substitute a default.
    pub fn material(&self, port: &str) -> Option<Material> {
        self.backend.material(self.slot(port)?)
    }

    /// [`Transform`] bound to the named [`PortType::Transform`] input port.
    /// `None` if unwired. Same CPU-struct drain shape as `Camera` / `Light` /
    /// `Material` — produced by `node.transform_3d`, consumed by
    /// `render_scene`'s `transform_n` ports.
    pub fn transform(&self, port: &str) -> Option<Transform> {
        self.backend.transform(self.slot(port)?)
    }

    /// [`Atmosphere`] bound to the named [`PortType::Atmosphere`] input port.
    /// `None` if unwired — `render_scene` treats `None` as
    /// [`Atmosphere::default`] (fog off), so an unwired atmosphere is
    /// byte-identical to no atmosphere. Same CPU-struct drain shape as
    /// `Transform`.
    pub fn atmosphere(&self, port: &str) -> Option<Atmosphere> {
        self.backend.atmosphere(self.slot(port)?)
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
    /// Sibling scratch for `Camera` writes — same shape as scalars.
    pending_camera_writes: &'a mut Vec<(Slot, Camera)>,
    /// Sibling scratch for `Light` writes — same shape as cameras.
    pending_light_writes: &'a mut Vec<(Slot, Light)>,
    /// Sibling scratch for `Material` writes — same shape as lights.
    pending_material_writes: &'a mut Vec<(Slot, Material)>,
    /// Sibling scratch for `Transform` writes — same shape as materials.
    pending_transform_writes: &'a mut Vec<(Slot, Transform)>,
    /// Sibling scratch for `Atmosphere` writes — same shape as transforms.
    pending_atmosphere_writes: &'a mut Vec<(Slot, Atmosphere)>,
}

impl<'a> NodeOutputs<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        bindings: &'a [(&'static str, Slot)],
        backend: &'a dyn Backend,
        pending_scalar_writes: &'a mut Vec<(Slot, ParamValue)>,
        pending_camera_writes: &'a mut Vec<(Slot, Camera)>,
        pending_light_writes: &'a mut Vec<(Slot, Light)>,
        pending_material_writes: &'a mut Vec<(Slot, Material)>,
        pending_transform_writes: &'a mut Vec<(Slot, Transform)>,
        pending_atmosphere_writes: &'a mut Vec<(Slot, Atmosphere)>,
    ) -> Self {
        Self {
            bindings,
            backend,
            pending_scalar_writes,
            pending_camera_writes,
            pending_light_writes,
            pending_material_writes,
            pending_transform_writes,
            pending_atmosphere_writes,
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

    /// 3D `&GpuTexture` an EffectNode should *write to* for the named
    /// [`PortType::Texture3D`] output port. Pre-bound by chain build at
    /// dimensions sized for the producing primitive's volume-resolution
    /// param. Same lifetime semantics as `texture_2d`.
    pub fn texture_3d(&self, port: &str) -> Option<&'a GpuTexture> {
        self.backend.texture_3d(self.slot(port)?)
    }

    /// `&GpuBuffer` an EffectNode should *write to* for the named
    /// [`PortType::Array`] output port. Pre-bound by chain build at
    /// `(item_size × max_capacity)` bytes — the primitive fills items
    /// 0..active_count via compute shader stores. Same lifetime
    /// semantics as `texture_2d`.
    pub fn array(&self, port: &str) -> Option<&'a GpuBuffer> {
        self.backend.array_buffer(self.slot(port)?)
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

    /// Queue a [`Camera`] write to the named output port. Drained by the
    /// executor into the backend after `evaluate` returns; same semantics
    /// as `set_scalar`.
    pub fn set_camera(&mut self, port: &str, value: Camera) {
        if let Some(slot) = self.slot(port) {
            self.pending_camera_writes.push((slot, value));
        }
    }

    /// Queue a [`Light`] write to the named output port. Drained by the
    /// executor into the backend after `evaluate` returns; same semantics
    /// as `set_camera`.
    pub fn set_light(&mut self, port: &str, value: Light) {
        if let Some(slot) = self.slot(port) {
            self.pending_light_writes.push((slot, value));
        }
    }

    /// Queue a [`Material`] write to the named output port. Drained by the
    /// executor into the backend after `evaluate` returns; same semantics
    /// as `set_light`.
    pub fn set_material(&mut self, port: &str, value: Material) {
        if let Some(slot) = self.slot(port) {
            self.pending_material_writes.push((slot, value));
        }
    }

    /// Queue a [`Transform`] write to the named output port. Drained by the
    /// executor into the backend after `evaluate` returns; same semantics
    /// as `set_material`.
    pub fn set_transform(&mut self, port: &str, value: Transform) {
        if let Some(slot) = self.slot(port) {
            self.pending_transform_writes.push((slot, value));
        }
    }

    /// Queue an [`Atmosphere`] write to the named output port. Drained by the
    /// executor into the backend after `evaluate` returns; same semantics as
    /// `set_transform`.
    pub fn set_atmosphere(&mut self, port: &str, value: Atmosphere) {
        if let Some(slot) = self.slot(port) {
            self.pending_atmosphere_writes.push((slot, value));
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

#[cfg(all(test, feature = "gpu-proofs"))]
mod array_accessor_tests {
    //! Phase A.5 of `BUFFER_PORT_PLAN`. Verifies the
    //! [`NodeInputs::array`] / [`NodeOutputs::array`] accessors
    //! resolve port names through the backend's [`PortType::Array`]
    //! storage end-to-end.

    use manifold_gpu::GpuTextureFormat;

    use super::*;
    use crate::node_graph::MetalBackend;
    use crate::node_graph::execution_plan::ResourceId;

    #[test]
    fn inputs_array_resolves_pre_bound_buffer_by_port_name() {
        let device = crate::test_device();
        let mut backend = MetalBackend::new(device.arc(), 16, 16, GpuTextureFormat::Rgba16Float);
        let buffer = device.create_buffer(2048);
        let expected_size = buffer.size;

        let slot = backend.pre_bind_array(ResourceId(0), buffer);
        let bindings: &[(&'static str, Slot)] = &[("particles", slot)];
        let inputs = NodeInputs::new(bindings, &backend, &[]);

        let got = inputs.array("particles").expect("should resolve");
        assert_eq!(got.size, expected_size);
        assert!(inputs.array("missing_port").is_none());
    }

    #[test]
    fn outputs_array_resolves_pre_bound_buffer_by_port_name() {
        let device = crate::test_device();
        let mut backend = MetalBackend::new(device.arc(), 16, 16, GpuTextureFormat::Rgba16Float);
        let buffer = device.create_buffer(4096);
        let expected_size = buffer.size;

        let slot = backend.pre_bind_array(ResourceId(0), buffer);
        let bindings: &[(&'static str, Slot)] = &[("particles_out", slot)];
        let mut scratch = Vec::new();
        let mut cam_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let outputs = NodeOutputs::new(
            bindings,
            &backend,
            &mut scratch,
            &mut cam_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
        );

        let got = outputs.array("particles_out").expect("should resolve");
        assert_eq!(got.size, expected_size);
    }
}
