//! The last-resort cut of a scene layer and the layer notices the stage shows.
//! A scene layer whose frame holds one command buffer past
//! `RUNAWAY_LAYER_FRAME_SECONDS` for `RUNAWAY_LAYER_FRAMES` frames in a row
//! stops drawing, so the window server, which shares the GPU and gets it
//! between buffers, keeps the Mac responsive. A slow frame spread over many
//! short buffers is allowed: it renders slowly and never freezes the Mac.
//! Only Draw Again or a recompile of the scene brings it back; a knob,
//! modulation or row drag never does. Other layers and the transport carry
//! on, and export renders every layer.

use manifold_core::LayerId;
use manifold_gpu::{GpuSpanTimer, RUNAWAY_LAYER_FRAME_SECONDS, RUNAWAY_LAYER_FRAMES};
use manifold_node_engine::exec::node_errors::{NodeErrorNews, NodeErrors};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;

use super::GeneratorRenderer;

/// A layer's news for the stage, taken by the host after each frame.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerNews {
    /// A node of the layer's graph started failing; the log has its text.
    NodeError(LayerId, NodeErrorNews),
    /// The layer stopped drawing after runaway GPU frames.
    Stopped(LayerId),
    /// A stopped layer draws again.
    Restored(LayerId),
}

/// One scene layer's runaway count. Pure, so the cut decision is testable on
/// the CPU.
#[derive(Default)]
pub struct RunawayWatch {
    over: u32,
    stopped: bool,
    /// The stopped layer's target was cleared, so it shows nothing.
    cleared: bool,
    /// The frame being gathered and its longest buffer so far. A crossfade
    /// draws a layer's clips as separate spans of one frame.
    gathering: Option<(u64, f64)>,
}

impl RunawayWatch {
    /// Fold one finished span of `frame`. A span of a later frame closes the
    /// earlier one first. True when that stops the layer.
    pub fn span(&mut self, frame: u64, seconds: f64) -> bool {
        let stopped = match self.gathering {
            Some((gathered, _)) if gathered != frame => self.close(),
            _ => false,
        };
        let longest = self.gathering.map_or(seconds, |(_, longest)| longest.max(seconds));
        self.gathering = Some((frame, longest));
        stopped
    }

    /// Close the gathered frame once no span of it is still on the GPU
    /// (`in_flight` is the oldest frame that has one). True when that stops
    /// the layer.
    pub fn settle(&mut self, in_flight: Option<u64>) -> bool {
        match self.gathering {
            Some((frame, _)) if in_flight.is_none_or(|oldest| oldest > frame) => self.close(),
            _ => false,
        }
    }

    fn close(&mut self) -> bool {
        self.gathering.take().is_some_and(|(_, longest)| self.observe(longest))
    }

    /// Fold one whole frame: `seconds` is its longest command buffer.
    fn observe(&mut self, seconds: f64) -> bool {
        if self.stopped {
            return false;
        }
        if seconds <= RUNAWAY_LAYER_FRAME_SECONDS {
            self.over = 0;
            return false;
        }
        self.over += 1;
        self.stopped = self.over >= RUNAWAY_LAYER_FRAMES;
        self.stopped
    }

    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Draw again from a clean count. True when the layer was stopped.
    pub fn restore(&mut self) -> bool {
        std::mem::take(self).stopped
    }
}

/// The renderer's side: one timer over every scene layer's frame, and the
/// news waiting for the host.
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

    /// Time the scene layer about to encode in `frame`, when this frame is
    /// chunked.
    pub(super) fn begin(&mut self, gpu: &mut GpuEncoder, key: u64, frame: u64) {
        if self.live && gpu.chunking_enabled {
            self.timer.begin(gpu.native_enc, key, frame);
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

/// Whether the cut watches this layer: scene layers only.
pub(super) fn watched(layer: &manifold_core::layer::Layer) -> bool {
    layer.gen_params().is_some_and(|instance| instance.scene.is_some())
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

    /// Hand the news since the last call to `each`, oldest first.
    pub fn drain_layer_news(&mut self, mut each: impl FnMut(LayerNews)) {
        for news in self.watch.news.drain(..) {
            each(news);
        }
    }

    /// Fold every finished scene-layer frame into its layer's runaway count.
    pub(super) fn poll_layer_frames(&mut self) {
        let LayerWatch { timer, news, .. } = &mut self.watch;
        let states = &mut self.layer_generators;
        let mut stop = |layer_id: &LayerId| {
            eprintln!(
                "[runaway] layer {layer_id}: one GPU buffer ran over {RUNAWAY_LAYER_FRAME_SECONDS} s \
                 {RUNAWAY_LAYER_FRAMES} frames in a row; it stops drawing"
            );
            news.push(LayerNews::Stopped(layer_id.clone()));
        };
        timer.poll(|key, frame, seconds| {
            if let Some((layer_id, state)) = states.iter_mut().find(|(_, state)| state.span_key == key)
                && state.runaway.span(frame, seconds)
            {
                stop(layer_id);
            }
        });
        let in_flight = timer.in_flight_frame();
        for (layer_id, state) in states.iter_mut() {
            if state.runaway.settle(in_flight) {
                stop(layer_id);
            }
        }
    }

    /// Whether a stopped scene layer skips this frame. Its target is cleared
    /// once, so it shows nothing instead of its last runaway frame.
    pub(super) fn skip_stopped(
        live: bool,
        runaway: &mut RunawayWatch,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
    ) -> bool {
        if !live || !runaway.stopped {
            return false;
        }
        if !runaway.cleared {
            gpu.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            runaway.cleared = true;
        }
        true
    }

    /// Queue the layer's node errors that are news.
    pub(super) fn note_node_errors(watch: &mut LayerWatch, layer_id: &LayerId, errors: &mut NodeErrors) {
        errors.take_news(|news| watch.push(LayerNews::NodeError(layer_id.clone(), news)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOW: f64 = RUNAWAY_LAYER_FRAME_SECONDS * 2.0;
    const FAST: f64 = RUNAWAY_LAYER_FRAME_SECONDS / 4.0;

    /// Feed whole frames, one span each, settling each as the next frame's
    /// span would.
    fn frames(watch: &mut RunawayWatch, from: u64, seconds: &[f64]) -> Vec<bool> {
        seconds
            .iter()
            .enumerate()
            .map(|(index, &seconds)| {
                let stopped = watch.span(from + index as u64, seconds);
                stopped | watch.settle(None)
            })
            .collect()
    }

    #[test]
    fn many_short_buffers_never_stop_a_layer_and_one_long_buffer_does() {
        let mut watch = RunawayWatch::default();
        // A heavy ray-traced frame: twenty tiles of 50 ms is a full second,
        // and the timer reports its longest tile.
        let tiled = [0.05; 20];
        assert!(tiled.iter().sum::<f64>() > RUNAWAY_LAYER_FRAME_SECONDS);
        assert_eq!(frames(&mut watch, 0, &[0.05; 10]), [false; 10], "a slow tiled frame renders slowly, never stops");
        // One huge instanced draw in a single buffer, three frames in a row.
        assert_eq!(frames(&mut watch, 10, &[SLOW; 3]), [false, false, true], "the third long frame stops it");
        assert!(watch.stopped());
    }

    #[test]
    fn a_buffer_killed_by_the_gpu_counts_as_over_the_limit() {
        let mut watch = RunawayWatch::default();
        assert_eq!(frames(&mut watch, 0, &[f64::INFINITY; 3]), [false, false, true]);
    }

    #[test]
    fn three_runaway_frames_in_a_row_stop_the_layer_and_only_draw_again_restores_it() {
        assert_eq!(RUNAWAY_LAYER_FRAMES, 3);
        let mut watch = RunawayWatch::default();
        // A fast frame between slow ones starts the count again.
        assert_eq!(frames(&mut watch, 0, &[SLOW, SLOW, FAST, SLOW, SLOW]), [false; 5]);
        assert!(!watch.stopped());
        assert_eq!(frames(&mut watch, 5, &[SLOW]), [true], "the third slow frame in a row stops the layer");
        // Frames still in flight when it stopped report no second stop.
        assert_eq!(frames(&mut watch, 6, &[SLOW, FAST, SLOW]), [false; 3]);
        assert!(watch.stopped());
        // A frame exactly at the limit is not runaway.
        assert!(watch.restore());
        assert_eq!(frames(&mut watch, 9, &[RUNAWAY_LAYER_FRAME_SECONDS; 10]), [false; 10]);
        // Draw Again restores from a clean count.
        frames(&mut watch, 19, &[SLOW; 3]);
        assert!(watch.restore());
        assert_eq!(frames(&mut watch, 22, &[SLOW; 2]), [false; 2]);
        assert!(!watch.restore(), "two slow frames after Draw Again do not stop it");
    }

    #[test]
    fn a_crossfade_counts_once_per_frame_by_its_longest_clip() {
        let mut watch = RunawayWatch::default();
        for frame in 0..2 {
            // Two clips of one layer: two spans of one frame.
            assert!(!watch.span(frame, SLOW));
            assert!(!watch.span(frame, FAST));
            // The second clip's span is still on the GPU: nothing closes.
            assert!(!watch.settle(Some(frame)));
        }
        // Two slow frames so far, however many spans they had.
        assert!(!watch.settle(None));
        assert!(!watch.stopped());
        assert!(watch.span(2, FAST) | watch.span(2, SLOW) | watch.settle(None), "the third slow frame stops it");
    }
}
