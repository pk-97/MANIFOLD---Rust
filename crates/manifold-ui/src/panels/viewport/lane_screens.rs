//! Automation lane screen geometry: one [`AutomationLaneScreen`] per visible
//! lane. The whole-viewport rebuild and the one-lane rebuild a drag preview
//! uses (UI_INVALIDATION_DESIGN.md P7a) run the same per-lane code, so there
//! is one copy of the screen math.

use super::model::{self, AutomationLaneScreen};
use super::*;
use crate::view::UiGraphTarget;

/// The viewport-wide inputs every lane screen reads.
struct LaneGeometry {
    tx0: f32,
    tx1: f32,
    ty0: f32,
    ty1: f32,
    min_beat: f32,
    max_beat: f32,
    pixels_per_beat: f32,
    grid_step: f32,
    beats_per_bar: f32,
    bar_skip: u32,
}

impl TimelineViewportPanel {
    fn lane_geometry(&self) -> LaneGeometry {
        let (min_beat, max_beat) = self.visible_beat_range();
        LaneGeometry {
            tx0: self.tracks_rect.x,
            tx1: self.tracks_rect.x_max(),
            ty0: self.tracks_rect.y,
            ty1: self.tracks_rect.y + self.tracks_rect.height,
            min_beat,
            max_beat,
            // Lane timing geometry is sourced from the same mapper/grid policy
            // as the ruler and the track grid. The renderer consumes these
            // scalars while walking its bounded grid loop; it must not invent
            // a second snap grid.
            pixels_per_beat: self.mapper.pixels_per_beat(),
            grid_step: self.grid_step(),
            beats_per_bar: self.beats_per_bar as f32,
            bar_skip: self.bar_skip(),
        }
    }

    /// Whether this layer's lanes are on screen at all.
    fn layer_lanes_visible(&self, g: &LaneGeometry, layer: usize) -> bool {
        if self.is_group_layer(layer) {
            return false;
        }
        let track_y = self.track_y(layer);
        let track_h = self.track_height(layer);
        track_h > 0.0 && track_y + track_h >= g.ty0 && track_y <= g.ty1
    }

    /// The strip of one lane. Lanes stack below the fixed base card, in the
    /// extra height `CoordinateMapper::layer_height` reserved for them.
    fn lane_strip_rect(&self, g: &LaneGeometry, layer: usize, lane_index: usize) -> Rect {
        let height_of = |k: usize| {
            self.automation_lane_heights
                .get(layer)
                .and_then(|heights| heights.get(k))
                .copied()
                .unwrap_or(color::AUTOMATION_LANE_STRIP_HEIGHT)
        };
        let above: f32 = (0..lane_index).map(height_of).sum();
        let strip_y = self.track_y(layer) + color::TRACK_HEIGHT + above;
        Rect::new(g.tx0, strip_y, (g.tx1 - g.tx0).max(0.0), height_of(lane_index))
    }

    /// A lane's screen with its facts copied from the lane and its geometry
    /// filled by [`Self::fill_lane_geometry`].
    fn new_lane_screen(&self, g: &LaneGeometry, strip_rect: Rect, lane: &UiAutomationLane) -> AutomationLaneScreen {
        let mut screen = AutomationLaneScreen {
            strip_rect,
            pixels_per_beat: g.pixels_per_beat,
            visible_beat_start: g.min_beat,
            grid_step: g.grid_step,
            beats_per_bar: g.beats_per_bar,
            bar_skip: g.bar_skip,
            label: lane.label.clone(),
            effect_id: lane.effect_id.clone(),
            polyline: Vec::new(),
            dots: Vec::new(),
            target: lane.target.clone(),
            param_id: lane.param_id.clone(),
            param_min: lane.param_min,
            param_max: lane.param_max,
            whole_numbers: lane.whole_numbers,
            placeholder_value: None,
            display: lane.display.clone(),
            title_rect: strip_rect,
            title_value_w: 0.0,
            playhead_value: 0.0,
        };
        self.fill_lane_geometry(g, strip_rect, lane, &mut screen);
        screen
    }

    /// Everything about a lane screen that follows from the lane's points and
    /// the viewport: strip, stroke, dots, title, readouts. The stroke and dot
    /// buffers are cleared and refilled, so a drag preview allocates nothing.
    fn fill_lane_geometry(
        &self,
        g: &LaneGeometry,
        strip_rect: Rect,
        lane: &UiAutomationLane,
        screen: &mut AutomationLaneScreen,
    ) {
        let graph = AutomationLaneScreen::curve_rect_for(strip_rect);
        let (title_rect, title_value_w) = model::automation_title_rect(&*self.text_measure, strip_rect, lane);

        // Segment-aware geometry keeps exact breakpoint attachment and gives
        // Hold segments their vertical edge. Curves receive more samples as
        // their on-screen span grows, without a fixed-step sampler skipping a
        // breakpoint between two samples.
        model::build_automation_polyline_into(
            &mut screen.polyline,
            lane,
            g.min_beat,
            g.max_beat,
            g.tx0,
            g.tx1,
            graph,
            g.pixels_per_beat,
        );

        // Placeholder lanes carry one synthetic point at the param's current
        // value so the flat-line polyline samples correctly, but it isn't a
        // real breakpoint yet: no dot until the first click creates one.
        screen.dots.clear();
        if !(lane.placeholder || lane.points.is_empty()) {
            // Keep the adjacent point on either side of the visible interval.
            // Those endpoints are needed to identify and drag a segment
            // crossing the viewport edge, while the binary-search bounds keep
            // the work proportional to the visible points.
            let lower = lane.points.partition_point(|p| p.beat.as_f32() < g.min_beat);
            let upper = lane.points.partition_point(|p| p.beat.as_f32() <= g.max_beat);
            let first = lower.saturating_sub(1);
            let last = (upper + 1).min(lane.points.len());
            screen.dots.extend(lane.points[first..last].iter().map(|p| model::AutomationDotScreen {
                x: self.beat_to_pixel(p.beat),
                y: graph.y + graph.height * (1.0 - p.value_norm),
                beat: p.beat,
                value_norm: p.value_norm,
                shape: p.shape,
                past_end: p.past_end,
            }));
        }

        screen.strip_rect = strip_rect;
        screen.pixels_per_beat = g.pixels_per_beat;
        screen.visible_beat_start = g.min_beat;
        screen.grid_step = g.grid_step;
        screen.beats_per_bar = g.beats_per_bar;
        screen.bar_skip = g.bar_skip;
        screen.placeholder_value = lane
            .placeholder
            .then(|| lane.points.first())
            .flatten()
            .map(|p| lane.param_min + p.value_norm * (lane.param_max - lane.param_min));
        screen.title_rect = title_rect;
        screen.title_value_w = title_value_w;
        screen.playhead_value = lane.denormalize(lane.value_at_norm(self.playhead_beat));
    }

    /// One [`AutomationLaneScreen`] per visible lane (P4,
    /// `docs/AUTOMATION_LANES_DESIGN.md` section 7); empty whenever
    /// `set_automation_lanes` last got no data. Every mutator of an input read
    /// here (lanes, lane heights, tracks, layout rect, scroll, zoom, beats per
    /// bar) calls this, so the cache is never stale. May allocate; reads never do.
    pub(super) fn rebuild_automation_lane_screens(&mut self) {
        let mut out = std::mem::take(&mut self.automation_lane_screens);
        out.clear();
        self.automation_lane_sources.clear();
        let g = self.lane_geometry();
        for (i, lanes) in self.automation_lanes_by_layer.iter().enumerate() {
            if lanes.is_empty() || !self.layer_lanes_visible(&g, i) {
                continue;
            }
            for (lane_index, lane) in lanes.iter().enumerate() {
                let strip_rect = self.lane_strip_rect(&g, i, lane_index);
                out.push(self.new_lane_screen(&g, strip_rect, lane));
                self.automation_lane_sources.push((i, lane_index));
            }
        }
        self.automation_lane_screens = out;
    }

    /// A drag preview rewrote one lane's points: let `fill` write them into
    /// the lane's entry, then rebuild that lane's screen alone, reusing its
    /// buffers. Returns false when the lane is not held here or is a
    /// placeholder (the first click creates its real lane, which is a
    /// structural change), so the caller re-projects everything instead.
    pub fn update_automation_lane(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        fill: impl FnOnce(&mut UiAutomationLane),
    ) -> bool {
        let Some((layer, lane_index)) = self.automation_lanes_by_layer.iter().enumerate().find_map(|(i, lanes)| {
            lanes
                .iter()
                .position(|l| l.target == *target && l.param_id == *param_id)
                .map(|k| (i, k))
        }) else {
            return false;
        };
        let lane = &mut self.automation_lanes_by_layer[layer][lane_index];
        if lane.placeholder {
            return false;
        }
        fill(lane);
        let g = self.lane_geometry();
        // A lane whose track is off screen has no screen to rebuild.
        let Some(slot) = self.automation_lane_sources.iter().position(|&s| s == (layer, lane_index)) else {
            return true;
        };
        let strip_rect = self.lane_strip_rect(&g, layer, lane_index);
        let mut screens = std::mem::take(&mut self.automation_lane_screens);
        if let Some(screen) = screens.get_mut(slot) {
            let lane = &self.automation_lanes_by_layer[layer][lane_index];
            self.fill_lane_geometry(&g, strip_rect, lane, screen);
        }
        self.automation_lane_screens = screens;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LayerType;
    use crate::view::{UiAutomationPoint, UiLayer, UiSegmentShape};

    fn lane(name: &str, norms: &[(f32, f32)]) -> UiAutomationLane {
        UiAutomationLane {
            effect_id: EffectId::new(name),
            param_id: ParamId::from("amount"),
            target: UiGraphTarget::Effect(EffectId::new(name)),
            label: format!("{name}: amount"),
            points: norms
                .iter()
                .map(|&(beat, value_norm)| UiAutomationPoint {
                    beat: Beats::from_f32(beat),
                    value_norm,
                    shape: UiSegmentShape::Linear,
                    past_end: false,
                })
                .collect(),
            param_min: 0.0,
            param_max: 1.0,
            whole_numbers: false,
            placeholder: false,
            display: crate::view::UiLaneDisplay::default(),
        }
    }

    fn panel_with_three_lanes() -> TimelineViewportPanel {
        let mut panel = TimelineViewportPanel::new();
        panel.tracks_rect = Rect::new(0.0, 0.0, 1000.0, 600.0);
        panel.set_tracks(vec![TrackInfo::default()]);
        panel.set_automation_lane_layout(&[vec![112.0, 112.0, 112.0]]);
        panel.rebuild_mapper_layout(&[UiLayer {
            layer_id: LayerId::new("L0"),
            parent_layer_id: None,
            layer_type: LayerType::Video,
            is_collapsed: false,
            automation_lane_count: 3,
        }]);
        panel.set_automation_lanes(
            ["a", "b", "c"]
                .into_iter()
                .map(|n| ViewportAutomationLane {
                    layer_index: 0,
                    lane: lane(n, &[(4.0, 0.5), (8.0, 0.8)]),
                })
                .collect(),
        );
        panel
    }

    /// A placeholder lane (its real lane does not exist yet) and an unknown
    /// lane are not an in-place case.
    #[test]
    fn placeholder_and_missing_lanes_fall_back() {
        let mut panel = panel_with_three_lanes();
        let missing = UiGraphTarget::Effect(EffectId::new("zzz"));
        assert!(!panel.update_automation_lane(&missing, &ParamId::from("amount"), |_| {}));
        panel.automation_lanes_by_layer[0][0].placeholder = true;
        let a = UiGraphTarget::Effect(EffectId::new("a"));
        assert!(!panel.update_automation_lane(&a, &ParamId::from("amount"), |_| {}));
    }
}
