//! `TexturePane` — a UI region whose pixels come from a live GPU texture.
//!
//! The present pass blits several live GPU textures into reserved UI rects
//! (node-output preview, master-out monitor, video thumbnails). Today each is a
//! hand-rolled block that imports a texture from an IOSurface bridge, tracks the
//! bridge's generation to re-import after a window resize, and emits a
//! `draw_fullscreen_viewport`. The generation check is correctness-by-
//! convention: a consumer that forgets it samples a texture whose IOSurface was
//! freed by a resize — a GPU fault.
//!
//! `TexturePane` is the typed home for that pattern. It unifies the two ways a
//! panel surfaces a texture — a UI-device-produced texture ([`PaneSource::Local`])
//! and a cross-device IOSurface bridge ([`PaneSource::Bridged`]) — behind one
//! [`TexturePane::current`] accessor that **owns** the generation-driven
//! re-import. The accessor never hands out a cached texture, so a caller
//! physically cannot retain one across a resize. Local panes need no bridge, no
//! triple-buffer, and no fence discipline; they are the simple, first-class case
//! (the audio spectrogram is one).
//!
//! This is built fresh for new consumers (the spectrogram); the existing
//! hand-rolled blits are intentionally left as-is — `TexturePane` is the clean
//! path forward, not a consolidation pass over old code.

use std::sync::Arc;

use manifold_gpu::{
    GpuBinding, GpuDevice, GpuEncoder, GpuLoadAction, GpuRenderPipeline, GpuSampler, GpuTexture,
};

use crate::shared_texture::{SURFACE_COUNT, SharedTextureBridge};

/// Where a [`TexturePane`]'s pixels come from.
pub enum PaneSource {
    /// A texture produced on the UI device and owned here. No triple-buffer, no
    /// cross-device fence, no generation tracking. The producer renders into it
    /// (via [`TexturePane::local_target`]) and the present pass blits it.
    Local(GpuTexture),
    /// A cross-device IOSurface bridge. Holds the per-surface imported textures
    /// plus the last-seen generation; [`TexturePane::current`] re-imports every
    /// surface on a generation change (resize) and returns the published front.
    ///
    /// Unused today — the spectrogram is `Local`. This is the ready path for a
    /// future cross-device consumer (e.g. migrating node-preview / master-out).
    #[allow(dead_code)]
    Bridged {
        bridge: Arc<SharedTextureBridge>,
        /// One imported texture per IOSurface (`len == SURFACE_COUNT`). `None`
        /// until first imported / after an invalidation.
        imported: Vec<Option<GpuTexture>>,
        /// Last bridge generation these imports were built against. `None` forces
        /// an import on first `current()`.
        last_generation: Option<u64>,
    },
}

/// A UI region backed by a live GPU texture. See module docs.
pub struct TexturePane {
    source: PaneSource,
}

impl TexturePane {
    /// A pane backed by a UI-device texture the caller renders into each frame.
    pub fn local(texture: GpuTexture) -> Self {
        Self { source: PaneSource::Local(texture) }
    }

    /// A pane backed by a cross-device IOSurface bridge. Ready for a future
    /// cross-device consumer; the spectrogram uses [`Self::local`].
    #[allow(dead_code)]
    pub fn bridged(bridge: Arc<SharedTextureBridge>) -> Self {
        Self {
            source: PaneSource::Bridged {
                bridge,
                imported: (0..SURFACE_COUNT).map(|_| None).collect(),
                last_generation: None,
            },
        }
    }

    /// The texture to sample this frame, or `None` if none is ready.
    ///
    /// - **Local:** the owned texture.
    /// - **Bridged:** if the bridge's generation changed since the last call
    ///   (i.e. a resize replaced the IOSurfaces), re-import every surface, then
    ///   return the published front surface.
    ///
    /// The returned borrow is bounded to the call site, so a caller cannot hold
    /// a texture across a resize — invalidation lives here, not in a per-consumer
    /// generation check.
    pub fn current(&mut self, ui_device: &GpuDevice) -> Option<&GpuTexture> {
        match &mut self.source {
            PaneSource::Local(tex) => Some(tex),
            PaneSource::Bridged { bridge, imported, last_generation } => {
                let generation = bridge.generation();
                if *last_generation != Some(generation) {
                    for (i, slot) in imported.iter_mut().enumerate() {
                        // SAFETY: the bridge outlives this pane (held by `Arc`),
                        // so the IOSurface backing each imported texture stays
                        // alive at least as long as the texture.
                        *slot = Some(unsafe { bridge.import_texture_native(ui_device, i) });
                    }
                    *last_generation = Some(generation);
                }
                let front = bridge.front_index() as usize;
                imported.get(front).and_then(|t| t.as_ref())
            }
        }
    }

    /// For a [`PaneSource::Local`] pane, the texture to render into. `None` for a
    /// bridged pane (its producer writes through the bridge, not here).
    pub fn local_target(&self) -> Option<&GpuTexture> {
        match &self.source {
            PaneSource::Local(tex) => Some(tex),
            PaneSource::Bridged { .. } => None,
        }
    }

    /// Replace a local pane's texture — e.g. when the scope is resized and its
    /// render target is reallocated. No-op for a bridged pane.
    ///
    /// Unused today — the spectrogram (the only `Local` consumer so far)
    /// doesn't yet resize its render target at runtime. Un-suppresses when a
    /// `TexturePane::local` consumer needs to reallocate on resize.
    #[allow(dead_code)]
    pub fn set_local(&mut self, texture: GpuTexture) {
        if let PaneSource::Local(slot) = &mut self.source {
            *slot = texture;
        }
    }
}

/// Blit a pane's current texture into a logical `rect` of `target` (the
/// drawable), scaling logical → physical pixels by `scale`. Collapses the
/// texture + sampler binding boilerplate the present pass otherwise repeats per
/// consumer. A no-op if the pane has no texture ready this frame.
///
/// Loads (not clears) the target, so it composites over whatever the UI atlas
/// already drew there — same convention as the existing sidebar-monitor blits.
#[allow(clippy::too_many_arguments)]
pub fn blit_texture_pane(
    pane: &mut TexturePane,
    ui_device: &GpuDevice,
    encoder: &mut GpuEncoder,
    pipeline: &GpuRenderPipeline,
    sampler: &GpuSampler,
    target: &GpuTexture,
    rect_logical: (f32, f32, f32, f32),
    scale: f32,
    label: &str,
) {
    let Some(texture) = pane.current(ui_device) else {
        return;
    };
    let (x, y, w, h) = rect_logical;
    encoder.draw_fullscreen_viewport(
        pipeline,
        target,
        &[
            GpuBinding::Texture { binding: 0, texture },
            GpuBinding::Sampler { binding: 1, sampler },
        ],
        (x * scale, y * scale, w * scale, h * scale),
        GpuLoadAction::Load,
        label,
    );
}
