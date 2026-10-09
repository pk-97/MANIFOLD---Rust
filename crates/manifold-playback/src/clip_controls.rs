//! Beat-domain clip-control facts produced by the playback synchronizer.
//!
//! This module deliberately does not inspect a project or infer clip edges.
//! `sync_clips_to_time` is the scheduling authority and records the spans and
//! starts it has already decided to deliver. Consumers only query this frame.

use ahash::AHashMap;
use manifold_core::params::ClipTriggerSource;
use manifold_core::project::{ClipTriggerLayer, clip_trigger_source_is_eligible};
use manifold_core::types::LayerType;
use manifold_core::{Beats, ClipId, LayerId};

/// A period during which a clip-control source is active.
///
/// `end_beat: None` is the lifetime of a live MIDI note before NoteOff commits
/// its final duration.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipControlSpan {
    pub clip_id: ClipId,
    pub start_beat: Beats,
    /// Start of control membership; phase remains relative to `start_beat`.
    pub control_from: Beats,
    pub end_beat: Option<Beats>,
}

/// A clip-control start event retained for the current frame.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipControlStart {
    pub clip_id: ClipId,
    pub beat: Beats,
    /// Main clip mute suppresses envelopes; legacy audio clip-edge responses
    /// still observe its start. Source-level mute is applied by the producer.
    pub is_muted: bool,
}

#[derive(Debug, Default)]
struct ClipControlSource {
    layer_type: Option<LayerType>,
    parent: Option<LayerId>,
    spans: Vec<ClipControlSpan>,
    starts: Vec<ClipControlStart>,
}

/// Read-only view of one source's clip-control facts.
#[derive(Clone, Copy, Debug)]
pub struct ClipControlSourceView<'a> {
    pub spans: &'a [ClipControlSpan],
    pub starts: &'a [ClipControlStart],
}

/// Per-frame clip-control facts keyed by stable source layer identity.
#[derive(Debug, Default)]
pub struct ClipControlFrame {
    by_source: AHashMap<LayerId, ClipControlSource>,
}

impl ClipControlFrame {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace sampled spans without discarding starts awaiting modulation.
    pub(crate) fn clear_spans(&mut self) {
        for source in self.by_source.values_mut() {
            source.spans.clear();
        }
    }

    /// Consume starts while keeping phase available to parked evaluations.
    pub(crate) fn clear_starts(&mut self) {
        for source in self.by_source.values_mut() {
            source.starts.clear();
        }
    }

    /// Deleted sources cannot deliver starts queued before the edit.
    pub(crate) fn retain_sources(&mut self, mut exists: impl FnMut(&LayerId) -> bool) {
        self.by_source.retain(|id, _| exists(id));
    }

    /// Publish the current layer hierarchy alongside its control facts. This
    /// is derived scope metadata, never a second authored routing table.
    pub(crate) fn set_layer_scope(
        &mut self,
        id: LayerId,
        layer_type: LayerType,
        parent: Option<LayerId>,
    ) {
        let source = self.by_source.entry(id).or_default();
        source.layer_type = Some(layer_type);
        source.parent = parent;
    }

    /// Suppress a source without discarding its reusable storage or changing
    /// the scheduler's membership. Unmute can resume phase without a new edge.
    pub(crate) fn suppress_source(&mut self, source: &LayerId) {
        if let Some(source) = self.by_source.get_mut(source) {
            source.spans.clear();
            source.starts.clear();
        }
    }

    /// A muted trigger clip also cancels starts queued before the mute edit.
    pub(crate) fn suppress_clip(&mut self, source: &LayerId, clip: &ClipId) {
        if let Some(source) = self.by_source.get_mut(source) {
            source.spans.retain(|span| &span.clip_id != clip);
            source.starts.retain(|start| &start.clip_id != clip);
        }
    }

    /// Drop all source keys and retained storage when replacing a project.
    pub(crate) fn reset(&mut self) {
        self.by_source.clear();
    }

    /// Record an already-scheduled active span for `source_layer`.
    pub(crate) fn record_span(&mut self, source_layer: LayerId, span: ClipControlSpan) {
        self.by_source
            .entry(source_layer)
            .or_default()
            .spans
            .push(span);
    }

    /// Record an already-scheduled clip start for `source_layer`.
    pub(crate) fn record_start(&mut self, source_layer: LayerId, start: ClipControlStart) {
        self.by_source
            .entry(source_layer)
            .or_default()
            .starts
            .push(start);
    }

    /// Finish producer writes and order starts deterministically by beat, then
    /// stable clip ID. The comparator allocates nothing.
    pub(crate) fn finish(&mut self) {
        for source in self.by_source.values_mut() {
            source.starts.sort_unstable_by(|a, b| {
                a.beat
                    .0
                    .total_cmp(&b.beat.0)
                    .then_with(|| a.clip_id.as_str().cmp(b.clip_id.as_str()))
            });
        }
    }

    /// Return the retained facts for one source layer.
    pub fn view(&self, source_layer: &LayerId) -> Option<ClipControlSourceView<'_>> {
        self.by_source
            .get(source_layer)
            .map(|source| ClipControlSourceView {
                spans: &source.spans,
                starts: &source.starts,
            })
    }

    pub(crate) fn source_layer<'a>(
        &self,
        source: &'a ClipTriggerSource,
        owner: Option<&'a LayerId>,
    ) -> Option<&'a LayerId> {
        match source {
            ClipTriggerSource::OwnLayer => owner,
            ClipTriggerSource::Disabled => None,
            ClipTriggerSource::Lane { layer_id } => {
                let owner = owner?;
                clip_trigger_source_is_eligible(owner, layer_id, self.by_source.len(), |id| {
                    let source = self.by_source.get(id)?;
                    Some(ClipTriggerLayer {
                        layer_type: source.layer_type?,
                        parent: source.parent.as_ref(),
                    })
                })
                .then_some(layer_id)
            }
        }
    }

    /// Return elapsed beat time for the last matching span at `beat`.
    /// Membership is half-open `[control_from, end)`; phase is measured from
    /// the original clip start. An open span lasts until its producer closes it.
    pub fn elapsed(
        &self,
        source: &ClipTriggerSource,
        owner: Option<&LayerId>,
        beat: Beats,
    ) -> Option<Beats> {
        let source_layer = self.source_layer(source, owner)?;
        let source = self.by_source.get(source_layer)?;
        let span = source.spans.iter().rev().find(|span| {
            if beat < span.start_beat || beat < span.control_from {
                return false;
            }
            match span.end_beat {
                Some(end) => beat < end,
                None => true,
            }
        })?;
        Some(beat - span.start_beat)
    }

    /// Return all retained starts for the selected source, in `finish` order.
    pub fn starts(
        &self,
        source: &ClipTriggerSource,
        owner: Option<&LayerId>,
    ) -> &[ClipControlStart] {
        let Some(source_layer) = self.source_layer(source, owner) else {
            return &[];
        };
        self.by_source
            .get(source_layer)
            .map_or(&[], |source| source.starts.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(id: &str, start: f64, end: Option<f64>) -> ClipControlSpan {
        ClipControlSpan {
            clip_id: ClipId::new(id),
            start_beat: Beats(start),
            control_from: Beats(start),
            end_beat: end.map(Beats),
        }
    }

    fn start(id: &str, beat: f64) -> ClipControlStart {
        ClipControlStart {
            clip_id: ClipId::new(id),
            beat: Beats(beat),
            is_muted: false,
        }
    }

    #[test]
    fn default_disabled_missing_and_source_isolation() {
        let mut frame = ClipControlFrame::default();
        let own_layer = LayerId::new("own");
        let lane_layer = LayerId::new("lane");
        frame.set_layer_scope(own_layer.clone(), LayerType::Video, None);
        frame.set_layer_scope(lane_layer.clone(), LayerType::Trigger, Some(own_layer.clone()));
        frame.record_span(own_layer.clone(), span("own-clip", 1.0, Some(4.0)));
        frame.record_span(lane_layer.clone(), span("lane-clip", 2.0, Some(5.0)));
        frame.finish();

        assert_eq!(
            frame.elapsed(&ClipTriggerSource::default(), Some(&own_layer), Beats(2.0)),
            Some(Beats(1.0))
        );
        assert_eq!(
            frame.elapsed(
                &ClipTriggerSource::Lane {
                    layer_id: lane_layer.clone(),
                },
                Some(&own_layer),
                Beats(3.0),
            ),
            Some(Beats(1.0))
        );
        assert_eq!(
            frame.elapsed(&ClipTriggerSource::Disabled, Some(&own_layer), Beats(2.0)),
            None
        );
        assert_eq!(
            frame.elapsed(&ClipTriggerSource::default(), None, Beats(2.0)),
            None
        );
        assert_eq!(
            frame.elapsed(
                &ClipTriggerSource::Lane {
                    layer_id: LayerId::new("missing"),
                },
                Some(&own_layer),
                Beats(2.0),
            ),
            None
        );
        assert_eq!(
            frame.elapsed(&ClipTriggerSource::default(), Some(&lane_layer), Beats(2.0)),
            Some(Beats(0.0))
        );
    }

    #[test]
    fn scope_changes_disable_both_phase_and_starts() {
        let mut frame = ClipControlFrame::default();
        let owner = LayerId::new("owner");
        let group = LayerId::new("group");
        let lane = LayerId::new("lane");
        let selection = ClipTriggerSource::Lane { layer_id: lane.clone() };
        frame.set_layer_scope(group.clone(), LayerType::Group, None);
        frame.set_layer_scope(owner.clone(), LayerType::Video, Some(group.clone()));
        frame.set_layer_scope(lane.clone(), LayerType::Trigger, Some(group.clone()));
        frame.record_span(lane.clone(), span("clip", 1.0, Some(4.0)));
        frame.record_start(lane.clone(), start("clip", 1.0));
        assert_eq!(frame.elapsed(&selection, Some(&owner), Beats(2.0)), Some(Beats(1.0)));
        assert_eq!(frame.starts(&selection, Some(&owner)).len(), 1);

        frame.set_layer_scope(owner.clone(), LayerType::Video, None);
        assert_eq!(frame.elapsed(&selection, Some(&owner), Beats(2.0)), None);
        assert!(frame.starts(&selection, Some(&owner)).is_empty());
        frame.set_layer_scope(owner.clone(), LayerType::Video, Some(group));
        frame.set_layer_scope(lane.clone(), LayerType::Video, Some(owner.clone()));
        assert_eq!(frame.elapsed(&selection, Some(&owner), Beats(2.0)), None);
        assert!(frame.starts(&selection, Some(&owner)).is_empty());
        frame.set_layer_scope(lane, LayerType::Trigger, None);
        assert_eq!(frame.elapsed(&selection, Some(&owner), Beats(2.0)), None);
        assert!(frame.starts(&selection, Some(&owner)).is_empty());
    }

    #[test]
    fn historical_samples_use_half_open_boundaries_and_last_span_priority() {
        let mut frame = ClipControlFrame::default();
        let layer = LayerId::new("layer");
        frame.record_span(layer.clone(), span("first", 1.0, Some(4.0)));
        frame.record_span(layer.clone(), span("second", 2.0, Some(3.0)));
        frame.finish();

        assert_eq!(
            frame.elapsed(&ClipTriggerSource::default(), Some(&layer), Beats(0.5)),
            None
        );
        assert_eq!(
            frame.elapsed(&ClipTriggerSource::default(), Some(&layer), Beats(2.5)),
            Some(Beats(0.5))
        );
        assert_eq!(
            frame.elapsed(&ClipTriggerSource::default(), Some(&layer), Beats(3.0)),
            Some(Beats(2.0))
        );
        assert_eq!(
            frame.elapsed(&ClipTriggerSource::default(), Some(&layer), Beats(4.0)),
            None
        );
    }

    #[test]
    fn open_ended_live_span_remains_active() {
        let mut frame = ClipControlFrame::default();
        let layer = LayerId::new("live");
        frame.record_span(layer.clone(), span("live-clip", 5.0, None));
        assert_eq!(
            frame.elapsed(&ClipTriggerSource::default(), Some(&layer), Beats(100.0)),
            Some(Beats(95.0))
        );
    }

    #[test]
    fn multiple_starts_are_retained_and_ordered() {
        let mut frame = ClipControlFrame::default();
        let layer = LayerId::new("layer");
        frame.record_start(layer.clone(), start("z", 2.0));
        frame.record_start(layer.clone(), start("b", 1.0));
        frame.record_start(layer.clone(), start("a", 1.0));
        frame.finish();
        let starts = frame.starts(&ClipTriggerSource::default(), Some(&layer));
        assert_eq!(starts, [start("a", 1.0), start("b", 1.0), start("z", 2.0)]);
    }

    #[test]
    fn clearing_consumed_facts_preserves_source_capacity() {
        let mut frame = ClipControlFrame::default();
        let layer = LayerId::new("layer");
        frame.record_span(layer.clone(), span("clip", 0.0, Some(1.0)));
        frame.record_start(layer.clone(), start("clip", 0.0));
        let before_span_ptr = frame.view(&layer).unwrap().spans.as_ptr();
        let before_start_ptr = frame.view(&layer).unwrap().starts.as_ptr();

        frame.clear_spans();
        frame.clear_starts();
        let after = frame.view(&layer).unwrap();
        assert!(after.spans.is_empty());
        assert!(after.starts.is_empty());
        assert_eq!(after.spans.as_ptr(), before_span_ptr);
        assert_eq!(after.starts.as_ptr(), before_start_ptr);
    }
}
