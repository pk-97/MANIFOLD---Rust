//! Per-thread work counters for the count gate (UI_INVALIDATION_DESIGN.md D10).
//!
//! Each counted site calls [`record_work`]. Counters are thread-local only, so
//! the UI thread and the content thread count separately and a test reads the
//! work its own thread did. Counts, never timings: a gate built on these can
//! not flake on a busy machine.
//!
//! Shaped like `cold_touch.rs`, minus the process-wide atomics. Counters are
//! never reset; callers keep a [`WorkSnapshot`] and take [`WorkSnapshot::since`].

use std::cell::Cell;
use std::sync::OnceLock;

/// One kind of counted work. A kind with no site yet is named in its doc line;
/// the phase that adds its code adds the site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkKind {
    /// `UIRoot::build`: the whole main-window tree.
    FullBuild,
    /// One region cleared and rebuilt alone in its own arena (D7).
    RegionRebuild,
    /// A region moved in place without rebuilding (D7).
    RegionTranslate,
    /// Existing nodes updated in place (scroll and similar).
    InPlaceUpdate,
    /// One UI atlas slot invalidated (all slots of `invalidate_all` count each).
    SlotInvalidate,
    /// `sync_project_data`.
    ProjectSync,
    /// `sync_inspector_data`.
    InspectorSync,
    /// `sync_clip_positions`.
    ClipProjection,
    /// `sync_automation_lane_geometry` or `sync_automation_lane_order`.
    LaneProjection,
    /// The embedded preset list sync in the dropdowns.
    PresetListProjection,
    /// `push_state`, the per-frame value push.
    ValuePush,
    /// Per-frame graph-editor projections (visible nodes, preview node, mini timeline).
    EditorProjection,
    /// `rebuild_editor_inspector`.
    EditorTreeBuild,
    /// One perform-mode HUD draw-list build.
    PerformHudBuild,
    /// One overview strip repaint.
    OverviewRepaint,
    /// Chrome (transport, header, footer) view build and layout.
    ChromeLayout,
    /// One text measure that missed every cache (today: every uncached measure).
    TextMeasureMiss,
    /// `sync_driver_bpm` scanning the drivers.
    DriverScan,
    /// A deep clone of the whole `Project`, on either thread.
    ProjectClone,
    /// The modulation snapshot copied into `ContentState`.
    ModSnapshotCopy,
    /// `on_project_loaded` on one renderer.
    RendererReload,
    /// Ableton `rebuild_listeners`.
    AbletonListenerRebuild,
    /// In-place refresh of cached Ableton ranges. No site until P3a.
    ListenerRangeRefresh,
    /// OSC parameter route rebuild.
    OscRouteRebuild,
    /// Project preset overlay install.
    PresetOverlayInstall,
    /// Audio modulation runtime reconcile.
    AudioReconcile,
    /// A CoreAudio device or process query.
    DeviceQuery,
    /// Clip-control binding reconcile that walked the clips.
    ClipBindingReconcile,
    /// `GeneratorRenderer::render_all` structure refresh.
    GeneratorStructureRefresh,
    /// `reconcile_tempo_edit`.
    TempoReconcile,
    /// `trigger_targets.refresh` that rebuilt the targets.
    TriggerTargetRefresh,
    /// A drag preview wrote the clips it drags into the viewport in place.
    ClipPreviewUpdate,
    /// A drag preview re-projected the lanes it drags in place.
    LanePreviewUpdate,
    /// One heap allocation, counted by the `work-trace` allocator wrapper.
    FrameAlloc,
}

pub const WORK_KIND_COUNT: usize = 34;

impl WorkKind {
    pub const ALL: [WorkKind; WORK_KIND_COUNT] = [
        WorkKind::FullBuild,
        WorkKind::RegionRebuild,
        WorkKind::RegionTranslate,
        WorkKind::InPlaceUpdate,
        WorkKind::SlotInvalidate,
        WorkKind::ProjectSync,
        WorkKind::InspectorSync,
        WorkKind::ClipProjection,
        WorkKind::LaneProjection,
        WorkKind::PresetListProjection,
        WorkKind::ValuePush,
        WorkKind::EditorProjection,
        WorkKind::EditorTreeBuild,
        WorkKind::PerformHudBuild,
        WorkKind::OverviewRepaint,
        WorkKind::ChromeLayout,
        WorkKind::TextMeasureMiss,
        WorkKind::DriverScan,
        WorkKind::ProjectClone,
        WorkKind::ModSnapshotCopy,
        WorkKind::RendererReload,
        WorkKind::AbletonListenerRebuild,
        WorkKind::ListenerRangeRefresh,
        WorkKind::OscRouteRebuild,
        WorkKind::PresetOverlayInstall,
        WorkKind::AudioReconcile,
        WorkKind::DeviceQuery,
        WorkKind::ClipBindingReconcile,
        WorkKind::GeneratorStructureRefresh,
        WorkKind::TempoReconcile,
        WorkKind::TriggerTargetRefresh,
        WorkKind::ClipPreviewUpdate,
        WorkKind::LanePreviewUpdate,
        WorkKind::FrameAlloc,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    /// The name ceilings files and trace lines use.
    pub fn name(self) -> &'static str {
        match self {
            WorkKind::FullBuild => "FullBuild",
            WorkKind::RegionRebuild => "RegionRebuild",
            WorkKind::RegionTranslate => "RegionTranslate",
            WorkKind::InPlaceUpdate => "InPlaceUpdate",
            WorkKind::SlotInvalidate => "SlotInvalidate",
            WorkKind::ProjectSync => "ProjectSync",
            WorkKind::InspectorSync => "InspectorSync",
            WorkKind::ClipProjection => "ClipProjection",
            WorkKind::LaneProjection => "LaneProjection",
            WorkKind::PresetListProjection => "PresetListProjection",
            WorkKind::ValuePush => "ValuePush",
            WorkKind::EditorProjection => "EditorProjection",
            WorkKind::EditorTreeBuild => "EditorTreeBuild",
            WorkKind::PerformHudBuild => "PerformHudBuild",
            WorkKind::OverviewRepaint => "OverviewRepaint",
            WorkKind::ChromeLayout => "ChromeLayout",
            WorkKind::TextMeasureMiss => "TextMeasureMiss",
            WorkKind::DriverScan => "DriverScan",
            WorkKind::ProjectClone => "ProjectClone",
            WorkKind::ModSnapshotCopy => "ModSnapshotCopy",
            WorkKind::RendererReload => "RendererReload",
            WorkKind::AbletonListenerRebuild => "AbletonListenerRebuild",
            WorkKind::ListenerRangeRefresh => "ListenerRangeRefresh",
            WorkKind::OscRouteRebuild => "OscRouteRebuild",
            WorkKind::PresetOverlayInstall => "PresetOverlayInstall",
            WorkKind::AudioReconcile => "AudioReconcile",
            WorkKind::DeviceQuery => "DeviceQuery",
            WorkKind::ClipBindingReconcile => "ClipBindingReconcile",
            WorkKind::GeneratorStructureRefresh => "GeneratorStructureRefresh",
            WorkKind::TempoReconcile => "TempoReconcile",
            WorkKind::TriggerTargetRefresh => "TriggerTargetRefresh",
            WorkKind::ClipPreviewUpdate => "ClipPreviewUpdate",
            WorkKind::LanePreviewUpdate => "LanePreviewUpdate",
            WorkKind::FrameAlloc => "FrameAlloc",
        }
    }
}

thread_local! {
    // Const-initialized and drop-free, so the counting allocator may touch it
    // from inside an allocation without recursing.
    static COUNTERS: [Cell<u64>; WORK_KIND_COUNT] = const { [const { Cell::new(0) }; WORK_KIND_COUNT] };
}

/// Count one unit of `kind` on the calling thread. No allocation.
#[inline]
pub fn record_work(kind: WorkKind) {
    // `try_with`: the allocator calls this during thread teardown too.
    let _ = COUNTERS.try_with(|c| {
        let cell = &c[kind.index()];
        cell.set(cell.get().wrapping_add(1));
    });
}

/// Count `n` units of `kind` on the calling thread.
#[inline]
pub fn record_work_n(kind: WorkKind, n: u64) {
    let _ = COUNTERS.try_with(|c| {
        let cell = &c[kind.index()];
        cell.set(cell.get().wrapping_add(n));
    });
}

/// Every counter of the calling thread.
pub fn thread_work_snapshot() -> WorkSnapshot {
    let mut out = [0u64; WORK_KIND_COUNT];
    let _ = COUNTERS.try_with(|c| {
        for (o, cell) in out.iter_mut().zip(c.iter()) {
            *o = cell.get();
        }
    });
    WorkSnapshot(out)
}

/// A copy of one thread's counters, or a difference of two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkSnapshot([u64; WORK_KIND_COUNT]);

impl Default for WorkSnapshot {
    fn default() -> Self {
        WorkSnapshot([0; WORK_KIND_COUNT])
    }
}

impl WorkSnapshot {
    /// Work done between `earlier` and `self`.
    pub fn since(&self, earlier: &WorkSnapshot) -> WorkSnapshot {
        let mut out = [0u64; WORK_KIND_COUNT];
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.0[i].wrapping_sub(earlier.0[i]);
        }
        WorkSnapshot(out)
    }

    /// `self` minus `other`, saturating at zero. Used to remove work a harness
    /// did on the measured thread that the live app does elsewhere.
    pub fn minus(&self, other: &WorkSnapshot) -> WorkSnapshot {
        let mut out = [0u64; WORK_KIND_COUNT];
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.0[i].saturating_sub(other.0[i]);
        }
        WorkSnapshot(out)
    }

    pub fn plus(&self, other: &WorkSnapshot) -> WorkSnapshot {
        let mut out = [0u64; WORK_KIND_COUNT];
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.0[i].wrapping_add(other.0[i]);
        }
        WorkSnapshot(out)
    }

    pub fn get(&self, kind: WorkKind) -> u64 {
        self.0[kind.index()]
    }

    /// Kinds with a non-zero count, in `WorkKind::ALL` order.
    pub fn nonzero(&self) -> impl Iterator<Item = (WorkKind, u64)> + '_ {
        WorkKind::ALL
            .into_iter()
            .map(|k| (k, self.0[k.index()]))
            .filter(|&(_, n)| n != 0)
    }

    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|&n| n == 0)
    }
}

/// Whether `MANIFOLD_WORK_TRACE=1` was set. The environment is read once per
/// process; the flag never changes after.
pub fn work_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("MANIFOLD_WORK_TRACE").is_ok_and(|v| v == "1"))
}

/// Per-frame trace for one thread: prints the work done since the last call
/// as one line, and nothing on a frame that did no work. Keep one per thread.
#[derive(Default)]
pub struct WorkTrace {
    last: WorkSnapshot,
    frame: u64,
}

impl WorkTrace {
    pub fn new() -> Self {
        Self { last: thread_work_snapshot(), frame: 0 }
    }

    /// Call once per frame on the owning thread. No-op unless tracing is on.
    pub fn frame(&mut self, thread: &str) {
        if !work_trace_enabled() {
            return;
        }
        self.frame += 1;
        let now = thread_work_snapshot();
        let delta = now.since(&self.last);
        self.last = now;
        if delta.is_zero() {
            return;
        }
        let mut line = String::with_capacity(160);
        line.push_str("[work] ");
        line.push_str(thread);
        line.push_str(" frame=");
        line.push_str(&self.frame.to_string());
        for (kind, n) in delta.nonzero() {
            line.push(' ');
            line.push_str(kind.name());
            line.push('=');
            line.push_str(&n.to_string());
        }
        eprintln!("{line}");
        // The trace's own formatting allocates; keep it out of the next frame.
        self.last = thread_work_snapshot();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_per_thread_and_diffable() {
        let before = thread_work_snapshot();
        record_work(WorkKind::ProjectSync);
        record_work(WorkKind::ProjectSync);
        record_work_n(WorkKind::FrameAlloc, 5);
        let delta = thread_work_snapshot().since(&before);
        assert_eq!(delta.get(WorkKind::ProjectSync), 2);
        assert_eq!(delta.get(WorkKind::FrameAlloc), 5);

        let other = std::thread::spawn(|| {
            let before = thread_work_snapshot();
            record_work(WorkKind::ProjectClone);
            thread_work_snapshot().since(&before)
        })
        .join()
        .unwrap();
        assert_eq!(other.get(WorkKind::ProjectClone), 1);
        assert_eq!(thread_work_snapshot().since(&before).get(WorkKind::ProjectClone), 0);
    }

    #[test]
    fn names_round_trip_and_cover_every_kind() {
        for (i, kind) in WorkKind::ALL.into_iter().enumerate() {
            assert_eq!(kind.index(), i, "ALL must follow declaration order");
        }
    }
}
