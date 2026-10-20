//! The content side of the editing chrome drawn over the pictures: the
//! physics overlay's frame entries and the selection outline's requests,
//! mask bridges and per-frame masks (D-102). All of it sits beside the
//! pictures, never in them (REALTIME_3D_DESIGN.md D9).

use super::ContentPipeline;

impl ContentPipeline {
    pub fn set_physics_overlay(&mut self, layer: Option<manifold_core::LayerId>) {
        self.physics_overlay.set_layer(layer);
    }

    pub fn physics_overlay_frames(&self) -> &crate::physics_overlay::PhysicsOverlayFrames {
        &self.physics_overlay.frames
    }

    /// What the solvers capped, clipped or refused on the layers that
    /// rendered, for the cards; recorded whatever Show gizmos says.
    pub fn setup_fits(&self) -> &crate::setup_check::SetupFitReport {
        &self.setup_fits.report
    }
}

#[cfg(target_os = "macos")]
mod outline {
    use std::sync::Arc;

    use manifold_compositor::generator_renderer::GeneratorRenderer;
    use manifold_gpu::{GpuDevice, GpuEncoder};

    use super::super::ContentPipeline;
    use super::super::scene_viewport_capture::{SceneViewportCapture, ViewportCopy};
    use crate::physics_overlay::PhysicsOverlayObservations;
    use crate::selection_outline::{OutlineCapture, OutlineSlots};
    use crate::shared_texture::SharedTextureBridge;

    impl ContentPipeline {
        /// The selection outline's render slots for the output and the scene
        /// viewport.
        pub fn set_selection_outline(&mut self, output: Option<OutlineSlots>, viewport: Option<OutlineSlots>) {
            self.outline.set(output, viewport);
        }

        /// Withdraw the outline requests, returning them for
        /// [`Self::set_selection_outline`] later: no mask renders meanwhile.
        pub fn take_selection_outline(&mut self) -> (Option<OutlineSlots>, Option<OutlineSlots>) {
            self.outline.take()
        }

        /// Install the outline mask bridges: the output preview's and the
        /// scene viewport's.
        pub fn set_outline_bridges(&mut self, output: Arc<SharedTextureBridge>, viewport: Arc<SharedTextureBridge>) {
            let Some(device) = self.native_device.clone() else { return };
            self.outline.output_masks.install(&device, output);
            self.outline.viewport_masks.install(&device, viewport);
        }
    }

    impl OutlineCapture {
        /// After the generators render, into surface `slot`'s bridges: the
        /// output scene's selection mask, stamped on its overlay entry, and
        /// a generator scene viewport's mask, stamped on its frame. Returns
        /// the viewport frame to copy when one was asked for.
        #[allow(clippy::too_many_arguments)] // Disjoint pipeline fields, borrowed while the frame's other borrows live.
        pub(in crate::content_pipeline) fn capture_generator(
            &mut self,
            device: &GpuDevice,
            enc: &mut GpuEncoder,
            slot: usize,
            frame: u64,
            generators: Option<&GeneratorRenderer>,
            overlay: &mut PhysicsOverlayObservations,
            viewport: &mut SceneViewportCapture,
        ) -> Option<ViewportCopy> {
            let mask = generators.zip(overlay.layer()).and_then(|(g, layer)| g.selection_mask(layer));
            let stamp = self.output_masks.write(device, enc, slot, mask);
            overlay.set_outline(slot, stamp);
            let copy = viewport.observe_generator(generators, slot, frame)?;
            let stamp = self.viewport_masks.write(device, enc, slot, viewport.generator_mask(generators));
            viewport.observations.set_outline(slot, stamp);
            Some(copy)
        }
    }
}
