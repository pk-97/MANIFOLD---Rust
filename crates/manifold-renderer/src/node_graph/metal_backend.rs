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

use ahash::AHashMap;
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
pub struct MetalBackend {
    device: Arc<GpuDevice>,
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
            device,
            pool: RenderTargetPool::new(),
            width,
            height,
            format,
            free_by_type: AHashMap::default(),
            bound: AHashMap::default(),
            next_slot: 0,
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
    pub fn device(&self) -> &GpuDevice {
        &self.device
    }

    /// Pre-bind a real `RenderTarget` to a slot. Used by the host to feed
    /// the input frame into `Source`'s output slot before each frame.
    /// The previous binding (if any) is returned to the pool.
    pub fn install_texture_2d(&mut self, slot: Slot, target: RenderTarget) {
        if let Some(old) = self.textures_2d.insert(slot, target) {
            self.pool.release(old);
        }
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
        self.next_slot = 0;
    }
}

impl Backend for MetalBackend {
    fn acquire(&mut self, id: ResourceId, ty: PortType) -> Slot {
        let pool = self.free_by_type.entry(ty).or_default();
        let slot = pool.pop().unwrap_or_else(|| {
            let s = Slot(self.next_slot);
            self.next_slot += 1;
            s
        });
        self.bound.insert(id, slot);

        // Lazily allocate a real backing resource for fresh Texture2D
        // slots. Reused slots already have their RenderTarget retained.
        if matches!(ty, PortType::Texture2D)
            && let std::collections::hash_map::Entry::Vacant(e) =
                self.textures_2d.entry(slot)
        {
            let rt = self
                .pool
                .get(&self.device, self.width, self.height, self.format, "node_graph");
            e.insert(rt);
        }

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
        // Drop bindings and free pools but retain backing textures so
        // subsequent acquires of the same slots don't re-allocate.
        self.bound.clear();
        self.free_by_type.clear();
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
