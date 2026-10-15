//! The main window's frame planner (UI_INVALIDATION_DESIGN.md D5, D6, P4b).
//!
//! `plan_frame` compares this frame's [`FrameInputs`] with last frame's and
//! returns the work to do. It is a pure function over integers: equal inputs
//! give an empty plan. Input code states facts by moving a version; it never
//! picks work. Section 4 of the design is the table this encodes, and the
//! unit tests below pin one test per row of both tables.
//!
//! The planner alone decides projections (P4d) and tree work (P5a): no hand-set
//! flag remains beside it.

use manifold_core::change::{ChangeSet, Part, PartStamps};

use crate::content_state::RuntimeSeqs;

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

    /// The local project was replaced wholesale (open, new, snapshot restore,
    /// the content's save snapshot): a load moves every part (D1, INV-8).
    pub(crate) fn project_replaced(&mut self) {
        self.parts.bump(ChangeSet::ALL);
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
/// the region rank of paint order (D7). The discriminant is the region's
/// arena id in the tree.
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

// Every region id must name an arena.
const _: () = assert!(MAIN_REGION_COUNT <= manifold_ui::tree::DEFAULT_REGION as usize);

impl MainRegion {
    /// The region's arena id in the tree.
    pub(crate) const fn id(self) -> u8 {
        self as u8
    }

    /// The region's atlas slot; overlays and the ghost draw outside the atlas.
    pub(crate) fn panel_slot(self) -> Option<manifold_ui_paint::ui_cache_manager::PanelSlot> {
        use manifold_ui_paint::ui_cache_manager::PanelSlot;
        match self {
            MainRegion::Transport => Some(PanelSlot::Transport),
            MainRegion::Header => Some(PanelSlot::Header),
            MainRegion::Footer => Some(PanelSlot::Footer),
            MainRegion::Inspector => Some(PanelSlot::Inspector),
            MainRegion::AudioDock => Some(PanelSlot::AudioDock),
            MainRegion::SplitHandles => Some(PanelSlot::SplitHandles),
            MainRegion::LayerHeaders => Some(PanelSlot::LayerHeaders),
            MainRegion::Viewport => Some(PanelSlot::Viewport),
            MainRegion::Overlays | MainRegion::Ghosts => None,
        }
    }

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

    pub(crate) fn remove(&mut self, region: MainRegion) {
        self.0 &= !(1 << region as u8);
    }

    pub(crate) fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The regions in the mask, in build order.
    pub(crate) fn iter(self) -> impl Iterator<Item = MainRegion> {
        MainRegion::ALL.into_iter().filter(move |&r| self.contains(r))
    }
}

/// One pixel-quantized `[x, y, w, h]` per region (D7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RegionRects(pub [[i32; 4]; MAIN_REGION_COUNT]);

impl RegionRects {
    pub(crate) fn get(&self, region: MainRegion) -> [i32; 4] {
        self.0[region as usize]
    }

    pub(crate) fn set(&mut self, region: MainRegion, rect: [i32; 4]) {
        self.0[region as usize] = rect;
    }
}

/// What the chrome push shows that no part stamp covers, at display
/// resolution (D15): one field per `push_chrome` read of `ContentState` and
/// the app. Project reads key on parts; the zoom label on `zoom_bits`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChromeInputs {
    pub playing: bool,
    pub recording: bool,
    pub automation_armed: bool,
    pub automation_draw_mode: bool,
    pub link_enabled: bool,
    pub link_peers: i32,
    pub midi_clock_enabled: bool,
    pub midi_clock_receiving: bool,
    pub osc_sync: u8,
    pub ableton_transport_enabled: bool,
    pub osc_sender_enabled: bool,
    pub ableton_sync: u8,
    pub clock_authority: u8,
    /// The shown BPM, in tenths.
    pub bpm_tenths: i32,
    /// Detect progress in tenths of a percent, and whether it shows.
    pub detect_permille: u16,
    pub detect_show: bool,
    /// Export progress is open: the header hides the detect status.
    pub export_open: bool,
    pub is_dirty: bool,
    /// Moves when a shown string changed: the clock device and position, the
    /// detect status, the project path (compared by retained pointer).
    pub text: u64,
}

/// The app state the chrome push reads besides `ContentState`.
pub(crate) struct ChromeSource<'a> {
    pub project: &'a manifold_core::project::Project,
    pub is_dirty: bool,
    pub project_path: Option<&'a std::path::Path>,
    pub automation_draw_mode: bool,
}

/// The strings the chrome shows, retained so a change is a pointer compare
/// (the content thread caches each as an `Arc<str>`), and the version that
/// moves when one changes.
#[derive(Default)]
struct ChromeText {
    clock_device: Option<std::sync::Arc<str>>,
    clock_position: Option<std::sync::Arc<str>>,
    detect_status: Option<std::sync::Arc<str>>,
    project_path: Option<std::path::PathBuf>,
    version: u64,
}

impl ChromeText {
    fn version(&mut self, content: &crate::content_state::ContentState, path: Option<&std::path::Path>) -> u64 {
        let mut moved = false;
        for (held, sent) in [
            (&mut self.clock_device, &content.midi_clock_device_name),
            (&mut self.clock_position, &content.midi_clock_position_display),
            (&mut self.detect_status, &content.percussion_status_message),
        ] {
            if !held.as_ref().is_some_and(|h| std::sync::Arc::ptr_eq(h, sent)) {
                *held = Some(std::sync::Arc::clone(sent));
                moved = true;
            }
        }
        if self.project_path.as_deref() != path {
            self.project_path = path.map(std::path::Path::to_path_buf);
            moved = true;
        }
        if moved {
            self.version += 1;
        }
        self.version
    }
}

/// Pack the header clock exactly as `push_clock` computes it.
pub(crate) fn clock_digits(content: &crate::content_state::ContentState, beats_per_bar: i32) -> u64 {
    let beat = content.current_beat.as_f32();
    let t = content.current_time.0;
    let mins = (t / 60.0).floor() as i32;
    let secs = (t % 60.0).floor() as i32;
    let tenths = ((t * 10.0) % 10.0).floor() as i32;
    let bpb = beats_per_bar.max(1) as f32;
    let bar = (beat / bpb).floor() as i32 + 1;
    let beat_in_bar = (beat % bpb).floor() as i32 + 1;
    let sixteenth = ((beat % 1.0) * 4.0).floor() as i32 + 1;
    let field = |v: i32, bits: u32| (v as u32 as u64) & ((1u64 << bits) - 1);
    field(mins, 16)
        | field(secs, 6) << 16
        | field(tenths, 4) << 22
        | field(bar, 20) << 26
        | field(beat_in_bar, 8) << 46
        | field(sixteenth, 4) << 54
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
    /// D19 (selected point's lane included).
    pub automation_view: u64,
    /// Automation lane heights the timeline shows.
    pub lane_heights: u64,
    pub chrome: ChromeInputs,
    pub rects: RegionRects,
    pub scroll_x_px: i32,
    pub scroll_y_px: i32,
    pub zoom_bits: u32,
    /// The exact scroll, so a sub-pixel move still updates the scroll panels.
    pub scroll_x_bits: u32,
    pub scroll_y_bits: u32,
    /// The overview strip's playhead only.
    pub playhead_px: i32,
    pub inspector_view: u64,
    /// The Scene panel's outliner selection (its Properties body).
    pub scene_selection: u64,
    /// A queued Scene tab or trigger-response reveal; the inspector
    /// projection takes it.
    pub inspector_reveal: u64,
    /// A queued timeline layer reveal; the project projection takes it.
    pub layer_reveal: u64,
    pub inspector_scroll_px: i32,
    pub dock_view: u64,
    pub dock_scroll_px: i32,
    pub overlays: u64,
    /// Any overlay is open: the project projection rewrites lists an open
    /// overlay shows (the trigger target picker's ticks).
    pub overlay_open: bool,
    pub audio_directory: u64,
    /// D15: `ContentState.values_seq`.
    pub values: u64,
    pub runtime: RuntimeSeqs,
    /// The header clock's shown minutes, seconds, tenths, bar, beat and
    /// sixteenth, packed. It moves several times a second while playing, so
    /// it has its own push and never lays the chrome out.
    pub clock_digits: u64,
    /// D20: the frame-quantized driver period of the content bpm.
    pub driver_period_key: u64,
    /// The LED controller state the master chrome shows (live push).
    pub led_enabled: bool,
}

impl FrameInputs {
    /// The live push's inputs, which the editor window's live push keys on
    /// too (D17, D22).
    pub(crate) fn live(&self) -> LiveInputs {
        LiveInputs {
            parts: self.parts,
            values: self.values,
            active_layer: self.active_layer,
            led_enabled: self.led_enabled,
            ableton_session: self.runtime.ableton_session,
        }
    }
}

/// What every display of a live scalar keys on, in either window (D22).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LiveInputs {
    pub parts: PartStamps,
    pub values: u64,
    pub active_layer: u64,
    pub led_enabled: bool,
    pub ableton_session: u64,
}

/// The live-push rows of the projection table, shared by both windows'
/// planners: Values displays follow the values sequence and the Values
/// stamp, and also show the active layer's name and opacity, the LED
/// controller state and the macro Ableton status (which follows the
/// session); Shapes displays and range bands follow their own stamps.
pub(crate) fn plan_live(last: &LiveInputs, next: &LiveInputs, p: &mut ProjectionMask) {
    let moved = next.parts.moved_since(&last.parts);
    if last.values != next.values
        || moved.intersects(LIVE_VALUE_PARTS)
        || last.active_layer != next.active_layer
        || last.led_enabled != next.led_enabled
        || last.ableton_session != next.ableton_session
    {
        p.insert(Projection::LiveValues);
    }
    if moved.contains(Part::Shapes) {
        p.insert(Projection::LiveShapes);
    }
    if moved.contains(Part::MappingRanges) {
        p.insert(Projection::LiveRanges);
    }
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
    /// `sync_track_layout`: the track and lane heights, which the layer
    /// headers and viewport then lay out in place.
    TrackLayout,
    /// The dragged marker: the viewport's marker list.
    MarkerPreview,
    /// Live push, Values displays.
    LiveValues,
    /// Live push, Shapes displays.
    LiveShapes,
    /// Live push, Ableton range bands.
    LiveRanges,
    /// The Scene outliner eyes, in place.
    SceneEyes,
    /// Insert cursor, selection band, layer highlight, selected clips.
    SelectionPush,
    /// Transport, sync, settings and macro label displays.
    ChromePush,
    /// The header clock's text, in place: its label has a fixed width, so a
    /// new digit never moves the chrome layout.
    ClockPush,
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
    /// Rect resized; the region's panel lays its nodes out again in place at
    /// the new rect.
    pub resize: RegionMask,
    pub in_place: RegionMask,
    /// Atlas slots to repaint.
    pub repaint: RegionMask,
    pub redraw: bool,
    /// Which timeline view axes moved. Applied as the scroll-panel update, so
    /// a scroll or zoom from any input path needs no structural sync.
    pub viewport: ViewportMoves,
    /// A region rect moved or resized: the atlas pixels it left behind belong
    /// to no slot, so the atlas clears and every slot repaints.
    pub rects_moved: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ViewportMoves {
    pub scroll_x: bool,
    pub scroll_y: bool,
    pub zoom: bool,
    /// The layer headers' or viewport's rect, or the track heights, moved:
    /// both lay their rows out again in place.
    pub relayout: bool,
    /// A marker drag moved the marker list: the flags lay out again in place.
    pub markers: bool,
}

impl ViewportMoves {
    pub(crate) fn any(self) -> bool {
        self.scroll_x || self.scroll_y || self.zoom || self.relayout || self.markers
    }
}

#[cfg(test)]
impl FramePlan {
    pub(crate) fn is_empty(&self) -> bool {
        *self == FramePlan::default()
    }
}

#[cfg(feature = "ui-snapshot")]
impl FramePlan {
    /// `AssertFresh`'s reference frame (D10): this plan plus every
    /// projection and every region rebuilt, so the full build reads the same
    /// inputs freshly projected.
    pub(crate) fn with_every_projection(mut self) -> Self {
        self.projections = ProjectionMask(u32::MAX);
        for region in MainRegion::ALL {
            self.rebuild.insert(region);
        }
        self.repaint = self.rebuild;
        self.redraw = true;
        self
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
// Layers: the send rows name their feeding layers. Effects: the consumer rows
// list the audio mods that read each send.
const AUDIO_DEVICE_PARTS: ChangeSet = ChangeSet::of(&[Part::AudioSetup, Part::Layers, Part::Effects]);
const CLIP_POSITION_PARTS: ChangeSet = ChangeSet::of(&[Part::Clips, Part::Layers]);
const LANE_GEOMETRY_PARTS: ChangeSet = ChangeSet::of(&[Part::Automation, Part::Effects, Part::Layers]);
// Clips: the footer and layer-header clip counts (projection_input_matrix).
// Effects: the master effect names behind the LED exit path list.
const CHROME_PARTS: ChangeSet =
    ChangeSet::of(&[Part::Settings, Part::Mappings, Part::Layers, Part::Clips, Part::Effects]);
const LIVE_VALUE_PARTS: ChangeSet = ChangeSet::of(&[Part::Values, Part::Layers]);
const DRIVER_READOUT_PARTS: ChangeSet = ChangeSet::of(&[Part::Effects]);
const PRESET_LIST_PARTS: ChangeSet = ChangeSet::of(&[Part::EmbeddedPresets]);
const OVERVIEW_PARTS: ChangeSet = ChangeSet::of(&[Part::Clips, Part::Layers]);
const WAVEFORM_PARTS: ChangeSet = ChangeSet::of(&[Part::Clips]);
// An eye shows a base value, which only an edit moves. Playback moves the
// values sequence alone, so the eye push never runs per frame.
const SCENE_EYE_PARTS: ChangeSet = ChangeSet::of(&[Part::Values, Part::Shapes]);

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

    if moved.intersects(PROJECT_SYNC_PARTS)
        || selection_identity
        || active_layer
        || automation_view
        || last.layer_reveal != next.layer_reveal
    {
        p.insert(Projection::ProjectSync);
    }
    if moved.intersects(INSPECTOR_SYNC_PARTS)
        || selection_identity
        || active_layer
        || automation_view
        // The scene projection fills the Properties card for the selected
        // outliner item, and takes a queued row reveal.
        || last.scene_selection != next.scene_selection
        || last.inspector_reveal != next.inspector_reveal
        || runtime.automation_latches != last.runtime.automation_latches
        || runtime.led_preview_presence != last.runtime.led_preview_presence
        || runtime.chain_failures != last.runtime.chain_failures
    {
        p.insert(Projection::InspectorSync);
    }
    // The dock view moves when it opens: the query fills it then.
    if moved.intersects(AUDIO_DEVICE_PARTS)
        || last.audio_directory != next.audio_directory
        || last.dock_view != next.dock_view
    {
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
    if last.preview.markers != next.preview.markers {
        p.insert(Projection::MarkerPreview);
    }
    plan_live(&last.live(), &next.live(), p);
    if moved.intersects(SCENE_EYE_PARTS) {
        p.insert(Projection::SceneEyes);
    }
    // The push writes the active layer as well as the selection sets.
    if selection_visual || cursor || active_layer {
        p.insert(Projection::SelectionPush);
    }
    // The chrome push also shows the lanes toggle, the primary clip's chrome
    // values and the zoom label.
    if moved.intersects(CHROME_PARTS)
        || last.chrome != next.chrome
        || automation_view
        || selection_identity
        || last.zoom_bits != next.zoom_bits
    {
        p.insert(Projection::ChromePush);
    }
    if last.clock_digits != next.clock_digits {
        p.insert(Projection::ClockPush);
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

    let lane_heights = last.lane_heights != next.lane_heights;
    if lane_heights {
        p.insert(Projection::TrackLayout);
        p.insert(Projection::LaneGeometry);
    }

    plan.viewport = ViewportMoves {
        scroll_x: last.scroll_x_bits != next.scroll_x_bits,
        scroll_y: last.scroll_y_bits != next.scroll_y_bits,
        zoom: last.zoom_bits != next.zoom_bits,
        relayout: lane_heights,
        markers: false,
    };
    // The timeline lists change shape only when their project parts move or
    // the lanes open or close; a selection or active-layer change re-projects
    // them but repaints the same nodes in place (the selection push).
    let timeline_shape = moved.intersects(PROJECT_SYNC_PARTS) || automation_view;
    plan_regions(&mut plan, last, next, timeline_shape, selection_visual, cursor, active_layer, automation_view, preview);
    plan.redraw = true;
    plan
}

/// The section 4 region table.
///
/// Translate is live for the regions whose panels take a new rect after a
/// move: the three chrome bars (their host re-lays out at the rect it is
/// handed, [`crate::ui_root::UIRoot::follow_translate`]), the split handle
/// (ids only) and the audio dock (its scroll container and hit rects follow).
/// The bars and the split handle also take a new size in place
/// ([`crate::ui_root::UIRoot::follow_resize`]). The layer headers and
/// viewport lay out again in place on a rect move
/// ([`ViewportMoves::relayout`]). The inspector lays its cards out at its
/// width while it builds, so any rect change rebuilds it, and a new dock
/// size rebuilds the dock.
#[allow(clippy::too_many_arguments)]
fn plan_regions(
    plan: &mut FramePlan,
    last: &FrameInputs,
    next: &FrameInputs,
    timeline_shape: bool,
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
    let origin = |r: MainRegion| {
        let (a, b) = (last.rects.get(r), next.rects.get(r));
        a[0] != b[0] || a[1] != b[1]
    };
    let moved = |r: MainRegion| last.rects.get(r) != next.rects.get(r);
    let mut rebuild = RegionMask::NONE;
    let mut translate = RegionMask::NONE;
    let mut resize = RegionMask::NONE;
    let mut in_place = RegionMask::NONE;
    let live = projections.contains(Projection::LiveValues)
        || projections.contains(Projection::LiveShapes)
        || projections.contains(Projection::LiveRanges)
        || projections.contains(Projection::SceneEyes);

    for chrome in [MainRegion::Transport, MainRegion::Header, MainRegion::Footer] {
        // A resized bar takes its new rect and lays out in place.
        if size(chrome) {
            resize.insert(chrome);
            in_place.insert(chrome);
        } else if origin(chrome) {
            translate.insert(chrome);
        }
        // The bars lay out only when a value they show moved: the chrome and
        // selection pushes, and the header's dock button on a dock toggle.
        if projections.contains(Projection::ChromePush)
            || projections.contains(Projection::SelectionPush)
            || (chrome == MainRegion::Header && last.dock_view != next.dock_view)
        {
            in_place.insert(chrome);
        }
    }

    let inspector = MainRegion::Inspector;
    if moved(inspector)
        || projections.contains(Projection::InspectorSync)
        || last.inspector_view != next.inspector_view
    {
        rebuild.insert(inspector);
    }
    if last.inspector_scroll_px != next.inspector_scroll_px || live {
        in_place.insert(inspector);
    }

    // The dock moves when the inspector beside it resizes: it translates.
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
        resize.insert(handles);
    } else if origin(handles) {
        translate.insert(handles);
    }

    // A header row's controls sit at panel-local x, so a new left edge or
    // width rebuilds the headers. Any other move of either rect lays both out
    // again in place, as a lane height change does.
    let headers = MainRegion::LayerHeaders;
    let viewport = MainRegion::Viewport;
    let header_columns = {
        let (a, b) = (last.rects.get(headers), next.rects.get(headers));
        a[0] != b[0] || a[2] != b[2]
    };
    if moved(headers) || moved(viewport) {
        plan.viewport.relayout = true;
    }
    let relayout = plan.viewport.relayout;
    if header_columns || timeline_shape {
        rebuild.insert(headers);
    }
    if last.scroll_y_px != next.scroll_y_px
        || plan.viewport.scroll_y
        || relayout
        || selection_visual
        || active_layer
        || automation_view
    {
        in_place.insert(headers);
    }

    if timeline_shape {
        rebuild.insert(viewport);
    }
    // A marker drag keeps the marker count, so the flags move in place.
    plan.viewport.markers = projections.contains(Projection::MarkerPreview);
    if relayout
        || plan.viewport.markers
        || last.scroll_x_px != next.scroll_x_px
        || last.scroll_y_px != next.scroll_y_px
        || last.zoom_bits != next.zoom_bits
        || plan.viewport.scroll_x
        || plan.viewport.scroll_y
        || selection_visual
        || cursor
        || preview
        || automation_view
    {
        in_place.insert(viewport);
    }

    let overlays = MainRegion::Overlays;
    if last.overlays != next.overlays
        || moved(overlays)
        || (projections.contains(Projection::ProjectSync) && next.overlay_open)
    {
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
    plan.resize = resize;
    plan.in_place = in_place;
    plan.rects_moved = last.rects != next.rects;
    let mut repaint = rebuild.union(translate).union(resize).union(in_place);
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
    chrome_text: ChromeText,
}

impl FramePlanner {
    /// The chrome push's inputs, read the way `push_chrome` reads them.
    pub(crate) fn chrome_inputs(
        &mut self,
        content: &crate::content_state::ContentState,
        source: &ChromeSource<'_>,
    ) -> ChromeInputs {
        use manifold_core::types::ClockAuthority;
        let bpm = if content.clock_authority == ClockAuthority::Internal {
            source.project.settings.bpm.0
        } else {
            content.bpm as f32
        };
        let progress = content.percussion_progress.clamp(0.0, 1.0);
        ChromeInputs {
            playing: content.is_playing,
            recording: content.is_recording,
            automation_armed: content.automation_armed,
            automation_draw_mode: source.automation_draw_mode,
            link_enabled: content.link_enabled,
            link_peers: content.link_peers,
            midi_clock_enabled: content.midi_clock_enabled,
            midi_clock_receiving: content.midi_clock_receiving,
            osc_sync: content.osc_sync_mode as u8,
            ableton_transport_enabled: content.ableton_transport_enabled,
            osc_sender_enabled: content.osc_sender_enabled,
            ableton_sync: content.ableton_sync_status as u8,
            clock_authority: content.clock_authority as u8,
            bpm_tenths: (bpm * 10.0).round() as i32,
            detect_permille: (progress * 1000.0).round() as u16,
            detect_show: content.percussion_show_progress,
            // The gatherer, which holds the UI, fills this in.
            export_open: false,
            is_dirty: source.is_dirty,
            text: self.chrome_text.version(content, source.project_path),
        }
    }

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
        let mut next = base();
        next.runtime.chain_failures = 1;
        assert!(runs(&plan(next), Projection::InspectorSync));
        assert!(!runs(&plan(with_parts(&[Part::Markers])), Projection::InspectorSync));
    }

    #[test]
    fn row_scene_switch_param_rebuilds_the_inspector() {
        // ChangeGraphParamCommand touches Values | Effects for a scene.switch
        // param: the dimmed and folded rows gate on that feature (SCENE_SURFACES
        // D12). A plain card param is Values.
        assert!(runs(&plan(with_parts(&[Part::Values, Part::Effects])), Projection::InspectorSync));
        assert!(!runs(&plan(with_parts(&[Part::Values])), Projection::InspectorSync));
    }

    #[test]
    fn row_audio_setup_device_list() {
        // Layers name the feeding layers, Effects the consumer rows.
        for part in [Part::AudioSetup, Part::Layers, Part::Effects] {
            assert!(runs(&plan(with_parts(&[part])), Projection::AudioDevices), "{part:?}");
        }
        let mut next = base();
        next.audio_directory = 1;
        assert!(runs(&plan(next), Projection::AudioDevices));
        // Opening the dock fills it.
        let mut next = base();
        next.dock_view = 1;
        assert!(runs(&plan(next), Projection::AudioDevices));
        assert!(!runs(&plan(with_parts(&[Part::Markers])), Projection::AudioDevices));
        assert!(!runs(&plan(with_parts(&[Part::Values])), Projection::AudioDevices));
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
        // The eyes follow base-value edits, not the playback values sequence.
        assert!(runs(&plan(with_parts(&[Part::Values])), Projection::SceneEyes));
        assert!(runs(&plan(with_parts(&[Part::Shapes])), Projection::SceneEyes));
        let mut playback = base();
        playback.values = 1;
        assert!(!runs(&plan(playback), Projection::SceneEyes));
        assert!(runs(&plan(with_parts(&[Part::Layers])), Projection::LiveValues));
        let mut next = base();
        next.active_layer = 1;
        assert!(runs(&plan(next), Projection::LiveValues));
        // The master chrome shows the LED controller state and the macro
        // Ableton status, which follows the session.
        let mut next = base();
        next.led_enabled = true;
        assert!(runs(&plan(next), Projection::LiveValues));
        let mut next = base();
        next.runtime.ableton_session = 1;
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
        // Effects: the master effect names behind the LED exit path list.
        for part in [Part::Settings, Part::Mappings, Part::Layers, Part::Clips, Part::Effects] {
            assert!(runs(&plan(with_parts(&[part])), Projection::ChromePush), "{part:?}");
        }
        // The zoom label.
        let mut next = base();
        next.zoom_bits = 1;
        assert!(runs(&plan(next), Projection::ChromePush));
        // Each chrome input on its own.
        for edit in [
            |i: &mut FrameInputs| i.chrome.playing = true,
            |i: &mut FrameInputs| i.chrome.link_peers = 2,
            |i: &mut FrameInputs| i.chrome.detect_permille = 500,
            |i: &mut FrameInputs| i.chrome.text = 3,
            |i: &mut FrameInputs| i.chrome.is_dirty = true,
        ] {
            let mut next = base();
            edit(&mut next);
            assert!(runs(&plan(next), Projection::ChromePush));
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
    }

    #[test]
    fn row_clock_push() {
        // A new clock digit pushes the clock alone: no chrome push, so no
        // chrome bar lays out.
        let mut next = base();
        next.clock_digits = 7;
        let p = plan(next);
        assert!(runs(&p, Projection::ClockPush));
        assert!(!runs(&p, Projection::ChromePush));
        for bar in [MainRegion::Transport, MainRegion::Header, MainRegion::Footer] {
            assert!(!p.in_place.contains(bar), "{bar:?}");
        }
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
            let p = plan(resized(region));
            assert!(p.resize.contains(region) && p.in_place.contains(region) && !p.rebuild.contains(region));
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
        // The Scene panel's selection re-runs the scene projection that fills
        // its Properties card; the timeline lists stay.
        let mut next = base();
        next.scene_selection = 1;
        let p = plan(next);
        assert!(p.rebuild.contains(r));
        assert!(p.projections.contains(Projection::InspectorSync));
        assert!(!p.projections.contains(Projection::ProjectSync));
        // The inspector lays its cards out at its width while it builds: any
        // rect change rebuilds it.
        assert!(plan(moved(r)).rebuild.contains(r));
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
        let p = plan(moved(r));
        assert!(p.translate.contains(r) && !p.rebuild.contains(r), "the dock follows the inspector's edge");
        let mut next = base();
        next.dock_scroll_px = 3;
        let p = plan(next);
        assert!(p.in_place.contains(r) && !p.rebuild.contains(r), "dock scrolls in place");
    }

    #[test]
    fn region_split_handles() {
        let r = MainRegion::SplitHandles;
        let p = plan(resized(r));
        assert!(p.resize.contains(r) && !p.rebuild.contains(r));
        let p = plan(moved(r));
        assert!(p.translate.contains(r) && !p.rebuild.contains(r));
    }

    #[test]
    fn region_layer_headers() {
        let r = MainRegion::LayerHeaders;
        assert!(plan(resized(r)).rebuild.contains(r), "width");
        assert!(plan(with_parts(&[Part::Layers])).rebuild.contains(r), "project projection ran");
        // A new top or height lays the rows out again in place.
        let p = plan(moved(r));
        assert!(p.in_place.contains(r) && !p.rebuild.contains(r) && p.viewport.relayout, "top");
        let mut next = base();
        let mut rect = next.rects.get(r);
        rect[3] += 5;
        next.rects.set(r, rect);
        let p = plan(next);
        assert!(p.in_place.contains(r) && !p.rebuild.contains(r) && p.viewport.relayout, "height");
        let mut next = base();
        next.lane_heights = 1;
        let p = plan(next);
        assert!(p.in_place.contains(r) && !p.rebuild.contains(r) && p.viewport.relayout, "lane heights");
        assert!(runs(&p, Projection::TrackLayout) && !runs(&p, Projection::ProjectSync));
        assert!(!runs(&p, Projection::InspectorSync), "a lane resize leaves the inspector alone");
        let in_place: [fn(&mut FrameInputs); 4] = [
            |i| i.scroll_y_px = 9,
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
        assert!(plan(with_parts(&[Part::Clips])).rebuild.contains(r), "project projection ran");
        assert!(plan(with_parts(&[Part::Markers])).rebuild.contains(r), "markers");
        for next in [moved(r), resized(r)] {
            let p = plan(next);
            assert!(p.in_place.contains(r) && !p.rebuild.contains(r) && p.viewport.relayout, "a rect move lays out in place");
        }
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
        let mut next = base();
        next.preview.markers = 1;
        let p = plan(next);
        assert!(p.in_place.contains(r) && !p.rebuild.contains(r), "marker flags move in place");
        assert!(!runs(&p, Projection::ProjectSync), "a marker drag projects only the markers");
    }

    #[test]
    fn row_viewport_scroll_and_zoom() {
        let mut next = base();
        next.scroll_x_bits = 0.3f32.to_bits();
        let p = plan(next);
        assert_eq!(p.viewport, ViewportMoves { scroll_x: true, ..Default::default() }, "a sub-pixel scroll");
        assert!(p.in_place.contains(MainRegion::Viewport));

        let mut next = base();
        next.scroll_y_bits = 12.0f32.to_bits();
        let p = plan(next);
        assert_eq!(p.viewport, ViewportMoves { scroll_y: true, ..Default::default() });
        assert!(p.in_place.contains(MainRegion::LayerHeaders));

        let mut next = base();
        next.zoom_bits = 2;
        assert_eq!(plan(next).viewport, ViewportMoves { zoom: true, ..Default::default() });

        for edit in [
            |i: &mut FrameInputs| i.scroll_x_bits = 1,
            |i: &mut FrameInputs| i.scroll_y_bits = 1,
            |i: &mut FrameInputs| i.zoom_bits = 1,
        ] {
            let mut next = base();
            edit(&mut next);
            let p = plan(next);
            assert!(!runs(&p, Projection::ProjectSync), "a view move never projects");
            assert!(!runs(&p, Projection::InspectorSync));
            assert!(p.rebuild.is_empty(), "a view move never rebuilds a region");
        }
    }

    #[test]
    fn region_overlays() {
        let r = MainRegion::Overlays;
        let mut next = base();
        next.overlays = 1;
        assert!(plan(next).rebuild.contains(r));
        // The project projection rewrites the lists an open overlay shows.
        let mut next = with_parts(&[Part::Layers]);
        assert!(!plan(next).rebuild.contains(r), "nothing open, nothing to rebuild");
        next.overlay_open = true;
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

    /// A split drag moves the chrome bar and the handle under it in place and
    /// rebuilds the two timeline regions; every slot repaints because the
    /// pixels a moved region leaves behind belong to no slot.
    #[test]
    fn a_rect_change_flags_the_atlas_and_moves_regions_in_place() {
        assert!(!plan(base()).rects_moved);
        let mut next = base();
        for region in [MainRegion::Footer, MainRegion::SplitHandles] {
            let mut rect = next.rects.get(region);
            rect[1] += 4;
            next.rects.set(region, rect);
        }
        for region in [MainRegion::LayerHeaders, MainRegion::Viewport] {
            let mut rect = next.rects.get(region);
            rect[3] += 4;
            next.rects.set(region, rect);
        }
        let p = plan(next);
        assert!(p.rects_moved);
        assert!(p.translate.contains(MainRegion::Footer) && p.translate.contains(MainRegion::SplitHandles));
        assert!(p.viewport.relayout, "the layer headers and viewport lay out again in place");
        for untouched in [
            MainRegion::Transport,
            MainRegion::Header,
            MainRegion::Inspector,
            MainRegion::AudioDock,
            MainRegion::LayerHeaders,
            MainRegion::Viewport,
        ] {
            assert!(!p.rebuild.contains(untouched) && !p.translate.contains(untouched), "{untouched:?}");
        }
        let mut next = base();
        next.rects.set(MainRegion::Overlays, [0, 0, 9, 9]);
        assert!(plan(next).rebuild.contains(MainRegion::Overlays), "a resized screen rebuilds the overlays");
    }

    #[test]
    fn region_ghosts_have_no_atlas_slot() {
        let mut next = base();
        next.rects.set(MainRegion::Ghosts, [1, 2, 3, 4]);
        let p = plan(next);
        assert!(!p.repaint.contains(MainRegion::Ghosts));
    }

    #[test]
    fn region_mask_iterates_in_build_order() {
        let mut mask = RegionMask::NONE;
        mask.insert(MainRegion::Viewport);
        mask.insert(MainRegion::Inspector);
        assert_eq!(mask.iter().collect::<Vec<_>>(), [MainRegion::Inspector, MainRegion::Viewport]);
        mask.remove(MainRegion::Inspector);
        assert_eq!(mask.iter().collect::<Vec<_>>(), [MainRegion::Viewport]);
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
    fn row_queued_reveals() {
        // A layer reveal is taken by the project projection, a Scene row
        // reveal by the inspector projection, whatever else moved.
        let mut next = base();
        next.layer_reveal = 1;
        assert!(runs(&plan(next), Projection::ProjectSync));
        let mut next = base();
        next.inspector_reveal = 1;
        let p = plan(next);
        assert!(runs(&p, Projection::InspectorSync));
        assert!(!runs(&p, Projection::ProjectSync));
        assert!(p.rebuild.contains(MainRegion::Inspector));
    }

    #[test]
    fn row_marker_preview() {
        let mut next = base();
        next.preview.markers = 1;
        let p = plan(next);
        assert!(runs(&p, Projection::MarkerPreview));
        assert!(!runs(&p, Projection::InspectorSync));
        assert!(!runs(&p, Projection::ClipPreview));
        assert!(p.viewport.markers && p.in_place.contains(MainRegion::Viewport));
        assert!(!p.rebuild.contains(MainRegion::Viewport), "a marker drag moves the flags in place");
    }

    #[test]
    fn a_project_load_runs_every_part_keyed_projection() {
        let mut stamps = UiStamps::default();
        let mut next = base();
        stamps.project_replaced();
        next.parts = stamps.parts();
        let p = plan(next);
        for projection in [
            Projection::ProjectSync,
            Projection::InspectorSync,
            Projection::AudioDevices,
            Projection::ClipPositions,
            Projection::LaneGeometry,
            Projection::LiveValues,
            Projection::LiveShapes,
            Projection::LiveRanges,
            Projection::SceneEyes,
            Projection::SelectionPush,
            Projection::ChromePush,
            Projection::DriverReadout,
            Projection::PresetList,
        ] {
            assert!(runs(&p, projection), "{projection:?}");
        }
        for region in [MainRegion::Inspector, MainRegion::AudioDock, MainRegion::LayerHeaders, MainRegion::Viewport] {
            assert!(p.rebuild.contains(region), "{region:?}");
        }
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
