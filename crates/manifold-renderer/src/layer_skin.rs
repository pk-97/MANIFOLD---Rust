//! Layer-skin registry: previous-frame composited output per layer.
//!
//! The compositor snapshots the final post-effect texture of every layer that
//! was READ as a layer source (via `get`) since the last publish. Graph
//! execution reads from the registry next frame, so a layer bound as a scene
//! object's emissive/base-color map is always the previous frame — loops
//! become one-frame feedback instead of a render-order hazard. Missing,
//! deleted, or unreferenced layers emit a 1×1 transparent-black fallback.
//!
//! Content thread only. The read set is interior mutability (`RefCell`)
//! because `get` takes `&self` — readers hold shared references through
//! `LayerSkinPtr` while the compositor owns the registry mutably.

use std::cell::RefCell;

use ahash::{AHashMap, AHashSet};
use manifold_core::LayerId;
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

struct LayerSkin {
    texture: GpuTexture,
    owned_snapshot: bool,
    visible: bool,
}

/// Previous-frame layer textures, owned and accessed by the content thread.
/// Production publication copies pixels after all graph readers finish.
pub struct LayerSkinRegistry {
    textures: AHashMap<LayerId, LayerSkin>,
    /// Layers read via `get` since the last `finish_snapshots` — the
    /// snapshot candidates for the next publish. Holds "reads since last
    /// publish", not "reads this frame": no frame-start ordering
    /// dependency, and warmup/thumbnail reads count too (harmless superset).
    reads: RefCell<AHashSet<LayerId>>,
    fallback: GpuTexture,
    /// Metal texture contents are undefined at creation — the fallback is
    /// cleared to transparent black once, lazily, at the first publish
    /// (the registry has no encoder at construction time).
    fallback_cleared: bool,
    format: GpuTextureFormat,
}

impl LayerSkinRegistry {
    /// Create a registry with a 1×1 transparent-black fallback texture.
    pub fn new(device: &GpuDevice, format: GpuTextureFormat) -> Self {
        let fallback = device.create_texture(&GpuTextureDesc {
            width: 1,
            height: 1,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::SHADER_READ,
            label: "LayerSkin fallback",
            mip_levels: 1,
        });
        Self {
            textures: AHashMap::new(),
            reads: RefCell::new(AHashSet::new()),
            fallback,
            fallback_cleared: false,
            format,
        }
    }

    /// Clear the fallback texture to transparent black exactly once. Callers
    /// with an encoder (the compositor's publish site) invoke this every
    /// frame; only the first call dispatches.
    pub fn ensure_fallback_cleared(&mut self, gpu: &mut crate::gpu_encoder::GpuEncoder) {
        if self.fallback_cleared {
            return;
        }
        gpu.clear_texture(&self.fallback, 0.0, 0.0, 0.0, 0.0);
        self.fallback_cleared = true;
    }

    /// Begin end-of-frame publication while keeping reusable snapshot storage.
    /// No graph may read the registry until `finish_snapshots` completes.
    pub(crate) fn begin_snapshots(&mut self) {
        for entry in self.textures.values_mut() {
            entry.visible = false;
        }
    }

    /// Freeze pixels, rather than retaining a render target that the next
    /// frame will overwrite. Allocation occurs only for a new layer or size.
    pub(crate) fn publish_snapshot(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder,
        layer_id: &LayerId,
        source: &GpuTexture,
    ) {
        let needs_texture = self.textures.get(layer_id).is_none_or(|entry| {
            !entry.owned_snapshot
                || entry.texture.width != source.width
                || entry.texture.height != source.height
                || entry.texture.format != source.format
        });
        if needs_texture {
            let texture = gpu.device.create_texture(&GpuTextureDesc {
                width: source.width,
                height: source.height,
                depth: 1,
                format: source.format,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::SHADER_READ,
                label: "Layer source snapshot",
                mip_levels: 1,
            });
            self.textures.insert(layer_id.clone(), LayerSkin {
                texture,
                owned_snapshot: true,
                visible: true,
            });
        }
        let entry = self.textures.get_mut(layer_id).expect("snapshot storage exists");
        gpu.copy_texture_to_texture(source, &entry.texture, source.width, source.height);
        entry.visible = true;
    }

    /// Drop sources which did not render this frame, including deleted
    /// layers, and clear the read set so the next publish tracks fresh reads.
    pub(crate) fn finish_snapshots(&mut self) {
        self.reads.borrow_mut().clear();
        self.textures.retain(|_, entry| entry.visible);
    }

    /// Whether `layer_id` was read since the last `finish_snapshots`. The
    /// compositor's publish loop uses this to snapshot only referenced
    /// layers; `publish_snapshot` itself stays unconditional.
    pub(crate) fn was_read(&self, layer_id: &LayerId) -> bool {
        self.reads.borrow().contains(layer_id)
    }

    /// Borrow the texture for `layer_id`, or the fallback if absent. Records
    /// the read BEFORE the lookup, on both hit and fallback paths — a read of
    /// a missing layer must trigger snapshotting at the next publish.
    pub fn get(&self, layer_id: &LayerId) -> &GpuTexture {
        self.reads.borrow_mut().insert(layer_id.clone());
        self.textures.get(layer_id)
            .filter(|entry| entry.visible)
            .map_or(&self.fallback, |entry| &entry.texture)
    }

    /// Discard all stored layer textures. The fallback is preserved.
    pub fn clear(&mut self) {
        self.textures.clear();
    }

    /// Number of currently stored layer textures.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.textures.len()
    }

    /// Whether no layer textures are stored.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }

    /// Format of textures stored in this registry.
    pub fn format(&self) -> GpuTextureFormat {
        self.format
    }

    /// Clone the fallback texture (e.g. for asserting its dimensions in tests).
    #[cfg(test)]
    pub fn fallback(&self) -> GpuTexture {
        self.fallback.clone()
    }
}

// The registry is Send because it only moves with the content thread, but the
// AHashMap + GpuTexture fields do not automatically implement Send in some
// configurations. The raw pointer safety argument is identical to
// LayerCompositor's LayerOutput.
unsafe impl Send for LayerSkinRegistry {}

/// A frame-scoped borrowed pointer to the registry that stays `Send`.
///
/// `GeneratorRenderer`, `Executor`, and `PresetRuntime` must be `Send`
/// (they sit inside `ClipRenderer`/`Compositor` objects that cross
/// threads at construction), so they store this wrapper between
/// `set_layer_skin_registry` and the frame's render instead of a bare
/// `*const`, which would make the whole struct non-`Send`. Safety mirrors
/// the registry's own `unsafe impl Send` above: the pointer is set and
/// dereferenced only on the content thread, and the compositor that owns
/// the registry outlives every frame it hands out.
#[derive(Clone, Copy)]
pub struct LayerSkinPtr(*const LayerSkinRegistry);

impl LayerSkinPtr {
    /// Wrap a registry reference for cross-frame storage.
    pub fn new(registry: &LayerSkinRegistry) -> Self {
        Self(registry as *const LayerSkinRegistry)
    }

    /// Dereference for this frame's graph execution. The returned
    /// lifetime is unconstrained: validity is the caller's safety
    /// obligation, not the wrapper's.
    ///
    /// # Safety
    /// The caller guarantees the registry outlives the returned borrow —
    /// the content-thread frame guarantee (the owning compositor outlives
    /// the render call the pointer was set for).
    pub unsafe fn get<'a>(&self) -> &'a LayerSkinRegistry {
        unsafe { &*self.0 }
    }
}

unsafe impl Send for LayerSkinPtr {}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;
    use crate::test_device;

    #[test]
    fn missing_layer_returns_fallback() {
        let device = test_device();
        let registry = LayerSkinRegistry::new(&device, GpuTextureFormat::Rgba16Float);
        let tex = registry.get(&LayerId::new("no-such-layer"));
        assert_eq!(tex.width, 1);
        assert_eq!(tex.height, 1);
        assert_eq!(tex.format, GpuTextureFormat::Rgba16Float);
    }

    #[test]
    fn publish_then_lookup_round_trips() {
        let device = test_device();
        let mut registry = LayerSkinRegistry::new(&device, GpuTextureFormat::Rgba16Float);
        let layer_id = LayerId::new("layer-a");
        let published = device.create_texture(&GpuTextureDesc {
            width: 64,
            height: 64,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "published",
            mip_levels: 1,
        });
        let mut encoder = device.create_encoder("round-trip proof");
        {
            let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut encoder, &device);
            registry.begin_snapshots();
            registry.publish_snapshot(&mut gpu, &layer_id, &published);
            registry.finish_snapshots();
        }
        encoder.commit_and_wait_completed();
        assert_eq!(registry.len(), 1);
        let looked_up = registry.get(&layer_id);
        assert_eq!(looked_up.width, 64);
        assert_eq!(looked_up.height, 64);

        // A different id falls back.
        let other = registry.get(&LayerId::new("layer-b"));
        assert_eq!(other.width, 1);
        assert_eq!(other.height, 1);
    }

    #[test]
    fn clear_drops_all_textures() {
        let device = test_device();
        let mut registry = LayerSkinRegistry::new(&device, GpuTextureFormat::Rgba16Float);
        let layer_id = LayerId::new("layer-a");
        let published = device.create_texture(&GpuTextureDesc {
            width: 32,
            height: 32,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "published",
            mip_levels: 1,
        });
        let mut encoder = device.create_encoder("clear proof");
        {
            let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut encoder, &device);
            registry.begin_snapshots();
            registry.publish_snapshot(&mut gpu, &layer_id, &published);
            registry.finish_snapshots();
        }
        encoder.commit_and_wait_completed();
        registry.clear();
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.get(&layer_id).width, 1);
    }
    #[test]
    fn group_mask_published_source_survives_target_reuse() {
        let device = test_device();
        let id = LayerId::new("reused-source");
        let mut registry = LayerSkinRegistry::new(&device, GpuTextureFormat::Rgba16Float);
        let target = crate::render_target::RenderTarget::new(&device, 4, 4, GpuTextureFormat::Rgba16Float, "reused source");
        let mut encoder = device.create_encoder("previous frame source proof");
        {
            let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut encoder, &device);
            gpu.clear_texture(&target.texture, 0.25, 0.0, 0.0, 1.0);
            registry.begin_snapshots();
            registry.publish_snapshot(&mut gpu, &id, &target.texture);
            registry.finish_snapshots();
            gpu.clear_texture(&target.texture, 0.75, 0.0, 0.0, 1.0);
        }
        encoder.commit_and_wait_completed();
        let raw = crate::headless_readback::readback_raw_halves(&device, registry.get(&id), 4, 4);
        let red = half::f16::from_bits(u16::from_le_bytes([raw[0], raw[1]])).to_f32();
        assert!((red - 0.25).abs() < 0.001, "published frame changed when source was reused: {red}");
    }

    #[test]
    fn unread_layer_is_not_snapshotted() {
        let device = test_device();
        let id = LayerId::new("unread-layer");
        let mut registry = LayerSkinRegistry::new(&device, GpuTextureFormat::Rgba16Float);
        let source = crate::render_target::RenderTarget::new(&device, 4, 4, GpuTextureFormat::Rgba16Float, "unread source");
        // Publish cycle with no recorded reads → nothing snapshotted.
        let mut encoder = device.create_encoder("unread layer proof");
        {
            let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut encoder, &device);
            gpu.clear_texture(&source.texture, 1.0, 0.0, 0.0, 1.0);
            registry.begin_snapshots();
            registry.finish_snapshots();
        }
        encoder.commit_and_wait_completed();
        assert_eq!(registry.get(&id).width, 1, "unreferenced layer must serve the fallback");
        assert_eq!(registry.len(), 0);

        // A recorded read + publish → real pixels.
        let mut encoder = device.create_encoder("read then publish proof");
        {
            let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut encoder, &device);
            registry.get(&id); // records the read
            registry.begin_snapshots();
            registry.publish_snapshot(&mut gpu, &id, &source.texture);
            registry.finish_snapshots();
        }
        encoder.commit_and_wait_completed();
        assert_eq!(registry.get(&id).width, 4, "referenced layer must snapshot");

        // A cycle with no reads clears the set and drops the stale entry.
        registry.begin_snapshots();
        registry.finish_snapshots();
        assert_eq!(registry.len(), 0, "stale snapshot must drop when not re-published");
        assert_eq!(registry.get(&id).width, 1);
    }

}
