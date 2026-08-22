//! Layer-skin registry: previous-frame composited output per layer.
//!
//! The compositor publishes every layer's final post-effect texture here at
//! end of frame. Graph execution reads from the registry next frame, so a
//! layer bound as a scene object's emissive/base-color map is always the
//! previous frame — loops become one-frame feedback instead of a render-order
//! hazard. Missing or deleted layers emit a 1×1 transparent-black fallback.

use ahash::AHashMap;
use manifold_core::LayerId;
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

/// Previous-frame layer textures, content-thread only.
///
/// Owned by the compositor; graph execution receives a borrowed reference for
/// the current frame. No shared-state wrapper — the content thread is the
/// sole writer and reader, and the borrow checker enforces the frame
/// lifetime.
pub struct LayerSkinRegistry {
    textures: AHashMap<LayerId, GpuTexture>,
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

    /// Store `texture` as the skin for `layer_id`. The texture is retained by
    /// clone (one atomic refcount bump), so the original can continue to live
    /// in the compositor's ping-pong or effect chain.
    pub fn publish(&mut self, layer_id: LayerId, texture: GpuTexture) {
        self.textures.insert(layer_id, texture);
    }

    /// Borrow the texture for `layer_id`, or the fallback if absent.
    pub fn get(&self, layer_id: &LayerId) -> &GpuTexture {
        self.textures.get(layer_id).unwrap_or(&self.fallback)
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
        registry.publish(layer_id.clone(), published);
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
        registry.publish(layer_id.clone(), published);
        registry.clear();
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.get(&layer_id).width, 1);
    }
}
