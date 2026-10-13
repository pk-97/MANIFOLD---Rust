# UI Invalidation — work runs only when its inputs change

**Status:** PROPOSED · 2026-10-13 · Opus 5.5 · lead-reviewed; Peter's answers folded into section 8; awaits Peter's approval.
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs — refactors and API changes) before starting any phase.

**The governing insight: every redundant-work finding in the October UI audit has one root. Nothing can answer "did my input change?" The project has one version for everything, so the content thread and the UI either recompute, deep-compare, or rebuild everything. Input handlers fill the gap by hand-picking flags, and the safe flag is "rebuild it all".** This design puts a version on each part of the project at the one place edits happen (`EditingService`), puts a version on each piece of UI view state where it is owned, and makes one per-frame planner compare those versions and decide all work. Input code states facts ("I wrote the clips"); it never chooses work. An idle frame compares a few integers and does nothing.

On stage: dragging LED brightness or an Ableton trim stops doing project-load work on the render thread, so the output never hitches on a mixer move. Hovering, scrolling and modifier keys stop rebuilding the UI, so controls stay responsive mid-set (the lag behind BUG-y3a9a (automation-lane-hover-drag-full-rebuild-lag)). An open graph editor stops costing frames when nobody touches it. And a count gate stops the next feature from adding per-frame work a few percent at a time.

Direction agreed by Peter and the lead on 2026-10-13 (change stamps at the gateway, inputs declared by consumers, content reacts to the same stamps, one cached text measurer, a count gate). Where this doc departs from it, the decision says so and why.

**Binding constraints** (docs/DESIGN_AUTHORING.md section 1 (The intake)): *Hot path* — the planner and the counters run every frame on both threads, so both are fixed-size integer work, no allocation. *Thread residency* — content stamps are owned by `EditingService` on the content thread and travel in `ContentState`; UI stamps are owned by the UI thread; nothing is shared. *Persistence* — stamps are never serialized; a load moves every part. *Performance surface* — slider, opacity and LED drags must stay value-only writes.

Companion docs: `UI_HARNESS_UNIFICATION_DESIGN.md` (the `ui_frame.rs` seam and the script runner this extends), `WIDGET_TREE_DESIGN.md` (its forbidden live data-binding registry applies here too), `PERF_BUDGET_GATE_DESIGN.md` (frame-time gate; this is its count-based sibling), `HEADLESS_UI_HARNESS.md` (flows and dumps).

---

## 1. Audit — what exists (verified 2026-10-13 on main `84ddd8dfe`)

| Piece | Where | State · use |
|---|---|---|
| Project-wide version | `service.rs` `EditingService.data_version` :49, bumped on execute/undo/redo/record/load | Exists. Kept for "publish a snapshot" and "unsaved". Stops being a change detector. |
| Per-instance graph versions | `instance.rs` `bump_graph_version` / `bump_graph_structure_version` :273 | Exists. Reused as the finer key for graph consumers (editor badge, canvas, viewport def). |
| Command scope hook precedent | `command.rs` `Command::graph_admission_targets` :13 (append-into-out, default empty) | Exists. `touches()` sits beside it. |
| Commands | `rg 'impl Command for' crates -g '*.rs' \| wc -l` → 204 | Each gets `touches()`. |
| Mutation closures that bypass `EditingService` | `content_command.rs` `MutateProject` :179, `MutateProjectLive` :192, `MutateProjectPreview` :207; handlers `content_commands.rs` :1699–1767 | Three variants that hand-pick reactions. 48 + 19 + 6 send sites. Collapse to one. |
| Content reactions after every command | `content_commands.rs` Execute arm :1229–1241 (OSC routes, Ableton listeners, preset overlay); MutateProject arm :1712–1733 (`on_project_loaded` on every renderer, listeners, overlay) | Rewired to stamps. |
| Change detection by serializing | `project_io.rs` `embedded_presets_fingerprint` :135 (serde_json of every embedded def); callers `content_commands.rs` :212, `dropdowns.rs` :639 (every UI frame via `app_render.rs` :624) | Deleted. |
| Audio reconcile keyed on any edit | `audio_mod_runtime.rs` :369 (`data_version != self.last_version`) | Rekeyed on the audio-setup stamp. Device hot-plug flags `devices_dirty`/`processes_dirty` :213–217 already exist and are reused. |
| Snapshot publish | `content_thread.rs` `send_state` :1187 (deep clone on `data_version` change); UI deep clone `app_render.rs` :287 | Exists. Measured, not changed (Deferred). |
| UI selection version | `ui_state.rs` `selection_version` :85 | Exists. A planner input. |
| Tree structure version | `tree.rs` `structure_version` :117 | Exists. Counts rebuilds for the gate. |
| Region rebuild by truncation | `ui_root/mod.rs` `rebuild_scroll_panels` :1050 (`truncate_from` the scroll panels, rebuild the tail) | Precedent for suffix rebuild. Generalized. |
| In-place scroll updates | `viewport/render.rs` `try_update_horizontal_scroll` :861, `try_update_vertical_scroll` :1178; `layer_header.rs` :2288; inspector `take_scrolled_in_place` (`inspector/mod.rs` :971) | Exist. Reused as the in-place path. |
| Invalidation flags | `needs_rebuild = true` 95 sites · `needs_structural_sync = true` 68 · `DispatchResult::structural()` 165 · `scroll_dirty.*` 23 · host `mark_dirty`/`request_redraw`/`request_automation_lane_resync` 24 · `automation_lanes_dirty` 1 | All die (P4b, P5a). |
| The shared frame seam | `ui_frame.rs` `apply_ui_frame_invalidations` :178, `composite_main_ui_frame` :250 | Exists. Grows the planner. |
| Atlas slots | `ui_cache_manager.rs` `PanelSlot` :13 (7 slots); `invalidate_all` :101, `invalidate_scroll_panels` :115, `invalidate_inspector` :126 | One `invalidate_slot` replaces the three. |
| Script runner | `script.rs` `Runner` :390 — drives the real seam, but keeps its own `needs_structural_sync` and its own undo stack | Partial lookalike of `tick_and_render`. P4a removes the transcription. |
| Real content handler, headless | `headless_harness.rs` `headless_content_thread`; `ContentThread::handle_command` (`content_commands.rs` :779); precedent test `sdr_controls_route_pointer_gestures_to_content_and_undo` (`ui_bridge/project.rs` :2556) | Exists. The content half of the gate uses it. |
| Thread-local counters | `cold_touch.rs` (foundation): per-kind thread counters, "tests assert zero" | Precedent for the work counters. |
| Baseline-and-record gate | `scripts/rt_noise_gate.py` + `rt_noise_baseline.json` (`--record`) | Precedent for work ceilings. |
| Flow runner, nightly sweep | `scripts/run_ui_flows.py` (name filters); `scripts/trunk_health.py` GPU gate list, where `rt_noise_gate.py` runs nightly | Exist. Work flows run through both. |
| Text measurement | `native_text.rs` `CoreTextMeasure` :1688 (no cache, used by the tree, installed `ui_root/mod.rs` :429); `NativeTextRenderer::measure_text_cached` :1106 (cached, renderer only); `NativeTextRenderer`'s own `TextMeasure` impl :1665 is a character-count guess; `text.rs` `truncate_with_ellipsis` :14 measures once per byte trimmed | Three measurers. Collapse to one cached one. |

**A finding the brief did not list:** the BUG-y3a9a fix routed hover through `request_redraw`, which sets `scroll_dirty.visual` (`editing_host.rs` :464). That still runs `rebuild_scroll_panels`, which truncates and rebuilds layer headers, the viewport and every overlay (`ui_root/mod.rs` :1081–1085). Hover still rebuilds; it just rebuilds less.

Negative claims, checked: no part-level versioning exists (`rg 'struct DataVersion|PartStamps|ChangeSet' crates` → only comments); no work counter beyond `cold_touch` (`rg 'GlobalAlloc|alloc_count' crates` → only the `mimalloc` line in `main.rs`).

---

## 2. Decisions

**D1 — The project has parts; each part has a stamp.** `Part` names a slice of `Project` (table in section 3). `PartStamps` is a fixed array of `u64`, one per part. `ChangeSet` is a bit set of parts. They live in `manifold-core` (new module `change`), because parts are project-model vocabulary and `manifold-foundation` forbids domain types. `EditingService` owns the content stamps and bumps them on every edit, undo, redo, record and load. Rejected: *versions stored on each model object, hand-bumped by commands* — that is how graph versions work, and it only holds by discipline; one owner at the gateway cannot be forgotten.

**D2 — Commands declare what they touch.** `Command::touches(&self) -> ChangeSet`. Undo touches the same set. `CompositeCommand` returns the union. Until P6 the method defaults to `ChangeSet::ALL`, which is today's behavior; P6 removes the default so every command states its set and the compiler lists the stragglers. A wrong narrow set is a stale-UI bug, so the change audit (D10) checks every declaration in tests. Rejected: *deriving the set by wrapping project fields in auto-bumping cells* — the content thread writes live values into the project every tick (`content_thread.rs` :1208–1210), and the UI writes back drag values after every snapshot (`app_render.rs` :299), so stamps would move every frame; it would also rewrite every model access in the codebase. Rejected: *deriving the set by hashing parts after each edit* — that is `embedded_presets_fingerprint`, the disease.

**D3 — One mutation closure, with a required change set, through the gateway.** `ContentCommand::MutateProject { touches: ChangeSet, f }` replaces all three variants. The content handler calls `EditingService::mutate(touches, project, f)`, so closures stop bypassing the gateway. A live value write declares `ChangeSet::of(&[Part::Values])`; nothing reacts to that part (D4). `MutateProjectLive` and `MutateProjectPreview` are deleted. **Departure from the direction:** the brief asked what happens to closures that can't declare scope. Every closure can name the fields it writes; one that can't is an escalation (it should be a command). `mutate` does not bump `data_version`, matching today's `MutateProject`.

**D4 — Content reactions run once per drain pass, from the stamps that moved.** After the command drain loop in `ContentThread::run`, one function reads `stamps.moved_since(last_reacted)` and runs the reaction table (section 4). Per-command reaction code in the Execute and MutateProject arms is deleted. A drag that sends five ticks in one frame now pays one reaction. The fingerprint dies; the preset overlay installs when `EmbeddedPresets` moved. Rejected: *reacting inside each command arm with a narrower check* — that keeps reactions scattered across arms, which is how three variants grew.

**D5 — The UI keeps its own stamps in its own counter space.** `UiStamps.parts` is a `PartStamps` the UI owns. Two sources move it: (a) when a snapshot is accepted, every part that moved between the last accepted snapshot's stamps and this one is bumped locally; (b) UI-side writes to `local_project` bump the parts they write. UI-side writes happen in two places, and both stamp internally: `AppEditingHost`'s mutating methods (clip move, trim, automation point drags) and a new `DispatchCtx::mutate(touches, f)` that applies `f` to `local_project`, stamps it, and sends the same closure to the content thread. Today those writes are written twice by hand (LED brightness: `scrub.rs` :852 and :860). Input code never stamps directly. Rejected: *comparing content stamp values on the UI side* — UI writes have no content stamp, so the two would disagree.

**D6 — One per-frame planner per window decides all UI work.** Each frame the UI builds `FrameInputs` (a `Copy` struct of versions and pixel-quantized view values) and calls `plan_frame(&last, &next) -> FramePlan`, a pure function. The plan says which projections run, which regions rebuild, which update in place, and which atlas slots repaint. `apply_frame_plan` executes it. Equal inputs give an empty plan. The live app and the script runner both call it (the `ui_frame.rs` seam rule, UI_HARNESS_UNIFICATION_DESIGN.md D3 (shared seam)). **This is pull, not push.** Rejected by name: *a reactive subscription system* where panels register on model paths and callbacks fire on change — WIDGET_TREE_DESIGN.md section 4 (Gesture routing) forbids a live data-binding registry, and a second dispatch system beside the frame loop fails the zero-new-systems test.

**D7 — The rebuild unit is the region, rebuilt as a suffix.** A region is a `begin_region` panel in the tree, which is also its atlas slot's range. `rebuild_from(slot)` truncates the tree at the earliest region whose inputs moved and rebuilds that region and every later one, generalizing `rebuild_scroll_panels`. Region order is fixed, most-rebuilt last: transport, header, footer, inspector, audio dock, scene dock, split handles, layer headers, viewport, overlays. Only rebuilt regions' slots are invalidated. Scroll and zoom of the timeline, inspector and docks update nodes in place. **Departure from the direction:** true per-region rebuild (rebuild the inspector, keep the viewport) needs a different tree store, because `NodeId` carries its index and the tree is one flat vector. Suffix rebuild is the proven shape and already keeps chrome and its atlas slots out of every inspector edit. Consequences, stated honestly: an inspector rebuild still rebuilds layer headers, viewport and overlays as collateral. The gate counts the collateral. Per-region storage is Deferred with that trigger.

**D8 — Hover, modifier keys and the playhead are not rebuild inputs.** Hover is a node flag (`UIFlags::HOVERED`, `node.rs` :377) or an immediate pass. Modifiers are read where they are used. The playhead and clip bodies are immediate passes. `redraw()` only marks the offscreen dirty; it never touches the tree. A region that truly shows modifier state adds a modifier field to `FrameInputs` and a line in the plan; it never adds a flag.

**D9 — One cached text measurer per tree.** `CachedTextMeasure` (in `native_text.rs`) owns a CoreText font manager and a bounded hash cache behind a `RefCell` (UI thread only, one owner, not shared). Each `UITree` owns one. The renderer measures through the tree it draws, so its own cache and the character-count `TextMeasure` impl are deleted. `truncate_with_ellipsis` binary-searches char boundaries: O(log n) cached measures instead of one CTLine per byte. Rejected: *one measurer shared by both windows through `Rc<RefCell<>>`* — shared state for a cache that costs a few kilobytes per window.

**D10 — The gate counts work; the change audit and the freshness check guard correctness.** Three machine checks, each catching a different error:
- **Work counters** catch too much work. A `WorkKind` counter set in `manifold-foundation` (new module `work_count`, shaped like `cold_touch.rs`, thread-local only). Work flows drive real gestures and compare counts to committed ceilings, nightly and as each phase's local gate. A content test drives the real `handle_command`. Counts, never timings. Allocations are counted too, so "no per-frame allocation" stops being prose.
- **The change audit** catches a declared set that is too narrow. Under `cfg(test)` and feature `change-audit`, `EditingService` fingerprints each part before and after every edit and panics if an undeclared part changed. The UI side runs the same audit around each runner step. Over-declaring is allowed; it only costs work, which the counters show.
- **The freshness check** catches a planner input that is missing. A flow step `AssertFresh` dumps the tree, forces every projection and a full build from the same inputs, dumps again, and requires no difference. Incremental and full rebuild are two paths that must agree, so the agreement is tested every run (docs/DESIGN_AUTHORING.md section 4 (Alternatives), essential duplication).

**D11 — Ceilings only move by an explicit record.** Ceilings live in `scripts/ui-flows/work_ceilings.json`. A kind not listed has a ceiling of zero. `--record-work` is the only writer, and a raised ceiling shows in the diff for review. Each phase re-records its gestures downward in the same commit.

---

## 3. Data model (committed signatures)

**`manifold-core`, module `change`** (content and UI thread both use it; app side only — `manifold-ui` never sees it):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Layers, Clips, Automation, Markers, Effects, Values, Mappings,
    EmbeddedPresets, Settings, Led, AudioSetup, Media, Midi, Session, ViewPrefs,
}
pub const PART_COUNT: usize = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ChangeSet(u16);
impl ChangeSet {
    pub const NONE: Self; pub const ALL: Self;
    pub const fn of(parts: &[Part]) -> Self;
    pub const fn union(self, other: Self) -> Self;
    pub fn contains(self, part: Part) -> bool;
    pub fn is_empty(self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PartStamps([u64; PART_COUNT]);
impl PartStamps {
    pub fn get(&self, part: Part) -> u64;
    pub fn bump(&mut self, set: ChangeSet);
    pub fn moved_since(&self, earlier: &PartStamps) -> ChangeSet;
}
```

**Part → fields.** Every serialized field belongs to exactly one part. The change audit's fingerprint function destructures `Project`, `ProjectSettings`, `Timeline` and `Layer` exhaustively (no `..`), so a new field is a compile error until it is assigned a part.

| Part | Fields |
|---|---|
| Layers | layer identity, name, type, order, parent/group, mute, solo, blend, collapse, lane heights; `timeline.name` |
| Clips | `layer.clips` |
| Automation | automation lanes and points (wherever they live on layers and instances) |
| Markers | `timeline.markers`, export in/out/enabled |
| Effects | layer effects, effect groups, generator instance and its graph, `master_effects`, `master_effect_groups`, drivers, envelopes, audio modulation config, card collapse flags |
| Values | per-frame scalars both threads read fresh: base param values, layer and master opacity, macro values, `led_brightness`, `led_gain` |
| Mappings | `ableton_mappings` on instances, macro-slot Ableton mappings, `ableton_set_context` |
| EmbeddedPresets | `embedded_presets` |
| Settings | the remaining settings scalars and `tempo_map` |
| Led | `led_enabled`, `led_exit_index`, `stage_layout` |
| AudioSetup | `audio_setup` |
| Media | `video_library`, `video_library_paths`, `video_player_pool_size` |
| Midi | `midi_config` |
| Session | `session` |
| ViewPrefs | persisted view fields: `inspector_width`, `timeline_height_percent`, browser width/open, `viewport_*`, `*_collapsed` |

`⚠ VERIFY-AT-IMPL (P3): re-derive this table against the struct definitions (project/mod.rs :73, settings.rs, timeline.rs, layer.rs). A field that fits no row, or a Values field that some consumer reads only on change, is an escalation.`

**`manifold-editing`:**

```rust
pub trait Command: Debug + Send {
    // existing methods unchanged
    fn touches(&self) -> ChangeSet { ChangeSet::ALL }   // default removed in P6
}
impl EditingService {
    pub fn stamps(&self) -> PartStamps;
    pub fn mutate(&mut self, touches: ChangeSet, project: &mut Project, f: impl FnOnce(&mut Project));
    pub fn notify_external_change(&mut self, touches: ChangeSet);   // gains the set
}
```

**`manifold-app`:**

```rust
// content_command.rs — replaces MutateProject, MutateProjectLive, MutateProjectPreview
MutateProject { touches: ChangeSet, f: Box<dyn FnOnce(&mut Project) + Send> },

// content_state.rs
pub stamps: PartStamps,                 // EditingService::stamps() at publish
pub audio_directory_version: u64,       // bumped when devices_dirty / processes_dirty drain

// ui_bridge/context.rs — the one way dispatch code changes the project without a command
impl DispatchCtx<'_> {
    pub fn mutate<F>(&mut self, touches: ChangeSet, f: F)
    where F: Fn(&mut Project) + Clone + Send + 'static;
}

// ui_frame.rs
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FrameInputs {
    pub parts: PartStamps,               // UiStamps.parts
    pub selection: u64,                  // UIState::selection_version
    pub active_layer: u64,               // bumped when the active layer changes
    pub automation_view: u64,            // lane order / hidden / pinned / chosen params
    pub layout: u64,                     // ScreenLayout version: size, splits, dock widths
    pub scroll_x_px: i32, pub scroll_y_px: i32, pub zoom_bits: u32,
    pub playhead_px: i32,                // overview strip only
    pub inspector_view: u64, pub inspector_scroll_px: i32,
    pub dock_view: u64, pub dock_scroll_px: i32,
    pub overlays: u64,
    pub audio_directory: u64,
    pub editor: EditorInputs,            // snapshot version, catalog default version, canvas
                                         // scope version, watched graph_version, preview aspect bits
}
pub(crate) struct FramePlan {
    pub projections: ProjectionMask,     // bit set over the projection table, section 4
    pub rebuild_from: Option<RegionSlot>,
    pub in_place: RegionMask,
}
pub(crate) enum RegionSlot { Transport, Header, Footer, Inspector, AudioDock, SceneDock,
                             SplitHandles, LayerHeaders, Viewport, Overlays }
pub(crate) fn plan_frame(last: &FrameInputs, next: &FrameInputs) -> FramePlan;   // pure
pub(crate) fn apply_frame_plan(plan: &FramePlan, frame: &mut UiFrame<'_>);       // interior free
```

**No raw `Arc` addresses as keys.** A freed `Arc` and a new one can share an address, so a stored integer would read "unchanged". Shared snapshots enter as versions instead: where an `Arc` arrives (`editor_ui_snapshot` in `editor_bridge.rs`, and the setter of `watched_catalog_default`), the intake compares it with `Arc::ptr_eq` against the `Arc` it already retains, which keeps the address alive, and bumps a `u64` when they differ. `FrameInputs` carries the version. Versions keep `FrameInputs` a `Copy` struct of integers, and the `ptr_eq` stays sound because it only ever runs against a retained `Arc`.

`UiFrame` bundles what projections read today (`ui_root`, `local_project`, `selection`, `content_state`, cache). Its field list is the executor's, re-derived from the current `sync_*` signatures.

**`manifold-ui`:** `TimelineEditingHost` drops `mark_dirty`, `request_redraw` and `request_automation_lane_resync` and gains `fn redraw(&mut self)`. Panels that own view state expose `fn view_version(&self) -> u64`, bumped inside their own setters (inspector collapse, fold, tab, compact, drawer target; docks; overlays). The fields behind them become private. `UIState`'s automation lane fields (`ui_state.rs` :190–200) become private with setters that bump `automation_view`.

**`manifold-ui-paint`:** `UICacheManager::invalidate_slot(PanelSlot)` replaces `invalidate_all`, `invalidate_scroll_panels` and `invalidate_inspector`. `CachedTextMeasure` replaces `CoreTextMeasure`.

**`manifold-foundation`, module `work_count`:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkKind {
    FullBuild, RegionRebuild, InPlaceUpdate, SlotInvalidate,
    ProjectSync, InspectorSync, ClipProjection, LaneProjection, PresetListProjection,
    EditorProjection, OverviewRepaint, ChromeLayout, TextMeasureMiss,
    ProjectClone, RendererReload, AbletonListenerRebuild, OscRouteRebuild,
    PresetOverlayInstall, PresetFingerprint, AudioReconcile, DeviceQuery, FrameAlloc,
}
pub fn record_work(kind: WorkKind);              // thread-local Cell increment, no allocation
pub fn thread_work_snapshot() -> WorkSnapshot;   // Copy array
impl WorkSnapshot { pub fn since(&self, earlier: &WorkSnapshot) -> WorkSnapshot; }
```

The executor adds a kind when it adds a counted site and deletes a kind when its code is deleted. `FrameAlloc` is recorded by a counting wrapper around `mimalloc`, compiled only under feature `work-trace` (which `ui-snapshot` enables); shipping builds keep the plain allocator.

---

## 4. The tables the planner and the reaction pass encode

These are the contract. If the freshness check fails, the missing input is added here and in `plan_frame`; a region never gets a flag.

**Projections (UI thread).**

| Projection | Runs when these inputs move |
|---|---|
| `sync_project_data` | Layers, Clips, Automation, Markers; selection; active layer |
| `sync_inspector_data` | Layers, Effects, Mappings, Clips, Led, AudioSetup, EmbeddedPresets; selection; active layer; automation view |
| Audio Setup device list (inside the inspector projection today) | AudioSetup; audio directory version — the CoreAudio query runs only then |
| `sync_clip_positions` | Clips, Layers |
| `sync_automation_lane_geometry`, `sync_automation_lane_order` | Automation, Effects, Layers; automation view |
| Embedded preset list (`dropdowns.rs` `sync_embedded_presets`) | EmbeddedPresets |
| Editor canvas `set_snapshot` | snapshot version; canvas scope version; preview aspect |
| Editor MODIFIED badge, viewport def | watched graph_version; catalog default version |
| Editor mini timeline | Clips, Layers; watched target |
| Overview strip repaint | Clips, Layers; scroll; zoom; `playhead_px`; layout |
| Audio waveform requests (`app_render.rs` :458) | Clips |
| Detect status (`clip_chrome.rs` :715) | in place, only when status text or progress pixel moves |

**Regions.**

| Region | Rebuild when | In place when |
|---|---|---|
| Transport, Header, Footer | layout; their displayed values at display resolution | values update through the existing chrome diff |
| Inspector | layout; inspector projection ran; inspector view | inspector scroll |
| Audio dock, Scene dock | layout; their projection ran; dock view | dock scroll |
| Split handles | layout | — |
| Layer headers | layout; project projection ran; selection; automation view; zoom | scroll y |
| Viewport | layout; project projection or lane projection ran; Markers; selection; zoom | scroll x, scroll y |
| Overlays | overlays version | — |

**Content reactions (once per drain pass).**

| Moved | Reaction |
|---|---|
| Layers, Effects, Mappings | OSC route rebuild |
| Layers, Effects, Mappings | Ableton listener rebuild (when connected) |
| Media | `on_project_loaded` on each renderer (only `VideoRenderer` overrides it, `video_renderer.rs` :687) |
| EmbeddedPresets | install the project preset overlay |
| AudioSetup | audio reconcile (plus the existing device and process flags) |
| any part | `mark_compositor_dirty_now` |
| Values, Settings, Led, Markers, Clips, Automation, Session, Midi, ViewPrefs | nothing beyond the above |

`⚠ VERIFY-AT-IMPL (P3): confirm LED output, MIDI routing and session launch read their settings fresh each tick. A consumer that caches one of them gets a row here, never a per-command call.`

---

## 5. Every finding, and what removes it

| # | Finding | Removed by | Phase |
|---|---|---|---|
| 1 | Embedded-preset fingerprint every UI frame and after every content command | Function deleted. UI list keyed on the EmbeddedPresets stamp; content overlay installs only when that stamp moves | P3, P4b |
| 2 | Scrollbar and overview drags return `structural()` per move | `DispatchResult::structural_change` deleted. Scroll is a `FrameInputs` field; the plan updates the viewport in place | P4b |
| 3 | Text measurer uncached; chrome view + layout every frame; truncation one CTLine per byte | One cached measurer; binary-search truncation; chrome rebuilds only when its displayed values move | P2, P5b |
| 4 | LED and Ableton trim drags trigger project-load work per move | LED declares Values: no reaction. Trim declares Mappings: one listener rebuild per content frame, nothing else | P3 |
| 5 | Open graph editor re-derives everything each frame | Canvas, badge, mini timeline and viewport def keyed on snapshot version, graph_version and stamps; editor window runs the same planner | P4b, P5a |
| 6 | Selection syncs the inspector 2–3x; device queries inside the sync | The planner runs each projection at most once per frame. Device query keyed on the audio directory version | P4b |
| 7 | Modifier keys, dock wheel, import progress, browser search rebuild | Modifiers are not an input. Dock scroll in place. Import progress updates the header in place and completion arrives as stamps. Search bumps the overlays version, so only the overlay region rebuilds | P5a, P5b |
| 8 | Per-frame work regardless of change | Overview keyed on the playhead pixel, not the beat float. Detect status in place on change. Waveform requests keyed on Clips. Allocation residue (`push_state` churn, param-card name clone, `present.rs` scratch, unused `duration_beats()`) cleared to an allocation count of zero | P4b, P6 |
| 9 | Inspector, drawer and split tweens force full build + `invalidate_all` | Tween progress is a view input: suffix rebuild from the inspector (P5a), then in place (P5b). Split tween moves layout, so every region rebuilds for its ~200 ms (decided item 12) | P5a, P5b |
| 10 | Whole-project deep clones; `sync_driver_bpm` under clock jitter | Measured: `ProjectClone` counted on both threads; driver BPM scan counted under a jitter gesture. Structural sharing is Deferred with a trigger | P1 |
| — | Hover still rebuilds scroll panels after the BUG-y3a9a fix | `redraw()` never touches the tree | P5a |

---

## 6. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| INV-1 Every project mutation declares a change set that covers what it changed | `MutateProject` has a required `touches` field (compile-time). `Command::touches` has no default after P6 (compile-time). Change audit (`change-audit` feature, on in `manifold-editing` tests, `command_roundtrips.rs`, the content budget test and the runner) panics on an undeclared part change |
| INV-2 Content reactions run only from the reaction pass | `rg -n 'on_project_loaded\|rebuild_listeners\|osc_param_router.rebuild\|install_project_preset_overlay' crates/manifold-app/src/content_commands.rs` → hits only inside the reaction function and the load path. Content budget test ceilings |
| INV-3 An idle frame does zero work | Work flow `work-idle`: every counter zero, `FrameAlloc` included |
| INV-4 Incremental UI equals a full rebuild from the same inputs | `AssertFresh` closes every work flow; the dump diff ignores generations, node indices and input-state flags |
| INV-5 Only the planner builds, rebuilds or invalidates | `only_the_frame_planner_builds` test: source scan (shaped like `no_bespoke_row_infra`) fails on `UIRoot::build`, `rebuild_from`, `invalidate_slot` outside `ui_frame.rs` |
| INV-6 No hand-set invalidation flags | P5a negative gate: `rg 'needs_rebuild\|needs_structural_sync\|ScrollDirty\|automation_lanes_dirty\|structural_change\|fn mark_dirty\|request_automation_lane_resync' crates/` → 0 |
| INV-7 One text measurer, cached | `rg 'impl TextMeasure for' crates/ -g '!*test*'` → `CachedTextMeasure` and `HeuristicTextMeasure` only; `measure_text_ct` called only from the cache-miss path |
| INV-8 Stamps never serialize; load moves every part | `load_moves_every_part` test; `rg 'PartStamps' crates/manifold-io` → 0 |
| INV-9 Ceilings rise only by explicit record | The flow runner and content test fail on any count above its ceiling; unlisted kinds have ceiling zero |

---

## 7. Phasing

Phased by layer: measure, then the measurer, then the content gateway, then UI projections, then UI regions, then close-out. Each phase lands alone, re-records its ceilings downward, and leaves the old code it replaces deleted. Work flows use built-in scenes (deterministic, present in worktrees). A Liveschool soak run (`project:<path>`) is on demand, not a gate. No GPU proofs anywhere: nothing touches kernels.

### P1 — Work counters and the gate (measure the before)

- **Entry:** anchors in section 1 re-verified (`rg -n 'fn apply_ui_frame_invalidations' crates/manifold-app/src/ui_frame.rs`; `rg -n 'struct Runner' crates/manifold-app/src/ui_snapshot/script.rs`; `rg -n 'pub fn headless_content_thread' crates/manifold-app/src/headless_harness.rs`).
- **Read-back:** this doc sections 1–3 and 6; `cold_touch.rs`; `script.rs` :1–74 and `record_executed_commands`; `scripts/rt_noise_gate.py`; `scripts/run_ui_flows.py`. Restate D10, D11, the forbidden moves, and the entry findings.
- **Deliverables:** `work_count` module; `record_work` at every site the `WorkKind` list names, across both threads; the counting allocator under `work-trace`; `MANIFOLD_WORK_TRACE=1` per-frame trace line on both threads (env read once at startup, the `MANIFOLD_AUDIO_TRACE` precedent `audio_mod_runtime.rs` :318); `AutomationAction::ResetWork`, `AutomationAction::AssertWork { window }`; a `Step` that advances `ContentState` beat position while playing (a driver input, not an engine copy); work flows `scripts/ui-flows/work-{idle,playback,clock-jitter,hover,automation-hover,scrollbar-drag,slider-drag,select-click,modifier,drawer-tween,dock-scroll,browser-search,editor-open}.json` (clock-jitter steps frames while the `ContentState` BPM wobbles by ±0.01); `work_ceilings.json` recorded from today's code; `--record-work` on `ui-snap` and `run_ui_flows.py`; content test module `content_reaction_budget` driving the real `handle_command` on `headless_content_thread` with commands produced by real `ui_bridge::dispatch` for: LED tick, Ableton trim tick, mapping-drawer tick, param slider tick, param Execute, clip move Execute, undo; a nightly leg `["python3", "scripts/run_ui_flows.py", "work-"]` in `scripts/trunk_health.py`'s GPU gate list (beside `rt_noise_gate.py`), so a red files a bead; a CPU-scope mapping so touching `crates/manifold-editing/src/commands/` runs `content_reaction_budget`. `⚠ VERIFY-AT-IMPL: how scripts/cpu_scope.py maps paths to filtersets — read it before adding the mapping.` `⚠ VERIFY-AT-IMPL: whether the runner can drive the editor window; if not, editor-open is measured by the live trace only and filed as verification debt.`
- **Gate (positive):** every work flow exits 0 against its recorded ceilings; `content_reaction_budget` green; the trace prints on a live run. Report the before table (counts per gesture) in the landing commit. **Gate (negative):** no `Instant`, `elapsed` or duration in any work assertion (`rg 'elapsed|Instant' crates/manifold-app/src/ui_snapshot/` shows no new hits in the work code).
- **Demo:** L3 — the work flows drive real input; plus one `MANIFOLD_WORK_TRACE=1` live run on Liveschool by the lead for the app-only per-frame work the runner does not yet execute.
- **Performer gesture:** hover the automation lanes for 2 seconds. The count of rebuilds that produces is the headline number of the before table.
- **Forbidden moves:** timing-based assertions · counters behind `Arc`, atomics or globals (thread-local only) · a content-side lookalike (no hand-built `ContentSide` — use `handle_command`) · fixing any finding in this phase.
- **Test scope:** `manifold-foundation`, `manifold-app` focused nextest; work flows locally via `scripts/run_ui_flows.py work-`, as every later phase does.

### P2 — One cached text measurer

- **Entry:** P1 landed; `rg -n 'impl TextMeasure for' crates/` matches the three impls in section 1.
- **Read-back:** `native_text.rs` :1095–1140 and :1660–1730; `text.rs`; `tree.rs` `set_text_measure`. Restate D9.
- **Seam brief:** old `CoreTextMeasure` (uncached), `NativeTextRenderer::measure_text_cached` + its guess impl → new `CachedTextMeasure`, one per `UITree`; renderer calls go through `tree.text_measure()`. Inventory: `rg -n 'measure_text_cached|CoreTextMeasure|measure_text\(' crates/manifold-ui-paint crates/manifold-app crates/manifold-ui -g '*.rs'` — re-run, list the count in the phase notes, stop if a call site has no tree in reach. Compiler-driven: delete `CoreTextMeasure` and `measure_text_cached` first.
- **Deliverables:** `CachedTextMeasure`; binary-search `truncate_with_ellipsis`; `TextMeasureMiss` counted at the miss; INV-7 check.
- **Gate:** existing UI suites green untouched; work ceilings for `TextMeasureMiss` re-recorded down; INV-7 rg; a unit test proves truncation output equals the old linear result on a fixed string set. Demo: none — L1.
- **Forbidden moves:** a second cache "for the renderer" · `Rc<RefCell<>>` sharing across windows · changing truncation output.
- **Test scope:** `manifold-ui`, `manifold-ui-paint`, `manifold-app` focused.

### P3 — Change stamps at the gateway, and content reactions

- **Entry:** P1 landed. Re-run the section 1 counts for `MutateProject*` (48/19/6) and `impl Command for` (204).
- **Read-back:** sections 2–4; `service.rs`; `command.rs`; `content_commands.rs` :1180–1245 and :1690–1770; `content_thread.rs` `run` drain loop and `send_state`; `audio_mod_runtime.rs` :340–440. Restate D1–D4, the Part table, the reaction table.
- **Seam brief:** old `MutateProject(f)` / `MutateProjectLive(f)` / `MutateProjectPreview(f)` → new `MutateProject { touches, f }`. Delete the two extra variants first; the compiler lists every sender. Worked examples: LED brightness tick (`scrub.rs` :860) → `touches: ChangeSet::of(&[Part::Values])`; Ableton trim (`scrub.rs` :1288) → `Mappings`; rack expand (`dispatch/params.rs` :115) → `Effects`. A closure that writes fields of more than three parts, or one whose fields you cannot name, is an escalation.
- **Deliverables:** `change` module; `Command::touches` with default `ALL`; explicit `touches` on the commands the P1 content gestures use and on effect-param, clip move/trim, automation point and mapping commands; `EditingService::{stamps, mutate}`; `notify_external_change(touches)` (the Ableton status caller passes `Mappings`); the reaction pass and the deletion of per-arm reactions; `embedded_presets_fingerprint` removed from the content side; audio reconcile keyed on the AudioSetup stamp; `ContentState.stamps` and `audio_directory_version`; the change audit (`change-audit` feature, part fingerprints in `manifold-core`, exhaustive destructuring); INV-1 (partial), INV-2, INV-8.
- **Gate (positive):** `content_reaction_budget` ceilings re-recorded down: LED tick → zero reactions; trim tick → one listener rebuild per drain pass, zero renderer reloads; param slider tick → zero. Editing and app suites green with the change audit on. `MANIFOLD_RENDER_TRACE=1` live run on Liveschool while dragging LED brightness and an Ableton trim: no frame over 20 ms (DESIGN_DOC_STANDARD.md section 5 (Phase briefs), content-thread work gate). **Gate (negative):** `rg 'MutateProjectLive|MutateProjectPreview' crates/` → 0; INV-2 rg.
- **Demo:** L3 for the counts; the render trace line for the content thread.
- **Performer gesture:** drag LED brightness hard during playback; the output must not hitch and the trace shows no reaction work.
- **Forbidden moves:** keeping `MutateProjectLive` "for speed" · a closure declaring `ALL` to dodge the audit · reactions left inside command arms · bumping `data_version` from `mutate`.
- **Test scope:** `manifold-core`, `manifold-editing`, `manifold-app` focused; clippy on those three.

### P4a — The projection pass moves into the shared seam (no behavior change)

- **Entry:** P3 landed. `rg -n 'needs_structural_sync' crates/manifold-app/src/ui_snapshot/script.rs` hits (the transcription to delete).
- **Read-back:** `app_render.rs` :240–420 (snapshot accept) and :2990–3260 (selection sync, structural sync, clip and lane resync); `ui_frame.rs`; UI_HARNESS_UNIFICATION_DESIGN.md section 4 (The shared seam). Restate D6 and that this phase moves code without changing a decision.
- **Deliverables:** one function in `ui_frame.rs` that runs the projection block exactly as `tick_and_render` does today, called by the app and the runner; the runner's own `needs_structural_sync` and its sync calls deleted; `AssertFresh` action and the `ui-snap diff` fix that ignores generations (reviving UI_HARNESS_UNIFICATION's Deferred diff item, whose trigger this is); ceilings re-recorded (they rise: the runner now does the real per-frame work — record it as the true before, named in the commit).
- **Gate:** all existing flows exit 0; every work flow passes `AssertFresh` (today's code always rebuilds, so it must); demo L3; negative: `rg 'needs_structural_sync' crates/manifold-app/src/ui_snapshot/` → 0.
- **Forbidden moves:** changing any decision while moving it · a runner-only branch in the seam (vary on input presence only, the D3 hardening rule).
- **Test scope:** `manifold-app` with `--features ui-snapshot`; all flows.

### P4b — The planner lands beside the old flags

- **Entry:** P4a landed.
- **Read-back:** sections 2–5; `editing_host.rs`; `context.rs`; `ui_root/mod.rs` `build` :719 and `rebuild_scroll_panels` :1050. Restate D5–D8, both planner tables, the forbidden registry.
- **Shape:** `plan_frame` lands complete and runs first each frame. `needs_structural_sync`, `structural()`, `needs_rebuild` and `ScrollDirty` all stay live after it. They can only add work, never hide it, so nothing goes stale while sites move over in P4c and P5a.
- **Deliverables:** `UiStamps` with the snapshot fold; `FrameInputs`, `plan_frame` (projections, regions, in-place) and `apply_frame_plan`; `rebuild_from(RegionSlot)` with region starts recorded at build; `DispatchCtx::mutate`; host mutators stamp; panel view versions with private fields; `UIState` lane fields private with a version; device query keyed on `audio_directory_version`; editor projections keyed (section 4); overview keyed on `playhead_px`; detect status in place; waveform requests keyed on Clips; `sync_driver_bpm` counted; `embedded_presets_fingerprint` and the `PresetFingerprint` kind deleted; a `plan_frame` unit test per row of both section 4 tables (pure, no GPU).
- **Gate:** `AssertFresh` passes in every flow; `plan_frame` tests green; `work-idle`, `work-playback` and `work-modifier` show zero projections; `work-editor-open` zero editor projections after the first frame; ceilings re-recorded; demo L3. **Negative:** `rg 'embedded_presets_fingerprint' crates/` → 0.
- **Forbidden moves:** a new flag under another name · a subscription or callback from model to panel · `ctx.mutate` with a non-`Clone` closure worked around by boxing (escalate) · re-adding a hash of project content as a key · moving the inspector after the viewport in build order · deleting old sites in this phase.
- **Test scope:** `manifold-ui`, `manifold-app` focused; work flows locally.

### P4c — `structural()` and `needs_structural_sync` die, in batches

- **Entry:** P4b landed. Re-run: `rg -F 'needs_structural_sync = true' crates -g '*.rs' | wc -l` (68) and `rg -F 'DispatchResult::structural()' crates -g '*.rs' | wc -l` (165). A different count: list the new sites before touching any.
- **Seam brief:** four batches by site kind, each landing alone. (a) It sent a command or closure: local writes move to `ctx.mutate` or the host mutator; return `handled()`. Example: rack expand (`dispatch/params.rs` :123). (b) It changed panel view state: the setter already bumps the view version; `handled()`. Example: `MacrosCollapseToggle` (:194). (c) It changed timeline scroll or zoom: the planner sees the pixel value; `handled()`. Example: `TimelineScrollbarH` (`ui_bridge/transport.rs` :89). (d) It changed selection: `selection_version` already moves; delete. The last batch deletes the `needs_structural_sync` field and `DispatchResult::structural_change`. A site fitting no kind is an escalation.
- **Gate per batch:** `AssertFresh` in every flow; ceilings re-recorded down (batch c: `work-scrollbar-drag` zero project and inspector syncs; batch d: `work-select-click` one inspector sync). **Last batch negative:** `rg 'needs_structural_sync|structural_change' crates/` → 0.
- **Demo:** L3. **Performer gesture:** click between three clips quickly mid-playback; one inspector sync per click.
- **Forbidden moves:** converting a site whose kind the planner does not yet cover · deleting the field before the last batch. Half the sites converted is fine while the planner covers them; the field goes last.
- **Test scope:** `manifold-ui`, `manifold-app` focused; work flows locally.

### P5a — The remaining flags die

- **Entry:** P4c landed. Re-run counts: `needs_rebuild = true` (95), `scroll_dirty.` writes (23), host dirty calls (24), `automation_lanes_dirty` (1).
- **Read-back:** section 4 region table; `ui_frame.rs` `apply_ui_frame_invalidations`; `ui_cache_manager.rs` :101–175. Restate D7, D8.
- **Seam brief:** delete `needs_rebuild`, `ScrollDirty`, `automation_lanes_dirty`, `mark_dirty`, `request_redraw`, `request_automation_lane_resync` first, then `apply_ui_frame_invalidations`' flag branches. Site kinds: view change (setter already bumps; delete the line); scroll (the planner already sees it; delete); tween poll (the tween's progress bumps the view version; delete); modifier keys (delete); import progress (delete; the header updates in place); browser search (`set_filter` bumps overlays; delete); hover (`redraw()`). Old `invalidate_all`/`invalidate_scroll_panels`/`invalidate_inspector` → `invalidate_slot`. `⚠ VERIFY-AT-IMPL: which atlas slot covers the docks and overlays (ui_root/mod.rs panel_cache_info :564).`
- **Deliverables:** the deletions; `invalidate_slot`; the editor window's `UIRoot` on the same planner; INV-5, INV-6.
- **Gate (positive):** ceilings re-recorded down: hover, automation hover, modifier, scrollbar drag → zero region rebuilds and zero slot invalidations beyond in-place updates; select click → rebuild from the inspector, chrome slots untouched; browser search → overlays only. `AssertFresh` in every flow. **Gate (negative):** INV-6 rg → 0; INV-5 test green.
- **Demo:** L3. **Performer gesture:** hover and scrub along an automation lane for five seconds; zero rebuilds.
- **Forbidden moves:** a flag "just for the tween" · a region rebuilding itself outside the plan · moving the inspector after the viewport in build order.
- **Test scope:** `manifold-ui`, `manifold-ui-paint`, `manifold-app` focused; all flows.

### P5b — In-place motion and keyed chrome

- **Entry:** P5a landed.
- **Read-back:** `inspector/mod.rs` `drawer_anim_active` :528 and the in-place scroll path; chrome `View` and `diff.rs`. Restate D7 and decided item 10.
- **Deliverables:** drawer and object-card tweens by reveal: content laid out once at final size, revealed by the animated drawer clip while cards below move in place (shaped like inspector in-place scroll); dock wheel scroll in place; chrome regions keyed on displayed values at display resolution (the clock keyed on its shown digits, not the beat float).
- **Gate:** `work-drawer-tween` and `work-dock-scroll` show zero region rebuilds; `work-playback` shows no chrome layout except when shown digits change; `AssertFresh` at tween end.
- **Demo:** L3, plus a drawer filmstrip (the existing `inspector-drawer-filmstrip.json`) for Peter to look at. **Performer gesture:** open a modulation drawer mid-playback; the drawer reveals and costs no rebuilds.
- **Forbidden moves:** keeping a per-frame rebuild for "just the last frame of the tween" · moving the split tween in place (it stays a full rebuild, decided item 12).
- **Test scope:** `manifold-ui`, `manifold-app` focused; all flows.

### P6 — Close-out

- **Entry:** P5b landed.
- **Deliverables:** `Command::touches` default removed; every command declares its set (compiler-driven: 204 impls, mostly one line each; re-run the count first); allocation residue removed until `work-idle` and `work-playback` show `FrameAlloc` zero (param-card name clone `param_card/render.rs` :2136, `push_state` churn, `present.rs` scratch, `check_auto_scroll`'s unused `duration_beats()`); final ceilings; one CLAUDE.md line ("UI work is decided by the frame planner; never add an invalidation flag — add an input. Spec: docs/UI_INVALIDATION_DESIGN.md"); supersession sweep (this doc's status, `rg` for `needs_rebuild`, `MutateProjectLive`, `structural()` across `docs/` and memory, fix or tombstone each hit); `.claude/skills/content-thread/SKILL.md` updated for the one `MutateProject`.
- **Gate:** INV-1 complete (no default); `work-idle` all zeros including `FrameAlloc`; a `MANIFOLD_WORK_TRACE=1` live idle run on Liveschool prints nothing for 10 seconds.
- **Demo:** the live idle trace — L2.
- **Forbidden moves:** `ChangeSet::ALL` as a blanket answer for the 204 (each states its parts; `ALL` only for load-scale commands, named in the phase notes).
- **Test scope:** `manifold-editing`, `manifold-app` focused; all flows; nightly workspace sweep covers the rest.

Phasing completeness: every mechanism in sections 2–5 appears in exactly one phase deliverable above, or in Deferred.

---

## 8. Decided — do not reopen

1. Parts and stamps at the gateway; `EditingService` owns content stamps; the UI owns its own. (D1, D5)
2. Commands declare `touches()`; no default after P6; the change audit checks every declaration. (D2, D10)
3. One `MutateProject` with a required change set, through `EditingService::mutate`. Live and Preview variants are gone. (D3)
4. Content reactions run once per drain pass from moved stamps, by one table. (D4)
5. One pure planner per window decides projections, rebuilds, in-place updates and slot repaints. Pull, never push; no subscription registry. (D6)
6. Region suffix rebuild in a fixed order; in-place scroll and motion. (D7)
7. Hover, modifiers and playhead are not rebuild inputs; `redraw()` never touches the tree. (D8)
8. One cached measurer per tree; the renderer measures through it. (D9)
9. Counts, not timings; allocations counted; ceilings move only by explicit record; missing inputs are found by the freshness check, never by adding flags. (D10, D11)
10. Drawers open by reveal: content is laid out once at its final size and revealed as the drawer slides open. (Peter, 2026-10-13)
11. An Ableton range follows live mid-drag, with one listener rebuild per content frame. (Peter, 2026-10-13)
12. The split-reset tween rebuilds every region for its ~200 ms; every region's size moves. An implementation call, not a question for Peter.
13. Work flows run nightly through `scripts/trunk_health.py`, never in the landing gate; landing time is a priority. Each phase runs its own work flows locally as its gate. `content_reaction_budget` is ordinary nextest, scoped normally. (Peter, 2026-10-13)

---

## 9. Deferred

| Item | Revive when |
|---|---|
| Per-region tree storage (rebuild one middle region without the suffix) | A work flow shows suffix collateral as the largest count on a hot gesture after P5b |
| Structural sharing of project parts (`Arc` per part) to drop snapshot deep clones | `ProjectClone` work dominates an editing gesture on Liveschool. Needs live values moved out of `Project` first, or every tick copies |
| Per-layer or per-effect stamps | An edit on one layer causes work proportional to all layers that the existing per-layer bitmap fingerprints do not skip |
| Projection output equality as a second gate (skip the rebuild when a projection's output equals the last) | A projection runs often with unchanged output, visible as region rebuilds without visible change |
| Scoped Ableton listener update (one mapping, not all) | A trim or mapping drag on Liveschool shows a content frame over 20 ms in `MANIFOLD_RENDER_TRACE` |
