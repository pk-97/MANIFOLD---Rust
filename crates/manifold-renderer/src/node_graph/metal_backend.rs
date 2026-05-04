//! [`MetalBackend`] — production [`Backend`] implementation using
//! `manifold_gpu`'s `RenderTargetPool` for real `GpuTexture` allocation.
//!
//! This is the first file in `node_graph/` that imports types from outside
//! the module (it pulls in `manifold_gpu::GpuTexture` / `GpuDevice` and
//! `crate::render_target::RenderTarget`). The rest of `node_graph/`
//! remains backend-agnostic and routes through the [`Backend`] trait.
//!
//! ## Lifecycle
//!
//! The backend's slot-recycling logic is identical to [`MockBackend`]:
//! per-`PortType` free pools, monotonic high-water mark, idempotent
//! release. The difference is what a slot *physically* is — for
//! `Texture2D`, a real `RenderTarget` allocated against the host's
//! `GpuDevice` (and optionally an `MTLHeap`-backed `TexturePool`).
//!
//! Textures are allocated lazily on first acquire of a fresh slot and
//! retained across acquire/release cycles for the slot — releasing a
//! slot returns its abstract identity to the free pool, but the
//! underlying `RenderTarget` stays available for the next acquire of the
//! same slot. Cleanup happens on `drop` or via [`MetalBackend::clear`].
//!
//! ## What's not yet integrated
//!
//! - `Texture3D` and `Scalar` resource backing. Only `Texture2D` allocates
//!   real GPU resources; the rest fall back to mock semantics.
//! - The host wiring that pre-binds the input frame to `Source`'s output
//!   slot before each frame and reads `FinalOutput`'s input slot afterward.
//!   That comes in the next step alongside the `GpuEncoder` plumbing.

use std::sync::Arc;

use ahash::{AHashMap, AHashSet};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, TexturePool};

use crate::node_graph::backend::Backend;
use crate::node_graph::bindings::Slot;
use crate::node_graph::execution_plan::ResourceId;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::ports::PortType;
use crate::render_target::RenderTarget;
use crate::render_target_pool::RenderTargetPool;

/// `Backend` impl that allocates real `GpuTexture`s via
/// `RenderTargetPool`. Used by production code paths.
///
/// `device` is optional. With `Some(device)` the backend lazy-allocates a
/// `RenderTarget` on first acquire of a fresh slot — used by the
/// integration tests that build a MetalBackend up-front. With `None`
/// (`without_device`) the host must pre-bind every Texture2D resource via
/// `pre_bind_texture_2d`; lazy-alloc would panic. The "no device" path is
/// what the live renderer uses, since it constructs effects through
/// `EffectFactory`'s `&GpuDevice` (no `Arc`) at registry-build time.
pub struct MetalBackend {
    device: Option<Arc<GpuDevice>>,
    pool: RenderTargetPool,

    /// Render resolution and format used for `Texture2D` slot allocations.
    /// All Texture2D resources in a graph instance share these dimensions
    /// — they're "the project's render resolution". Future versions may
    /// support per-port resolution overrides for things like Bloom's mip
    /// levels (which currently encode multiple resolutions inside a single
    /// node-managed mip chain).
    width: u32,
    height: u32,
    format: GpuTextureFormat,

    // ---- Slot-recycling logic (mirrors MockBackend) ----
    free_by_type: AHashMap<PortType, Vec<Slot>>,
    bound: AHashMap<ResourceId, Slot>,
    next_slot: u32,

    /// Resource IDs that the host has pinned to a host-supplied texture
    /// via `pre_bind_texture_2d`. The executor's `release` is a no-op for
    /// these — their bindings persist across frames so the host can call
    /// `pre_bind_texture_2d` once at graph construction (instead of every
    /// frame, which would leak slots).
    pinned: AHashSet<ResourceId>,

    // ---- Real backing storage ----
    textures_2d: AHashMap<Slot, RenderTarget>,
    scalars: AHashMap<Slot, ParamValue>,
}

impl MetalBackend {
    /// Construct a backend tied to a specific `GpuDevice`, render
    /// resolution, and texture format. Width/height are the dimensions of
    /// every `Texture2D` slot the backend allocates.
    pub fn new(
        device: Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
    ) -> Self {
        Self {
            device: Some(device),
            pool: RenderTargetPool::new(),
            width,
            height,
            format,
            free_by_type: AHashMap::default(),
            bound: AHashMap::default(),
            next_slot: 0,
            pinned: AHashSet::default(),
            textures_2d: AHashMap::default(),
            scalars: AHashMap::default(),
        }
    }

    /// Construct a backend with no internal device. The host MUST
    /// pre-bind every Texture2D resource via `pre_bind_texture_2d`
    /// before `execute_frame_with_gpu` — lazy-allocation on a fresh
    /// `Texture2D` slot panics in this mode. Used by the live
    /// renderer's effect path, where `EffectFactory` hands out
    /// `&GpuDevice` (no `Arc`) at construction.
    pub fn without_device(width: u32, height: u32, format: GpuTextureFormat) -> Self {
        Self {
            device: None,
            pool: RenderTargetPool::new(),
            width,
            height,
            format,
            free_by_type: AHashMap::default(),
            bound: AHashMap::default(),
            next_slot: 0,
            pinned: AHashSet::default(),
            textures_2d: AHashMap::default(),
            scalars: AHashMap::default(),
        }
    }

    /// Set the underlying heap-backed `TexturePool` so allocation goes
    /// through `MTLHeap` sub-allocation rather than direct device calls.
    /// Call once before the first acquire if heap pooling is desired.
    pub fn set_texture_pool(&mut self, pool: &TexturePool) {
        self.pool.set_texture_pool(pool);
    }

    /// Borrow the `GpuDevice` for nodes that need to allocate their own
    /// internal resources (e.g. FluidSim's persistent density grid).
    /// Returns `None` if the backend was constructed via
    /// [`MetalBackend::without_device`].
    pub fn device(&self) -> Option<&GpuDevice> {
        self.device.as_deref()
    }

    /// Pre-bind a real `RenderTarget` to a slot. Used by the host to feed
    /// the input frame into `Source`'s output slot before each frame.
    /// The previous binding (if any) is returned to the pool.
    pub fn install_texture_2d(&mut self, slot: Slot, target: RenderTarget) {
        if let Some(old) = self.textures_2d.insert(slot, target) {
            self.pool.release(old);
        }
    }

    /// Bind a [`ResourceId`] (from the executor's plan) to a host-supplied
    /// [`RenderTarget`]. Allocates a fresh `Slot`, stores `target` there,
    /// and records the binding so the executor's next `acquire(id, ...)`
    /// is idempotent and returns this slot instead of pulling a default
    /// texture from the pool.
    ///
    /// Used by the host to feed input frames into [`Source`] nodes:
    /// after `compile`, the host looks up `Source.out`'s `ResourceId` in
    /// the plan and pre-binds the camera/decoder texture to it via this
    /// method. Re-call each frame to swap in the next frame's input.
    /// Pair with [`Backend::acquire`]'s idempotency on existing bindings.
    ///
    /// [`Source`]: crate::node_graph::Source
    pub fn pre_bind_texture_2d(&mut self, id: ResourceId, target: RenderTarget) -> Slot {
        let slot = Slot(self.next_slot);
        self.next_slot += 1;
        self.textures_2d.insert(slot, target);
        self.bound.insert(id, slot);
        self.pinned.insert(id);
        slot
    }

    /// Borrow the `RenderTarget` bound to a slot, if any. Used by the host
    /// to read `FinalOutput`'s input slot after each frame.
    pub fn render_target_2d(&self, slot: Slot) -> Option<&RenderTarget> {
        self.textures_2d.get(&slot)
    }

    /// Set a scalar value for a slot (e.g. driven by an upstream scalar
    /// output port).
    pub fn set_scalar(&mut self, slot: Slot, value: ParamValue) {
        self.scalars.insert(slot, value);
    }

    /// Return all retained textures and scalars to their pools and drop
    /// the high-water mark. Call on graph topology change or shutdown.
    pub fn drop_all_resources(&mut self) {
        for (_, rt) in self.textures_2d.drain() {
            self.pool.release(rt);
        }
        self.scalars.clear();
        self.bound.clear();
        self.free_by_type.clear();
        self.pinned.clear();
        self.next_slot = 0;
    }
}

impl Backend for MetalBackend {
    fn acquire(&mut self, id: ResourceId, ty: PortType) -> Slot {
        // Idempotent: if `id` is already bound (host pre-bound a frame
        // input via `pre_bind_texture_2d`, or this is a duplicate
        // acquire within the same frame), return the existing slot
        // rather than pulling a fresh one from the pool.
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

        // Lazily allocate a real backing resource for fresh Texture2D
        // slots. Reused slots already have their RenderTarget retained.
        // Requires `Some(device)` — `without_device` mode expects all
        // Texture2D resources to have been pre-bound via
        // `pre_bind_texture_2d`.
        if matches!(ty, PortType::Texture2D)
            && let std::collections::hash_map::Entry::Vacant(e) =
                self.textures_2d.entry(slot)
        {
            let device = self.device.as_deref().expect(
                "MetalBackend lazy-alloc requires a device — use `pre_bind_texture_2d` for every Texture2D resource when constructing via `without_device`",
            );
            let rt = self
                .pool
                .get(device, self.width, self.height, self.format, "node_graph");
            e.insert(rt);
        }

        slot
    }

    fn release(&mut self, id: ResourceId, ty: PortType) {
        // Host-pinned resources (frame inputs pre-bound by the renderer)
        // stay bound across frames — the host owns their lifetime.
        if self.pinned.contains(&id) {
            return;
        }
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
        // Drop bindings and free pools but retain backing textures so
        // subsequent acquires of the same slots don't re-allocate.
        // Pinned bindings are wiped too — host must re-pre-bind if it
        // wants the binding back.
        self.bound.clear();
        self.free_by_type.clear();
        self.pinned.clear();
    }

    fn texture_2d(&self, slot: Slot) -> Option<&GpuTexture> {
        self.textures_2d.get(&slot).map(|rt| &rt.texture)
    }

    fn scalar(&self, slot: Slot) -> Option<ParamValue> {
        self.scalars.get(&slot).copied()
    }
}

impl Drop for MetalBackend {
    fn drop(&mut self) {
        self.drop_all_resources();
    }
}
