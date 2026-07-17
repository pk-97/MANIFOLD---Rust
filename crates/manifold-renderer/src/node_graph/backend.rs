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
use manifold_gpu::{GpuBuffer, GpuTexture, GpuTextureFormat};

use crate::node_graph::bindings::Slot;
use crate::node_graph::camera::Camera;
use crate::node_graph::execution_plan::ResourceId;
use crate::node_graph::atmosphere::Atmosphere;
use crate::node_graph::light::Light;
use crate::node_graph::material::Material;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::ports::PortType;
use crate::node_graph::scene_object::SceneObject;
use crate::node_graph::transform::Transform;

/// Abstracts physical resource allocation behind the slot-based runtime.
pub trait Backend: Send {
    /// Acquire a slot for `id` of the given [`PortType`]. Backends are
    /// expected to recycle freed slots of the same `(type, format, dims)`
    /// tuple before allocating fresh ones.
    ///
    /// `format` is honored only for `Texture2D` slots; other port types
    /// ignore it. `None` means "use the backend's default format"
    /// (typically `Rgba16Float`) — most primitives pass `None` and
    /// only the native-precision escape hatches declare a non-default
    /// format via [`EffectNode::output_format`](crate::node_graph::EffectNode::output_format).
    ///
    /// `dims` is honored only for `Texture2D` slots and must be
    /// concrete — the executor resolves
    /// [`ExecutionPlan::resource_dims`](crate::node_graph::execution_plan::ExecutionPlan::resource_dims)'
    /// `None` (canvas-default) against `canvas_dims()` before calling
    /// here. Slots at different dims pool independently so a freed
    /// quarter-res rgba16float slot won't be handed back for a
    /// full-res rgba16float acquire.
    fn acquire(
        &mut self,
        id: ResourceId,
        ty: PortType,
        format: Option<GpuTextureFormat>,
        dims: (u32, u32),
    ) -> Slot;

    /// Release `id`'s slot back to the per-`(type, format, dims)` free
    /// pool. Idempotent — releasing an already-released id is a no-op.
    /// `format` and `dims` must match what was passed to
    /// [`Backend::acquire`] so the slot returns to the correct bucket.
    fn release(
        &mut self,
        id: ResourceId,
        ty: PortType,
        format: Option<GpuTextureFormat>,
        dims: (u32, u32),
    );

    /// Install the set of resources whose `Texture2D` slots must carry a
    /// full mip chain (producer declared
    /// [`EffectNode::output_mipmapped`](crate::node_graph::EffectNode::output_mipmapped);
    /// recorded per plan by `compile()`). The executor calls this before
    /// any `acquire` each frame; the backend consults it inside
    /// `acquire`/`release` so mipped and flat slots never share a recycle
    /// bucket. Default no-op — mock backends have no real textures, so
    /// mip-chained allocation doesn't change their observable slot
    /// arithmetic. IMPORT_FIDELITY F-P6.
    fn declare_mipmapped(&mut self, _ids: &[ResourceId]) {}

    /// Currently bound slot for `id`, or `None` if unbound.
    fn slot_for(&self, id: ResourceId) -> Option<Slot>;

    /// High-water mark — total physical slots ever allocated. Useful for
    /// asserting that resource recycling actually happens in tests, and as
    /// a baseline pool-size hint for production backends.
    fn slot_count(&self) -> u32;

    /// Canvas dimensions — the output texture size this backend was
    /// constructed (or resized) against. Used by primitives that need
    /// to size their dispatch or allocate buffers to match the canvas
    /// (scatter accumulators, fluid sim grids, density textures —
    /// anything whose output must align pixel-for-pixel with the final
    /// frame). Returns `(0, 0)` on mock backends with no allocation
    /// state; production backends return the host's actual canvas dims.
    fn canvas_dims(&self) -> (u32, u32) {
        (0, 0)
    }

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

    /// Real 3D `GpuTexture` bound to a slot, if this backend tracks 3D
    /// textures and the slot was allocated as a `Texture3D`. Mock
    /// backends return `None`.
    ///
    /// Unlike `texture_2d`, there is no lazy-alloc path: the host
    /// pre-binds every Texture3D resource via
    /// [`MetalBackend::pre_bind_texture_3d`] before the chain runs.
    /// Volume dimensions vary per use case (volume resolution, depth
    /// thickness), so the pool can't fabricate one without per-primitive
    /// metadata that doesn't exist yet.
    fn texture_3d(&self, _slot: Slot) -> Option<&GpuTexture> {
        None
    }

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

    /// Swap the owned textures bound at two PERSISTENT slots — the
    /// zero-copy feedback ping-pong primitive: `node.feedback` requests
    /// this from `late_capture` so its `out` slot (read next frame)
    /// adopts the back-edge producer's fresh write while the producer's
    /// slot adopts the old `out` texture to overwrite next frame. Both
    /// slots must hold owned textures and neither may carry a borrowed
    /// shadow. Returns `false` (no-op) when either condition fails —
    /// callers fall back to the copy path. Mock backends: `false`.
    fn swap_texture_2d(&mut self, _a: Slot, _b: Slot) -> bool {
        false
    }

    /// Write a scalar value into a slot. The runtime invokes this after
    /// a control-rate node's `evaluate` returns, draining the
    /// per-step scratch buffer populated via
    /// [`NodeOutputs::set_scalar`](crate::node_graph::NodeOutputs::set_scalar).
    /// Downstream nodes read the value through
    /// [`Backend::scalar`] in the same frame — control wires evaluate
    /// synchronously in topological order.
    fn set_scalar(&mut self, slot: Slot, value: ParamValue);

    /// [`Camera`] value bound to a slot. Mirrors `scalar` for the
    /// [`PortType::Camera`] wire shape — CPU-only struct payload.
    /// Mock and default impls return `None`.
    fn camera(&self, _slot: Slot) -> Option<Camera> {
        None
    }

    /// Write a [`Camera`] value into a slot. Drained from the per-step
    /// scratch by the executor, same shape as `set_scalar`.
    fn set_camera(&mut self, _slot: Slot, _value: Camera) {}

    /// [`Light`] value bound to a slot. Mirrors `camera` for the
    /// [`PortType::Light`] wire shape — CPU-only struct payload set by
    /// the producing light primitive's evaluate and drained by the
    /// executor before consumers run. Default impls return `None`.
    fn light(&self, _slot: Slot) -> Option<Light> {
        None
    }

    /// Write a [`Light`] value into a slot. Drained from the per-step
    /// scratch by the executor, same shape as `set_camera` / `set_scalar`.
    fn set_light(&mut self, _slot: Slot, _value: Light) {}

    /// [`Material`] value bound to a slot. Mirrors `light` / `camera` for the
    /// [`PortType::Material`] wire shape — CPU-only struct payload set by
    /// the producing material atom's evaluate and drained by the executor
    /// before consumers run. Default impls return `None`.
    fn material(&self, _slot: Slot) -> Option<Material> {
        None
    }

    /// Write a [`Material`] value into a slot. Drained from the per-step
    /// scratch by the executor, same shape as `set_light` / `set_camera`.
    fn set_material(&mut self, _slot: Slot, _value: Material) {}

    /// [`Transform`] value bound to a slot. Mirrors `material` for the
    /// [`PortType::Transform`] wire shape — CPU-only struct payload set by
    /// the producing `node.transform_3d` atom's evaluate and drained by the
    /// executor before consumers run. Default impls return `None`.
    fn transform(&self, _slot: Slot) -> Option<Transform> {
        None
    }

    /// Write a [`Transform`] value into a slot. Drained from the per-step
    /// scratch by the executor, same shape as `set_material` / `set_light`.
    fn set_transform(&mut self, _slot: Slot, _value: Transform) {}

    /// [`Atmosphere`] value bound to a slot. Mirrors `transform` for the
    /// [`PortType::Atmosphere`] wire shape — CPU-only struct payload set by the
    /// producing `node.atmosphere` atom's evaluate and drained by the executor
    /// before consumers run. Default impls return `None`.
    fn atmosphere(&self, _slot: Slot) -> Option<Atmosphere> {
        None
    }

    /// Write an [`Atmosphere`] value into a slot. Drained from the per-step
    /// scratch by the executor, same shape as `set_transform`.
    fn set_atmosphere(&mut self, _slot: Slot, _value: Atmosphere) {}

    /// [`SceneObject`] value bound to a slot. Mirrors `atmosphere` for the
    /// [`PortType::Object`] wire shape — CPU-only struct payload set by
    /// `node.scene_object`'s evaluate and drained by the executor before
    /// consumers run. Default impls return `None`.
    fn object(&self, _slot: Slot) -> Option<SceneObject> {
        None
    }

    /// Write a [`SceneObject`] value into a slot. Drained from the per-step
    /// scratch by the executor, same shape as `set_atmosphere`.
    fn set_object(&mut self, _slot: Slot, _value: SceneObject) {}

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

/// Slot-recycling pool key. Texture2D slots distinguish on
/// `(PortType, GpuTextureFormat, dims)`; all other slot kinds
/// collapse to `(PortType, None, (0, 0))` so the format and dims
/// axes don't fragment their recycle pools.
pub(crate) type PoolKey = (PortType, Option<GpuTextureFormat>, (u32, u32), bool);

/// Build the per-(type, format, dims, mipmapped) slot-recycling pool key.
/// `format`, `dims` and `mipmapped` are meaningful for `Texture2D` slots
/// only; for every other port type all three axes are normalized away so
/// non-texture slots pool by type alone (avoids fragmenting the
/// recycle pool by irrelevant axes). `mipmapped` keys mip-chained
/// material-map slots (IMPORT_FIDELITY F-P6) apart from flat ones — a
/// flat consumer recycled into a mipped slot would sample stale mip
/// tails, and vice versa a mipped consumer would sample garbage.
pub(crate) fn pool_key(
    ty: PortType,
    format: Option<GpuTextureFormat>,
    dims: (u32, u32),
    mipmapped: bool,
) -> PoolKey {
    // Both Texture2D variants share the same pool — the channel
    // signature is a validator concern, not a GPU allocation one.
    // Normalize Texture2DTyped down to Texture2D so a typed producer
    // and an untyped slot recycle through the same pool entry.
    if ty.is_texture_2d() {
        (PortType::Texture2D, format, dims, mipmapped)
    } else {
        (ty, None, (0, 0), false)
    }
}

/// In-memory backend with no real GPU resources. Tracks slot identity and
/// per-type recycling — same logic as step 4's `ResourcePool`, now behind
/// the [`Backend`] trait.
///
/// Used by every test in the `node_graph` module. Production code uses a
/// future `MetalBackend` that wraps `manifold_gpu::RenderTargetPool`.
pub struct MockBackend {
    free_by_type: AHashMap<PoolKey, Vec<Slot>>,
    bound: AHashMap<ResourceId, Slot>,
    next_slot: u32,
    /// Scalar values written via [`Backend::set_scalar`]. Mock has no
    /// real GPU resources, but the scalar map is needed so tests can
    /// observe control-wire dataflow without a Metal device.
    scalars: AHashMap<Slot, ParamValue>,
    /// Camera values written via [`Backend::set_camera`] — same shape.
    cameras: AHashMap<Slot, Camera>,
    /// Light values written via [`Backend::set_light`] — same shape.
    lights: AHashMap<Slot, Light>,
    /// Material values written via [`Backend::set_material`] — same shape.
    materials: AHashMap<Slot, Material>,
    /// Transform values written via [`Backend::set_transform`] — same shape.
    transforms: AHashMap<Slot, Transform>,
    /// Atmosphere values written via [`Backend::set_atmosphere`] — same shape.
    atmospheres: AHashMap<Slot, Atmosphere>,
    /// SceneObject values written via [`Backend::set_object`] — same shape.
    objects: AHashMap<Slot, SceneObject>,
    /// Skip-passthrough aliases installed this frame via
    /// [`Backend::alias_2d`]. Mock has no textures to re-point; recording
    /// the pairs (and returning `true`) lets executor tests observe the
    /// alias path the way `MetalBackend` takes it.
    skip_aliases: Vec<(Slot, Slot)>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            free_by_type: AHashMap::default(),
            bound: AHashMap::default(),
            next_slot: 0,
            scalars: AHashMap::default(),
            cameras: AHashMap::default(),
            lights: AHashMap::default(),
            materials: AHashMap::default(),
            transforms: AHashMap::default(),
            atmospheres: AHashMap::default(),
            objects: AHashMap::default(),
            skip_aliases: Vec::new(),
        }
    }

    /// Skip-passthrough aliases installed this frame (test observation).
    pub fn skip_aliases(&self) -> &[(Slot, Slot)] {
        &self.skip_aliases
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for MockBackend {
    fn acquire(
        &mut self,
        id: ResourceId,
        ty: PortType,
        format: Option<GpuTextureFormat>,
        dims: (u32, u32),
    ) -> Slot {
        // Idempotent on existing bindings — mirrors `MetalBackend`.
        if let Some(&slot) = self.bound.get(&id) {
            return slot;
        }
        let key = pool_key(ty, format, dims, false);
        let pool = self.free_by_type.entry(key).or_default();
        let slot = pool.pop().unwrap_or_else(|| {
            let s = Slot(self.next_slot);
            self.next_slot += 1;
            s
        });
        self.bound.insert(id, slot);
        slot
    }

    fn release(
        &mut self,
        id: ResourceId,
        ty: PortType,
        format: Option<GpuTextureFormat>,
        dims: (u32, u32),
    ) {
        if let Some(slot) = self.bound.remove(&id) {
            let key = pool_key(ty, format, dims, false);
            self.free_by_type.entry(key).or_default().push(slot);
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
        self.scalars.get(&slot).cloned()
    }

    fn set_scalar(&mut self, slot: Slot, value: ParamValue) {
        self.scalars.insert(slot, value);
    }

    fn camera(&self, slot: Slot) -> Option<Camera> {
        self.cameras.get(&slot).copied()
    }

    fn set_camera(&mut self, slot: Slot, value: Camera) {
        self.cameras.insert(slot, value);
    }

    fn light(&self, slot: Slot) -> Option<Light> {
        self.lights.get(&slot).copied()
    }

    fn set_light(&mut self, slot: Slot, value: Light) {
        self.lights.insert(slot, value);
    }

    fn material(&self, slot: Slot) -> Option<Material> {
        self.materials.get(&slot).copied()
    }

    fn set_material(&mut self, slot: Slot, value: Material) {
        self.materials.insert(slot, value);
    }

    fn transform(&self, slot: Slot) -> Option<Transform> {
        self.transforms.get(&slot).copied()
    }

    fn set_transform(&mut self, slot: Slot, value: Transform) {
        self.transforms.insert(slot, value);
    }

    fn atmosphere(&self, slot: Slot) -> Option<Atmosphere> {
        self.atmospheres.get(&slot).copied()
    }

    fn set_atmosphere(&mut self, slot: Slot, value: Atmosphere) {
        self.atmospheres.insert(slot, value);
    }

    fn object(&self, slot: Slot) -> Option<SceneObject> {
        self.objects.get(&slot).copied()
    }

    fn set_object(&mut self, slot: Slot, value: SceneObject) {
        self.objects.insert(slot, value);
    }

    fn alias_2d(&mut self, src_slot: Slot, dst_slot: Slot) -> bool {
        self.skip_aliases.push((src_slot, dst_slot));
        true
    }

    fn clear_skip_aliases(&mut self) {
        self.skip_aliases.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::ScalarType;

    /// Canvas-sized dims used by tests that don't care about
    /// resolution — matches the dim every pre-Phase-3 acquire would
    /// have resolved to via the new "None means canvas at acquire
    /// time" rule.
    const CANVAS: (u32, u32) = (1920, 1080);

    #[test]
    fn reuses_freed_slot_of_matching_type() {
        let mut b = MockBackend::new();
        let s0 = b.acquire(ResourceId(0), PortType::Texture2D, None, CANVAS);
        b.release(ResourceId(0), PortType::Texture2D, None, CANVAS);
        let s1 = b.acquire(ResourceId(1), PortType::Texture2D, None, CANVAS);
        assert_eq!(s0, s1);
        assert_eq!(b.slot_count(), 1);
    }

    #[test]
    fn does_not_cross_type_boundaries() {
        let mut b = MockBackend::new();
        b.acquire(ResourceId(0), PortType::Texture2D, None, CANVAS);
        b.release(ResourceId(0), PortType::Texture2D, None, CANVAS);
        let s = b.acquire(ResourceId(1), PortType::Texture3D, None, CANVAS);
        assert_eq!(s.0, 1);
        assert_eq!(b.slot_count(), 2);
    }

    #[test]
    fn scalar_subtypes_pool_independently() {
        // F32 and Vec3 scalars live in different physical-buffer kinds even
        // though they're both Scalar-flavoured.
        let mut b = MockBackend::new();
        b.acquire(ResourceId(0), PortType::Scalar(ScalarType::F32), None, CANVAS);
        b.release(ResourceId(0), PortType::Scalar(ScalarType::F32), None, CANVAS);
        let s = b.acquire(ResourceId(1), PortType::Scalar(ScalarType::Vec3), None, CANVAS);
        assert_eq!(s.0, 1);
        assert_eq!(b.slot_count(), 2);
    }

    #[test]
    fn clear_resets_bindings_but_preserves_high_water_mark() {
        let mut b = MockBackend::new();
        b.acquire(ResourceId(0), PortType::Texture2D, None, CANVAS);
        b.acquire(ResourceId(1), PortType::Texture2D, None, CANVAS);
        b.clear();
        assert!(b.slot_for(ResourceId(0)).is_none());
        // After clear, the next acquire allocates a fresh slot rather than
        // reusing — the cleared bindings don't repopulate the free pool.
        let s = b.acquire(ResourceId(2), PortType::Texture2D, None, CANVAS);
        assert_eq!(s.0, 2);
    }

    #[test]
    fn texture2d_pools_separately_by_format() {
        // A freed slot allocated as rgba16float must NOT be reused for an
        // acquire requesting rgba32float — handing back the wrong-format
        // texture would silently corrupt downstream reads.
        let mut b = MockBackend::new();
        let _s0 = b.acquire(
            ResourceId(0),
            PortType::Texture2D,
            Some(GpuTextureFormat::Rgba16Float),
            CANVAS,
        );
        b.release(
            ResourceId(0),
            PortType::Texture2D,
            Some(GpuTextureFormat::Rgba16Float),
            CANVAS,
        );
        let s1 = b.acquire(
            ResourceId(1),
            PortType::Texture2D,
            Some(GpuTextureFormat::Rgba32Float),
            CANVAS,
        );
        assert_eq!(s1.0, 1, "different-format acquire must allocate a fresh slot");
        assert_eq!(b.slot_count(), 2);
    }

    #[test]
    fn non_texture_ports_ignore_format() {
        // Scalar/Array slots aren't textures — the format parameter
        // shouldn't fragment their recycle pool.
        let mut b = MockBackend::new();
        b.acquire(
            ResourceId(0),
            PortType::Scalar(ScalarType::F32),
            Some(GpuTextureFormat::Rgba16Float),
            CANVAS,
        );
        b.release(
            ResourceId(0),
            PortType::Scalar(ScalarType::F32),
            Some(GpuTextureFormat::Rgba32Float),
            CANVAS,
        );
        let s = b.acquire(
            ResourceId(1),
            PortType::Scalar(ScalarType::F32),
            None,
            CANVAS,
        );
        assert_eq!(s.0, 0, "non-texture slots should recycle regardless of format hint");
    }

    #[test]
    fn texture2d_pools_separately_by_dims() {
        // Phase-3 invariant: a freed full-res rgba16float slot must
        // NOT be reused for a quarter-res rgba16float acquire — the
        // backing texture is allocated at a specific (width, height)
        // and handing it back at the wrong dims would crash on
        // dispatch or silently sample garbage.
        let mut b = MockBackend::new();
        let full = (1920u32, 1080u32);
        let quarter = (480u32, 270u32);
        let _s0 = b.acquire(ResourceId(0), PortType::Texture2D, None, full);
        b.release(ResourceId(0), PortType::Texture2D, None, full);
        let s1 = b.acquire(ResourceId(1), PortType::Texture2D, None, quarter);
        assert_eq!(s1.0, 1, "different-dims acquire must allocate a fresh slot");
        assert_eq!(b.slot_count(), 2);
    }
}
