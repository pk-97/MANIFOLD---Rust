//! The last-resort layer cut and the layer notices the stage shows. A layer
//! whose GPU frame runs past `RUNAWAY_LAYER_FRAME_SECONDS` for
//! `RUNAWAY_LAYER_FRAMES` frames in a row stops drawing, so the window
//! server, which shares the GPU, keeps the Mac responsive. Draw Again, or any
//! edit or undo on that layer, brings it back. Nothing else stops: other
//! layers and the transport carry on. Export renders every layer.

use std::sync::Arc;

use manifold_core::LayerId;
use manifold_gpu::{GpuSpanTimer, RUNAWAY_LAYER_FRAME_SECONDS, RUNAWAY_LAYER_FRAMES};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;

use super::GeneratorRenderer;

/// A layer's news for the stage, taken by the host after each frame.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerNews {
    /// A node of the layer's graph reported this error; it shows once.
    NodeError(LayerId, Arc<str>),
    /// The layer stopped drawing after runaway GPU frames.
    Stopped(LayerId),
    /// A stopped layer draws again.
    Restored(LayerId),
}

/// One layer's runaway count. Pure, so the cut decision is testable on the CPU.
#[derive(Default)]
pub struct RunawayWatch {
    over: u32,
    stopped: bool,
    /// The stopped layer's target was cleared, so it shows nothing.
    cleared: bool,
    /// The layer's card values when it stopped ([`card_fingerprint`]).
    card: u64,
}

impl RunawayWatch {
    /// Fold one measured frame of the layer, whose card values are `card`.
    /// True when this frame stops it.
    pub fn observe(&mut self, seconds: f64, card: u64) -> bool {
        if self.stopped {
            return false;
        }
        if seconds <= RUNAWAY_LAYER_FRAME_SECONDS {
            self.over = 0;
            return false;
        }
        self.over += 1;
        self.stopped = self.over >= RUNAWAY_LAYER_FRAMES;
        self.card = card;
        self.stopped
    }

    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Draw again from a clean count. True when the layer was stopped.
    pub fn restore(&mut self) -> bool {
        std::mem::take(self).stopped
    }

    /// A card value edit or undo since the stop draws the layer again. True
    /// when it restored the layer.
    pub fn restore_if_edited(&mut self, card: u64) -> bool {
        self.stopped && card != self.card && self.restore()
    }
}

/// The layer card's hand-set base values, folded. An edit or undo of a card
/// value moves it; modulation never writes a base, and a param its
/// automation lane plays is left out.
pub fn card_fingerprint(layer: &manifold_core::layer::Layer) -> u64 {
    layer.gen_params().map_or(0, |instance| {
        instance.params.iter().filter(|param| !param.automated).fold(0xcbf2_9ce4_8422_2325, |hash, param| {
            (hash ^ u64::from(param.base.to_bits())).wrapping_mul(0x0100_0000_01b3)
        })
    })
}

/// The renderer's side: one timer over every layer's frame, and the news
/// waiting for the host.
#[derive(Default)]
pub(super) struct LayerWatch {
    timer: GpuSpanTimer,
    /// The key of the next installed generator. A rebuilt generator gets a
    /// new key, so frames timed for the old one never count against it.
    next_key: u64,
    /// The span of the layer encoding now.
    open: bool,
    /// False in export, which renders every layer however slow. Set from
    /// the frame's sim step.
    pub(super) live: bool,
    news: Vec<LayerNews>,
}

impl LayerWatch {
    pub(super) fn new() -> Self {
        Self { live: true, ..Self::default() }
    }

    pub(super) fn next_key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }

    /// Time the layer about to encode, when this frame is chunked.
    pub(super) fn begin(&mut self, gpu: &mut GpuEncoder, key: u64) {
        if self.live && gpu.chunking_enabled {
            self.timer.begin(gpu.native_enc, key);
            self.open = true;
        }
    }

    /// End the open layer's span in the current buffer. The caller commits
    /// that buffer next. False when no span was open.
    pub(super) fn end(&mut self, gpu: &mut GpuEncoder) -> bool {
        if !std::mem::take(&mut self.open) {
            return false;
        }
        self.timer.end(gpu.native_enc);
        true
    }

    pub(super) fn push(&mut self, news: LayerNews) {
        self.news.push(news);
    }
}

impl GeneratorRenderer {
    /// Draw Again: the layer draws from a clean count.
    pub fn draw_layer_again(&mut self, layer_id: &LayerId) {
        if let Some(state) = self.layer_generators.get_mut(layer_id)
            && state.runaway.restore()
        {
            self.watch.push(LayerNews::Restored(layer_id.clone()));
        }
    }

    /// The layers stopped by runaway frames.
    pub fn stopped_layers(&self) -> impl Iterator<Item = &LayerId> {
        self.layer_generators.iter().filter(|(_, state)| state.runaway.stopped()).map(|(id, _)| id)
    }

    /// Hand the news since the last call to `each`, oldest first.
    pub fn drain_layer_news(&mut self, mut each: impl FnMut(LayerNews)) {
        for news in self.watch.news.drain(..) {
            each(news);
        }
    }

    /// Fold every finished layer frame into its layer's runaway count.
    pub(super) fn poll_layer_frames(&mut self, layers: &[manifold_core::layer::Layer]) {
        let LayerWatch { timer, news, .. } = &mut self.watch;
        let states = &mut self.layer_generators;
        timer.poll(|key, seconds| {
            let Some((layer_id, state)) = states.iter_mut().find(|(_, state)| state.span_key == key) else {
                return;
            };
            let card = layers.iter().find(|layer| &layer.layer_id == layer_id).map_or(0, card_fingerprint);
            if state.runaway.observe(seconds, card) {
                eprintln!(
                    "[runaway] layer {layer_id}: GPU frame {seconds:.2} s, over {RUNAWAY_LAYER_FRAME_SECONDS} s \
                     {RUNAWAY_LAYER_FRAMES} frames in a row; it stops drawing"
                );
                news.push(LayerNews::Stopped(layer_id.clone()));
            }
        });
    }

    /// Whether a stopped layer skips this frame. A card edit or undo since
    /// the stop draws it again; otherwise its target is cleared once, so it
    /// shows nothing instead of its last runaway frame.
    pub(super) fn skip_stopped(
        watch: &mut LayerWatch,
        layer: &manifold_core::layer::Layer,
        runaway: &mut RunawayWatch,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
    ) -> bool {
        if !watch.live || !runaway.stopped {
            return false;
        }
        if runaway.restore_if_edited(card_fingerprint(layer)) {
            watch.push(LayerNews::Restored(layer.layer_id.clone()));
            return false;
        }
        if !runaway.cleared {
            gpu.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            runaway.cleared = true;
        }
        true
    }

    /// Queue the layer's newest node error once.
    pub(super) fn note_node_errors(
        watch: &mut LayerWatch,
        layer_id: &LayerId,
        seen: &mut u64,
        errors: &manifold_node_engine::exec::node_errors::NodeErrors,
    ) {
        if errors.sequence() != *seen {
            *seen = errors.sequence();
            watch.push(LayerNews::NodeError(layer_id.clone(), Arc::from(errors.latest())));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOW: f64 = RUNAWAY_LAYER_FRAME_SECONDS * 2.0;
    const FAST: f64 = RUNAWAY_LAYER_FRAME_SECONDS / 4.0;
    const CARD: u64 = 7;

    #[test]
    fn three_runaway_frames_in_a_row_stop_the_layer_and_an_edit_restores_it() {
        assert_eq!(RUNAWAY_LAYER_FRAMES, 3);
        let mut watch = RunawayWatch::default();
        // A fast frame between slow ones starts the count again.
        assert!(!watch.observe(SLOW, CARD));
        assert!(!watch.observe(SLOW, CARD));
        assert!(!watch.observe(FAST, CARD));
        assert!(!watch.observe(SLOW, CARD));
        assert!(!watch.observe(SLOW, CARD));
        assert!(!watch.stopped());
        assert!(watch.observe(SLOW, CARD), "the third slow frame in a row stops the layer");
        assert!(watch.stopped());
        // Frames still in flight when it stopped report no second stop.
        assert!(!watch.observe(SLOW, CARD));
        // Unchanged card values keep it stopped; an edit or undo restores it.
        assert!(!watch.restore_if_edited(CARD));
        assert!(watch.stopped());
        assert!(watch.restore_if_edited(CARD + 1), "an edit restores a stopped layer");
        assert!(!watch.stopped());
        // A frame exactly at the limit is not runaway.
        for _ in 0..10 {
            assert!(!watch.observe(RUNAWAY_LAYER_FRAME_SECONDS, CARD));
        }
        // Draw Again restores from a clean count.
        for _ in 0..RUNAWAY_LAYER_FRAMES {
            watch.observe(SLOW, CARD);
        }
        assert!(watch.restore());
        assert!(!watch.observe(SLOW, CARD) && !watch.observe(SLOW, CARD));
        assert!(!watch.restore(), "two slow frames after Draw Again do not stop it");
    }
}
