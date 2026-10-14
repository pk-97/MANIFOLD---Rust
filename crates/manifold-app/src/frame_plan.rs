//! The main window's frame planner (UI_INVALIDATION_DESIGN.md D5, D6, P4b).
//!
//! `plan_frame` compares this frame's [`FrameInputs`] with last frame's and
//! returns the work to do. It is a pure function over integers: equal inputs
//! give an empty plan. Input code states facts by moving a version; it never
//! picks work. Section 4 of the design is the table this encodes, and the
//! unit tests below pin one test per row of both tables.
//!
//! P4b runs the planner first each frame beside the old flags
//! (`needs_structural_sync`, `structural()`, `needs_rebuild`, `ScrollDirty`),
//! which stay live and can only add work.

use manifold_core::change::{ChangeSet, Part, PartStamps};

use crate::content_state::RuntimeSeqs;
use crate::ui_root::RegionSlot;

/// The UI's own part stamps (D5), in the UI's counter space. Two sources move
/// them: an accepted snapshot (every part that moved between the last
/// accepted content stamps and these) and a UI-side write through one of the
/// three stamping helpers. Previews never move them (D12).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UiStamps {
    parts: PartStamps,
    accepted: PartStamps,
}

impl UiStamps {
    pub(crate) fn parts(&self) -> PartStamps {
        self.parts
    }

    /// A UI-side write to the local project.
    pub(crate) fn bump(&mut self, touches: ChangeSet) {
        self.parts.bump(touches);
    }

    /// Fold an accepted snapshot's content stamps: the parts that moved since
    /// the last accepted snapshot move here too. Returns them.
    pub(crate) fn fold_snapshot(&mut self, content: &PartStamps) -> ChangeSet {
        let moved = content.moved_since(&self.accepted);
        self.accepted = *content;
        self.parts.bump(moved);
        moved
    }
}

/// The main window's regions, in build order (`UIRoot::build`), which is also
/// the region rank of paint order (D7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum MainRegion {
    Transport,
    Header,
    Footer,
    Inspector,
    AudioDock,
    SplitHandles,
    LayerHeaders,
    Viewport,
    Overlays,
    /// The card-drag ghost: no atlas slot, refilled each frame one exists.
    Ghosts,
}

pub(crate) const MAIN_REGION_COUNT: usize = 10;

impl MainRegion {
    pub(crate) const ALL: [MainRegion; MAIN_REGION_COUNT] = [
        MainRegion::Transport,
        MainRegion::Header,
        MainRegion::Footer,
        MainRegion::Inspector,
        MainRegion::AudioDock,
        MainRegion::SplitHandles,
        MainRegion::LayerHeaders,
        MainRegion::Viewport,
        MainRegion::Overlays,
        MainRegion::Ghosts,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RegionMask(u16);

impl RegionMask {
    pub(crate) const NONE: Self = Self(0);

    pub(crate) fn insert(&mut self, region: MainRegion) {
        self.0 |= 1 << region as u8;
    }

    pub(crate) fn contains(self, region: MainRegion) -> bool {
        self.0 & (1 << region as u8) != 0
    }

    #[cfg(test)]
    pub(crate) fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The first region in build order, which is where a suffix rebuild of
    /// the flat tree has to start.
    pub(crate) fn earliest(self) -> Option<MainRegion> {
        MainRegion::ALL.into_iter().find(|&r| self.contains(r))
    }

    /// The flat-tree suffix that rebuilds these regions (P4b, until P4e).
    pub(crate) fn slot(self) -> Option<RegionSlot> {
        match self.earliest()? {
            MainRegion::LayerHeaders => Some(RegionSlot::ScrollPanels),
            MainRegion::Viewport | MainRegion::Overlays => Some(RegionSlot::ViewportPanels),
            MainRegion::Ghosts => None,
            _ => Some(RegionSlot::Full),
        }
    }
}

/// One pixel-quantized `[x, y, w, h]` per region (D7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RegionRects(pub [[i32; 4]; MAIN_REGION_COUNT]);

impl RegionRects {
    pub(crate) fn get(&self, region: MainRegion) -> [i32; 4] {
        self.0[region as usize]
    }

    #[cfg(test)]
    pub(crate) fn set(&mut self, region: MainRegion, rect: [i32; 4]) {
        self.0[region as usize] = rect;
    }
}

/// What the transport, header and footer show, at display resolution (D15).
/// The field list is re-derived from `push_state`'s chrome reads in P4c.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChromeInputs {
    pub playing: bool,
    pub recording: bool,
    pub automation_armed: bool,
    pub sync_state: u8,
    pub link_state: u8,
    pub clock_state: u8,
    pub link_peers: u16,
    pub bpm_tenths: u32,
    pub clock_authority: u8,
    /// Minutes, seconds, tenths, bar, beat and sixteenth, packed.
    pub time_digits: u64,
    /// The shown MIDI-clock position text, packed byte by byte.
    pub clock_digits: u64,
    pub export_range_px: [i32; 2],
    pub sdr_preview: bool,
    pub hdr: bool,
}

/// Everything the main window's UI work depends on, as versions and
/// pixel-quantized view values (D6). `Copy`, integers only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameInputs {
    pub parts: PartStamps,
    /// D12 gesture preview version.
    pub preview: crate::gesture_preview::PreviewVersions,
    pub selection_identity: u64,
    pub selection_visual: u64,
    /// D14: the insert cursor.
    pub cursor: u64,
    pub active_layer: u64,
    /// D19 (selected point's lane and lane heights included).
    pub automation_view: u64,
    pub chrome: ChromeInputs,
    pub rects: RegionRects,
    pub scroll_x_px: i32,
    pub scroll_y_px: i32,
    pub zoom_bits: u32,
    /// The overview strip's playhead only.
    pub playhead_px: i32,
    pub inspector_view: u64,
    pub inspector_scroll_px: i32,
    pub dock_view: u64,
    pub dock_scroll_px: i32,
    pub overlays: u64,
    pub audio_directory: u64,
    /// D15: `ContentState.values_seq`.
    pub values: u64,
    pub runtime: RuntimeSeqs,
    /// D20: the frame-quantized driver period of the content bpm.
    pub driver_period_key: u64,
}

/// Rows of the section 4 projection table, one bit each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Projection {
    /// `sync_automation_lane_order` + `sync_project_data`.
    ProjectSync,
    /// `sync_inspector_data` (and the editor window's mirror of it).
    InspectorSync,
    /// The Audio Setup device query.
    AudioDevices,
    /// `sync_clip_positions` over every clip.
    ClipPositions,
    /// The dragged clips and split pieces only, in place.
    ClipPreview,
    /// `sync_automation_lane_geometry`.
    LaneGeometry,
    /// The dragged lane only.
    LanePreview,
    /// Live push, Values displays.
    LiveValues,
    /// Live push, Shapes displays.
    LiveShapes,
    /// Live push, Ableton range bands.
    LiveRanges,
    /// Insert cursor, selection band, layer highlight, selected clips.
    SelectionPush,
    /// Transport, sync, settings and macro label displays.
    ChromePush,
    /// `sync_driver_bpm`, in place.
    DriverReadout,
    /// `dropdowns.rs` `sync_embedded_presets`.
    PresetList,
    OverviewRepaint,
    WaveformRequests,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProjectionMask(u32);

impl ProjectionMask {
    pub(crate) fn insert(&mut self, projection: Projection) {
        self.0 |= 1 << projection as u8;
    }

    pub(crate) fn contains(self, projection: Projection) -> bool {
        self.0 & (1 << projection as u8) != 0
    }
}

/// The frame's work, decided from inputs alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FramePlan {
    pub projections: ProjectionMask,
    pub rebuild: RegionMask,
    /// Rect origin moved, size did not.
    pub translate: RegionMask,
    pub in_place: RegionMask,
    /// Atlas slots to repaint.
    pub repaint: RegionMask,
    pub redraw: bool,
}

#[cfg(test)]
impl FramePlan {
    pub(crate) fn is_empty(&self) -> bool {
        *self == FramePlan::default()
    }
}

// Section 4 projection-table keys. Values, Shapes and MappingRanges are live
// scalars and key only in-place pushes (D22).
const PROJECT_SYNC_PARTS: ChangeSet =
    ChangeSet::of(&[Part::Layers, Part::Clips, Part::Automation, Part::Markers, Part::Settings]);
const INSPECTOR_SYNC_PARTS: ChangeSet = ChangeSet::of(&[
    Part::Layers,
    Part::Effects,
    Part::Mappings,
    Part::Clips,
    Part::Automation,
    Part::Led,
    Part::AudioSetup,
    Part::EmbeddedPresets,
    Part::Settings,
]);
const AUDIO_DEVICE_PARTS: ChangeSet = ChangeSet::of(&[Part::AudioSetup]);
const CLIP_POSITION_PARTS: ChangeSet = ChangeSet::of(&[Part::Clips, Part::Layers]);
const LANE_GEOMETRY_PARTS: ChangeSet = ChangeSet::of(&[Part::Automation, Part::Effects, Part::Layers]);
// Clips: the footer and layer-header clip counts (projection_input_matrix).
const CHROME_PARTS: ChangeSet = ChangeSet::of(&[Part::Settings, Part::Mappings, Part::Layers, Part::Clips]);
const LIVE_VALUE_PARTS: ChangeSet = ChangeSet::of(&[Part::Values, Part::Layers]);
const DRIVER_READOUT_PARTS: ChangeSet = ChangeSet::of(&[Part::Effects]);
const PRESET_LIST_PARTS: ChangeSet = ChangeSet::of(&[Part::EmbeddedPresets]);
const OVERVIEW_PARTS: ChangeSet = ChangeSet::of(&[Part::Clips, Part::Layers]);
const WAVEFORM_PARTS: ChangeSet = ChangeSet::of(&[Part::Clips]);

/// Decide the frame's work from what moved (D6). Pure; equal inputs give an
/// empty plan.
pub(crate) fn plan_frame(last: &FrameInputs, next: &FrameInputs) -> FramePlan {
    let mut plan = FramePlan::default();
    if last == next {
        return plan;
    }
    let moved = next.parts.moved_since(&last.parts);
    let selection_identity = last.selection_identity != next.selection_identity;
    let selection_visual = last.selection_visual != next.selection_visual;
    let cursor = last.cursor != next.cursor;
    let active_layer = last.active_layer != next.active_layer;
    let automation_view = last.automation_view != next.automation_view;
    let preview = last.preview != next.preview;
    let runtime = &next.runtime;
    let p = &mut plan.projections;

    if moved.intersects(PROJECT_SYNC_PARTS) || selection_identity || active_layer || automation_view {
        p.insert(Projection::ProjectSync);
    }
    if moved.intersects(INSPECTOR_SYNC_PARTS)
        || selection_identity
        || active_layer
        || automation_view
        || runtime.automation_latches != last.runtime.automation_latches
        || runtime.led_preview_presence != last.runtime.led_preview_presence
    {
        p.insert(Projection::InspectorSync);
    }
    if moved.intersects(AUDIO_DEVICE_PARTS) || last.audio_directory != next.audio_directory {
        p.insert(Projection::AudioDevices);
    }
    if moved.intersects(CLIP_POSITION_PARTS) {
        p.insert(Projection::ClipPositions);
    }
    if last.preview.clips != next.preview.clips {
        p.insert(Projection::ClipPreview);
    }
    if moved.intersects(LANE_GEOMETRY_PARTS) || automation_view {
        p.insert(Projection::LaneGeometry);
    }
    if last.preview.lanes != next.preview.lanes {
        p.insert(Projection::LanePreview);
    }
    // The live push also writes the active layer's name and opacity.
    if last.values != next.values || moved.intersects(LIVE_VALUE_PARTS) || active_layer {
        p.insert(Projection::LiveValues);
    }
    if moved.contains(Part::Shapes) {
        p.insert(Projection::LiveShapes);
    }
    if moved.contains(Part::MappingRanges) {
        p.insert(Projection::LiveRanges);
    }
    // The push writes the active layer as well as the selection sets.
    if selection_visual || cursor || active_layer {
        p.insert(Projection::SelectionPush);
    }
    // The chrome push also shows the lanes toggle and the primary clip's
    // chrome values.
    if moved.intersects(CHROME_PARTS) || last.chrome != next.chrome || automation_view || selection_identity {
        p.insert(Projection::ChromePush);
    }
    if moved.intersects(DRIVER_READOUT_PARTS) || last.driver_period_key != next.driver_period_key {
        p.insert(Projection::DriverReadout);
    }
    if moved.intersects(PRESET_LIST_PARTS) {
        p.insert(Projection::PresetList);
    }
    if moved.intersects(OVERVIEW_PARTS)
        || preview
        || last.scroll_x_px != next.scroll_x_px
        || last.scroll_y_px != next.scroll_y_px
        || last.zoom_bits != next.zoom_bits
        || last.playhead_px != next.playhead_px
        || last.rects.get(MainRegion::Viewport) != next.rects.get(MainRegion::Viewport)
    {
        p.insert(Projection::OverviewRepaint);
    }
    if moved.intersects(WAVEFORM_PARTS) {
        p.insert(Projection::WaveformRequests);
    }
    // `sync_project_data` writes the clip list and lanes that the two
    // drag-time projections then overwrite (clip alpha, lane geometry), and
    // rebuilds the lists the selection push indexes into: whenever it runs,
    // all three run after it, as in the frame before the planner.
    if p.contains(Projection::ProjectSync) {
        p.insert(Projection::ClipPositions);
        p.insert(Projection::LaneGeometry);
        p.insert(Projection::SelectionPush);
    }

    plan_regions(&mut plan, last, next, selection_visual, cursor, active_layer, automation_view, preview);
    plan.redraw = true;
    plan
}

/// The section 4 region table.
#[allow(clippy::too_many_arguments)]
fn plan_regions(
    plan: &mut FramePlan,
    last: &FrameInputs,
    next: &FrameInputs,
    selection_visual: bool,
    cursor: bool,
    active_layer: bool,
    automation_view: bool,
    preview: bool,
) {
    let projections = plan.projections;
    let size = |r: MainRegion| {
        let (a, b) = (last.rects.get(r), next.rects.get(r));
        a[2] != b[2] || a[3] != b[3]
    };
    let width = |r: MainRegion| last.rects.get(r)[2] != next.rects.get(r)[2];
    let height = |r: MainRegion| last.rects.get(r)[3] != next.rects.get(r)[3];
    let origin = |r: MainRegion| {
        let (a, b) = (last.rects.get(r), next.rects.get(r));
        a[0] != b[0] || a[1] != b[1]
    };
    let mut rebuild = RegionMask::NONE;
    let mut translate = RegionMask::NONE;
    let mut in_place = RegionMask::NONE;
    let live = projections.contains(Projection::LiveValues)
        || projections.contains(Projection::LiveShapes)
        || projections.contains(Projection::LiveRanges);

    for chrome in [MainRegion::Transport, MainRegion::Header, MainRegion::Footer] {
        if size(chrome) {
            rebuild.insert(chrome);
        } else if origin(chrome) {
            translate.insert(chrome);
        }
        if projections.contains(Projection::ChromePush) || projections.contains(Projection::SelectionPush) {
            in_place.insert(chrome);
        }
    }

    let inspector = MainRegion::Inspector;
    if size(inspector)
        || projections.contains(Projection::InspectorSync)
        || last.inspector_view != next.inspector_view
    {
        rebuild.insert(inspector);
    } else if origin(inspector) {
        translate.insert(inspector);
    }
    if last.inspector_scroll_px != next.inspector_scroll_px || live {
        in_place.insert(inspector);
    }

    let dock = MainRegion::AudioDock;
    if size(dock) || projections.contains(Projection::AudioDevices) || last.dock_view != next.dock_view {
        rebuild.insert(dock);
    } else if origin(dock) {
        translate.insert(dock);
    }
    if last.dock_scroll_px != next.dock_scroll_px {
        in_place.insert(dock);
    }

    let handles = MainRegion::SplitHandles;
    if size(handles) {
        rebuild.insert(handles);
    } else if origin(handles) {
        translate.insert(handles);
    }

    let headers = MainRegion::LayerHeaders;
    if width(headers) || projections.contains(Projection::ProjectSync) {
        rebuild.insert(headers);
    } else if origin(headers) {
        translate.insert(headers);
    }
    if last.scroll_y_px != next.scroll_y_px
        || height(headers)
        || selection_visual
        || active_layer
        || automation_view
    {
        in_place.insert(headers);
    }

    let viewport = MainRegion::Viewport;
    if width(viewport)
        || projections.contains(Projection::ProjectSync)
        || next.parts.moved_since(&last.parts).contains(Part::Markers)
    {
        rebuild.insert(viewport);
    } else if origin(viewport) {
        translate.insert(viewport);
    }
    if last.scroll_x_px != next.scroll_x_px
        || last.scroll_y_px != next.scroll_y_px
        || last.zoom_bits != next.zoom_bits
        || height(viewport)
        || selection_visual
        || cursor
        || preview
        || automation_view
    {
        in_place.insert(viewport);
    }

    let overlays = MainRegion::Overlays;
    if last.overlays != next.overlays {
        rebuild.insert(overlays);
    }
    // Warmup and export progress are overlay in-place inputs too, but their
    // sequences do not move yet (their states arrive as separate notification
    // snapshots), so nothing keys on them until they bump.
    if last.runtime.ableton_session != next.runtime.ableton_session {
        in_place.insert(overlays);
    }

    // Ghosts are refilled from input while a ghost exists; no input here.
    plan.rebuild = rebuild;
    plan.translate = translate;
    plan.in_place = in_place;
    let mut repaint = rebuild.union(translate).union(in_place);
    repaint.0 &= !(1 << MainRegion::Ghosts as u8);
    plan.repaint = repaint;
}

/// The planner's state for one window: the inputs the last plan saw, and the
/// active-layer version (the active layer is an id, so the planner owns the
/// counter that says it moved).
#[derive(Default)]
pub(crate) struct FramePlanner {
    pub last: FrameInputs,
    active_layer_seen: Option<manifold_core::LayerId>,
    active_layer_version: u64,
}

impl FramePlanner {
    pub(crate) fn active_layer_version(&mut self, current: &Option<manifold_core::LayerId>) -> u64 {
        if *current != self.active_layer_seen {
            self.active_layer_seen.clone_from(current);
            self.active_layer_version += 1;
        }
        self.active_layer_version
    }

    /// Plan against the last frame's inputs and remember these.
    pub(crate) fn plan(&mut self, next: FrameInputs) -> FramePlan {
        let plan = plan_frame(&self.last, &next);
        self.last = next;
        plan
    }
}

#[cfg(test)]
mod tests {
    //! One test per row of the section 4 projection table and the main-window
    //! region table. Pure: no tree, no GPU.
    use super::*;

    fn base() -> FrameInputs {
        let mut inputs = FrameInputs::default();
        for (i, region) in MainRegion::ALL.into_iter().enumerate() {
            inputs.rects.set(region, [i as i32 * 10, 0, 100, 50]);
        }
        inputs
    }

    fn with_parts(parts: &[Part]) -> FrameInputs {
        let mut next = base();
        next.parts.bump(ChangeSet::of(parts));
        next
    }

    fn plan(next: FrameInputs) -> FramePlan {
        plan_frame(&base(), &next)
    }

    fn runs(plan: &FramePlan, projection: Projection) -> bool {
        plan.projections.contains(projection)
    }

    #[test]
    fn equal_inputs_give_an_empty_plan() {
        assert!(plan(base()).is_empty());
        let mut busy = with_parts(&[Part::Clips]);
        busy.selection_visual = 3;
        assert!(plan_frame(&busy, &busy).is_empty());
    }

    // ── Projection table ────────────────────────────────────────────

    #[test]
    fn row_project_sync() {
        for part in [Part::Layers, Part::Clips, Part::Automation, Part::Markers, Part::Settings] {
            assert!(runs(&plan(with_parts(&[part])), Projection::ProjectSync), "{part:?}");
        }
        let mut next = base();
        next.selection_identity = 1;
        assert!(runs(&plan(next), Projection::ProjectSync));
        let mut next = base();
        next.active_layer = 1;
        assert!(runs(&plan(next), Projection::ProjectSync));
        let mut next = base();
        next.automation_view = 1;
        assert!(runs(&plan(next), Projection::ProjectSync));
        for part in [Part::Effects, Part::Mappings, Part::Led, Part::AudioSetup, Part::EmbeddedPresets] {
            assert!(!runs(&plan(with_parts(&[part])), Projection::ProjectSync), "{part:?}");
        }
    }

    #[test]
    fn row_inspector_sync() {
        for part in [
            Part::Layers,
            Part::Effects,
            Part::Mappings,
            Part::Clips,
            Part::Automation,
            Part::Led,
            Part::AudioSetup,
            Part::EmbeddedPresets,
            Part::Settings,
        ] {
            assert!(runs(&plan(with_parts(&[part])), Projection::InspectorSync), "{part:?}");
        }
        let mut next = base();
        next.selection_identity = 1;
        assert!(runs(&plan(next), Projection::InspectorSync));
        let mut next = base();
        next.active_layer = 1;
        assert!(runs(&plan(next), Projection::InspectorSync));
        let mut next = base();
        next.automation_view = 1;
        assert!(runs(&plan(next), Projection::InspectorSync));
        let mut next = base();
        next.runtime.automation_latches = 1;
        assert!(runs(&plan(next), Projection::InspectorSync));
        let mut next = base();
        next.runtime.led_preview_presence = 1;
        assert!(runs(&plan(next), Projection::InspectorSync));
        assert!(!runs(&plan(with_parts(&[Part::Markers])), Projection::InspectorSync));
    }

    #[test]
    fn row_scene_param_with_switch_or_gate_rebuilds_the_inspector() {
        // ChangeGraphParamCommand touches Values | Effects for a scene.switch
        // or gate target (SCENE_SURFACES D12); a plain card param is Values.
        assert!(runs(&plan(with_parts(&[Part::Values, Part::Effects])), Projection::InspectorSync));
        assert!(!runs(&plan(with_parts(&[Part::Values])), Projection::InspectorSync));
    }

    #[test]
    fn row_audio_setup_device_list() {
        assert!(runs(&plan(with_parts(&[Part::AudioSetup])), Projection::AudioDevices));
        let mut next = base();
        next.audio_directory = 1;
        assert!(runs(&plan(next), Projection::AudioDevices));
        assert!(!runs(&plan(with_parts(&[Part::Effects])), Projection::AudioDevices));
    }

    #[test]
    fn row_clip_positions() {
        for part in [Part::Clips, Part::Layers] {
            assert!(runs(&plan(with_parts(&[part])), Projection::ClipPositions), "{part:?}");
        }
        let mut next = base();
        next.preview.clips = 1;
        let p = plan(next);
        assert!(runs(&p, Projection::ClipPreview));
        assert!(!runs(&p, Projection::ClipPositions), "a drag preview updates the dragged clips only");
        assert!(!runs(&p, Projection::LanePreview), "a clip drag never re-projects lanes");
    }

    #[test]
    fn row_automation_lane_geometry() {
        for part in [Part::Automation, Part::Effects, Part::Layers] {
            assert!(runs(&plan(with_parts(&[part])), Projection::LaneGeometry), "{part:?}");
        }
        let mut next = base();
        next.automation_view = 1;
        assert!(runs(&plan(next), Projection::LaneGeometry));
        let mut next = base();
        next.preview.lanes = 1;
        let p = plan(next);
        assert!(runs(&p, Projection::LanePreview));
        assert!(!runs(&p, Projection::LaneGeometry));
        assert!(!runs(&p, Projection::ClipPreview), "a point drag never re-projects clips");
    }

    #[test]
    fn row_live_push() {
        let mut next = base();
        next.values = 1;
        assert!(runs(&plan(next), Projection::LiveValues));
        assert!(runs(&plan(with_parts(&[Part::Values])), Projection::LiveValues));
        assert!(runs(&plan(with_parts(&[Part::Shapes])), Projection::LiveShapes));
        assert!(runs(&plan(with_parts(&[Part::MappingRanges])), Projection::LiveRanges));
        assert!(runs(&plan(with_parts(&[Part::Layers])), Projection::LiveValues));
        let mut next = base();
        next.active_layer = 1;
        assert!(runs(&plan(next), Projection::LiveValues));
    }

    #[test]
    fn row_live_scalars_never_key_a_projection() {
        // D22: Values, Shapes and MappingRanges are only ever in place.
        for part in [Part::Values, Part::Shapes, Part::MappingRanges] {
            let p = plan(with_parts(&[part]));
            for rebuild in [
                Projection::ProjectSync,
                Projection::InspectorSync,
                Projection::ClipPositions,
                Projection::LaneGeometry,
                Projection::AudioDevices,
            ] {
                assert!(!runs(&p, rebuild), "{part:?} keyed {rebuild:?}");
            }
            assert!(p.rebuild.is_empty(), "{part:?} rebuilt {:?}", p.rebuild);
        }
    }

    #[test]
    fn row_selection_push() {
        for edit in [
            |i: &mut FrameInputs| i.selection_visual = 1,
            |i: &mut FrameInputs| i.cursor = 1,
            |i: &mut FrameInputs| i.active_layer = 1,
        ] {
            let mut next = base();
            edit(&mut next);
            assert!(runs(&plan(next), Projection::SelectionPush));
        }
        // A cursor or visual-only change never re-projects (D14).
        let mut next = base();
        next.cursor = 1;
        next.selection_visual = 1;
        let p = plan(next);
        assert!(!runs(&p, Projection::ProjectSync));
        assert!(!runs(&p, Projection::InspectorSync));
    }

    #[test]
    fn row_chrome_push() {
        for part in [Part::Settings, Part::Mappings, Part::Layers, Part::Clips] {
            assert!(runs(&plan(with_parts(&[part])), Projection::ChromePush), "{part:?}");
        }
        let mut next = base();
        next.automation_view = 1;
        assert!(runs(&plan(next), Projection::ChromePush));
        let mut next = base();
        next.selection_identity = 1;
        assert!(runs(&plan(next), Projection::ChromePush));
        let mut next = base();
        next.chrome.bpm_tenths = 1205;
        assert!(runs(&plan(next), Projection::ChromePush));
        let mut next = base();
        next.chrome.time_digits = 7;
        assert!(runs(&plan(next), Projection::ChromePush));
    }

    #[test]
    fn row_driver_period_readout() {
        assert!(runs(&plan(with_parts(&[Part::Effects])), Projection::DriverReadout));
        let mut next = base();
        next.driver_period_key = 9;
        let p = plan(next);
        assert!(runs(&p, Projection::DriverReadout));
        assert!(!runs(&p, Projection::ProjectSync), "never a structural sync (D20)");
        assert!(p.rebuild.is_empty());
    }

    #[test]
    fn row_embedded_preset_list() {
        assert!(runs(&plan(with_parts(&[Part::EmbeddedPresets])), Projection::PresetList));
        assert!(!runs(&plan(with_parts(&[Part::Effects])), Projection::PresetList));
    }

    #[test]
    fn row_overview_strip() {
        let edits: [fn(&mut FrameInputs); 7] = [
            |i| i.parts.bump(ChangeSet::of(&[Part::Clips])),
            |i| i.parts.bump(ChangeSet::of(&[Part::Layers])),
            |i| i.preview.clips = 1,
            |i| i.scroll_x_px = 4,
            |i| i.zoom_bits = 1,
            |i| i.playhead_px = 300,
            |i| i.rects.set(MainRegion::Viewport, [0, 0, 1, 1]),
        ];
        for edit in edits {
            let mut next = base();
            edit(&mut next);
            assert!(runs(&plan(next), Projection::OverviewRepaint));
        }
    }

    #[test]
    fn row_audio_waveform_requests() {
        assert!(runs(&plan(with_parts(&[Part::Clips])), Projection::WaveformRequests));
        assert!(!runs(&plan(with_parts(&[Part::Layers])), Projection::WaveformRequests));
    }

    #[test]
    fn hover_modifiers_and_playhead_are_not_rebuild_inputs() {
        // D8: none of these is a FrameInputs field except the overview's
        // playhead pixel, which repaints the strip and rebuilds nothing.
        let mut next = base();
        next.playhead_px = 640;
        let p = plan(next);
        assert!(p.rebuild.is_empty());
        assert!(!runs(&p, Projection::ProjectSync));
    }

    // ── Region table (main window) ──────────────────────────────────

    fn resized(region: MainRegion) -> FrameInputs {
        let mut next = base();
        let mut rect = next.rects.get(region);
        rect[2] += 7;
        next.rects.set(region, rect);
        next
    }

    fn moved(region: MainRegion) -> FrameInputs {
        let mut next = base();
        let mut rect = next.rects.get(region);
        rect[1] += 7;
        next.rects.set(region, rect);
        next
    }

    #[test]
    fn region_transport_header_footer() {
        for region in [MainRegion::Transport, MainRegion::Header, MainRegion::Footer] {
            assert!(plan(resized(region)).rebuild.contains(region));
            let p = plan(moved(region));
            assert!(p.translate.contains(region) && !p.rebuild.contains(region));
            assert!(plan(with_parts(&[Part::Settings])).in_place.contains(region));
            let mut next = base();
            next.cursor = 1;
            assert!(plan(next).in_place.contains(region), "selection push values");
        }
    }

    #[test]
    fn region_inspector() {
        let r = MainRegion::Inspector;
        assert!(plan(resized(r)).rebuild.contains(r));
        assert!(plan(with_parts(&[Part::Effects])).rebuild.contains(r));
        let mut next = base();
        next.inspector_view = 1;
        assert!(plan(next).rebuild.contains(r));
        assert!(plan(moved(r)).translate.contains(r));
        let mut next = base();
        next.inspector_scroll_px = 12;
        let p = plan(next);
        assert!(p.in_place.contains(r) && !p.rebuild.contains(r));
        let p = plan(with_parts(&[Part::Values]));
        assert!(p.in_place.contains(r) && !p.rebuild.contains(r), "card values in place");
    }

    #[test]
    fn region_audio_dock() {
        let r = MainRegion::AudioDock;
        assert!(plan(resized(r)).rebuild.contains(r));
        assert!(plan(with_parts(&[Part::AudioSetup])).rebuild.contains(r));
        let mut next = base();
        next.dock_view = 1;
        assert!(plan(next).rebuild.contains(r));
        assert!(plan(moved(r)).translate.contains(r));
        let mut next = base();
        next.dock_scroll_px = 3;
        let p = plan(next);
        assert!(p.in_place.contains(r) && !p.rebuild.contains(r));
    }

    #[test]
    fn region_split_handles() {
        let r = MainRegion::SplitHandles;
        assert!(plan(resized(r)).rebuild.contains(r));
        let p = plan(moved(r));
        assert!(p.translate.contains(r) && !p.rebuild.contains(r));
    }

    #[test]
    fn region_layer_headers() {
        let r = MainRegion::LayerHeaders;
        assert!(plan(resized(r)).rebuild.contains(r), "width");
        assert!(plan(with_parts(&[Part::Layers])).rebuild.contains(r), "project projection ran");
        assert!(plan(moved(r)).translate.contains(r));
        let in_place: [fn(&mut FrameInputs); 5] = [
            |i| i.scroll_y_px = 9,
            |i| {
                let mut rect = i.rects.get(MainRegion::LayerHeaders);
                rect[3] += 5;
                i.rects.set(MainRegion::LayerHeaders, rect);
            },
            |i| i.selection_visual = 1,
            |i| i.active_layer = 1,
            |i| i.automation_view = 1,
        ];
        for edit in in_place {
            let mut next = base();
            edit(&mut next);
            assert!(plan(next).in_place.contains(r));
        }
        let mut next = base();
        next.scroll_y_px = 9;
        assert!(!plan(next).rebuild.contains(r), "scroll never rebuilds");
    }

    #[test]
    fn region_viewport() {
        let r = MainRegion::Viewport;
        assert!(plan(resized(r)).rebuild.contains(r), "width");
        assert!(plan(with_parts(&[Part::Clips])).rebuild.contains(r), "project projection ran");
        assert!(plan(with_parts(&[Part::Markers])).rebuild.contains(r), "markers");
        assert!(plan(moved(r)).translate.contains(r));
        let in_place: [fn(&mut FrameInputs); 6] = [
            |i| i.scroll_x_px = 9,
            |i| i.zoom_bits = 2,
            |i| i.selection_visual = 1,
            |i| i.cursor = 1,
            |i| i.preview.clips = 1,
            |i| i.automation_view = 1,
        ];
        for edit in in_place {
            let mut next = base();
            edit(&mut next);
            let p = plan(next);
            assert!(p.in_place.contains(r));
        }
        let mut next = base();
        next.preview.clips = 1;
        assert!(!plan(next).rebuild.contains(r), "a drag preview never rebuilds");
    }

    #[test]
    fn region_overlays() {
        let r = MainRegion::Overlays;
        let mut next = base();
        next.overlays = 1;
        assert!(plan(next).rebuild.contains(r));
        let mut next = base();
        next.runtime.ableton_session = 1;
        let p = plan(next);
        assert!(p.in_place.contains(r) && !p.rebuild.contains(r));
        // Not keyed until their sequences bump (P4b runtime-sequence rule).
        for edit in [|i: &mut FrameInputs| i.runtime.warmup = 1, |i: &mut FrameInputs| i.runtime.export_progress = 1] {
            let mut next = base();
            edit(&mut next);
            assert!(!plan(next).in_place.contains(r));
        }
    }

    #[test]
    fn region_ghosts_have_no_atlas_slot() {
        let mut next = base();
        next.rects.set(MainRegion::Ghosts, [1, 2, 3, 4]);
        let p = plan(next);
        assert!(!p.repaint.contains(MainRegion::Ghosts));
    }

    #[test]
    fn earliest_rebuild_region_is_in_build_order() {
        let mut mask = RegionMask::NONE;
        mask.insert(MainRegion::Viewport);
        mask.insert(MainRegion::Inspector);
        assert_eq!(mask.earliest(), Some(MainRegion::Inspector));
    }

    #[test]
    fn ui_stamps_fold_only_the_parts_that_moved() {
        let mut stamps = UiStamps::default();
        let mut content = PartStamps::default();
        content.bump(ChangeSet::of(&[Part::Clips]));
        let before = stamps.parts();
        assert_eq!(stamps.fold_snapshot(&content), ChangeSet::of(&[Part::Clips]));
        assert_eq!(stamps.parts().moved_since(&before), ChangeSet::of(&[Part::Clips]));
        let before = stamps.parts();
        assert!(stamps.fold_snapshot(&content).is_empty(), "the same snapshot twice moves nothing");
        assert_eq!(stamps.parts(), before);
        stamps.bump(ChangeSet::of(&[Part::Effects]));
        assert_eq!(stamps.parts().moved_since(&before), ChangeSet::of(&[Part::Effects]));
    }

    #[test]
    fn active_layer_version_moves_only_on_change() {
        let mut planner = FramePlanner::default();
        let a = Some(manifold_core::LayerId::new("a"));
        let v1 = planner.active_layer_version(&a);
        assert_eq!(planner.active_layer_version(&a), v1);
        assert!(planner.active_layer_version(&None) > v1);
    }
}
