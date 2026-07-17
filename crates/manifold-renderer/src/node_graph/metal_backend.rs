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
//! - `Texture3D` resources are pre-bind-only (mirror of `pre_bind_array`).
//!   No lazy-alloc — the host pre-binds every volume via
//!   [`MetalBackend::pre_bind_texture_3d`] before the chain runs.
//!   `manifold-gpu`'s `GpuDevice::create_texture` supports 3D fully (used
//!   by the existing FluidSim3D atomic generator); the gap was just at
//!   the graph-runtime layer, now closed.
//! - `Scalar` resource backing falls back to mock semantics today.
//! - The host wiring that pre-binds the input frame to `Source`'s output
//!   slot before each frame and reads `FinalOutput`'s input slot afterward.
//!   That comes in the next step alongside the `GpuEncoder` plumbing.

use ahash::{AHashMap, AHashSet};
use manifold_gpu::{GpuBuffer, GpuDevice, GpuTexture, GpuTextureFormat, TexturePool};
use std::sync::Arc;

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
    /// Shared handle to the content thread's GpuDevice (or `None` for
    /// the `without_device` test path). An `Arc` clone instead of a cached
    /// raw pointer means this survives any future move of the owning
    /// `GpuDevice`/`ContentPipeline` (BUG-054).
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
    /// Free pool keyed by `(PortType, GpuTextureFormat)` — Texture2D
    /// slots with different formats never alias each other, which would
    /// otherwise let a downstream node read through a wrong-format texture
    /// and silently corrupt the frame. `format = None` is the natural
    /// "use the backend's default format" key for non-Texture2D ports
    /// and for Texture2D producers that didn't override the format.
    free_by_type: AHashMap<crate::node_graph::backend::PoolKey, Vec<Slot>>,
    bound: AHashMap<ResourceId, Slot>,
    next_slot: u32,

    /// Resources whose `Texture2D` slot must carry a full mip chain —
    /// installed per plan via [`Backend::declare_mipmapped`], consulted by
    /// `acquire`/`release` so mipped and flat slots pool separately.
    /// IMPORT_FIDELITY F-P6 (`node.gltf_texture_source` material maps).
    mipmapped_ids: AHashSet<ResourceId>,

    /// Resource IDs that the host has pinned to a host-supplied texture
    /// via `pre_bind_texture_2d`. The executor's `release` is a no-op for
    /// these — their bindings persist across frames so the host can call
    /// `pre_bind_texture_2d` once at graph construction (instead of every
    /// frame, which would leak slots).
    pinned: AHashSet<ResourceId>,

    // ---- Real backing storage ----
    textures_2d: AHashMap<Slot, RenderTarget>,
    /// "Borrowed" textures installed via [`Self::replace_texture_2d`].
    /// These are clones (one `Retained` bump on the underlying
    /// `MTLTexture`) of textures the *upstream* caller still owns and
    /// writes to — typically the layer compositor's tonemap output
    /// feeding into a chain graph's `Source` slot.
    ///
    /// **Must NOT** be returned to the texture pool when the backend
    /// drops: the upstream still holds its own `Retained`, so releasing
    /// here would make the pool hand the same `MTLTexture` to another
    /// caller, who would then write through it and corrupt the
    /// upstream's frame data. The visible symptom is severe glitching
    /// on downstream feedback / temporal effects, because feedback
    /// loops amplify any aliased writes catastrophically.
    ///
    /// `texture_2d(slot)` returns the borrowed texture when present,
    /// shadowing the slot's `textures_2d` entry — so the slot's
    /// original pool-allocated `RenderTarget` stays untouched and gets
    /// released back to the pool normally on drop.
    borrowed_2d: AHashMap<Slot, GpuTexture>,
    /// Slots whose `borrowed_2d` entry was installed by the runtime via
    /// [`Backend::alias_2d`] as a skip-passthrough (zero-GPU-cost
    /// "this effect is a no-op this frame, so its output slot just
    /// shadows the input slot's texture"). Cleared each frame by
    /// [`Backend::clear_skip_aliases`] so a non-skip frame's real write
    /// isn't shadowed by a stale borrow from a previous skip frame.
    ///
    /// Distinct from host-installed borrows (e.g. the chain source
    /// slot's per-frame `replace_texture_2d`): those aren't tracked
    /// here, so `clear_skip_aliases` leaves them alone.
    skip_aliased_slots: Vec<Slot>,
    scalars: AHashMap<Slot, ParamValue>,
    /// Real `GpuBuffer` backing for [`PortType::Array`] slots. Pre-bound
    /// by the chain-build code at sizes computed from each producing
    /// primitive's `max_capacity` param. No lazy-alloc path for arrays
    /// — capacity is per-instance data, not a port-static property.
    buffers_array: AHashMap<Slot, GpuBuffer>,

    /// Real `GpuTexture` backing for [`PortType::Texture3D`] slots.
    /// Pre-bound by the chain build at dimensions computed from each
    /// producing primitive's volume-resolution params. No lazy-alloc —
    /// volume size is per-instance data, not a port-static property
    /// (same reasoning as `buffers_array`).
    textures_3d: AHashMap<Slot, GpuTexture>,

    /// CPU-only [`Camera`] values written via [`Backend::set_camera`].
    /// Same shape as `scalars` — drained from per-step scratch by the
    /// executor after the producing camera primitive's `evaluate`.
    cameras: AHashMap<Slot, crate::node_graph::camera::Camera>,

    /// CPU-only [`Light`] values written via [`Backend::set_light`].
    /// Same shape as `cameras` — drained from per-step scratch by the
    /// executor after the producing light primitive's `evaluate`.
    lights: AHashMap<Slot, crate::node_graph::light::Light>,

    /// CPU-only [`Material`] values written via [`Backend::set_material`].
    /// Same shape as `lights` — drained from per-step scratch by the
    /// executor after the producing material atom's `evaluate`.
    materials: AHashMap<Slot, crate::node_graph::material::Material>,

    /// CPU-only [`Transform`] values written via [`Backend::set_transform`].
    /// Same shape as `materials` — drained from per-step scratch by the
    /// executor after the producing `node.transform_3d` atom's `evaluate`.
    transforms: AHashMap<Slot, crate::node_graph::transform::Transform>,
    /// CPU-only [`Atmosphere`] values written via [`Backend::set_atmosphere`].
    /// Same shape as `transforms` — drained after `node.atmosphere`'s
    /// `evaluate`.
    atmospheres: AHashMap<Slot, crate::node_graph::atmosphere::Atmosphere>,
    /// CPU-only [`SceneObject`] values written via [`Backend::set_object`].
    /// Same shape as `atmospheres` — drained after `node.scene_object`'s
    /// `evaluate`.
    objects: AHashMap<Slot, crate::node_graph::scene_object::SceneObject>,
}

// Safety: `MetalBackend` is only ever used on the content thread; the
// ObjC-backed texture/buffer handles it holds (`GpuTexture`/`GpuBuffer` etc.)
// aren't automatically `Send`, but never cross a thread boundary in practice.
// `device` no longer needs a safety argument of its own — it's an `Arc`
// clone, safe on any thread `GpuDevice`'s own `Send + Sync` impl allows.
unsafe impl Send for MetalBackend {}

impl MetalBackend {
    /// Construct a backend tied to a specific `GpuDevice`, render
    /// resolution, and texture format. Width/height are the dimensions of
    /// every `Texture2D` slot the backend allocates.
    ///
    /// Takes an `Arc` clone of the device (BUG-054) — the backend holds its
    /// own strong reference, so it's independent of wherever the caller's
    /// `GpuDevice` allocation ends up living.
    pub fn new(device: Arc<GpuDevice>, width: u32, height: u32, format: GpuTextureFormat) -> Self {
        Self {
            device: Some(device),
            pool: RenderTargetPool::new(),
            width,
            height,
            format,
            free_by_type: AHashMap::default(),
            bound: AHashMap::default(),
            next_slot: 0,
            mipmapped_ids: AHashSet::default(),
            pinned: AHashSet::default(),
            textures_2d: AHashMap::default(),
            borrowed_2d: AHashMap::default(),
            skip_aliased_slots: Vec::new(),
            scalars: AHashMap::default(),
            buffers_array: AHashMap::default(),
            textures_3d: AHashMap::default(),
            cameras: AHashMap::default(),
            lights: AHashMap::default(),
            materials: AHashMap::default(),
            transforms: AHashMap::default(),
            atmospheres: AHashMap::default(),
            objects: AHashMap::default(),
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
            mipmapped_ids: AHashSet::default(),
            pinned: AHashSet::default(),
            textures_2d: AHashMap::default(),
            borrowed_2d: AHashMap::default(),
            skip_aliased_slots: Vec::new(),
            scalars: AHashMap::default(),
            buffers_array: AHashMap::default(),
            textures_3d: AHashMap::default(),
            cameras: AHashMap::default(),
            lights: AHashMap::default(),
            materials: AHashMap::default(),
            transforms: AHashMap::default(),
            atmospheres: AHashMap::default(),
            objects: AHashMap::default(),
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

    /// Install a *borrowed* `GpuTexture` clone at `slot` for the
    /// current frame. Subsequent `texture_2d(slot)` calls return this
    /// texture instead of the slot's owned `RenderTarget` texture.
    /// Zero GPU allocation. Returns `false` if `slot` has no owned
    /// RT (would be a bug — caller must `allocate_slot` first).
    ///
    /// The borrowed texture is held in a *separate* map from
    /// `textures_2d`. Critical: it is **never released to the pool**
    /// on backend drop. The upstream caller (e.g. the layer
    /// compositor's tonemap output) still owns the underlying
    /// `MTLTexture`, so returning our clone to the pool would alias
    /// it into a future allocation. See [`Self::borrowed_2d`] for the
    /// failure mode this prevents.
    ///
    /// Used by `ChainGraph::run` to install the upstream input
    /// texture into the `Source` node's output slot each frame —
    /// avoids the per-chain `copy_texture_to_texture` blit that
    /// `install_texture_2d` would have caused, and avoids ending the
    /// active compute encoder (which the blit would trigger).
    pub fn replace_texture_2d(&mut self, slot: Slot, texture: GpuTexture) -> bool {
        if !self.textures_2d.contains_key(&slot) {
            return false;
        }
        // Replaces any previous borrow at this slot. Dropping the
        // previous borrowed `GpuTexture` is one atomic `Retained`
        // release — the upstream still holds its own ref so the
        // underlying `MTLTexture` stays alive across the swap.
        self.borrowed_2d.insert(slot, texture);
        true
    }

    /// Swap the `RenderTarget` at a slot, returning the previous one
    /// (or `None` if the slot was empty). Unlike
    /// [`install_texture_2d`], the previous target is **not** released
    /// to the pool — the caller takes ownership and is responsible
    /// for its lifecycle.
    ///
    /// Used by the chain's graph-runtime dispatch: each frame, the
    /// chain moves its ping/pong `RenderTarget`s into the runner's
    /// pre-bound source/output slots, executes, then moves them back
    /// out so the next effect's ping-pong swap can reuse them. Avoids
    /// the per-effect `copy_texture_to_texture` overhead that
    /// `install_texture_2d` + pool ownership would force.
    pub fn swap_texture_2d(&mut self, slot: Slot, new: RenderTarget) -> Option<RenderTarget> {
        self.textures_2d.insert(slot, new)
    }

    /// Remove and return the owned `RenderTarget` at a slot, leaving the
    /// slot empty. Used by the state harvest to MOVE a persistent target (a
    /// feedback trail) out of a dying runtime's backend into the rebuilt
    /// one — ownership and pool bookkeeping transfer, and crucially the
    /// destination slot stays an OWNED target. (A `replace_texture_2d`
    /// borrow would shadow the slot, making the feedback ping-pong's
    /// `Backend::swap_texture_2d` refuse every frame — the frozen-trail /
    /// "swap failed" spam class.)
    pub fn take_render_target(&mut self, slot: Slot) -> Option<RenderTarget> {
        self.textures_2d.remove(&slot)
    }

    /// Allocate a fresh slot with the supplied `target`. Unlike
    /// [`Self::pre_bind_texture_2d`] this does **not** bind any
    /// `ResourceId` to the slot — callers wire resources up
    /// separately via [`Self::bind_resource_to_slot`], typically after
    /// running a slot-recycling simulator against the
    /// [`ExecutionPlan`](crate::node_graph::ExecutionPlan).
    ///
    /// Used by `ChainGraph::try_build` to pre-allocate exactly K
    /// pooled `RenderTarget`s (where K = the plan's true high-water
    /// mark, after lifetime-planner recycling) and bind multiple
    /// resources to each slot. With this, an N-effect chain holds
    /// 2–3 textures resident (the ping-pong + source) instead of N+1.
    pub fn allocate_slot(&mut self, target: RenderTarget) -> Slot {
        let slot = Slot(self.next_slot);
        self.next_slot += 1;
        self.textures_2d.insert(slot, target);
        slot
    }

    /// Pin a [`ResourceId`] to a pre-existing slot. Idempotent.
    /// Pairs with [`Self::allocate_slot`] to wire the plan's logical
    /// resources to a small set of physical render targets.
    pub fn bind_resource_to_slot(&mut self, id: ResourceId, slot: Slot) {
        self.bound.insert(id, slot);
        self.pinned.insert(id);
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

    /// Pin a [`ResourceId`] to a host-supplied [`GpuBuffer`] for an
    /// [`PortType::Array`] wire. The chain-build code allocates the
    /// buffer at `(item_size × max_capacity)` bytes — reading
    /// `max_capacity` from the producing primitive's params — and
    /// hands it off to the backend here.
    ///
    /// Same lifecycle as [`Self::pre_bind_texture_2d`]: the resource
    /// is `pinned`, so [`Backend::release`] is a no-op for it. The
    /// buffer is dropped when [`Self::drop_all_resources`] runs (chain
    /// rebuild) or the backend itself drops. There is no lazy-alloc
    /// path for arrays — capacity is per-instance data the chain
    /// build must know, so the host always pre-binds.
    pub fn pre_bind_array(&mut self, id: ResourceId, buffer: GpuBuffer) -> Slot {
        let slot = Slot(self.next_slot);
        self.next_slot += 1;
        self.buffers_array.insert(slot, buffer);
        self.bound.insert(id, slot);
        self.pinned.insert(id);
        slot
    }

    /// Alias an array `ResourceId` to an already-pre-bound `Slot`. No
    /// new buffer is allocated; both resources resolve to the same
    /// physical `GpuBuffer`. Used by the chain builder for primitives
    /// that declare aliased in/out array ports (see
    /// [`crate::node_graph::EffectNode::aliased_array_io`]): the
    /// output's resource id maps to the input's slot so the simulator
    /// reads and writes the same storage in place.
    ///
    /// The aliased resource is pinned — `release()` is a no-op for it,
    /// since the underlying buffer is owned by the primary
    /// pre_bind_array call.
    pub fn alias_array_resource(&mut self, dst: ResourceId, src_slot: Slot) {
        self.bound.insert(dst, src_slot);
        self.pinned.insert(dst);
    }

    /// Borrow the `GpuBuffer` bound to an [`PortType::Array`] slot, if
    /// any. Mirrors [`Self::render_target_2d`]. Primitives read this
    /// through the [`Backend::array_buffer`] trait method via the
    /// effect-node context.
    pub fn array_buffer(&self, slot: Slot) -> Option<&GpuBuffer> {
        self.buffers_array.get(&slot)
    }

    /// Pin a [`ResourceId`] to a host-supplied 3D [`GpuTexture`] for a
    /// [`PortType::Texture3D`] wire. The chain-build code creates the
    /// volume at dimensions computed from the producing primitive's
    /// volume-resolution params, then hands it off here.
    ///
    /// Same lifecycle as [`Self::pre_bind_array`]: the resource is
    /// `pinned`, so [`Backend::release`] is a no-op for it. The texture
    /// is dropped when [`Self::drop_all_resources`] runs or the backend
    /// itself drops. There is no lazy-alloc path — volume size is
    /// per-instance data, so the host always pre-binds.
    pub fn pre_bind_texture_3d(&mut self, id: ResourceId, texture: GpuTexture) -> Slot {
        let slot = Slot(self.next_slot);
        self.next_slot += 1;
        self.textures_3d.insert(slot, texture);
        self.bound.insert(id, slot);
        self.pinned.insert(id);
        slot
    }

    /// Borrow the 3D `GpuTexture` bound to a slot, if any. Mirrors
    /// [`Self::array_buffer`]. Primitives read this through the
    /// [`Backend::texture_3d`] trait method via the effect-node context.
    pub fn texture_3d(&self, slot: Slot) -> Option<&GpuTexture> {
        self.textures_3d.get(&slot)
    }

    /// Return all retained textures and scalars to their pools and drop
    /// the high-water mark. Call on graph topology change or shutdown.
    ///
    /// Borrowed textures (installed via [`Self::replace_texture_2d`])
    /// are dropped — **not** pool-released. The upstream owner still
    /// holds its own `Retained`; pool-releasing would alias the
    /// underlying `MTLTexture` into a future allocation that some
    /// other chain would then write through.
    ///
    /// Array buffers are dropped directly (no pool yet — fresh
    /// allocations on chain rebuild). When buffer-allocation cost
    /// shows up in profiles, add a [`GpuBuffer`] pool keyed by
    /// `(item_size, item_align, capacity_bucket)`.
    pub fn drop_all_resources(&mut self) {
        for (_, rt) in self.textures_2d.drain() {
            self.pool.release(rt);
        }
        self.borrowed_2d.clear();
        self.skip_aliased_slots.clear();
        self.scalars.clear();
        self.buffers_array.clear();
        self.textures_3d.clear();
        self.bound.clear();
        self.free_by_type.clear();
        self.pinned.clear();
        self.next_slot = 0;
    }

    /// Change the size used by future lazy-allocation acquires and drop
    /// every cached resource so the next frame re-allocates at the new
    /// dimensions. Used by hosts that swap a generator's output
    /// resolution mid-session (project render-resolution change, dpi
    /// flip, etc.) — without this, lazy-allocated intermediate
    /// textures stay frozen at the construction-time size and the
    /// final pass that writes into the host's larger target only
    /// covers the original sub-rect.
    ///
    /// Callers that pre-bind any resources (e.g. JsonGraphGenerator's
    /// final-output slot) must re-pre-bind after `resize` — every
    /// pinned binding is wiped.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.drop_all_resources();
        self.pool.clear();
        self.width = width;
        self.height = height;
    }

    /// Texture format the backend allocates new Texture2D slots with.
    pub fn format(&self) -> GpuTextureFormat {
        self.format
    }
}

impl Backend for MetalBackend {
    fn acquire(
        &mut self,
        id: ResourceId,
        ty: PortType,
        format: Option<GpuTextureFormat>,
        dims: (u32, u32),
    ) -> Slot {
        // Idempotent: if `id` is already bound (host pre-bound a frame
        // input via `pre_bind_texture_2d`, or this is a duplicate
        // acquire within the same frame), return the existing slot
        // rather than pulling a fresh one from the pool.
        if let Some(&slot) = self.bound.get(&id) {
            return slot;
        }
        let mipmapped = ty.is_texture_2d() && self.mipmapped_ids.contains(&id);
        let key = crate::node_graph::backend::pool_key(ty, format, dims, mipmapped);
        let pool = self.free_by_type.entry(key).or_default();
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
        // `pre_bind_texture_2d`. The requested `format` (if any) overrides
        // the backend's default — that's how Tier 3 escape-hatch nodes
        // pull native-precision textures (`r32float`, `rgba32float`).
        // `dims` is honoured for the allocation size — pool-keyed so
        // a quarter-res slot doesn't collide with a full-res one.
        if ty.is_texture_2d()
            && let std::collections::hash_map::Entry::Vacant(e) = self.textures_2d.entry(slot)
        {
            // Clone the Arc (cheap refcount bump) rather than going through
            // self.device() (which would borrow &self) so we can hold
            // `&mut self.pool` alongside.
            let device = self.device.clone().expect(
                "MetalBackend lazy-alloc requires a device — use `pre_bind_texture_2d` for every Texture2D resource when constructing via `without_device`",
            );
            let device: &GpuDevice = &device;
            let alloc_format = format.unwrap_or(self.format);
            let rt = self
                .pool
                .get(device, dims.0, dims.1, alloc_format, mipmapped, "node_graph");
            e.insert(rt);
        }

        slot
    }

    fn release(
        &mut self,
        id: ResourceId,
        ty: PortType,
        format: Option<GpuTextureFormat>,
        dims: (u32, u32),
    ) {
        // Host-pinned resources (frame inputs pre-bound by the renderer)
        // stay bound across frames — the host owns their lifetime.
        if self.pinned.contains(&id) {
            return;
        }
        if let Some(slot) = self.bound.remove(&id) {
            let mipmapped = ty.is_texture_2d() && self.mipmapped_ids.contains(&id);
            let key = crate::node_graph::backend::pool_key(ty, format, dims, mipmapped);
            self.free_by_type.entry(key).or_default().push(slot);
        }
    }

    fn declare_mipmapped(&mut self, ids: &[ResourceId]) {
        // Per-plan install: replace, don't accumulate — a stale id from a
        // previous plan would silently upgrade an unrelated resource.
        self.mipmapped_ids.clear();
        self.mipmapped_ids.extend(ids.iter().copied());
    }

    fn slot_for(&self, id: ResourceId) -> Option<Slot> {
        self.bound.get(&id).copied()
    }

    fn canvas_dims(&self) -> (u32, u32) {
        (self.width, self.height)
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
        // Borrowed textures (installed via `replace_texture_2d`)
        // shadow the slot's owned RT for the current frame. The owned
        // RT still exists in `textures_2d` so it can be released back
        // to the pool on drop without disturbing the borrowed clone.
        self.borrowed_2d
            .get(&slot)
            .or_else(|| self.textures_2d.get(&slot).map(|rt| &rt.texture))
    }

    fn swap_texture_2d(&mut self, a: Slot, b: Slot) -> bool {
        // Zero-copy feedback ping-pong: swap the owned RenderTargets of
        // two persistent slots. Refuse (caller falls back to copies)
        // when either slot is missing or carries a borrowed shadow —
        // swapping under a shadow would silently swap textures nobody
        // reads this frame.
        if a == b
            || self.borrowed_2d.contains_key(&a)
            || self.borrowed_2d.contains_key(&b)
            || !self.textures_2d.contains_key(&a)
            || !self.textures_2d.contains_key(&b)
        {
            return false;
        }
        let ta = self.textures_2d.remove(&a).expect("checked above");
        let tb = self.textures_2d.insert(b, ta).expect("checked above");
        self.textures_2d.insert(a, tb);
        true
    }

    fn scalar(&self, slot: Slot) -> Option<ParamValue> {
        self.scalars.get(&slot).cloned()
    }

    fn set_scalar(&mut self, slot: Slot, value: ParamValue) {
        self.scalars.insert(slot, value);
    }

    fn camera(&self, slot: Slot) -> Option<crate::node_graph::camera::Camera> {
        self.cameras.get(&slot).copied()
    }

    fn set_camera(&mut self, slot: Slot, value: crate::node_graph::camera::Camera) {
        self.cameras.insert(slot, value);
    }

    fn light(&self, slot: Slot) -> Option<crate::node_graph::light::Light> {
        self.lights.get(&slot).copied()
    }

    fn set_light(&mut self, slot: Slot, value: crate::node_graph::light::Light) {
        self.lights.insert(slot, value);
    }

    fn material(&self, slot: Slot) -> Option<crate::node_graph::material::Material> {
        self.materials.get(&slot).copied()
    }

    fn set_material(&mut self, slot: Slot, value: crate::node_graph::material::Material) {
        self.materials.insert(slot, value);
    }

    fn transform(&self, slot: Slot) -> Option<crate::node_graph::transform::Transform> {
        self.transforms.get(&slot).copied()
    }

    fn set_transform(&mut self, slot: Slot, value: crate::node_graph::transform::Transform) {
        self.transforms.insert(slot, value);
    }

    fn atmosphere(&self, slot: Slot) -> Option<crate::node_graph::atmosphere::Atmosphere> {
        self.atmospheres.get(&slot).copied()
    }

    fn set_atmosphere(&mut self, slot: Slot, value: crate::node_graph::atmosphere::Atmosphere) {
        self.atmospheres.insert(slot, value);
    }

    fn object(&self, slot: Slot) -> Option<crate::node_graph::scene_object::SceneObject> {
        self.objects.get(&slot).copied()
    }

    fn set_object(&mut self, slot: Slot, value: crate::node_graph::scene_object::SceneObject) {
        self.objects.insert(slot, value);
    }

    fn array_buffer(&self, slot: Slot) -> Option<&GpuBuffer> {
        self.buffers_array.get(&slot)
    }

    fn texture_3d(&self, slot: Slot) -> Option<&GpuTexture> {
        self.textures_3d.get(&slot)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn alias_2d(&mut self, src_slot: Slot, dst_slot: Slot) -> bool {
        // Refuse to touch a slot that has a host-installed borrow we
        // don't own (e.g. StylizedFeedback's inner output slot points
        // at the outer chain's target via `replace_texture_2d`). The
        // runtime falls back to calling `evaluate` when alias_2d
        // returns false, which is the correct behavior — overwriting
        // the host borrow would silently break whatever the host was
        // routing through that slot.
        if self.borrowed_2d.contains_key(&dst_slot) && !self.skip_aliased_slots.contains(&dst_slot)
        {
            return false;
        }

        // Look up the current texture at src_slot. Borrow shadow takes
        // priority over owned; this matches `texture_2d`'s lookup order
        // so the alias points at whatever a reader would see.
        let tex = self
            .borrowed_2d
            .get(&src_slot)
            .cloned()
            .or_else(|| self.textures_2d.get(&src_slot).map(|rt| rt.texture.clone()));
        let Some(t) = tex else {
            return false;
        };
        // Only alias into slots that actually exist (either owned or
        // already borrowed). Prevents aliasing onto an unallocated slot
        // id.
        if !self.textures_2d.contains_key(&dst_slot) && !self.borrowed_2d.contains_key(&dst_slot) {
            return false;
        }
        // Replaces any previous skip-alias on dst_slot (idempotent for
        // consecutive skip frames). Tracked for `clear_skip_aliases`
        // so the borrow auto-clears next frame and a non-skip frame's
        // real write isn't shadowed.
        self.borrowed_2d.insert(dst_slot, t);
        if !self.skip_aliased_slots.contains(&dst_slot) {
            self.skip_aliased_slots.push(dst_slot);
        }
        true
    }

    fn clear_skip_aliases(&mut self) {
        for slot in self.skip_aliased_slots.drain(..) {
            self.borrowed_2d.remove(&slot);
        }
    }
}

impl Drop for MetalBackend {
    fn drop(&mut self) {
        self.drop_all_resources();
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod array_buffer_tests {
    //! Phase A.4 of `BUFFER_PORT_PLAN`. Covers `pre_bind_array` →
    //! `array_buffer` round-trip, idempotency of acquire on a
    //! pre-bound slot, no-op release for pinned arrays, and
    //! `drop_all_resources` cleanup.

    use manifold_gpu::GpuTextureFormat;

    use super::*;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::{ArrayType, PortType};

    fn make_backend() -> (crate::TestDevice, MetalBackend) {
        let device = crate::test_device();
        let backend = MetalBackend::new(device.arc(), 16, 16, GpuTextureFormat::Rgba16Float);
        (device, backend)
    }

    fn particle_layout() -> ArrayType {
        // Canonical Particle layout. The Channels signature is load-
        // bearing — pre-bind_array keys its pool on the full
        // ArrayType (size, align, specs, match_mode) so two
        // ArrayTypes with the same byte layout but different
        // Channels signatures get separate buffers.
        ArrayType::of_known::<crate::generators::compute_common::Particle>()
    }

    /// IMPORT_FIDELITY F-P6: a resource declared mip-chained allocates its
    /// slot texture with a full mip chain, and mipped/flat slots never
    /// recycle into each other (pool-key separation) — a flat consumer in a
    /// mipped leftover would sample stale mip tails.
    #[test]
    fn declared_mipmapped_resource_allocates_a_mip_chain_and_pools_separately() {
        let (_device, mut b) = make_backend();
        b.declare_mipmapped(&[ResourceId(0)]);

        let mipped_slot = b.acquire(ResourceId(0), PortType::Texture2D, None, (64, 64));
        let mipped_tex = Backend::texture_2d(&b, mipped_slot).expect("real texture");
        assert_eq!(
            mipped_tex.mip_level_count(),
            7, // floor(log2(64)) + 1
            "declared resource must get the full mip chain"
        );

        // Release the mipped slot, then acquire an UNDECLARED resource of
        // the same (type, format, dims): it must NOT receive the recycled
        // mipped slot.
        b.release(ResourceId(0), PortType::Texture2D, None, (64, 64));
        let flat_slot = b.acquire(ResourceId(1), PortType::Texture2D, None, (64, 64));
        assert_ne!(
            mipped_slot, flat_slot,
            "flat acquire must not recycle a mip-chained slot"
        );
        let flat_tex = Backend::texture_2d(&b, flat_slot).expect("real texture");
        assert_eq!(flat_tex.mip_level_count(), 1, "undeclared resource stays flat");

        // And the declared id, re-acquired, DOES recycle its own bucket.
        let reacquired = b.acquire(ResourceId(0), PortType::Texture2D, None, (64, 64));
        assert_eq!(
            reacquired, mipped_slot,
            "mipped free-pool must hand the mipped slot back to a declared id"
        );
    }

    #[test]
    fn pre_bind_array_makes_buffer_readable_through_array_buffer() {
        let (device, mut b) = make_backend();
        let layout = particle_layout();
        let capacity = 1024u32;
        let buffer = device.create_buffer(u64::from(layout.item_size) * u64::from(capacity));
        let buf_size = buffer.size;

        let slot = b.pre_bind_array(ResourceId(0), buffer);

        let read = b.array_buffer(slot).expect("array buffer should be bound");
        assert_eq!(read.size, buf_size);
        // Also through the trait method — that's what primitive code calls.
        let trait_read = Backend::array_buffer(&b, slot).expect("trait reads same slot");
        assert_eq!(trait_read.size, buf_size);
    }

    #[test]
    fn acquire_array_after_pre_bind_is_idempotent() {
        // The chain build pre-binds an Array resource, then the
        // executor's first acquire for the same ResourceId must hand
        // back the pinned slot — not allocate a fresh one.
        let (device, mut b) = make_backend();
        let layout = particle_layout();
        let buffer = device.create_buffer(u64::from(layout.item_size) * 256);

        let pinned = b.pre_bind_array(ResourceId(0), buffer);
        // Array slots ignore dims (pool_key normalizes them); pass
        // the backend's canvas as a no-op concrete value.
        let acquired = b.acquire(ResourceId(0), PortType::Array(layout), None, (16, 16));
        assert_eq!(
            pinned, acquired,
            "acquire on a pre-bound resource must return the pinned slot",
        );
    }

    #[test]
    fn release_is_a_noop_for_pinned_array_resources() {
        // Same lifecycle as pre_bind_texture_2d: the host owns the
        // buffer lifetime; the executor's per-frame release must not
        // unpin it.
        let (device, mut b) = make_backend();
        let layout = particle_layout();
        let buffer = device.create_buffer(u64::from(layout.item_size) * 64);

        let slot = b.pre_bind_array(ResourceId(0), buffer);
        b.release(ResourceId(0), PortType::Array(layout), None, (16, 16));

        assert_eq!(
            b.slot_for(ResourceId(0)),
            Some(slot),
            "pinned array binding should survive a release",
        );
        assert!(
            b.array_buffer(slot).is_some(),
            "underlying buffer should still be reachable",
        );
    }

    #[test]
    fn pre_bind_texture_3d_makes_volume_readable_through_texture_3d() {
        let (device, mut b) = make_backend();
        let volume = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: 16,
            height: 16,
            depth: 16,
            format: GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D3,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
            label: "test_3d",
            mip_levels: 1,
        });

        let slot = b.pre_bind_texture_3d(ResourceId(0), volume);

        assert!(
            b.texture_3d(slot).is_some(),
            "pre-bound 3D texture should be readable",
        );
        assert!(
            Backend::texture_3d(&b, slot).is_some(),
            "trait reads same slot",
        );
        // 2D accessor must NOT return the 3D texture — separate storage.
        assert!(
            b.texture_2d(slot).is_none(),
            "texture_2d accessor must not see 3D textures",
        );
    }

    #[test]
    fn acquire_texture_3d_after_pre_bind_is_idempotent() {
        let (device, mut b) = make_backend();
        let volume = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: 8,
            height: 8,
            depth: 8,
            format: GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D3,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "test_3d_idempotent",
            mip_levels: 1,
        });

        let pinned = b.pre_bind_texture_3d(ResourceId(0), volume);
        // Texture3D slots ignore dims (volumes are pre-bound with
        // their own dimensions); pass canvas as no-op concrete value.
        let acquired = b.acquire(ResourceId(0), PortType::Texture3D, None, (16, 16));
        assert_eq!(
            pinned, acquired,
            "acquire on a pre-bound 3D resource must return the pinned slot",
        );
    }

    #[test]
    fn release_is_a_noop_for_pinned_texture_3d() {
        let (device, mut b) = make_backend();
        let volume = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: 4,
            height: 4,
            depth: 4,
            format: GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D3,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "test_3d_pin",
            mip_levels: 1,
        });

        let slot = b.pre_bind_texture_3d(ResourceId(0), volume);
        b.release(ResourceId(0), PortType::Texture3D, None, (16, 16));

        assert_eq!(
            b.slot_for(ResourceId(0)),
            Some(slot),
            "pinned 3D binding should survive a release",
        );
        assert!(
            b.texture_3d(slot).is_some(),
            "underlying 3D texture should still be reachable",
        );
    }

    #[test]
    fn drop_all_resources_clears_texture_3d() {
        let (device, mut b) = make_backend();
        let volume = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: 4,
            height: 4,
            depth: 4,
            format: GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D3,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "test_3d_drop",
            mip_levels: 1,
        });
        let slot = b.pre_bind_texture_3d(ResourceId(0), volume);
        assert!(b.texture_3d(slot).is_some());

        b.drop_all_resources();

        assert!(
            b.texture_3d(slot).is_none(),
            "3D texture should be dropped",
        );
    }

    #[test]
    fn drop_all_resources_clears_array_buffers() {
        let (device, mut b) = make_backend();
        let layout = particle_layout();
        let buffer = device.create_buffer(u64::from(layout.item_size) * 32);
        let slot = b.pre_bind_array(ResourceId(0), buffer);
        assert!(b.array_buffer(slot).is_some());

        b.drop_all_resources();

        assert!(
            b.array_buffer(slot).is_none(),
            "array buffer should be dropped",
        );
        assert!(
            b.slot_for(ResourceId(0)).is_none(),
            "binding should be cleared",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod alias_tests {
    //! Regression tests for [`MetalBackend::alias_2d`] +
    //! [`MetalBackend::clear_skip_aliases`] — the zero-GPU-cost
    //! skip-passthrough mechanism that replaced the per-skip
    //! `copy_texture_to_texture` blit. See `EffectNode::skip_passthrough`
    //! for the runtime hook that drives this.

    use manifold_gpu::GpuTextureFormat;

    use super::*;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::PortType;

    /// Canvas dims the helper backend is constructed with; matches
    /// what the executor would resolve `ExecutionPlan::resource_dims =
    /// None` against at acquire time.
    const W: u32 = 16;
    const H: u32 = 16;

    fn make_backend() -> (crate::TestDevice, MetalBackend) {
        let device = crate::test_device();
        let backend = MetalBackend::new(device.arc(), W, H, GpuTextureFormat::Rgba16Float);
        (device, backend)
    }

    #[test]
    fn alias_2d_makes_dst_read_through_src() {
        let (_device, mut b) = make_backend();
        let src = b.acquire(ResourceId(0), PortType::Texture2D, None, (W, H));
        let dst = b.acquire(ResourceId(1), PortType::Texture2D, None, (W, H));

        // Pre-alias, each slot has its own distinct texture. We compare
        // raw MTLTexture pointers — each acquire allocates a fresh
        // texture, so the pointers differ.
        let pre_src_ptr = b.texture_2d(src).expect("src allocated").raw_ptr();
        let pre_dst_ptr = b.texture_2d(dst).expect("dst allocated").raw_ptr();
        assert_ne!(pre_src_ptr, pre_dst_ptr);

        // After alias, dst reads what src reads — same raw pointer.
        assert!(b.alias_2d(src, dst), "alias should succeed");
        assert_eq!(
            b.texture_2d(dst).expect("dst still readable").raw_ptr(),
            pre_src_ptr,
            "dst should now shadow src's texture",
        );
        // src is unaffected.
        assert_eq!(
            b.texture_2d(src).expect("src untouched").raw_ptr(),
            pre_src_ptr
        );
    }

    #[test]
    fn clear_skip_aliases_restores_dst_to_owned_texture() {
        let (_device, mut b) = make_backend();
        let src = b.acquire(ResourceId(0), PortType::Texture2D, None, (W, H));
        let dst = b.acquire(ResourceId(1), PortType::Texture2D, None, (W, H));

        let pre_dst_ptr = b.texture_2d(dst).expect("dst allocated").raw_ptr();
        assert!(b.alias_2d(src, dst));

        // Now clear — dst should be back to its OWN texture, not the
        // alias.
        b.clear_skip_aliases();
        assert_eq!(
            b.texture_2d(dst).expect("dst still readable").raw_ptr(),
            pre_dst_ptr,
            "after clear, dst reads its owned texture again",
        );
    }

    #[test]
    fn alias_2d_refuses_to_clobber_host_installed_borrow() {
        // StylizedFeedback's inner executor pre-installs the outer
        // chain's target as a borrowed override on the inner output
        // slot. The runtime must NOT replace that borrow with a
        // skip-alias — the host borrow has different lifecycle and
        // points at off-backend data.
        let (device, mut b) = make_backend();
        let src = b.acquire(ResourceId(0), PortType::Texture2D, None, (W, H));
        let dst = b.acquire(ResourceId(1), PortType::Texture2D, None, (W, H));

        // Host installs a borrowed override on dst (simulating
        // StylizedFeedback's `replace_texture_2d` for its output slot).
        let host_tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: 16,
            height: 16,
            depth: 1,
            mip_levels: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "host-borrow",
        });
        let host_ptr = host_tex.raw_ptr();
        assert!(b.replace_texture_2d(dst, host_tex));

        // alias_2d must refuse — host borrow takes priority.
        assert!(
            !b.alias_2d(src, dst),
            "alias_2d must not clobber a host-installed borrow",
        );
        assert_eq!(
            b.texture_2d(dst).expect("dst readable").raw_ptr(),
            host_ptr,
            "host borrow survives",
        );

        // After clear_skip_aliases, the host borrow STILL survives —
        // clear only wipes runtime-installed aliases.
        b.clear_skip_aliases();
        assert_eq!(
            b.texture_2d(dst).expect("dst still readable").raw_ptr(),
            host_ptr,
            "clear_skip_aliases leaves host borrows alone",
        );
    }

    #[test]
    fn consecutive_alias_2d_calls_idempotent() {
        // A driven `amount` crossing zero on multiple frames means the
        // runtime calls alias_2d every frame the effect skips. Verify
        // the alias state stays consistent (no leak in
        // skip_aliased_slots).
        let (_device, mut b) = make_backend();
        let src = b.acquire(ResourceId(0), PortType::Texture2D, None, (W, H));
        let dst = b.acquire(ResourceId(1), PortType::Texture2D, None, (W, H));

        for _ in 0..5 {
            b.clear_skip_aliases();
            assert!(b.alias_2d(src, dst));
        }
        assert_eq!(
            b.skip_aliased_slots.len(),
            1,
            "skip_aliased_slots should contain exactly one entry (dst), not duplicated",
        );
    }
}
