# UI Invalidation — work runs only when its inputs change

**Status:** APPROVED · 2026-10-14 · Opus 5.5 · Peter approved on condition that an independent adversarial review finds no perf, UI or UX regression; five review rounds ran, the last found text gaps only, fixed here.
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs — refactors and API changes) before starting any phase.

**The governing insight: every redundant-work finding in the October UI audit has one root. Nothing can answer "did my input change?" The project has one version for everything, so the content thread and the UI either recompute, deep-compare, or rebuild everything. Input handlers fill the gap by hand-picking flags, and the safe flag is "rebuild it all".** This design puts a version on each part of the project at the one place edits happen (`EditingService`), puts a version on each piece of UI view state where it is owned, and makes one per-frame planner per window compare those versions and decide all work. Input code states facts ("I wrote the clips"); it never chooses work. An idle frame compares a few integers and does nothing.

On stage: dragging LED brightness or an Ableton trim stops doing project-load work on the render thread, so the output never hitches on a mixer move. Hovering, scrolling, zooming and modifier keys stop rebuilding the UI, so controls stay responsive mid-set (the lag behind BUG-y3a9a (automation-lane-hover-drag-full-rebuild-lag)). Clip and automation drags cost the dragged items, not the project. An open graph editor stops rebuilding its whole inspector every frame. A jittery Link or MIDI clock stops forcing rebuilds. The content tick stops re-hashing every clip. And a count gate stops the next feature from adding per-frame work a few percent at a time.

Direction agreed by Peter and the lead on 2026-10-13 (change stamps at the gateway, inputs declared by consumers, content reacts to the same stamps, one cached text measurer, a count gate). Revision 2 follows Peter's posture of 2026-10-14: the code lands in its end-game state; a known gap is designed in now, not parked behind a "revive when measured" trigger. A deferral survives only when it is genuinely expensive and needs its own design, and then it is named as the committed end state. Where this doc departs from the direction, the decision says so and why.

**Binding constraints** (docs/DESIGN_AUTHORING.md section 1 (The intake)): *Hot path* — the planner and the counters run every frame on both threads, so both are fixed-size integer work, no allocation. *Thread residency* — content stamps are owned by `EditingService` on the content thread and travel in `ContentState`; UI stamps are owned by the UI thread; nothing is shared. *Persistence* — stamps are never serialized; a load moves every part. *Performance surface* — slider, opacity, LED and trim drags stay value-only writes; drags never pay project-scale work on either thread.

Companion docs: `UI_HARNESS_UNIFICATION_DESIGN.md` (the `ui_frame.rs` seam and the script runner this extends), `WIDGET_TREE_DESIGN.md` (its forbidden live data-binding registry applies here too), `PERF_BUDGET_GATE_DESIGN.md` (frame-time gate; this is its count-based sibling), `HEADLESS_UI_HARNESS.md` (flows and dumps), `EDITOR_WINDOW_UNIFICATION_DESIGN.md` (the editor window's frame seam, which D17 puts on the planner).

---

## 1. Audit — what exists (verified 2026-10-14 on main `8ef8616d3`)

| Piece | Where | State · use |
|---|---|---|
| Project-wide version | `service.rs` `EditingService.data_version` :49, bumped on execute/undo/redo/record/load | Exists. Kept for "publish a snapshot" and "unsaved". Stops being a change detector. |
| Per-instance graph versions | `instance.rs` `bump_graph_version` :273 / `bump_graph_structure_version` :284 | Exists. Reused as the finer key for graph consumers (editor badge, canvas, viewport def). |
| Command scope hook precedent | `command.rs` `Command::graph_admission_targets` :13 (append-into-out, default empty) | Exists. `touches()` sits beside it. |
| Commands | `rg 'impl Command for' crates -g '*.rs' \| wc -l` → 204 | Each gets `touches()`. |
| Mutation closures that bypass `EditingService` | `content_command.rs` `MutateProject` :179, `MutateProjectLive` :192, `MutateProjectPreview` :207; handlers `content_commands.rs` :1710–1780 | Three variants that hand-pick reactions. 48 + 19 + 6 send sites. Collapse to one. |
| Live drags that skip reactions today | `MutateProjectLive`/`MutateProjectPreview` carry the drags whose cheapness depends on doing no reaction: driver, envelope and audio-mod trims and shapes (`dispatch/resolve.rs` :21, :61, :104 via `scrub.rs` :1241, :1468, :1672), clip-trigger shape (`resolve.rs` :148 via `scrub.rs` :1983), layer audio gain (`scrub.rs` :1023), relight params (:1111), audio send gain (:2066), band split `low_hz`/`mid_hz` (:2141), an unbound node param on the editor canvas (`app_render.rs` :2510), the mapping-drawer reshape preview (`editor_bridge.rs` :1082) | Each writes a per-frame scalar of something that already exists. They declare Values or Shapes, which have no reactions (D21). |
| Content writes that bypass `EditingService` entirely | `rg -c 'project_mut\(\)' crates/manifold-app/src crates/manifold-playback/src` → 58 (tests included): `content_commands.rs` 26, `engine.rs` 11, `content_thread.rs` 9, `transport_controller.rs` 5. Examples: tempo recording into `tempo_map` each tick (`content_thread.rs` `update_recording_session_state` :1997, `apply_resolved_tempo` :2030), clock-authority auto-switch :1740, `SetFrameRate` `content_commands.rs` :1663, `DetectClip`/`ReplanClip`/`ClearClipTriggers` :1811–1830, `AbletonUnmapParam` :1887, `ToggleOscSyncMode` :1903; MIDI live record (BUG-mmct0.5 (MIDI recording commits bypass EditingService), child of BUG-mmct0 (playback code audit pilot)); offline imports (BUG-7f4d (midi-percussion-import-commands-executed-outside-editing-service)) | Side doors. Closed by D13 (BUG-cg8gj (content thread writes the project outside EditingService)). |
| Tick writes into the project | The clock-following BPM into `settings.bpm` (`sync_project_bpm_from_current_beat`, `engine.rs` :2362–2382); the auto clock authority (`content_thread.rs` :1706–1741); tempo-recorder points into `tempo_map` during a session (`tempo_recorder.rs` :149–167); about 35 tick readers read `settings.bpm` and `tempo_map`. After Ableton discovery, statuses are validated and listeners rebuilt inline, then `notify_external_change` (:1804–1811) | The tick writes stay where they are as audited tick writes: compare first, no stamp, no publish (D24). The inline rebuild is deleted (INV-2). |
| Live scalars a consumer caches | Send `gain_db` is copied into the capture `GainBank` and synced only inside the reconcile (`audio_mod_runtime.rs` :886–890, :993) | In-place reaction row (D25). |
| Content reactions after every command | `content_commands.rs` Execute arm :1244–1252 (OSC routes, Ableton listeners, preset overlay), repeated in the Batch/Undo/Redo arms :1290–1294, :1373–1376, :1451–1454; MutateProject arm :1710–1745 (`on_project_loaded` on every renderer, listeners, overlay) | Rewired to stamps. |
| Per-command and per-tick content work keyed on nothing or on `data_version` | `handle_command` clones `tempo_map` per Execute/Undo/Redo :791–797, reconciles :2229; clip-control bindings reconciled after every command :2232 and every `tick()` (`engine.rs` :874) because every mutable borrow sets `clip_bindings_dirty` (`engine.rs` :463, :534) and the render path borrows mutably every frame (`content_pipeline.rs` :2365); the reconcile SHA-256s every clip of every layer (`bindings.rs` :54–84, `trigger_sources.rs` :10–35); `GeneratorRenderer::render_all` refreshes on any `data_version` change (`generator_renderer.rs` :724); `trigger_targets.refresh` keyed on `data_version` (`content_pipeline.rs` :2359); `graph_snapshot` cache keyed on the preset fingerprint (`content_thread.rs` :1543, :1571) | Rekeyed on stamps in the reaction table (section 4). |
| Output-resize validator | `content_commands.rs` :1213–1232 (inside `execute_with_validation`, can reject the edit) | Stays inside execute: it is validation, not a reaction. |
| Change detection by serializing | `project_io.rs` `embedded_presets_fingerprint` :135 (serde_json of every embedded def); callers `content_commands.rs` :212, `dropdowns.rs` :639 (every UI frame via `app_render.rs` :626) | Deleted. |
| Audio reconcile keyed on any edit | `audio_mod_runtime.rs` :369 (`data_version != self.last_version`) | Rekeyed on the AudioSetup stamp. Device hot-plug flags `devices_dirty`/`processes_dirty` :213–217 are reused. |
| Snapshot publish | `content_thread.rs` `send_state` :1187 deep-clones and publishes the project only when `data_version` changed (:1204–1225); UI deep clone `app_render.rs` :287 | Exists. Once the side doors move onto `mutate`, which does not bump `data_version`, a content-side write such as an Ableton status update would never publish. Publish rule in D4. Structural sharing is the committed end state with its own design (section 9). |
| Live values to the UI | Modulation snapshot captured and its buffer cloned every tick (`content_thread.rs` :1236–1240, `content_state.rs` `capture_into` :411); `push_state` (`ui_bridge/state_sync.rs` :49, about 700 lines, 93 setters) runs every UI frame unconditionally; MIDI device names compared slice-by-slice every frame (:121) | Keyed on a values sequence (D15). |
| What else `push_state` drives | Sequence-gated events: undo/redo, graph-edit and trigger-delivery toasts (:133, :145, :153), content-driven modifier selection (:160–187), Ableton session arrival (:190); transport and settings chrome from `ContentState` (sync, Link and clock states, playing, recording, export range, SDR and HDR, zoom label, footer selection info :431); selection visuals: insert cursor (:447), selection band (:454), layer highlight and labels (:468, :485 on); macro labels, Ableton displays and trim ranges (:544–616) | Four pieces with their own inputs (D15). Keyed on values alone, the selection, chrome and event pieces would freeze while paused. |
| Content runtime state that shapes the UI | `sync_inspector_data` takes `automation_latched_params` and `led_preview` (`app_render.rs` :3019–3020); LED preview presence rides the inspector projection (`state_sync.rs` :641–651); warmup progress forces a full build on change (`app_render.rs` :594–601); an arriving `ableton_session` rebuilds the open picker (`state_sync.rs` :191–196); `sdr_preview` (:417–423) | Versioned (D16). |
| UI selection version | `ui_state.rs` `selection_version` :85; bumped by clip, layer, marker, region, insert-cursor (`set_insert_cursor` :712) and scope pin (`pin_scope` :427) changes; `selection_identity()` :392 already names what the inspector cares about (BUG-266 (inspector tab pin killed by side-effect selection bumps)) | Split (D14). |
| In-place selection paths | `layer_header.rs` `set_selection` :1295; viewport active-lane recolor (`viewport/render.rs` `track_bg_color`, `sync_active_track_lane`); marker outline `set_visible` (`viewport/render.rs` :1142); clip selection drawn by the immediate clip pass (`frame/present.rs` :344–372) | Exist. Selection goes in place (D14). |
| Drag projections today | Clip move/trim: `sync_clip_positions` every frame of the drag (`app_render.rs` :3233–3241) plus a repaint of every layer's grid bitmap per move (`invalidate_all_layer_bitmaps` in `handle_move_drag`, `handle_trim_left_drag`, `handle_trim_right_drag`, `interaction_overlay.rs` :3184, :3274, :3324); automation point drags: `sync_automation_lane_geometry` only (:3242–3253, `automation_lanes_dirty`) | Cheaper than a rebuild, not free. D5's stamping would have made it a rebuild. Kept as previews (D12); the bitmap repaint dies (P5a). |
| Drag-defer guards | `apply_ui_frame_invalidations` defers rebuilds during inspector/layer drags (`ui_frame.rs` :202–227) | Exist because panels hold raw `NodeId`s. Input already holds presses by durable `WidgetId` (`input.rs` :389). Deleted (D18). |
| Mid-drag snapshot hold | Snapshots are refused while an overlay drag or automation press is live (`app_render.rs` :247–276), because the host writes clip positions and region-move splits (`editing_host.rs` `split_clips_for_region_move` :539) into `local_project` mid-drag and a snapshot would wipe them | Deleted: the splits go into the preview with the dragged timings (D12). |
| Tree structure version | `tree.rs` `structure_version` :117 | Exists. Counts rebuilds for the gate. |
| Tree storage | One flat SoA set of Vecs (`tree.rs` `UITree` :53); `NodeId` = `generation << 32 \| index` (`node.rs` `from_parts` :274); `hit_test` walks reverse array index (`tree.rs` :827); render walks the region list in tier order (`traverse_regions`); `root_minted` workaround for partial rebuild salts (:102); a root's `WidgetId` is `WidgetId::ROOT` mixed with the tree-wide root count (`tree.rs` :305–328), so per-arena counts from zero would give every region root the same id | Becomes one arena per region, root salts seeded by region id (D7, P4e). |
| `begin_region` sites | 18 outside tests, each with its own tier: the ten main panels in `ui_root/mod.rs` (:732–1143), warmup at Chrome (:915), the overlay stack at its overlay's tier (`ui_root/overlay.rs` :158, toast at Ghost), the card-drag ghost at Ghost (`panels/inspector/card_drag.rs` :285), dropdowns and the Ableton picker at Overlay, built from inside their owning panel (`dropdown.rs` :345, `ableton_picker.rs` :815), the editor preview column (`editor_frame.rs` :134), `live_ui.rs` :585 | A region arena holds tiered sub-regions (D7). |
| Region rebuild by truncation | `ui_root/mod.rs` `rebuild_scroll_panels` :1050 (`truncate_from` the scroll panels, rebuild the tail) | Replaced by per-region rebuild (D7). |
| In-place scroll updates | `viewport/render.rs` `try_update_horizontal_scroll` :861 (falls back to a rebuild when the ruler tick or label count changes, :935), `try_update_vertical_scroll` :1178; `layer_header.rs` :2288; inspector `take_scrolled_in_place` (`inspector/mod.rs` :971) | Exist. Reused; the ruler gets a node pool (D7). |
| Layout | One `ScreenLayout`; a timeline split moves only header y, split handle, layer headers and viewport; transport, footer and inspector rects do not read the split ratio (`layout.rs` :107–202) | Per-region rects (D7). |
| Split/resize drags | `window_input.rs` :376–399 set `needs_rebuild` per move for the timeline split, inspector width and both dock widths | Full build per mouse move today. Per-region rects end it (BUG-m22n9 (split and resize drags rebuild every region)). |
| Zoom | Region rebuild today; layer headers read only the mapper's y layout (`layer_header.rs` `get_layer_y_offset`/`get_layer_height`) | In place (D7). |
| Invalidation flags | `needs_rebuild = true` 95 sites · `needs_structural_sync = true` 68 · `DispatchResult::structural()` 165 · `scroll_dirty.*` 28 · host `mark_dirty`/`request_redraw`/`request_automation_lane_resync` 24 · `automation_lanes_dirty` 1 set site · layer-bitmap `invalidate_layer_bitmap`/`invalidate_all_layer_bitmaps` 12 calls + the `invalidate_layers` drain (`app_render.rs` :3256) | All die (P4d, P5a). The bitmap calls are a Unity-port leftover: the grid bitmap depends only on viewport and time signature (`bitmap_renderer.rs` :179–195), yet hover repaints it (`interaction_overlay.rs` :2567, :2573) and every clip move or trim move repaints all of them (:3184, :3274, :3324). |
| Dead and per-frame hashes | `Viewport::compute_fingerprint` :471 / `needs_rebuild` :519 have no callers; `set_clips` hashes every clip for the overview (`viewport.rs` :611–625) and runs each frame of a clip drag | Deleted (BUG-972o1 (stale layer-bitmap invalidation and dead viewport fingerprints)). |
| UI writes to `local_project` | Host mutators (`editing_host.rs`); `MutateProject` closures written twice by hand (LED brightness `scrub.rs` :852 and :860); about 100 sites run `Command::execute` straight on the local project (`ui_bridge/project.rs` 17, `dispatch/modulation.rs` 13, `app.rs` 11 text-commit arms :1210–1544, `ui_bridge/editing.rs` 8, others); `DispatchCtx.project` is `pub &mut Project` (`context.rs` :27) | Three helpers, read-only `ctx.project` (D5). |
| Automation view state | `ui_state.rs` `automation_mode_visible` :142, `chosen_automation_params` :190, lane order/hidden/pinned :190–200, `automation_lane_heights` :204; mode toggled at `transport.rs` :257 and `dispatch/params.rs` :1105; every layer-scope slider grab calls `set_chosen_automation_param` (`scrub.rs` :706), which reorders lanes even with lanes hidden; lane resize calls `mark_dirty` per move (`interaction_overlay.rs` :2782, BUG-veaap (automation-lane-resize-full-rebuild)) | One `automation_view` version over all of it, bumped only on a visible change (D19). |
| Driver readout under clock jitter | `sync_driver_bpm` scans every driver on each new bpm (`ui_root/mod.rs` :411) and forces a full structural sync and rebuild on a period flip (`app_render.rs` :347–353) | In place (D20, BUG-ilntv (clock jitter forces full UI rebuilds)). |
| Ableton trim drag | `rebuild_listeners` walks every mapping and allocates two hash maps (`ableton_bridge.rs` :2061–2063), once per content frame during a trim drag | Scoped refresh (D20, BUG-kn2cm (Ableton trim drag rebuilds every listener)). |
| Graph editor window | `rebuild_editor_inspector` clears the tree and builds the whole inspector column every vsync (`editor_bridge.rs` :13–20, called :1791); the editor card value push `sync_card_values` (`projection/cards.rs` :53, called from `editor_bridge.rs` :1776–1793) runs every frame; no atlas, "recomposites every vsync pulse regardless" (:1462); per-frame `visible_nodes` (:1407), preview-node resolve (:1322–1347), `mini_timeline_data` (:1436), `watched_def_cloned` while the viewport is open (:1397) | On the planner with its own regions and atlas (D17, BUG-36mjb (open graph editor rebuilds the whole inspector every vsync)). |
| Perform-mode HUD | `tick_perform_mode` (`perform_mode/render.rs` :28) redraws every frame; allocates track-name Vecs (:130), the macros Vec (:174), countdown and bar strings; the section-progress fill moves with the playhead (:125, drawn :814) | Keyed on displayed values at pixel resolution (D17, BUG-1d6sh (perform-mode HUD rebuilds its text and lists every frame)). |
| Output window, tooltips | The output window only blits the compositor; tooltips draw in the immediate TOOLTIP pass (`tree_passes.rs` :138) | No change needed. |
| The shared frame seam | `ui_frame.rs` `apply_ui_frame_invalidations` :178, `composite_main_ui_frame` :250 | Exists. Grows the planner. |
| Atlas slots | `ui_cache_manager.rs` `PanelSlot` :13 (7 slots); `invalidate_all` :101, `invalidate_scroll_panels` :115, `invalidate_inspector` :126 | One slot per region, one `invalidate_slot`. |
| Script runner | `script.rs` `Runner` :436 — drives the real seam, but keeps its own `needs_structural_sync` and its own undo stack | Partial lookalike of `tick_and_render`. P4a removes the transcription. |
| Real content handler, headless | `headless_harness.rs` `headless_content_thread` :24; `ContentThread::handle_command` (`content_commands.rs` :790); precedent test `sdr_controls_route_pointer_gestures_to_content_and_undo` (`ui_bridge/project.rs` :2556) | Exists. The content half of the gate uses it. |
| Thread-local counters | `cold_touch.rs` (foundation): per-kind thread counters, "tests assert zero" | Precedent for the work counters. |
| Baseline-and-record gate | `scripts/rt_noise_gate.py` + `rt_noise_baseline.json` (`--record`) | Precedent for work ceilings. |
| Flow runner, nightly sweep | `scripts/run_ui_flows.py` (name filters); `scripts/trunk_health.py` GPU gate list, where `rt_noise_gate.py` runs nightly | Exist. Work flows run through both. |
| Text measurement | `native_text.rs` `CoreTextMeasure` :1688 (no cache, used by the tree, installed `ui_root/mod.rs` :430); `NativeTextRenderer::measure_text_cached` :1106 (cached, renderer only); `NativeTextRenderer`'s own `TextMeasure` impl :1665 is a character-count guess; `text.rs` `truncate_with_ellipsis` :14 measures once per byte trimmed | Three measurers. Collapse to one cached one. |

**A finding the brief did not list:** the BUG-y3a9a fix routed hover through `request_redraw`, which sets `scroll_dirty.visual` (`editing_host.rs` :464). That still runs `rebuild_scroll_panels`, which truncates and rebuilds layer headers, the viewport and every overlay (`ui_root/mod.rs` :1081–1090). Hover still rebuilds; it just rebuilds less.

**Revision 2 audit, the class it found:** the first revision listed the flags but not every input behind them, and it let three cheap paths quietly get worse. Clip and automation drags were cheap because nothing stamped them; stamping host mutators would have made them full rebuilds. The content side doors and the clip-binding reconcile sit outside `EditingService`, so stamps alone never see them. And "the region is the rebuild unit" was only half true while the tree is one flat vector. The rows above carry each one; section 5 maps each to its phase. An adversarial review of revision 2 found the same class one level down: live drags that only stay cheap because they skip reactions, `push_state` doing four jobs, projection inputs read deep inside the projections, a publish rule tied to `data_version`, and region roots that would share an id. The hand-written tables cannot be trusted to find these, so revision 2 derives them by test (INV-16).

Negative claims, checked: no part-level versioning exists (`rg 'struct DataVersion|PartStamps|ChangeSet' crates` → only comments); no work counter beyond `cold_touch` (`rg 'GlobalAlloc|alloc_count' crates` → only the `mimalloc` line in `main.rs`); no other caller of `Viewport::needs_rebuild` (`rg 'compute_fingerprint\(\)|\.needs_rebuild\(\)' crates` → the definition only).

---

## 2. Decisions

**D1 — The project has parts; each part has a stamp.** `Part` names a slice of `Project` (table in section 3). `PartStamps` is a fixed array of `u64`, one per part. `ChangeSet` is a bit set of parts. They live in `manifold-core` (new module `change`), because parts are project-model vocabulary and `manifold-foundation` forbids domain types. `EditingService` owns the content stamps and bumps them on every edit, undo, redo, record, load and `mutate`. Rejected: *versions stored on each model object, hand-bumped by commands* — that is how graph versions work, and it only holds by discipline; one owner at the gateway cannot be forgotten, once D13 closes the side doors.

**D2 — Commands declare what they touch.** `Command::touches(&self) -> ChangeSet`. Undo touches the same set. `CompositeCommand` returns the union. Until P6 the method defaults to `ChangeSet::ALL`, which is today's behavior; P6 removes the default so every command states its set and the compiler lists the stragglers. A wrong narrow set is a stale-UI bug, so the change audit (D10) checks every declaration in tests. Rejected: *deriving the set by wrapping project fields in auto-bumping cells* — the content thread writes live values into the project every tick (`content_thread.rs` :1208–1210), and the UI writes back drag values after every snapshot (`app_render.rs` :299), so stamps would move every frame; it would also rewrite every model access in the codebase. Rejected: *deriving the set by hashing parts after each edit* — that is `embedded_presets_fingerprint`, the disease.

**D3 — One mutation closure, with a required change set, through the gateway.** `ContentCommand::MutateProject { touches: ChangeSet, f }` replaces all three variants. The content handler calls `EditingService::mutate(touches, project, f)`, so closures stop bypassing the gateway. A live value write declares `ChangeSet::of(&[Part::Values])`, and a live drag of a modulator, gain, crossover or mapping shape declares `Shapes`; nothing reacts to either part (D4, D21). `MutateProjectLive` and `MutateProjectPreview` are deleted. **Departure from the direction:** the brief asked what happens to closures that can't declare scope. Every closure can name the fields it writes; one that can't is an escalation (it should be a command). `mutate` does not bump `data_version`, matching today's `MutateProject`.

**D4 — Content reactions run once per drain pass, from the stamps that moved.** After the command drain loop in `ContentThread::run`, before the tick, one function reads `stamps.moved_since(last_reacted)` and runs the reaction table (section 4). Per-command reaction code in the Execute, Batch, Undo, Redo and MutateProject arms is deleted, and so is per-command work that is a reaction in disguise: the `tempo_before` clone, the clip-binding reconcile at the end of `handle_command`. A drag that sends five ticks in one frame now pays one reaction. The fingerprint dies; the preset overlay installs when `EmbeddedPresets` moved. Content consumers that keyed on `data_version` (`render_all`, `trigger_targets`, the `graph_snapshot` cache) key on `PartStamps::key(set)` instead (section 3). The output-resize validator stays inside `execute_with_validation`: it can reject an edit, so it is not a reaction. When the project snapshot publishes is D23. Rejected: *reacting inside each command arm with a narrower check* — that keeps reactions scattered across arms, which is how three variants grew.

**D5 — The UI keeps its own stamps in its own counter space.** `UiStamps.parts` is a `PartStamps` the UI owns. Two sources move it: (a) when a snapshot is accepted, every part that moved between the last accepted snapshot's stamps and this one is bumped locally; (b) UI-side writes to `local_project` bump the parts they write. **UI-side writes happen in exactly three helpers, and each stamps internally:** `AppEditingHost`'s committing methods; `DispatchCtx::mutate(touches, f)`, which applies `f` to `local_project`, stamps it, and sends the same closure to the content thread; and `DispatchCtx::execute(cmd)`, which executes the command on `local_project`, stamps `cmd.touches()`, and sends it. `DispatchCtx.project` becomes `&Project`, so a fourth path does not compile. Today the about 100 direct `Command::execute` sites write the local project with no helper, and LED brightness writes twice by hand (`scrub.rs` :852 and :860). Input code never stamps directly. Drag previews are not writes (D12). Rejected: *comparing content stamp values on the UI side* — UI writes have no content stamp, so the two would disagree.

**D6 — One per-frame planner per window decides all UI work.** Each frame the UI builds `FrameInputs` (a `Copy` struct of versions and pixel-quantized view values) and calls `plan_frame(&last, &next) -> FramePlan`, a pure function. The plan says which projections run, which regions rebuild, which translate, which update in place, and which atlas slots repaint. `apply_frame_plan` executes it. Equal inputs give an empty plan. The live app and the script runner both call it (the `ui_frame.rs` seam rule, UI_HARNESS_UNIFICATION_DESIGN.md D3 (shared seam)). The editor window and the perform HUD run the same pattern over their own inputs (D17). **This is pull, not push.** Rejected by name: *a reactive subscription system* where panels register on model paths and callbacks fire on change — WIDGET_TREE_DESIGN.md section 4 (Gesture routing) forbids a live data-binding registry, and a second dispatch system beside the frame loop fails the zero-new-systems test.

**D7 — The rebuild unit is the region, and a region rebuilds alone. (Revised 2026-10-14.)** A region is a `begin_region` panel in the tree and owns exactly one atlas slot. Each region lives in its own arena (section 3, `UITree`), so rebuilding the inspector leaves the layer headers, viewport and overlays untouched. Each region's rect is its own pixel-quantized input; a region whose rect only moved is translated in place (one offset over its arena), and only a region whose size changed rebuilds. Scroll, zoom, lane heights and selection visuals of the timeline, inspector and docks update nodes in place; the ruler keeps a fixed pool of `MAX_RULER_TICKS` tick and label nodes and hides the surplus, so no scroll or zoom ever falls back to a rebuild (BUG-3vs01 (horizontal scroll falls back to a viewport rebuild)). Region build order no longer matters for cost. **What revision 1 decided and why it changed:** revision 1 rebuilt a suffix (the earliest moved region and everything after it), because `NodeId` carries its index and the tree is one flat vector, and deferred per-region storage "until measured". That made every inspector rebuild also rebuild layer headers, viewport and overlays, and made a timeline split rebuild every region although only four move (`layout.rs` :107–202). Peter's posture (end-game now) and the size of the change (one phase, P4e) put it in. **Region ids and tiers (added after review):** each arena seeds its root salts with its region id, so two region roots can never share a `WidgetId` and hover, press, focus and tooltips resolve to the right region. An arena holds tiered sub-regions: a `begin_region` called while a region builds (a dropdown, the Ableton picker) is a sub-region of that arena with its own tier. The card-drag ghost (`card_drag.rs` :268–290, minted from input and drawn in the immediate tooltip pass today) goes in a separate `Ghosts` arena that has no atlas slot and keeps drawing in that pass; toasts stay a Ghost-tier sub-region of `Overlays`, drawn in the overlay loop as today (`ui_root/overlay.rs` :157); it is cleared and refilled each frame it is non-empty and costs nothing when empty, so a ghost appears or moves without rebuilding the panel that spawned it. Paint order runs over every sub-region of every arena by (tier, region rank, local insertion), where region rank is the fixed `MainRegion` order that matches today's full-build order; hit order is the reverse. That reproduces today's order exactly while letting one arena rebuild alone. Rejected: *mapping every `begin_region` site to its own top-level region* — dropdowns and pickers are built by the panel that owns them, so they would need a second build path outside the panel. Rejected: *tail-append replacement* (rebuild a middle region at the end of the flat vector and leave holes) — it breaks hit order, which walks reverse array index, and it leaves dead slots every scan pays for until a compaction nobody owns.

**D8 — Hover, modifier keys, the playhead and automation interaction state are not rebuild inputs.** Hover is a node flag (`UIFlags::HOVERED`, `node.rs` :377) or an immediate pass. Modifiers are read where they are used. The playhead and clip bodies are immediate passes. Automation point selection, time selection, marquee, draw mode and hover feedback are drawn by the immediate `automation_lane_draw`; they change with `redraw()` only. One exception, found by review: the inspector shows the selected point's lane as the chosen automation param (`projection/inspector.rs` :116), so the lane identity (target and param) of the selected point is part of `automation_view` (D19); moving or reselecting points inside the same lane bumps nothing. `redraw()` only marks the offscreen dirty; it never touches the tree. A region that truly shows modifier state adds a modifier field to `FrameInputs` and a line in the plan; it never adds a flag.

**D9 — One cached text measurer per tree.** `CachedTextMeasure` (in `native_text.rs`) owns a CoreText font manager and a bounded hash cache behind a `RefCell` (UI thread only, one owner, not shared). Each `UITree` owns one. The renderer measures through the tree it draws, so its own cache and the character-count `TextMeasure` impl are deleted. `truncate_with_ellipsis` binary-searches char boundaries: O(log n) cached measures instead of one CTLine per byte. Rejected: *one measurer shared by both windows through `Rc<RefCell<>>`* — shared state for a cache that costs a few kilobytes per window.

**D10 — The gate counts work; the change audit, the freshness check and the input matrix guard correctness.** Four machine checks, each catching a different error:
- **Work counters** catch too much work. A `WorkKind` counter set in `manifold-foundation` (new module `work_count`, shaped like `cold_touch.rs`, thread-local only). Work flows drive real gestures, continuous drags included, and compare counts to committed ceilings, nightly and as each phase's local gate. A content test drives the real `handle_command` and the real tick. Counts, never timings. Allocations are counted too, so "no per-frame allocation" stops being prose.
- **The change audit** catches a declared set that is too narrow and a write outside the gateway. Under `cfg(test)` and feature `change-audit`, `EditingService` fingerprints each part before and after every edit and `mutate` and panics if an undeclared part changed; the content thread runs the same audit around `PlaybackEngine::tick` and requires every non-Values part unchanged except the five `TickField`s opened through `tick_write` (D13, D24). The UI side runs the same audit around each runner step. Over-declaring is allowed; it only costs work, which the counters show.
- **The freshness check** catches a planner input that is missing. A flow step `AssertFresh` dumps the tree, forces every projection and a full build from the same inputs, dumps again, and requires no difference. Incremental and full rebuild are two paths that must agree, so the agreement is tested every run (docs/DESIGN_AUTHORING.md section 4 (Alternatives), essential duplication). The tree dump cannot see immediate passes or toasts, so `AssertFresh` also dumps an `ImmediateInputs` record (insert cursor, selection band, playhead, the clip draw list, the automation lane draw inputs, the overview strip inputs, the perform HUD inputs) and the pending toast queue, and compares those too.
- **The input matrix** catches a missing row in the tables themselves. Test `projection_input_matrix` (P4b) loads a fixture, then perturbs one part or one `FrameInputs` field at a time (a time-signature change, a bpm change, an automation lane added, a mapping range moved, a point selected), runs every projection, and records which projections changed their output. Every observed dependency must be a row in section 4 and in `plan_frame`; a row with no observed dependency is reported for deletion. The table is derived from the code, not remembered (INV-16).

**D11 — Ceilings only move by an explicit record.** Ceilings live in `scripts/ui-flows/work_ceilings.json`. A kind not listed has a ceiling of zero. `--record-work` is the only writer, and a raised ceiling shows in the diff for review. Each phase re-records its gestures downward in the same commit.

**D12 — A drag is a preview; stamps move on commit.** While a clip move or trim, an automation point, segment, sweep or draw drag, or a marquee is in flight, the dragged state lives in a gesture preview owned by `InteractionOverlay`, not in a stamped write. `FrameInputs.preview` is the preview's version. The planner answers a preview change with an in-place update of the dragged items only: the clip projection updates the dragged clips' `ViewportClip` entries, and the immediate GPU clip pass and `automation_lane_draw` draw them. The content thread already receives automation previews outside the edit path (`send_automation_preview`, `editing_host.rs` :138); clip drag previews stay UI-side until commit. On commit, the host's committing method stamps Clips or Automation once, and the normal projection runs once. This keeps today's cheap drag paths (`app_render.rs` :3233–3253) and removes their O(all clips) per-frame `sync_clip_positions` and their per-move grid-bitmap repaint. A region move's clip splits (`split_clips_for_region_move`, `editing_host.rs` :539) are preview state too: the preview holds the split pieces with the dragged timings and nothing touches `local_project` until commit, so a mid-drag snapshot cannot wipe anything and the snapshot hold (`app_render.rs` :247–276) is deleted. The overview strip and the editor mini timeline read the preview, so both follow a drag. `⚠ VERIFY-AT-IMPL (P4b): if a region-move split cannot be expressed as preview state (for example a split that must allocate clip ids the content thread also needs), keep the snapshot hold for that one gesture and state it in the phase notes; it is never widened to other drags.` Rejected: *stamping every drag write* — it turns every mouse move into a project projection plus layer-header, viewport and overlay rebuilds (BUG-w3csc (clip and automation drags would rebuild per mouse move)). Rejected: *per-layer stamps to bound drag cost* — previews make them unnecessary (section 9).

**D13 — Every project write goes through the gateway, on both threads.** Content side: `PlaybackEngine::project_mut` and `split_renderer_project_mut` become `pub(crate)`. The engine's public mutable entry is `PlaybackEngine::project_edit(&mut self) -> Option<ProjectEdit<'_>>`; `ProjectEdit` is a `manifold-editing` newtype over `&mut Project` with no public accessor, and only `EditingService`'s execute, undo, redo, record and `mutate` consume it. The non-test side doors move onto `mutate` or a command: `SetFrameRate` Settings, detect and trigger edits Clips, Ableton unmap Mappings, OSC sync mode Settings, OSC router value writes Values. Five tick writes stay where they are as audited tick writes through `tick_write` (D24): `settings.bpm`, `settings.clock_authority`, in-session tempo-recorder points, `recording_provenance`, and the beat-0 tempo replace outside a session. Otherwise engine-internal writes are restricted to the Values part; a structural write inside the tick (live clip commit, `ensure_layer_count`) returns a command in `TickResult`, the `pending_gesture_commits` precedent (`engine.rs` :103, executed at `content_thread.rs` :726). This claims BUG-mmct0.5 (MIDI recording commits bypass EditingService) and BUG-7f4d (midi-percussion-import-commands-executed-outside-editing-service), under the umbrella BUG-cg8gj (content thread writes the project outside EditingService). The render path stops borrowing mutably: `tempo_map` is kept sorted at write time, so `ensure_sorted` (`content_pipeline.rs` :2366–2368) leaves the frame path and the render takes a shared borrow. UI side: D5's three helpers and a read-only `ctx.project`. Rejected: *a `notify_external_change(touches)` call after each side door* — that is a hand-set flag with a set attached; the next side door forgets it, and the audit cannot see it.

**D14 — Selection splits into identity, visuals and cursor.** `selection_version` is replaced by two versions. `selection_identity` bumps only when `selection_identity()` (`ui_state.rs` :392: primary layer, primary clip, layer set) or the scope pin changes; it drives the inspector projection. `selection_visual` bumps when any selection set changes (clips, layers, markers, time region); it drives in-place updates in layer headers (`set_selection`) and the viewport (active-lane recolor, marker outline), never a rebuild. **Revised after review:** revision 2 said the insert cursor carried no version because `UIRoot::update` draws it. That was false: `push_state` pushes the cursor, the selection band, layer highlight and labels (`state_sync.rs` :447–490). So `cursor` is a third version, bumped by `set_insert_cursor` and `cursor_beat` changes; with `selection_visual` it drives the selection push (D15), an in-place write of those values. Arrow-key cursor moves (`input_host.rs` `navigate_cursor` :1187) cost one selection push and a redraw, never a projection. Rejected: *keeping one selection version and comparing projection output* — that is the deferred output-equality gate, and the inputs are cheap to split at the source.

**D15 — Live values carry a sequence.** The content thread's `capture_into` (`content_state.rs` :411) already walks every live value into a scratch buffer each tick. It now compares as it writes and bumps `ContentState.values_seq` only when a value changed; the buffer is cloned into the snapshot only when the sequence moved. **On the UI, `push_state` splits into four pieces, each with honest inputs (revised after review):** (1) the *live push* writes every display of a live scalar in place (D22): slider values, driver trim handles, rate, waveform and division buttons, envelope stages, audio-mod and clip-trigger shapes, Ableton range bands on cards and macros, mapping curves, relight knobs, layer and send gain readouts, the band split; it covers the main inspector, the layer headers, the docks and the editor card lane, and keys each display group on its own input: Values displays on `values_seq` and the UI Values stamp, Shapes displays on the UI Shapes stamp, range bands on the UI MappingRanges stamp (D22); (2) the *selection push* writes the insert cursor, selection band, layer highlight, labels and footer selection info, and runs when `selection_visual` or `cursor` moved; (3) the *event drain* runs every frame and costs O(1): it compares the undo/redo key, the graph-edit and trigger-delivery diagnostic sequences, the modifier-selection sequences and the Ableton session `Arc` against the last seen, and acts only on a new one; (4) the *chrome push* writes transport, sync, settings and macro label displays and runs when Settings, Mappings, Layers or `ChromeInputs` moved, where `ChromeInputs` is a `Copy` value of what the chrome shows, at display resolution: BPM in tenths and the time digits (minutes, seconds, tenths, bar, beat, sixteenth; `state_sync.rs` :84–106), playing, recording, clock authority, sync, Link and clock states, peer count, export range, SDR and HDR flags. Macro trim ranges are live scalars and go through the live push. Revision 2 keyed all of it on values alone, which would freeze the cursor, toasts and chrome while paused. MIDI device names arrive as an `Arc` compared with `Arc::ptr_eq` against the retained one, not slice-by-slice. With nothing modulated, an idle frame does no live push and the content tick copies nothing (BUG-p2t5o (push_state and the modulation snapshot copy run every frame)).

**D16 — Content runtime state that shapes the UI is versioned at the source.** `ContentState.runtime: RuntimeSeqs` carries one sequence per runtime input the UI builds structure from: the automation latch set, LED preview presence, warmup progress, the Ableton session, SDR preview, export progress. Each bumps on the content thread where the value changes. The UI folds them into `FrameInputs.runtime`. The inspector projection reads the latch and LED-presence sequences; warmup and export progress update their overlay in place, and only their open or close bumps the overlays version. The `Arc` intake rule (section 3) covers `ableton_session` like the editor snapshot.

**D17 — Every window and surface is on the planner.** The graph editor window gets its own region set (`EditorRegion`: preview column, card lane, overlays), its own arenas and its own `UICacheManager` atlas, shaped like the main window's (`composite_main_ui_frame`, `ui_frame.rs` :250). `rebuild_editor_inspector`'s per-vsync clear and rebuild is deleted; the card lane rebuilds when the inspector projection runs, exactly like the main inspector. The canvas redraws when canvas inputs move (snapshot version, scope, selection, pan and zoom, preview aspect); live node previews stay a per-frame pass while any is visible. `visible_nodes`, preview-node resolution, `mini_timeline_data` (which also reads the gesture preview, D12) and the viewport def are projections keyed in `EditorInputs`. The editor card live push (`sync_card_values`, `projection/cards.rs` :53, called from `editor_bridge.rs` :1776–1793, widened to every live display by D22) is the editor's live push: it runs when `values_seq`, the UI Values, Shapes or MappingRanges stamp, the active layer or the card lane rebuild moved, like the main value push. The perform HUD builds its draw list from `PerformHudInputs`, derived field by field from `draw_cue_hud`'s arguments at pixel resolution: current and next cue indices, the shown countdown and bar-beat digits, the section-progress fill width in whole pixels (`perform_mode/render.rs` :125, drawn :814), the playing-track set, BPM at one decimal, sync flags, macro values at display resolution, exit hover and the window size. Equal inputs reuse the last draw list and its text buffers. The output window has no UI work. Tooltips stay immediate. Modals, menus and the browser are overlays.

**D18 — No drag defers a rebuild.** Panels keep drag targets as `WidgetId`s (the input system already does, `input.rs` :389) and re-resolve them through `UITree::node_for_widget` after any rebuild. A rebuild mid-drag is then safe, so the guards in `apply_ui_frame_invalidations` (`ui_frame.rs` :202–227) and the plan's need to defer are deleted. The snapshot accept still holds the dragged value (`scrub.restore_dragged`), which is a value rule, not a rebuild rule (BUG-in6m0 (rebuilds deferred during drags because panels hold raw NodeIds)). Rejected: *a planner that carries "held" regions and does not fold their inputs into `last`* — a second freshness path that every new drag must remember.

**D19 — View-state setters bump only on a visible change.** `automation_view` covers automation mode visibility, lane order, hidden, pinned, chosen params, the lane identity of the selected point (D8) and lane heights. Its setters compare before bumping, and while automation mode is hidden, order and chosen-param changes bump nothing, because nothing shows them; showing the lanes bumps once. A slider grab therefore costs nothing when lanes are hidden. A lane-height drag updates lane y positions in place in the layer headers and the viewport, like vertical scroll (BUG-veaap (automation-lane-resize-full-rebuild)). The same compare-before-bump rule holds for every panel `view_version` setter.

**D20 — Live clock and Ableton gestures update in place.** The driver period readout is an in-place value in the driver row; `sync_driver_bpm` runs only when the Effects stamp or the frame-quantized period moved, and never forces a structural sync (BUG-ilntv (clock jitter forces full UI rebuilds)). Ableton mapping ranges are their own part, `MappingRanges`, separate from the mapping set: a trim tick declares `MappingRanges`, and its reaction rewrites the cached ranges of the existing `WriteTarget`s in place with no OSC and no allocation; the listener set rebuilds only when `Mappings` moved (BUG-kn2cm (Ableton trim drag rebuilds every listener)).

**D21 — A part is defined by who reacts to it; live scalars never share a part with set membership. (Added after review.)** Revision 2 put drivers, envelopes and audio-mod config in Effects and the send gains and crossover in AudioSetup, so their live drags would have declared parts that rebuild OSC routes, Ableton listeners, the audio reconcile and the inspector on every mouse move. The rule: a per-frame scalar that consumers read fresh goes in `Values` (written by the content thread every tick) or in `Shapes` (user-set scalars of things that already exist: driver, envelope, audio-mod and clip-trigger shapes and trims, binding-mapping reshape, relight params, unbound graph node param values, layer audio gain, audio send gain, band split). No projection keys on `Shapes`; the UI shows it only through the live push (D22), and the content side reacts to it only where a consumer caches it (D25). Effects and AudioSetup keep set membership: which drivers, envelopes, mods, sends and devices exist. Adding or removing a driver moves Effects; dragging its trim moves Shapes. A field whose value changes which controls exist (a driver's synced-or-free division mode, a mod's source kind) is membership, not a live scalar; the input matrix finds any such field by failing (INV-16). Each of the live drags in section 1 gets a P1 work flow and a `content_reaction_budget` case; all must show zero reactions beyond D25 rows and zero projections per move. **Graph overrides (revised round 5):** the first tick of a mapping-drawer or node-param scrub can materialize a graph override (`effects.rs` :1366–1368, `node_edit.rs` :941). Materializing copies the catalog default, so it changes no param address, mapping, binding or trigger kind; it is its own part, `Overrides`, with no reaction rows, because the instance's graph structure version already makes the renderer rebuild. The first move declares `Overrides | Shapes` and later ticks declare Shapes, so the first move costs nothing beyond today. The commit at drag end records materialize and edit as one undo step, so one Cmd+Z restores the un-materialized graph. Rejected: *putting all of them in Values* — the content thread writes Values every tick, so the tick audit (D10) could no longer catch a tick that writes a user setting.

**Round 3 — the four root rules.** A second review of the round-2 fixes found eight more regressions in four classes. Each class gets a rule, and each rule a check, so the next instance of the class fails a test instead of a review.

**D22 — Every dependency is either rebuild or in place, and live scalars are only ever in place.** A part says what changed; it does not say how a consumer reacts. Every consumer dependency in section 4 is one of two kinds. *Rebuild*: the input changes which nodes exist, so the projection reruns and the region rebuilds. *In place*: the input changes what an existing node shows, so a push writes it. Values, Shapes and MappingRanges are live scalars and are only ever in-place dependencies, in both windows and in every display that shows them (inspector drawers and handles, layer-header gain, dock gains and crossover, macro range bands, the editor card lane), never only in sliders. Today the inspector projection draws them (`projection/inspector.rs` :194, :1463–1469, :1488, :1537–1543; `projection/timeline.rs` :205), and the value push (`projection/cards.rs` :53) writes only slider values; so Cmd+Z of a trim would leave a stale drawer, the editor card lane would stop following a main-window drag, and keying the inspector on MappingRanges would rebuild it per trim move. The live push keys each display group on its own input (revised round 4): Values displays (slider values, opacity, macro values) on `values_seq` and the UI Values stamp; Shapes displays (drawers, handles, shapes, gains, crossover, mapping curves) on the UI Shapes stamp; Ableton range bands on the UI MappingRanges stamp. Modulated playback moves `values_seq` every frame and rewrites slider values only, as today; drawers are touched only when a Shapes or range stamp moves. The projections still read these fields to build rows with the right first value, but they are not inputs of the projection: a rebuild for another reason and the live push agree, which `AssertFresh` checks. **Check:** the input matrix (INV-16) records rebuild and in-place dependencies separately; a live-scalar part that is fresh only after a projection reruns is a failure, never a new table row.

**D23 — The publish rule stays today's, plus content-side `mutate`. (Replaced round 4.)** `send_state` publishes a project snapshot when `data_version` moved, exactly as today: every Execute, undo, redo, record and load still publishes (`service.rs` :133). It also publishes when a content-originated `mutate` moved a part outside Values, Shapes and MappingRanges, which covers the side doors D13 moves onto `mutate` (Ableton statuses after discovery, detect, replan, clear triggers, unmap, frame rate, OSC sync mode). The OSC router apply and the Ableton bridge apply (`content_thread.rs` :1796, :1814) are content-originated `mutate(Values)` every tick and publish nothing, as today; their values reach the UI through the values snapshot (D15). A UI-originated `MutateProject` does not publish, as today (the arm at `content_commands.rs` :1710 never bumps `data_version`). Audited tick writes (D24) never publish. **What round 3 got wrong:** it claimed mute and solo clicks deep-clone today; they are `MutateProject` closures and do not. Its `Command::mirrored`, publish baseline and mirror audit changed correct behaviour no finding named and opened three holes (a Values undo that never published, canvas commits and imports the UI never applied locally, a mirrored edit wiped and never healed). All three are deleted. Snapshot clone cost belongs to the structural-sharing design (section 9). Rejected: *bumping `data_version` from `mutate`* — `data_version` also means "unsaved" and drives autosave.

**D24 — Audited tick writes. (Replaced round 4.)** Three values the engine writes into the project during the tick stay exactly where they are today: `settings.bpm` from `sync_project_bpm_from_current_beat` (`engine.rs` :2362–2382), `settings.clock_authority` (`content_thread.rs` :1706–1741), and the tempo recorder's in-session points on the content `tempo_map` (`tempo_recorder.rs` :149–167). Around 35 tick readers read `settings.bpm` and `tempo_map` and keep doing so unchanged. These writes become one named kind, an *audited tick write*: compare before writing, move no stamp, publish nothing, run no reaction. **API (round 5):** the free function `manifold_editing::tick_write(field: TickField, project: ProjectEdit<'_>, f: impl FnOnce(&mut Project))` (signature in section 3) over five fields: `TickField::{Bpm, ClockAuthority, SessionTempoPoints, RecordingProvenance, BaseTempo}`. Besides the three above, `recording_provenance` is written by the recorder during a session (`update_recording_session_state`, `content_thread.rs` :2017–2024, and the per-point `append_tempo_change`, :2072–2079); session start writes nothing (`tempo_recorder.rs` :93–97), and outside a session the beat-0 tempo is replaced when the map has at most one point and the authority is Internal (:2083–2102). `tick_write` moves no stamp, so it needs no service state and is a free function, not an `EditingService` method; the content thread and the engine's own BPM write inside `PlaybackEngine::tick` both call it (`manifold-playback` depends on `manifold-editing`). Under `change-audit`, `tick_write` records which field it opened, and the audit window wraps the content thread's tick sequence (`content_thread.rs` :616–628: `tick_sync_controllers`, which writes the clock authority at :1740, then `derive_external_beat`, `update_recording_session_state`, `apply_resolved_tempo`) together with `engine.tick` (:653). Inside the window, only Values and the opened `TickField`s may change, and `tempo_map` may change only while a recording session is active (`SessionTempoPoints`) or on the beat-0 path (`BaseTempo`), so the exemption cannot hide a real tempo edit. A recording session ends through Pause or Stop (`content_commands.rs` :934, :945 → `end_tempo_recording_session` → `capture_tempo_lane_snapshot`, `tempo_recorder.rs` :101, :124); that end is a `mutate(Settings)`, not a `tick_write`, and it stamps Settings once, so the UI receives the recorded lane, updates the retained tempo copy in the same step, and skips `reconcile_tempo_edit` for that stamp, so the playhead does not jump and the next reconcile does not see the session as an edit. The UI reads live BPM and clock authority from `ContentState`, which already carries `settings.bpm` (`content_thread.rs` :1343) and so shows the followed or mapped BPM: the chrome (`state_sync.rs` :84–118), `projection/inspector.rs` :101, `ui_root/mod.rs` :412, `app_render.rs` :3272 and `perform_mode/render.rs` :103. Clock authority has no separate user choice, so nothing is written when Link drops. **D13 rule:** every content-side write compares before it mutates, so an unchanged value never moves a stamp. **Oracle:** content test `tempo_ramp_matches_today` records a tempo ramp through a session and compares beat and time per tick against values recorded from today's code. **What round 3 got wrong:** it moved these values out of the project, which would have changed about 35 readers that are correct today, for no named finding.

**D25 — A live scalar a consumer caches gets an in-place reaction row.** Most consumers read Values and Shapes fresh each tick. One that caches a copy gets a reaction row scoped to it, allocation-free, never a move of the field back to a membership part. Known case: audio send `gain_db` lives in the capture `GainBank` and syncs only inside the reconcile (`audio_mod_runtime.rs` :886–890, :993), so the row is "Shapes moved → `sync_gains`". **Check (INV-20):** a consumer table in `manifold-core` names every Values and Shapes field with its consumers, each marked read-fresh or cached-with-row; the `shapes_fields_reach_consumers` test perturbs each field through `mutate` and asserts every listed consumer sees the new value after one drain pass and one tick. A new Values or Shapes field fails the test until it is listed. **Settled by review (round 5):** every Values and Shapes field is read fresh except send gain (`audio_mod_runtime.rs` :519, :586; `modulation.rs` :956–960; `live_trigger.rs` :259–261; `generator_renderer.rs` :788–801), so `sync_gains` is the only cached-with-row entry today. **D25 is an improvement, stated as one:** the `GainBank` feeds only the analysis worker, not playback, so applying the gain during the drag changes what the analysis hears mid-drag and nothing on the output.

**Round 4 — the governing rule: the design changes only what a finding names; where today's behaviour is correct, it stays.** Self-attack against that rule, item by item. D1–D6, D8–D11 and D14–D20 each remove a named finding (section 5) and change nothing else a performer sees. D7 changes storage and order only where findings 17–19, 34–35 and 44 name it; INV-14's golden pins today's paint and hit order. D12 changes drags as findings 13, 36 and 37 name; a drag looks the same. D13 leaves the tick writes where they are (D24). D21 keeps every live drag as cheap as today; a scrub's first move declares the reaction-free Overrides part, so it costs nothing beyond today. D22 replaces the slider-only push and makes drawers follow undo and the other window (finding 39); Values displays key on `values_seq` and Shapes and range displays on their own stamps, so modulated playback rewrites slider values only, as today. D23 is today's rule plus the side doors. D25's `sync_gains` row keeps send gain reaching the capture `GainBank`, which a Shapes-only commit would otherwise lose; as a side effect the gain also applies during the drag, not only at commit. Retracted: round 3's publish baseline, `Command::mirrored`, the mirror audit, runtime-only BPM and authority, the Link-drop write, and the mute, solo, collapse and export-range work flows that rested on the false deep-clone premise.

---

## 3. Data model (committed signatures)

**`manifold-core`, module `change`** (content and UI thread both use it; app side only — `manifold-ui` never sees it):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Layers, Clips, Automation, Markers, Effects, Overrides, Values, Shapes, Mappings, MappingRanges,
    EmbeddedPresets, Settings, Led, AudioSetup, Media, Midi, Session, ViewPrefs,
}
pub const PART_COUNT: usize = 18;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ChangeSet(u32);
impl ChangeSet {
    pub const NONE: Self; pub const ALL: Self;
    /// Live scalars: only ever in-place dependencies, never rebuild inputs (D22).
    pub const LIVE: Self;   // Values | Shapes | MappingRanges
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
    /// Sum of the stamps in `set`. Stamps only grow, so the key changes exactly
    /// when a part in `set` moved. For consumers that keep one `u64` cache key.
    pub fn key(&self, set: ChangeSet) -> u64;
}
```

**Part → fields.** Every serialized field belongs to exactly one part. **Re-derived after review on the D21 principle:** a part is the set of fields with the same consumers. Membership (what exists) and live scalars (how much) never share a part. The change audit's fingerprint function destructures `Project`, `ProjectSettings`, `Timeline` and `Layer` exhaustively (no `..`), so a new field is a compile error until it is assigned a part.

| Part | Fields |
|---|---|
| Layers | layer identity, name, type, order, parent/group, mute, solo, blend, collapse, lane heights; `timeline.name` (not `audio_gain_db`, which is Shapes) |
| Clips | `layer.clips` |
| Automation | automation lanes and points (wherever they live on layers and instances) |
| Markers | `timeline.markers`, export in/out/enabled |
| Effects | which effects, groups and nodes exist and how they are wired: layer effects, effect groups, generator instance and its graph structure, `master_effects`, `master_effect_groups`; which drivers, envelopes, audio mods, clip triggers and param mappings exist and what they target; card collapse flags |
| Overrides | whether an instance's graph is a materialized copy of its catalog default (D21); a copy changes no address, mapping, binding or trigger kind |
| Values | per-frame scalars the content thread writes every tick and both threads read fresh: base param values, layer and master opacity, macro values, `led_brightness`, `led_gain` |
| Shapes | per-frame scalars the user sets on things that exist, read fresh, never written by the tick: driver rate, phase, waveform and trim; envelope stages and depth; audio-mod and clip-trigger shape (sensitivity, attack, release, range); param-mapping reshape (range, curve, invert); `relight_params`; unbound graph node param values; `layer.audio_gain_db`; audio send `gain_db`; band split `low_hz`, `mid_hz` |
| Mappings | which params are mapped to Ableton: `ableton_mappings` entries on instances (target, Live device and param, status), macro-slot Ableton mappings, `ableton_set_context` |
| MappingRanges | the trim range (min, max) of each Ableton mapping |
| EmbeddedPresets | `embedded_presets` |
| Settings | the remaining settings scalars (time signature, bpm, frame rate, clock authority among them), `tempo_map`, `recording_provenance`. `bpm`, `clock_authority` and in-session `tempo_map` points are also written by audited tick writes, which move no stamp (D24) |
| Led | `led_enabled`, `led_exit_index`, `stage_layout` |
| AudioSetup | which audio inputs, sends, buses and devices exist and how they route (everything in `audio_setup` except the Shapes scalars) |
| Media | `video_library`, `video_library_paths`, `video_player_pool_size` |
| Midi | `midi_config` |
| Session | `session` |
| ViewPrefs | persisted view fields: `inspector_width`, `timeline_height_percent`, browser width/open, `viewport_*`, `*_collapsed` |

`⚠ VERIFY-AT-IMPL (P3a): re-derive this table against the struct definitions (project/mod.rs :73, settings.rs, timeline.rs, layer.rs, the Ableton mapping struct). A field that fits no row, or a Values or Shapes field that some consumer reads only on change, is an escalation. The live drags in section 1 are the worked examples: each writes only Shapes or Values fields.`

**`manifold-editing`:**

```rust
pub trait Command: Debug + Send {
    // existing methods unchanged
    fn touches(&self) -> ChangeSet { ChangeSet::ALL }   // default removed in P6
}
/// The only mutable path into the content thread's project. No public accessor:
/// only EditingService opens it, so every content write is stamped and audited.
pub struct ProjectEdit<'a>(&'a mut Project);
impl<'a> ProjectEdit<'a> { pub fn new(project: &'a mut Project) -> Self; }  // called by PlaybackEngine only

/// D24: the five fields the tick may write. Moves no stamp, publishes nothing;
/// compares before writing; under `change-audit` records the opened field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickField { Bpm, ClockAuthority, SessionTempoPoints, RecordingProvenance, BaseTempo }
pub fn tick_write(field: TickField, project: ProjectEdit<'_>, f: impl FnOnce(&mut Project));
impl EditingService {
    pub fn stamps(&self) -> PartStamps;
    pub fn mutate(&mut self, touches: ChangeSet, project: ProjectEdit<'_>, f: impl FnOnce(&mut Project));
    // execute, execute_with_validation, undo, redo, record take ProjectEdit<'_> in place of &mut Project
}
```

`notify_external_change` is deleted: every external write is a `mutate` now (D13).

**`manifold-playback`:** `PlaybackEngine::project_mut` and `split_renderer_project_mut` become `pub(crate)`. New public entries: `fn project_edit(&mut self) -> Option<ProjectEdit<'_>>` and `fn split_renderer_project_edit(&mut self) -> (&mut Vec<Box<dyn ClipRenderer>>, Option<ProjectEdit<'_>>)` for the resize validator. `clip_bindings_dirty` is set only by `stop_all_clips`, `initialize` and `fn mark_clip_bindings_stale(&mut self)`, which the reaction pass calls; a borrow never sets it. `GeneratorRenderer::render_all`'s `data_version: u64` parameter becomes `structure_key: u64` = `stamps.key(Layers | Clips | Effects)`.

**`manifold-app`:**

```rust
// content_command.rs — replaces MutateProject, MutateProjectLive, MutateProjectPreview
MutateProject { touches: ChangeSet, f: Box<dyn FnOnce(&mut Project) + Send> },

// content_state.rs
pub stamps: PartStamps,                 // EditingService::stamps() at publish
pub values_seq: u64,                    // D15: moves only when a captured live value changed
pub audio_directory_version: u64,       // bumped when devices_dirty / processes_dirty drain
pub runtime: RuntimeSeqs,               // D16

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct RuntimeSeqs {
    pub automation_latches: u64, pub led_preview_presence: u64, pub warmup: u64,
    pub ableton_session: u64, pub sdr_preview: u64, pub export_progress: u64,
}

// ui_bridge/context.rs — the only ways dispatch code changes the project
pub project: &'a Project,               // was &'a mut Project
impl DispatchCtx<'_> {
    pub fn mutate<F>(&mut self, touches: ChangeSet, f: F)
    where F: Fn(&mut Project) + Clone + Send + 'static;
    pub fn execute(&mut self, cmd: Box<dyn Command + Send>);   // local execute, stamp touches(), send Execute
}

// ui_frame.rs
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FrameInputs {
    pub parts: PartStamps,               // UiStamps.parts
    pub preview: u64,                    // D12 gesture preview version
    pub selection_identity: u64,         // D14
    pub selection_visual: u64,           // D14
    pub cursor: u64,                     // D14: insert cursor and cursor_beat
    pub active_layer: u64,               // bumped when the active layer changes
    pub automation_view: u64,            // D19: mode visible, order, hidden, pinned, chosen params,
                                         //      selected point's lane, lane heights
    pub chrome: ChromeInputs,            // D15: ContentState fields the chrome shows, display resolution
    pub rects: RegionRects,              // D7: one pixel-quantized rect per MainRegion
    pub scroll_x_px: i32, pub scroll_y_px: i32, pub zoom_bits: u32,
    pub playhead_px: i32,                // overview strip only
    pub inspector_view: u64, pub inspector_scroll_px: i32,
    pub dock_view: u64, pub dock_scroll_px: i32,
    pub overlays: u64,
    pub audio_directory: u64,
    pub values: u64,                     // D15: ContentState.values_seq
    pub runtime: RuntimeSeqs,            // D16
    pub driver_period_key: u64,          // D20: frame-quantized driver period of the content bpm
}
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct RegionRects([[i32; 4]; MAIN_REGION_COUNT]);
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ChromeInputs {         // field list re-derived from push_state's chrome reads in P4c
    pub playing: bool, pub recording: bool, pub automation_armed: bool,
    pub sync_state: u8, pub link_state: u8, pub clock_state: u8, pub link_peers: u16,
    pub bpm_tenths: u32, pub clock_authority: u8,   // from ContentState (D24)
    pub time_digits: u64,                // minutes, seconds, tenths, bar, beat, sixteenth, packed
    pub clock_digits: u64,               // the shown MIDI-clock position, packed digits
    pub export_range_px: [i32; 2], pub sdr_preview: bool, pub hdr: bool,
}

// ui_snapshot — the freshness check's second half (D10)
pub(crate) struct ImmediateInputs { /* insert cursor, selection band, playhead px, clip draw list,
    automation lane draw inputs, overview strip inputs, PerformHudInputs */ }
// AssertFresh compares tree dump + ImmediateInputs + pending toast queue.

pub(crate) enum MainRegion { Transport, Header, Footer, Inspector, AudioDock, SceneDock,
                             SplitHandles, LayerHeaders, Viewport, Overlays, Ghosts }  // Ghosts: no atlas slot
pub(crate) enum EditorRegion { PreviewColumn, CardLane, Overlays }
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct RegionMask(u16);       // bit per region discriminant

pub(crate) struct FramePlan {
    pub projections: ProjectionMask,     // bit set over the projection table, section 4
    pub rebuild: RegionMask,
    pub translate: RegionMask,           // rect origin moved, size did not
    pub in_place: RegionMask,
    pub repaint: RegionMask,             // atlas slots to repaint
    pub redraw: bool,
}
pub(crate) fn plan_frame(last: &FrameInputs, next: &FrameInputs) -> FramePlan;          // pure
pub(crate) fn apply_frame_plan(plan: &FramePlan, frame: &mut UiFrame<'_>);              // interior free

// editor_frame.rs — same shape for the editor window (D17)
pub(crate) struct EditorInputs {         // Copy; snapshot version, catalog default version, canvas scope
    // version, canvas selection, canvas pan/zoom bits, watched graph_version, preview aspect bits,
    // plus parts, selection_identity, values and runtime folded from FrameInputs
}
pub(crate) fn plan_editor_frame(last: &EditorInputs, next: &EditorInputs) -> FramePlan;

// perform_mode/render.rs (D17)
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PerformHudInputs {     // one field per draw_cue_hud argument, at pixel resolution
    pub current_cue: Option<u16>, pub next_cue: Option<u16>,
    pub countdown_digits: u64, pub bar_beat_digits: u64,   // what the HUD shows, packed
    pub section_fill_px: Option<u16>,    // section-progress bar fill width in whole pixels
    pub playing_tracks: u64,             // bit set over track slots
    pub bpm_tenths: u32, pub sync_flags: u8,
    pub macro_values: [u16; MACRO_SLOTS], // display resolution
    pub exit_hover: bool, pub size_px: [u16; 2],
}
```

**No raw `Arc` addresses as keys.** A freed `Arc` and a new one can share an address, so a stored integer would read "unchanged". Shared snapshots enter as versions instead: where an `Arc` arrives (`editor_ui_snapshot` in `editor_bridge.rs`, the setter of `watched_catalog_default`, `ableton_session`, `midi_device_names`), the intake compares it with `Arc::ptr_eq` against the `Arc` it already retains, which keeps the address alive, and bumps a `u64` when they differ. `FrameInputs` carries the version. Versions keep `FrameInputs` a `Copy` struct of integers, and the `ptr_eq` stays sound because it only ever runs against a retained `Arc`.

`UiFrame` bundles what projections read today (`ui_root`, `local_project`, `selection`, `content_state`, cache). Its field list is the executor's, re-derived from the current `sync_*` signatures.

**`manifold-ui`:** `TimelineEditingHost` drops `mark_dirty`, `request_redraw`, `request_automation_lane_resync`, `invalidate_layer_bitmap` and `invalidate_all_layer_bitmaps` and gains `fn redraw(&mut self)` and `fn set_preview(&mut self, preview: GesturePreview)` (D12; `GesturePreview` holds the dragged clip timings, a region move's split pieces, or lane points, owned by `InteractionOverlay`; `split_clips_for_region_move` returns pieces into it instead of writing `local_project`). Panels that own view state expose `fn view_version(&self) -> u64`, bumped inside their own setters only on a visible change (inspector collapse, fold, tab, compact, drawer target; docks; overlays). The fields behind them become private. `UIState`'s selection sets and automation lane fields (`ui_state.rs` :77–204) become private with setters that bump `selection_identity`, `selection_visual` or `automation_view` (D14, D19). Panels hold drag targets as `WidgetId` (D18).

**`manifold-ui`, `UITree` per-region storage (D7, P4e):**

```rust
pub struct UITree {
    arenas: [RegionArena; MAX_REGIONS],   // MAX_REGIONS = 16; region id = MainRegion/EditorRegion discriminant
    paint_order: SmallVec<[PaintEntry; 32]>, // every sub-region, sorted by (tier, region rank, local insertion);
                                          // hit_test walks it reversed
    gen_counter: u32,
    text_measure: Box<dyn TextMeasure>,
    structure_version: u64,
}
struct RegionArena {                      // the current SoA Vecs, one copy per region
    nodes, parent_index, first_child, next_sibling, last_child, generations,
    widget_ids, names, child_counts, root_count, widget_to_node, has_dirty,
    sub_regions: SmallVec<[SubRegion; 4]>, // each begin_region inside this arena: tier, rect, root slot
}
struct PaintEntry { tier: ZTier, region_rank: u8, local: u8, region: u8 }
// Root WidgetId = WidgetId::ROOT.with(REGION_SALT_FLAG | region).with(root_count):
// seeded by region id, so roots of different arenas never collide.
// NodeId keeps its u64 layout: generation << 32 | (region << 24 | slot).
impl NodeId { pub fn region(self) -> u8; pub fn slot(self) -> usize; }   // index() is deleted
impl UITree {
    pub fn begin_region(&mut self, region: u8, rect: Rect, tier: ZTier, label: &'static str, flags: UIFlags) -> RegionToken;
    pub fn clear_region(&mut self, region: u8);     // replaces truncate_from
    pub fn translate_region(&mut self, region: u8, dx: f32, dy: f32);
    pub fn region_range(&self, region: u8) -> (usize, usize);   // slot range inside the arena
}
```

**Hit-order rule:** `hit_test` walks sub-regions in reverse paint order (tier descending, then region rank descending, then local insertion descending) and, inside a sub-region, reverse slot order. Region rank is the `MainRegion` discriminant order, which is today's full-build order, so the result equals today's order: overlays, ghosts and dropdowns are hit before the panels they cover. `root_minted` is deleted because each arena counts its own root salts from zero, seeded by its region id. **Site map:** the ten main panels map one-to-one to `MainRegion`; warmup (Chrome) and the overlay stack (its own tiers) are sub-regions of `Overlays`; the card-drag ghost (Ghost) goes in the `Ghosts` arena, outside the atlas; toasts (Ghost) stay a sub-region of `Overlays`; a dropdown or the Ableton picker (Overlay) is a sub-region of the panel that builds it; the editor preview column is `EditorRegion::PreviewColumn`. Test-only sites keep a single default arena. Atlas slot = region; `PanelCacheInfo` carries `region` and a region-local range. Call sites that change, by `rg` at design time: about 70 `NodeId::index()`, 66 `id_at`, 148 `node_range`/`first_node`, 123 `tree.count()` captures, `chrome/diff.rs` :508 `truncate_from`. Each becomes region-local; the compiler lists them once `index()` and `truncate_from` are deleted.

**`manifold-ui-paint`:** `UICacheManager::invalidate_slot(PanelSlot)` replaces `invalidate_all`, `invalidate_scroll_panels` and `invalidate_inspector`; `PanelSlot` becomes one slot per region. `CachedTextMeasure` replaces `CoreTextMeasure`.

**`manifold-foundation`, module `work_count`:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkKind {
    FullBuild, RegionRebuild, RegionTranslate, InPlaceUpdate, SlotInvalidate,
    ProjectSync, InspectorSync, ClipProjection, LaneProjection, PresetListProjection, ValuePush,
    EditorProjection, EditorTreeBuild, PerformHudBuild, OverviewRepaint, GridBitmapRepaint,
    ChromeLayout, TextMeasureMiss, DriverScan,
    ProjectClone, ModSnapshotCopy, RendererReload, AbletonListenerRebuild, ListenerRangeRefresh,
    OscRouteRebuild, PresetOverlayInstall, PresetFingerprint, AudioReconcile, DeviceQuery,
    ClipBindingReconcile, GeneratorStructureRefresh, TempoReconcile, TriggerTargetRefresh,
    FrameAlloc,
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
| `sync_project_data` | Layers, Clips, Automation, Markers, Settings (time signature, `projection/timeline.rs` :345); selection identity; active layer; automation view |
| `sync_inspector_data` | Layers, Effects, Mappings, Clips, Automation (the lane dot :1505 and `external_attachment`, `projection/material.rs` :294, which keeps mapped rows on the scene card: BUG-pmbd3.7 (Scene card hides mapped rows) under BUG-pmbd3 (3D scene surfaces epic)), Led, AudioSetup, EmbeddedPresets, Settings (bpm and frame rate for driver timing, `projection/inspector.rs` :101); selection identity; active layer; automation view (includes the selected point's lane, :116); runtime latch and LED-presence sequences |
| Audio Setup device list (inside the inspector projection today) | AudioSetup; audio directory version — the CoreAudio query runs only then |
| `sync_clip_positions` | Clips, Layers (all clips); preview (dragged clips and split pieces only, in place) |
| `sync_automation_lane_geometry`, `sync_automation_lane_order` | Automation, Effects, Layers; automation view; preview (dragged lane only) |
| Live push (`push_state` piece 1; D22) — every display of a live scalar, both windows, in place | Values displays: values sequence, UI Values stamp. Shapes displays: UI Shapes stamp. Range bands (the Ableton range on cards, `projection/cards.rs` :474, and macro trim ranges): UI MappingRanges stamp |
| Selection push (`push_state` piece 2) | selection visual; cursor |
| Event drain (`push_state` piece 3) | every frame, O(1): undo/redo key, diagnostic sequences, modifier-selection sequences, Ableton session `Arc` |
| Chrome push (`push_state` piece 4) | Settings, Mappings, Layers; `ChromeInputs` (BPM tenths and time digits included) |
| Editor card live push (`sync_card_values`, `editor_bridge.rs` :1776–1793, widened to every live display, D22) | same per-group keys as the main live push; active layer; card lane rebuilt |
| Driver period readout (`sync_driver_bpm`) | Effects; driver period key — in place, never a structural sync |
| Embedded preset list (`dropdowns.rs` `sync_embedded_presets`) | EmbeddedPresets |
| Editor canvas `set_snapshot` | snapshot version; canvas scope version; preview aspect |
| Editor MODIFIED badge, viewport def | watched graph_version; catalog default version |
| Editor visible nodes, preview node | snapshot version; canvas scope; canvas selection |
| Editor mini timeline | Clips, Layers; watched target; preview |
| Overview strip repaint | Clips, Layers; preview; scroll; zoom; `playhead_px`; rects |
| Audio waveform requests (`app_render.rs` :458) | Clips |
| Detect status (`clip_chrome.rs` :715) | in place, only when status text or progress pixel moves |
| Perform HUD draw list | `PerformHudInputs` |

The input matrix (D10, INV-16) checks this table: it perturbs each input and records, separately, which projections change (rebuild dependencies) and which in-place pushes change (in-place dependencies). A live-scalar part (Values, Shapes, MappingRanges) showing up as a rebuild dependency fails the test (D22). Review found the Settings, Automation and point-selection entries; the matrix exists so the next missing input is found by a test.

**Regions (main window).** Each region rebuilds alone (D7).

| Region | Rebuild when | Translate when | In place when |
|---|---|---|---|
| Transport, Header, Footer | own rect size; displayed values at display resolution change shape | own rect origin | chrome push and selection push values, through the existing chrome diff |
| Inspector | own rect size; inspector projection ran; inspector view | own rect origin | inspector scroll; card values |
| Audio dock, Scene dock | own rect size; their projection ran; dock view | own rect origin | dock scroll |
| Split handles | own rect size | own rect origin | — |
| Layer headers | own rect width; project projection ran | own rect origin | scroll y; own rect height; selection visual; active layer; lane heights |
| Viewport | own rect width; project projection ran; lane projection changed the lane count; Markers | own rect origin | scroll x, scroll y, zoom (pooled ruler); own rect height; selection visual; cursor; preview; lane heights |
| Overlays | overlays version | — | warmup and export progress; Ableton picker session refresh |
| Ghosts (no atlas slot) | refilled each frame a card-drag ghost exists; empty → no work | — | — |

**Regions (editor window).** Preview column: rebuild on snapshot version, canvas selection, own rect size; Card lane: rebuild when the inspector projection ran or on own rect size, in place on scroll and values; Overlays: overlays version.

**Content reactions (once per drain pass, before the tick).**

| Moved | Reaction |
|---|---|
| Layers, Effects, Mappings | OSC route rebuild |
| Layers, Effects, Mappings | Ableton listener rebuild (when connected) |
| MappingRanges | refresh cached ranges on existing `WriteTarget`s in place (no OSC, no allocation) |
| Layers, Clips, Effects, Session | `engine.mark_clip_bindings_stale()`; the next reconcile walks once (BUG-6ompp (content tick re-hashes every clip)) |
| Layers, Clips, Effects | `render_all`'s `structure_key` moves: layer-index refresh, string params dirty, eviction |
| Layers, Effects, AudioSetup | trigger-target key moves (`trigger_targets.refresh`) |
| Settings | `reconcile_tempo_edit` against the tempo map and BPM retained at the last reaction; the retained copy updates here, never per command |
| Media | `on_project_loaded` on each renderer (only `VideoRenderer` overrides it, `video_renderer.rs` :687) |
| EmbeddedPresets | install the project preset overlay; the `graph_snapshot` cache key moves |
| AudioSetup | audio reconcile (plus the existing device and process flags) |
| any part | `mark_compositor_dirty_now` |
| Shapes | in-place, allocation-free syncs for consumers that cache a live scalar (D25): `sync_gains` into the capture `GainBank` (`audio_mod_runtime.rs` :993); any further cached consumer the INV-20 table lists |
| Values, Overrides, Led, Markers, Automation, Midi, ViewPrefs | nothing beyond the above (Overrides: the graph structure version already rebuilds the renderer) |
| `data_version` moved, or a content-originated `mutate` moved a part outside Values, Shapes and MappingRanges (audited tick writes excepted) | publish a project snapshot in `send_state` (D23) |

The output-resize validator is not a row: it runs inside `execute_with_validation` and can reject the edit. The inline listener rebuild after Ableton discovery (`content_thread.rs` :1808) is deleted: discovery validation is a compare-before-write `mutate(Mappings)`, and the Mappings row rebuilds listeners once. A second validation finds nothing to change, so the two cannot feed each other (INV-2).

`⚠ VERIFY-AT-IMPL (P3a): confirm LED output, MIDI routing and session launch read their settings fresh each tick, and that trigger targets depend only on Layers, Effects and AudioSetup (read content_pipeline/trigger_targets.rs :32). A consumer that caches one of them gets a row here, never a per-command call.`

---

## 5. Every finding, and what removes it

| # | Finding | Removed by | Phase |
|---|---|---|---|
| 1 | Embedded-preset fingerprint every UI frame and after every content command | Function deleted. UI list keyed on the EmbeddedPresets stamp; content overlay installs only when that stamp moves | P3a, P4c |
| 2 | Scrollbar and overview drags return `structural()` per move | `DispatchResult::structural_change` deleted. Scroll is a `FrameInputs` field; the plan updates the viewport in place | P4d |
| 3 | Text measurer uncached; chrome view + layout every frame; truncation one CTLine per byte | One cached measurer; binary-search truncation; chrome rebuilds only when its displayed values move | P2, P5b |
| 4 | LED and Ableton trim drags trigger project-load work per move | LED declares Values: no reaction. Trim declares MappingRanges: an in-place range refresh, no listener rebuild (D20) | P3a |
| 5 | Open graph editor re-derives everything each frame and rebuilds its whole inspector every vsync | Editor regions, arenas and atlas; projections keyed in `EditorInputs` (D17, BUG-36mjb (open graph editor rebuilds the whole inspector every vsync)) | P4c, P5c |
| 6 | Selection syncs the inspector 2–3x; device queries inside the sync | The planner runs each projection at most once per frame, keyed on selection identity. Device query keyed on the audio directory version | P4b, P4c |
| 7 | Modifier keys, dock wheel, import progress, browser search rebuild | Modifiers are not an input. Dock scroll in place. Import progress updates the header in place and completion arrives as stamps. Search bumps the overlays version, so only the overlay region rebuilds | P5a, P5b |
| 8 | Per-frame work regardless of change | Overview keyed on the playhead pixel. Detect status in place on change. Waveform requests keyed on Clips. Value push keyed on the values sequence (D15). Allocation residue (`push_state` churn, param-card name clone, `present.rs` scratch, unused `duration_beats()`) cleared to an allocation count of zero | P3a, P4c, P6 |
| 9 | Inspector, drawer and split tweens force full build + `invalidate_all` | Drawer and card tweens by reveal, in place (P5b). The split tween translates the regions that move and resizes the two that change size in place (D7; decided item 12 revised) | P5b |
| 10 | Whole-project deep clones | Measured by `ProjectClone` on both threads; structural sharing is the committed end state with its own design (section 9) | P1, after P6 |
| 11 | `sync_driver_bpm` forces full rebuilds under clock jitter | Driver readout in place; scan keyed on Effects and the period key (D20, BUG-ilntv (clock jitter forces full UI rebuilds)) | P4c |
| 12 | Hover still rebuilds scroll panels after the BUG-y3a9a fix; hover repaints grid bitmaps | `redraw()` never touches the tree; bitmap invalidation deleted (BUG-972o1 (stale layer-bitmap invalidation and dead viewport fingerprints)) | P5a |
| 13 | Clip and automation drags would become full rebuilds once host writes stamp | Drags are previews; stamps move on commit (D12, BUG-w3csc (clip and automation drags would rebuild per mouse move)) | P4b |
| 14 | Content tick re-hashes every clip because borrows set `clip_bindings_dirty` | Reaction row; borrows never set the flag; render path borrows shared (D13, BUG-6ompp (content tick re-hashes every clip)) | P3a |
| 15 | About 45 content writes skip `EditingService` | `ProjectEdit`, crate-private `project_mut`, every write through `mutate` (D13, BUG-cg8gj (content thread writes the project outside EditingService)) | P3b |
| 16 | `push_state` and the modulation snapshot copy run every frame | Values sequence (D15, BUG-p2t5o (push_state and the modulation snapshot copy run every frame)) | P3a, P4c |
| 17 | Split, inspector-width and dock-width drags rebuild every region per move | Per-region rects: translate or rebuild only the resized regions (D7, BUG-m22n9 (split and resize drags rebuild every region)) | P4e, P5a, P5b |
| 18 | Inspector rebuild drags layer headers, viewport and overlays along (suffix collateral) | Per-region storage (D7, BUG-bluz6 (per-region UI tree storage)) | P4e |
| 19 | Zoom rebuilds layer headers and the viewport; scroll falls back to a rebuild on a tick-count change | Zoom in place; pooled ruler (D7, BUG-3vs01 (horizontal scroll falls back to a viewport rebuild)) | P5b |
| 20 | Selection, insert-cursor moves and scope pin bump one version that rebuilds the inspector, layer headers and viewport | Identity vs visual vs cursor (D14) | P4b, P5b |
| 21 | Automation view misses mode visibility and lane heights; lane resize rebuilds per move; slider grab reorders hidden lanes | `automation_view` covers all of it and bumps on visible change only; lane resize in place (D19, BUG-veaap (automation-lane-resize-full-rebuild)) | P4c, P5b |
| 22 | About 100 UI sites execute commands straight on `local_project`; `ctx.project` is mutable | `ctx.execute`, read-only `ctx.project` (D5); the scene skin edits move with it (BUG-bo2mx (scene setup skin edits run against the UI snapshot)) | P4b, P4d |
| 23 | Content runtime state shapes the UI with no version (latches, LED preview presence, warmup, Ableton session, SDR preview, MIDI device names) | `RuntimeSeqs`, `Arc` intake (D15, D16) | P3a, P4c |
| 24 | Rebuilds deferred during drags; a pure planner cannot defer | Drag targets by `WidgetId`; guards deleted (D18, BUG-in6m0 (rebuilds deferred during drags because panels hold raw NodeIds)) | P5a |
| 25 | `render_all`, the tempo reconcile, `trigger_targets` and the `graph_snapshot` cache key on `data_version`, per command or on the deleted fingerprint | Reaction rows and `PartStamps::key` (BUG-tsjs1 (re-key content work on part stamps)) | P3a |
| 26 | Ableton trim drag rebuilds every listener every content frame | `MappingRanges` part and in-place range refresh (D20, BUG-kn2cm (Ableton trim drag rebuilds every listener)) | P3a |
| 27 | Perform HUD rebuilds text and lists every frame | `PerformHudInputs` (D17, BUG-1d6sh (perform-mode HUD rebuilds its text and lists every frame)) | P5c |
| 28 | Dead viewport fingerprints; per-frame overview clip hash during drags | Deleted; overview keyed on Clips (BUG-972o1 (stale layer-bitmap invalidation and dead viewport fingerprints)) | P5a |
| 29 | Work flows covered no continuous drag, so the gate could not see 13, 17, 19–21 | Continuous-gesture work flows in P1 | P1 |
| 30 | Revision 2 regression: live drags of driver, envelope, audio-mod and trigger shapes, gains, crossover, relight, unbound node params and the mapping-drawer preview would declare Effects or AudioSetup and pay reactions and inspector rebuilds per move | `Shapes` part, re-derived Part table (D21); a P1 work flow and a `content_reaction_budget` case per drag (BUG-w3csc (clip and automation drags would rebuild per mouse move)) | P1, P3a |
| 31 | Revision 2 regression: `push_state` keyed on values alone would freeze the cursor, selection band, toasts, modifier selection and chrome while paused; `AssertFresh` cannot see immediate passes or toasts | Four pieces with honest inputs (D15); `cursor` version (D14); `ImmediateInputs` and the toast queue in `AssertFresh` (D10) (BUG-p2t5o (push_state and the modulation snapshot copy run every frame)) | P4a, P4b, P4c |
| 32 | Projection inputs missing from the table: time signature, bpm and frame rate, automation dots and scene-card attachment, Ableton ranges, the selected point's lane | Rows added (section 4); input matrix test derives the table (D10, INV-16, BUG-imm (ui-flow-harness first-gesture free rebuild masks missing invalidation)) | P4b |
| 33 | Revision 2 regression: content-side `mutate` writes (Ableton statuses after discovery) never publish a snapshot | Today's publish rule plus content-side `mutate`; the UI re-projects on a stamp change (D23, INV-17, BUG-cg8gj (content thread writes the project outside EditingService)) | P3a |
| 34 | Per-arena root salts would give every region root the same `WidgetId` | Root salts seeded by region id (D7, INV-18, BUG-bluz6 (per-region UI tree storage)) | P4e |
| 35 | 18 `begin_region` sites with their own tiers, several built inside panels | Tiered sub-regions inside an arena; paint and hit order equal today's (D7, INV-14) | P4e |
| 36 | Overview strip and editor mini timeline would stop following drags | Preview is an input of both (D12, section 4) | P4b, P4c |
| 37 | Mid-drag snapshot hold exists because region-move splits write `local_project` | Splits go into the preview; hold deleted (D12) | P4b |
| 38 | Clip move and trim drags repaint every grid bitmap per move | Bitmap invalidation deleted (BUG-972o1 (stale layer-bitmap invalidation and dead viewport fingerprints)) | P5a |
| 39 | Round 2 regression: the inspector projection draws Shapes, but only the slider push followed them, so an undo of a trim left stale drawers and the editor card lane stopped following main-window drags; keying the inspector on MappingRanges rebuilt it per trim move | Live push over every live display in both windows; live scalars never rebuild inputs; the matrix enforces it (D22, INV-16) | P4b, P4c, P5c |
| 40 | Retracted round 4: round 3 claimed mute, solo, collapse and export-range clicks deep-clone; they are `MutateProject` closures and do not | Today's publish rule kept, plus content-side `mutate` (D23, INV-17) | P3a |
| 41 | Tick writes of BPM, clock authority and tempo-recorder points would move stamps and publish under D13 | Audited tick writes: compare first, no stamp, no publish, no reaction; one Settings stamp at session end without a tempo reconcile (D24, INV-22) | P3b |
| 42 | Send gain is cached in the capture `GainBank` and synced only in the reconcile | `sync_gains` reaction row; consumer table (D25, INV-20) | P3a |
| 43 | First tick of a mapping-drawer or node-param scrub can materialize a graph override | The first move declares the reaction-free `Overrides` part with Shapes; later ticks declare Shapes (D21) | P3a |
| 44 | The card-drag ghost is minted from input and drawn in the immediate pass | `Ghosts` arena outside the atlas, refilled while present (D7) | P4e |
| 45 | Inline listener rebuild after Ableton discovery duplicates the reaction | Deleted; discovery is a compare-before-write `mutate(Mappings)` (INV-2) | P3a |

---

## 6. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| INV-1 Every project mutation declares a change set that covers what it changed | `MutateProject` has a required `touches` field (compile-time). `Command::touches` has no default after P6 (compile-time). Change audit (`change-audit` feature, on in `manifold-editing` tests, `command_roundtrips.rs`, the content budget test and the runner) panics on an undeclared part change |
| INV-2 Content reactions run only from the reaction pass | `rg -n 'on_project_loaded\|rebuild_listeners\|osc_param_router.rebuild\|install_project_preset_overlay\|reconcile_tempo_edit\|reconcile_clip_control_bindings\|mark_clip_bindings_stale' crates/manifold-app/src/content_commands.rs crates/manifold-app/src/content_thread.rs` → hits only inside the reaction function and the load path (the inline discovery rebuild at `content_thread.rs` :1808 is gone). Content budget test ceilings |
| INV-3 An idle frame does zero work, on every window | Work flows `work-idle`, `work-editor-idle`, `work-perform-idle`: every counter zero, `FrameAlloc` included; content test `idle_tick_does_no_reaction_work` (zero `ClipBindingReconcile`, `ModSnapshotCopy`, `GeneratorStructureRefresh`) |
| INV-4 Incremental UI equals a full rebuild from the same inputs, including what immediate passes draw and what toasts show | `AssertFresh` closes every work flow; it diffs the tree dump (ignoring generations, node slots and input-state flags), the `ImmediateInputs` record and the pending toast queue |
| INV-5 Only the planner builds, rebuilds, translates or invalidates | `only_the_frame_planner_builds` test: source scan (shaped like `no_bespoke_row_infra`) fails on `UIRoot::build`, `clear_region`, `translate_region`, `invalidate_slot` outside `ui_frame.rs` and `editor_frame.rs` |
| INV-6 No hand-set invalidation flags and no hash keys | P5a negative gate: `rg 'needs_rebuild\|needs_structural_sync\|ScrollDirty\|automation_lanes_dirty\|structural_change\|fn mark_dirty\|request_automation_lane_resync\|invalidate_layer_bitmap\|invalidate_all_layer_bitmaps\|invalidate_layers\|compute_fingerprint\|overview_last_clip_fingerprint\|embedded_presets_fingerprint' crates/` → 0 |
| INV-7 One text measurer, cached | `rg 'impl TextMeasure for' crates/ -g '!*test*'` → `CachedTextMeasure` and `HeuristicTextMeasure` only; `measure_text_ct` called only from the cache-miss path |
| INV-8 Stamps never serialize; load moves every part | `load_moves_every_part` test; `rg 'PartStamps' crates/manifold-io` → 0 |
| INV-9 Ceilings rise only by explicit record | The flow runner and content test fail on any count above its ceiling; unlisted kinds have ceiling zero |
| INV-10 Every content-thread project write goes through `EditingService` | `ProjectEdit` has no public accessor and `project_mut` is `pub(crate)` to `manifold-playback` (compile-time), so `content_thread.rs` and the rest of `manifold-app` reach the project only through `engine.project_edit()` into `execute`, `mutate` or `tick_write`; `rg 'project_mut\(\)' crates/manifold-app/src -g '!*test*'` → 0; the change audit wraps the tick window (`content_thread.rs` :616–628 plus `engine.tick` at :653) and requires every non-Values part unchanged except the five `TickField`s written through `tick_write`, with `tempo_map` changes allowed only while a recording session is active or on the beat-0 path (D24) |
| INV-11 UI writes go through the three helpers | `DispatchCtx.project: &Project` (compile-time); `rg 'execute\(ctx\.project\|\.execute\(&mut self\.local_project' crates/manifold-app/src -g '!*test*'` → hits only inside `DispatchCtx::execute` and the host |
| INV-12 A preview never moves a part stamp | Test `drag_preview_moves_no_stamp` drives a clip drag and an automation point drag through the real overlay and asserts `UiStamps` unchanged until release; `work-clip-drag` ceiling: zero `ProjectSync`, zero `RegionRebuild` |
| INV-13 No drag defers a rebuild | `rg 'is_dragging\(\)' crates/manifold-app/src/ui_frame.rs` → 0; flow `drag-survives-rebuild` forces a rebuild mid slider drag and mid layer drag and asserts the drag continues |
| INV-14 A region rebuild touches only its own arena; paint and hit order equal today's | `manifold-ui` tests `clear_region_keeps_other_regions_live` and `hit_test_follows_region_paint_order`; `manifold-app` test `paint_and_hit_order_match_full_build` records the paint sequence and a hit-test grid with a dropdown, the Ableton picker, a card-drag ghost, a toast, warmup, a hover tooltip and the text-input overlay open, on the flat tree before P4e and on arenas after, and requires them equal; `rg 'fn truncate_from\|fn index\(self\)' crates/manifold-ui/src` → 0 |
| INV-15 A view-state setter bumps only on a visible change | `plan_frame` row tests per setter (same value → no bump; lane order change with lanes hidden → no bump); `work-slider-grab` ceiling zero `InspectorSync` with lanes hidden |
| INV-16 Every dependency is in the table with its kind, and live scalars are only in place | `projection_input_matrix` (D10, D22): one perturbation per part and per `FrameInputs` field; it records rebuild dependencies (projection output changed) and in-place dependencies (push output changed) separately; a dependency its row does not list fails; Values, Shapes or MappingRanges appearing as a rebuild dependency fails; an unobserved row is reported |
| INV-17 Every `data_version` move and every content-originated change outside Values, Shapes and MappingRanges publishes; a UI `MutateProject` does not | Test `content_mutate_publishes`: a content-side `mutate(Mappings)` status write, an undo of a Values edit (slider) and an undo of a Shapes edit (trim) each appear in the next accepted snapshot; `content_reaction_budget` trim, driver-trim, send-gain, OSC-router and Ableton-bridge ticks show zero `ProjectClone` |
| INV-18 Widget ids are unique across regions | `manifold-ui` test `widget_ids_unique_across_regions`: build every main region and the editor regions, rebuild one, and require no `WidgetId` shared by two live nodes |
| INV-19 A live drag costs no reaction and no projection | Work flows for every section 1 live drag: zero `ProjectSync`, `InspectorSync`, `RegionRebuild` per move; `content_reaction_budget` cases for the same drags: zero reactions |
| INV-20 Every live scalar reaches every consumer | Consumer table in `manifold-core` lists each Values and Shapes field with its consumers, read-fresh or cached-with-row (D25); test `shapes_fields_reach_consumers` perturbs each field through `mutate` and asserts each consumer sees it after one drain pass and one tick; an unlisted field fails |
| INV-22 A clock wobble moves no stamp and publishes nothing; tick timing is unchanged | Change audit around `PlaybackEngine::tick` allows only Values and the five `TickField`s, with `tempo_map` changes only in a session or on the beat-0 path (D24); test `clock_wobble_is_not_an_edit` runs Link with a ±0.01 BPM wobble and asserts zero stamp moves and zero `ProjectClone`; `tempo_ramp_matches_today` compares beat and time per tick through a tempo-recording session against values recorded from today's code, and asserts exactly one Settings stamp at session end |

---

## 7. Phasing

Phased by layer: measure, then the measurer, then the content gateway, then UI projections, then UI regions, then the other windows, then close-out. Each phase lands alone, re-records its ceilings downward, and leaves the old code it replaces deleted. Work flows use built-in scenes (deterministic, present in worktrees). A Liveschool soak run (`project:<path>`) is on demand, not a gate. No GPU proofs anywhere: nothing touches kernels.

Order: P1 → P2 → P3a → P3b → P4a → P4b → P4c → P4d → P4e → P5a → P5b → P5c → P6. P2 and the P3 pair are independent of each other after P1.

### P1 — Work counters and the gate (measure the before)

- **Entry:** anchors in section 1 re-verified (`rg -n 'fn apply_ui_frame_invalidations' crates/manifold-app/src/ui_frame.rs`; `rg -n 'struct Runner' crates/manifold-app/src/ui_snapshot/script.rs`; `rg -n 'pub(crate) fn headless_content_thread' crates/manifold-app/src/headless_harness.rs`).
- **Read-back:** this doc sections 1–3 and 6; `cold_touch.rs`; `script.rs` :1–74 and `record_executed_commands`; `scripts/rt_noise_gate.py`; `scripts/run_ui_flows.py`. Restate D10, D11, the forbidden moves, and the entry findings.
- **Deliverables:** `work_count` module; `record_work` at every site the `WorkKind` list names, across both threads; the counting allocator under `work-trace`; `MANIFOLD_WORK_TRACE=1` per-frame trace line on both threads (env read once at startup, the `MANIFOLD_AUDIO_TRACE` precedent `audio_mod_runtime.rs` :318); `AutomationAction::ResetWork`, `AutomationAction::AssertWork { window }`; a `Step` that advances `ContentState` beat position while playing (a driver input, not an engine copy); work flows `scripts/ui-flows/work-{idle,playback,follow-scroll,clock-jitter,hover,automation-hover,scrollbar-drag,zoom,slider-drag,slider-grab,select-click,cursor-nav,modifier,clip-drag,clip-trim,automation-point-drag,marquee,lane-resize,layer-reorder,split-drag,inspector-resize,drawer-tween,dock-scroll,browser-search,editor-open,editor-idle,perform-idle,driver-trim,envelope-drag,audio-mod-range,trigger-shape,relight-drag,layer-gain,send-gain,band-split,node-param-drag,mapping-drawer,paused-cursor,region-move,trim-undo,values-undo}.json` (clock-jitter steps frames while the `ContentState` BPM wobbles by ±0.01; the ten live-drag flows drive the section 1 live drags; paused-cursor moves the insert cursor and selects while paused, so a frozen selection push shows; slider-grab grabs layer-scope params with lanes hidden); `work_ceilings.json` recorded from today's code; `--record-work` on `ui-snap` and `run_ui_flows.py`; content test module `content_reaction_budget` driving the real `handle_command` and tick on `headless_content_thread` with commands produced by real `ui_bridge::dispatch` for: idle ticks with no commands, LED tick, Ableton trim tick, mapping-drawer tick, param slider tick, param Execute, clip move Execute, undo, and one tick per section 1 live drag (driver, envelope, audio-mod and trigger shape, layer and send gain, band split, relight, unbound node param, mapping-drawer preview), plus an OSC router tick and an Ableton bridge tick, each asserting zero `ProjectClone`; a nightly leg `["python3", "scripts/run_ui_flows.py", "work-"]` in `scripts/trunk_health.py`'s GPU gate list (beside `rt_noise_gate.py`), so a red files a bead; a CPU-scope mapping so touching `crates/manifold-editing/src/commands/` runs `content_reaction_budget`. `⚠ VERIFY-AT-IMPL: how scripts/cpu_scope.py maps paths to filtersets — read it before adding the mapping.` `⚠ VERIFY-AT-IMPL: whether the runner can drive the editor window and perform mode (BUG-24s (VD-030 editor-window surfaces to L3)); a surface it cannot drive is measured by the live trace only and filed as verification debt.`
- **Gate (positive):** every work flow exits 0 against its recorded ceilings; `content_reaction_budget` green; the trace prints on a live run. Report the before table (counts per gesture) in the landing commit. **Gate (negative):** no `Instant`, `elapsed` or duration in any work assertion (`rg 'elapsed|Instant' crates/manifold-app/src/ui_snapshot/` shows no new hits in the work code).
- **Demo:** L3 — the work flows drive real input; plus one `MANIFOLD_WORK_TRACE=1` live run on Liveschool by the lead for the app-only per-frame work the runner does not yet execute.
- **Performer gesture:** hover the automation lanes for 2 seconds, then drag a clip for 2 seconds. Those two counts head the before table.
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

### P3a — Change stamps at the gateway, and content reactions

- **Entry:** P1 landed. Re-run the section 1 counts for `MutateProject*` (48/19/6) and `impl Command for` (204).
- **Read-back:** sections 2–4; `service.rs`; `command.rs`; `content_commands.rs` :780–800, :1200–1460, :1700–1780 and :2225–2235; `content_thread.rs` `run` drain loop, `send_state`, `graph_snapshot` :1535–1580; `content_state.rs` `capture_into` :411; `engine.rs` :455–540 and :869–880; `generator_renderer.rs` :700–760; `content_pipeline/trigger_targets.rs`; `ableton_bridge.rs` `rebuild_listeners` :2061; `audio_mod_runtime.rs` :340–440. Restate D1–D4, D15, D16, D20 (content half), the Part table, the reaction table.
- **Seam brief:** old `MutateProject(f)` / `MutateProjectLive(f)` / `MutateProjectPreview(f)` → new `MutateProject { touches, f }`. Delete the two extra variants first; the compiler lists every sender. Worked examples: LED brightness tick (`scrub.rs` :860) → `touches: ChangeSet::of(&[Part::Values])`; Ableton trim (`scrub.rs` :1288) → `MappingRanges`; rack expand (`dispatch/params.rs` :115) → `Effects`. A closure that writes fields of more than three parts, or one whose fields you cannot name, is an escalation. Old `render_all(.., data_version, ..)` → `render_all(.., structure_key, ..)`; old `trigger_targets.refresh(project, data_version, epoch)` → `refresh(project, stamps.key(Layers | Effects | AudioSetup), epoch)`.
- **Deliverables:** `change` module with `PartStamps::key`, the `Shapes` part and `ChangeSet::LIVE` (D21); every section 1 live drag declares Values or Shapes; the publish rule (D23, INV-17); the `sync_gains` reaction row and the live-scalar consumer table (D25, INV-20); on an accepted snapshot only, the UI re-projects when `data_version` moved or when `ContentState.stamps` moved outside Values, Shapes and MappingRanges (`ContentState` travels every tick, so a Values move alone never re-projects), so Ableton statuses (and in P3b detect and replan results) keep updating before the planner lands in P4b; the graph-editor canvas param path (`app_render.rs` :2462–2515), which sits outside `DispatchCtx`, declares Shapes on its ticks and `Overrides | Shapes` on its first move like the dispatch paths (D21); the inline listener rebuild after Ableton discovery deleted (`content_thread.rs` :1808; validation becomes a compare-before-write `mutate(Mappings)`, and the reaction rebuilds listeners); `Command::touches` with default `ALL`; explicit `touches` on the commands the P1 content gestures use and on effect-param, clip move/trim, automation point and mapping commands; `EditingService::{stamps, mutate}`; the reaction pass and the deletion of per-arm reactions, the per-command `tempo_before` clone and the end-of-command clip-binding reconcile; the new rows: clip-binding staleness (`mark_clip_bindings_stale`, BUG-6ompp (content tick re-hashes every clip)), `render_all` structure key, trigger-target key, tempo reconcile against the retained map, `graph_snapshot` cache keyed on the EmbeddedPresets stamp (these four are BUG-tsjs1 (re-key content work on part stamps)), `MappingRanges` range refresh (BUG-kn2cm (Ableton trim drag rebuilds every listener)); `embedded_presets_fingerprint` removed from the content side; audio reconcile keyed on the AudioSetup stamp; `ContentState.{stamps, values_seq, audio_directory_version, runtime}`; `capture_into` bumps `values_seq` only on change and the snapshot clone happens only then (BUG-p2t5o (push_state and the modulation snapshot copy run every frame), content half); `RuntimeSeqs` bumped at their sources; the change audit (`change-audit` feature, part fingerprints in `manifold-core`, exhaustive destructuring; "override present" is fingerprinted under Overrides separately from the graph's contents under Effects, so a structural graph edit declared `Overrides | Shapes` fails the audit); INV-1 (partial), INV-2, INV-8.
- **Gate (positive):** `content_reaction_budget` ceilings re-recorded down: idle ticks → zero `ClipBindingReconcile`, `ModSnapshotCopy`, `GeneratorStructureRefresh`; LED tick → zero reactions; trim tick → one `ListenerRangeRefresh`, zero `AbletonListenerRebuild`, zero renderer reloads; param slider tick → zero; every live-drag tick → zero reactions and zero `ProjectClone` (INV-17, INV-19); `content_mutate_publishes` green. Editing and app suites green with the change audit on. `MANIFOLD_RENDER_TRACE=1` live run on Liveschool while dragging LED brightness and an Ableton trim: no frame over 20 ms (DESIGN_DOC_STANDARD.md section 5 (Phase briefs), content-thread work gate). **Gate (negative):** `rg 'MutateProjectLive|MutateProjectPreview|let tempo_before' crates/` → 0; INV-2 rg.
- **Demo:** L3 for the counts; the render trace line for the content thread.
- **Performer gesture:** drag LED brightness hard during playback, then trim an Ableton range; the output must not hitch and the trace shows no reaction work beyond the range refresh.
- **Forbidden moves:** keeping `MutateProjectLive` "for speed" · a closure declaring `ALL` to dodge the audit · reactions left inside command arms · bumping `data_version` from `mutate` · keeping `data_version` as a key anywhere on the content side except publish and autosave.
- **Test scope:** `manifold-core`, `manifold-editing`, `manifold-playback`, `manifold-compositor`, `manifold-app` focused; clippy on those five.

### P3b — The content side doors close

- **Entry:** P3a landed. Re-derive the side doors: `rg -n 'project_mut\(\)|split_renderer_project_mut\(\)' crates/manifold-app/src crates/manifold-playback/src -g '*.rs'` (58 `project_mut` with tests at design time). A different count: list the new sites before touching any.
- **Read-back:** D13; `engine.rs` `project_mut` :462, `split_renderer_project_mut` :530, `TickResult.pending_gesture_commits` :103; `content_thread.rs` :700–730, :1730–1820 and :1990–2140; `transport_controller.rs`; `live_clip_manager.rs` `commit_live_clip` :745; `content_pipeline.rs` :2360–2370; the BUG-mmct0.5 (MIDI recording commits bypass EditingService) and BUG-7f4d (midi-percussion-import-commands-executed-outside-editing-service) descriptions. Restate D13 and INV-10.
- **Seam brief:** old `engine.project_mut() -> Option<&mut Project>` (public) → `pub(crate)`; new `engine.project_edit() -> Option<ProjectEdit<'_>>`; `EditingService` methods take `ProjectEdit`. Compiler-driven: make `project_mut` and `split_renderer_project_mut` `pub(crate)` first; every error is a side door. Kinds: (a) a value write (OSC router apply, Ableton bridge apply) → `mutate(Values)`; (b) a user-settings write (frame rate, OSC sync mode) → `mutate(Settings)`; (c) a clip or mapping write (detect, replan, clear triggers, unmap) → `mutate(Clips)` / `mutate(Mappings)`; (d) a structural write inside the tick (live clip commit, `ensure_layer_count`) → a command in `TickResult`, executed after the tick like `pending_gesture_commits`; (e) the render path's `ensure_sorted` → sorted at write time inside the tempo commands, `mutate(Settings)` and `tick_write(SessionTempoPoints | BaseTempo)`, render takes `split_renderer_project()`; (f) tests → `editing.mutate` or a command; (g) an audited tick write (followed BPM, clock authority, in-session tempo points, `recording_provenance`, the beat-0 tempo replace outside a session) → `tick_write(field, ..)` (D24). A site fitting no kind is an escalation.
- **Deliverables:** `ProjectEdit`; the migrations; every content-side write compares before it mutates; BPM, clock authority and in-session tempo-recorder writes become audited tick writes (compare first, no stamp, no publish, no reaction), session end stamps Settings once and skips the tempo reconcile, and `tempo_ramp_matches_today` is recorded from today's code before any change (D24, INV-22); the offline import paths execute through `EditingService` (closes BUG-7f4d (midi-percussion-import-commands-executed-outside-editing-service)); MIDI live record commits through `TickResult` (closes BUG-mmct0.5 (MIDI recording commits bypass EditingService)); `notify_external_change` deleted; the change audit around `PlaybackEngine::tick`; INV-10. Closes BUG-cg8gj (content thread writes the project outside EditingService).
- **Gate (positive):** a test records a MIDI take and asserts the clip arrives in the UI snapshot and undoes; a test runs a tempo-record session and asserts the Settings stamp moved; the change audit green around the tick across `manifold-app` suites. **Gate (negative):** INV-10 rg → 0; `rg 'notify_external_change' crates/` → 0.
- **Demo:** L3 — `content_reaction_budget` plus the MIDI-take test.
- **Performer gesture:** record a MIDI take during playback; the clip appears on the timeline and Cmd+Z removes it.
- **Forbidden moves:** a public `project_mut_for_x` escape hatch · `notify_external_change` under another name · a structural write left inside the tick "because it is internal" · `ChangeSet::ALL` on a side door.
- **Test scope:** `manifold-editing`, `manifold-playback`, `manifold-app` focused; clippy on those three.

### P4a — The projection pass moves into the shared seam (no behavior change)

- **Entry:** P3b landed. `rg -n 'needs_structural_sync' crates/manifold-app/src/ui_snapshot/script.rs` hits (the transcription to delete).
- **Read-back:** `app_render.rs` :240–420 (snapshot accept) and :2990–3260 (selection sync, structural sync, clip and lane resync); `ui_frame.rs`; UI_HARNESS_UNIFICATION_DESIGN.md section 4 (The shared seam). Restate D6 and that this phase moves code without changing a decision.
- **Deliverables:** one function in `ui_frame.rs` that runs the projection block exactly as `tick_and_render` does today, called by the app and the runner; the runner's own `needs_structural_sync` and its sync calls deleted, which ends the free first-gesture rebuild (closes BUG-imm (ui-flow-harness first-gesture free rebuild masks missing invalidation)); `AssertFresh` action, comparing the tree dump, `ImmediateInputs` and the pending toast queue (D10, INV-4), and the `ui-snap diff` fix that ignores generations (reviving UI_HARNESS_UNIFICATION's Deferred diff item, whose trigger this is); ceilings re-recorded (they rise: the runner now does the real per-frame work — record it as the true before, named in the commit).
- **Gate:** all existing flows exit 0; every work flow passes `AssertFresh` (today's code always rebuilds, so it must); demo L3; negative: `rg 'needs_structural_sync' crates/manifold-app/src/ui_snapshot/` → 0.
- **Forbidden moves:** changing any decision while moving it · a runner-only branch in the seam (vary on input presence only, the D3 hardening rule).
- **Test scope:** `manifold-app` with `--features ui-snapshot`; all flows.

### P4b — The planner core lands beside the old flags, with previews and the selection split

- **Entry:** P4a landed.
- **Read-back:** sections 2–5; `editing_host.rs`; `interaction_overlay.rs` drag paths (:2100–2260, :2440–2510, :2760–2850); `context.rs`; `ui_state.rs` :77–420 and `selection_identity` :392; `ui_root/mod.rs` `build` :719 and `rebuild_scroll_panels` :1050. Restate D5, D6, D8, D12, D14, the projection table, the forbidden registry.
- **Shape:** `plan_frame` lands complete for the projection table and runs first each frame. Region work still goes through suffix rebuild until P4e; the plan's `rebuild` mask maps to `rebuild_from(earliest)` meanwhile. `needs_structural_sync`, `structural()`, `needs_rebuild` and `ScrollDirty` all stay live after it. They can only add work, never hide it, so nothing goes stale while sites move over in P4d and P5a.
- **Deliverables:** `UiStamps` with the snapshot fold; `FrameInputs`, `plan_frame`, `apply_frame_plan`; `DispatchCtx::{mutate, execute}`; host committing methods stamp; `GesturePreview` and `TimelineEditingHost::set_preview`, with clip move/trim, region-move splits and every automation drag writing previews instead of stamped writes, the snapshot hold deleted, and the overview and mini timeline reading the preview (BUG-w3csc (clip and automation drags would rebuild per mouse move)); `UIState` selection fields private, `selection_identity`, `selection_visual` and `cursor` versions (D14); `push_state` split into its four pieces with the selection push and event drain on their inputs (D15); `drag_preview_moves_no_stamp` (INV-12); a `plan_frame` unit test per row of the projection table (pure, no GPU); `projection_input_matrix` (INV-16), which must pass against the section 4 table before this phase lands.
- **Gate:** `AssertFresh` passes in every flow; `plan_frame` tests green; `work-clip-drag`, `work-clip-trim` and `work-automation-point-drag` show zero `ProjectSync` and one `ClipProjection` or `LaneProjection` of the dragged item per frame; `work-cursor-nav` zero `InspectorSync`; `work-paused-cursor` passes `AssertFresh` with the cursor, band and highlight current while paused; `work-region-move` zero `ProjectSync` per move and no snapshot hold; `work-idle`, `work-playback` and `work-modifier` show zero projections; ceilings re-recorded; demo L3.
- **Performer gesture:** drag a clip across four bars during playback, then nudge the insert cursor with the arrow keys; neither costs a projection.
- **Forbidden moves:** a new flag under another name · a subscription or callback from model to panel · stamping a preview write · `ctx.mutate` with a non-`Clone` closure worked around by boxing (escalate) · re-adding a hash of project content as a key · deleting old sites in this phase.
- **Test scope:** `manifold-ui`, `manifold-app` focused; work flows locally.

### P4c — The remaining projections key on their inputs

- **Entry:** P4b landed.
- **Read-back:** section 4 projection table; `state_sync.rs` `push_state` :49; `dropdowns.rs` :630–650; `ui_root/mod.rs` `sync_driver_bpm` :411; `app_render.rs` :340–360, :450–480 and :590–630; `editor_bridge.rs` :1313–1440; `clip_chrome.rs` :715; `ui_state.rs` :135–330. Restate D15, D16, D19, D20 (UI half).
- **Deliverables:** panel `view_version`s with private fields and compare-before-bump setters; `UIState` automation lane fields private, one `automation_view` over mode visibility, order, hidden, pinned, chosen params, the selected point's lane and lane heights, no bump while lanes are hidden (D19); live push widened to every live-scalar display (drawers, handles, range bands, mapping curves, layer-header and dock gains, crossover) and keyed on `values` and the Values, Shapes and MappingRanges stamps, with those parts removed from every projection key (D22); chrome push keyed on its parts and `ChromeInputs`, BPM tenths and time digits included (D24) (BUG-p2t5o (push_state and the modulation snapshot copy run every frame), UI half); the editor card live push keyed the same way; `RuntimeSeqs` folded into `FrameInputs` and the inspector projection; `Arc` intake for `ableton_session` and `midi_device_names`; driver readout in place, scan keyed on Effects and the period key (BUG-ilntv (clock jitter forces full UI rebuilds)); device query keyed on `audio_directory_version`; editor projections keyed (`set_snapshot`, badge, viewport def, visible nodes, preview node, mini timeline); overview keyed on `playhead_px`; detect status in place; waveform requests keyed on Clips; `embedded_presets_fingerprint` and the `PresetFingerprint` kind deleted; a `plan_frame` unit test per new row; INV-15.
- **Gate:** `AssertFresh` in every flow; `work-idle` zero `ValuePush`; `work-clock-jitter` zero `ProjectSync`, `InspectorSync` and `FullBuild`; `work-slider-grab` zero `InspectorSync`; `work-editor-open` zero editor projections after the first frame; ceilings re-recorded; demo L3. **Negative:** `rg 'embedded_presets_fingerprint' crates/` → 0.
- **Performer gesture:** run the set on Link with the drummer's tempo drifting; touch three sliders with lanes hidden; the UI does no rebuild.
- **Forbidden moves:** a setter that bumps without comparing · keying the live push on time · a live scalar as a projection key · deleting old sites in this phase.
- **Test scope:** `manifold-ui`, `manifold-app` focused; work flows locally.

### P4d — `structural()` and `needs_structural_sync` die, in batches

- **Entry:** P4c landed. Re-run: `rg -F 'needs_structural_sync = true' crates -g '*.rs' | wc -l` (68), `rg -F 'DispatchResult::structural()' crates -g '*.rs' | wc -l` (165) and the local-execute count `rg 'execute\(&mut (self\.)?local_project|\.execute\(project\)|execute\(ctx\.project|execute\(&mut \*ctx' crates/manifold-app/src -g '*.rs' | wc -l` (100). A different count: list the new sites before touching any.
- **Seam brief:** four batches by site kind, each landing alone. (a) It sent a command or closure: local writes move to `ctx.execute`, `ctx.mutate` or the host committing method; return `handled()`. Examples: rack expand (`dispatch/params.rs` :123) → `ctx.mutate`; the text-commit arms (`app.rs` :1210–1544) → `ctx.execute`; scene skin edits → `ExecuteOnContent` (closes BUG-bo2mx (scene setup skin edits run against the UI snapshot)). (b) It changed panel view state: the setter already bumps the view version; `handled()`. Example: `MacrosCollapseToggle` (:194). (c) It changed timeline scroll or zoom: the planner sees the pixel value; `handled()`. Example: `TimelineScrollbarH` (`ui_bridge/transport.rs` :89). (d) It changed selection: the selection versions already move; delete. The last batch makes `DispatchCtx.project` `&Project`, and deletes the `needs_structural_sync` field and `DispatchResult::structural_change`. A site fitting no kind is an escalation.
- **Gate per batch:** `AssertFresh` in every flow; ceilings re-recorded down (batch c: `work-scrollbar-drag` zero project and inspector syncs; batch d: `work-select-click` one inspector sync). **Last batch negative:** `rg 'needs_structural_sync|structural_change' crates/` → 0; INV-11 rg.
- **Demo:** L3. **Performer gesture:** click between three clips quickly mid-playback; one inspector sync per click.
- **Forbidden moves:** converting a site whose kind the planner does not yet cover · deleting the field before the last batch · a fourth write helper. Half the sites converted is fine while the planner covers them; the field goes last.
- **Test scope:** `manifold-ui`, `manifold-app` focused; work flows locally.

### P4e — Per-region tree storage

- **Entry:** P4d landed. Re-run the call-site counts: `rg '\.index\(\)' crates/manifold-ui/src crates/manifold-app/src crates/manifold-ui-paint/src -g '*.rs' -g '!tree.rs' | wc -l` (70), `rg 'id_at\(' crates -g '*.rs' | wc -l` (66), `rg 'node_range|first_node' crates/manifold-ui/src/panels -g '*.rs' | wc -l` (148), `rg 'tree\.count\(\)' crates/manifold-ui/src crates/manifold-app/src -g '*.rs' | wc -l` (123). A different count: list the new sites first.
- **Read-back:** D7; section 3 `UITree` per-region storage and the hit-order rule; `tree.rs` whole; `node.rs` :255–300; `ui_root/mod.rs` `build` :719, `rebuild_scroll_panels` :1050, `panel_cache_info` :564; `ui_cache_manager.rs`; `chrome/diff.rs` :500–515; UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md D1 (region list) and D2 (tier order). Restate D7, INV-14.
- **Seam brief:** old `NodeId::index()` → `region()` + `slot()`; old `truncate_from(start)` → `clear_region(region)`; old `(start, end)` panel ranges over the whole tree → region-local `region_range`; old `PanelCacheInfo { node_start, node_end }` → `{ region, start, end }`. Compiler-driven: delete `index()` and `truncate_from` first. Kinds: (a) a panel range or captured `tree.count()` → region-local slot; (b) an index compare across panels → escalate (a cross-region index compare has no meaning); (c) `hit_test` and traversal → walk `paint_order`. `rebuild_scroll_panels`, `scroll_panels_start` and `viewport_panels_start` are deleted; `apply_frame_plan` clears and rebuilds exactly the regions in `plan.rebuild` and translates those in `plan.translate`.
- **Deliverables:** first, on the flat tree, the `paint_and_hit_order_match_full_build` golden (paint sequence and hit grid with dropdown, Ableton picker, card-drag ghost, toast, warmup, a hover tooltip and the text-input overlay open); then the arenas with tiered sub-regions, the atlas-free `Ghosts` arena for the card-drag ghost only (toasts stay in `Overlays`), and the site map (section 3); root salts seeded by region id; `clear_region`, `translate_region`; the hit-order rule; `root_minted` deleted; `PanelSlot` one per region; tests `clear_region_keeps_other_regions_live`, `hit_test_follows_region_paint_order`, `widget_ids_unique_across_regions` (INV-18), and a widget-id test proving salts are stable across a single-region rebuild (BUG-bluz6 (per-region UI tree storage)). The golden must still pass on arenas (INV-14).
- **Gate:** all flows exit 0 with `AssertFresh`; INV-14 golden and INV-18 green; `work-select-click` shows one `RegionRebuild` (inspector) and zero for layer headers, viewport and overlays; INV-14 tests and rg; demo L3.
- **Performer gesture:** select a clip with a long inspector mid-playback; only the inspector repaints.
- **Forbidden moves:** tail-append replacement · keeping `truncate_from` "for the chrome diff" · a compatibility `index()` shim · changing tier order.
- **Test scope:** `manifold-ui`, `manifold-ui-paint`, `manifold-app` focused; all flows.

### P5a — The remaining flags die

- **Entry:** P4e landed. Re-run counts: `needs_rebuild = true` (95), `scroll_dirty.` writes (28), host dirty calls (24), `automation_lanes_dirty` (1 set site), layer-bitmap invalidation calls (12) plus the drain.
- **Read-back:** section 4 region table; `ui_frame.rs` `apply_ui_frame_invalidations`; `ui_cache_manager.rs` :101–175; `input.rs` :380–460; `bitmap_renderer.rs` :175–265. Restate D7, D8, D18.
- **Seam brief:** delete `needs_rebuild`, `ScrollDirty`, `automation_lanes_dirty`, `mark_dirty`, `request_redraw`, `request_automation_lane_resync`, `invalidate_layer_bitmap`, `invalidate_all_layer_bitmaps` and `invalidate_layers` first, then `apply_ui_frame_invalidations`' flag branches and drag guards. Site kinds: view change (setter already bumps; delete the line); scroll (the planner already sees it; delete); tween poll (the tween's progress bumps the view version; delete); modifier keys (delete); import progress (delete; the header updates in place); browser search (`set_filter` bumps overlays; delete); hover (`redraw()`); split, inspector and dock resize (the rect input moves; delete); a panel holding a drag `NodeId` → hold `WidgetId` and re-resolve (D18). Old `invalidate_all`/`invalidate_scroll_panels`/`invalidate_inspector` → `invalidate_slot`.
- **Deliverables:** the deletions; per-region rects in `FrameInputs` and the region table's rebuild and translate columns live (BUG-m22n9 (split and resize drags rebuild every region)); drag targets by `WidgetId` and the guards deleted (BUG-in6m0 (rebuilds deferred during drags because panels hold raw NodeIds)); bitmap invalidation and the dead viewport fingerprints deleted, the overview keyed on Clips (BUG-972o1 (stale layer-bitmap invalidation and dead viewport fingerprints)); flow `drag-survives-rebuild`; INV-5, INV-6, INV-13.
- **Gate (positive):** ceilings re-recorded down: hover, automation hover, modifier, scrollbar drag → zero region rebuilds and zero slot invalidations beyond in-place updates, zero `GridBitmapRepaint` on hover; select click → inspector region only; browser search → overlays only; split drag → rebuild of layer headers and viewport only, translate of header and split handle. `AssertFresh` in every flow. **Gate (negative):** INV-6 rg → 0; INV-13 rg → 0; INV-5 test green.
- **Demo:** L3. **Performer gesture:** hover and scrub along an automation lane for five seconds; zero rebuilds.
- **Forbidden moves:** a flag "just for the tween" · a region rebuilding itself outside the plan · a drag guard kept "for safety".
- **Test scope:** `manifold-ui`, `manifold-ui-paint`, `manifold-app` focused; all flows.

### P5b — In-place motion and keyed chrome

- **Entry:** P5a landed.
- **Read-back:** `inspector/mod.rs` `drawer_anim_active` :528 and the in-place scroll path; chrome `View` and `diff.rs`; `viewport/render.rs` :855–1180; `layer_header.rs` :1295 and :2288; the split reset tween in `layout.rs`. Restate D7, D14, D19 and decided items 10 and 12.
- **Deliverables:** drawer and object-card tweens by reveal: content laid out once at final size, revealed by the animated drawer clip while cards below move in place (shaped like inspector in-place scroll); dock wheel scroll in place; chrome regions keyed on displayed values at display resolution (the clock keyed on its shown digits, not the beat float); the pooled ruler and zoom in place (BUG-3vs01 (horizontal scroll falls back to a viewport rebuild)); selection visuals in place in layer headers and viewport (D14); lane heights in place (BUG-veaap (automation-lane-resize-full-rebuild)); region height changes in place for layer headers and viewport; the split reset tween translates the header and split handle and resizes layer headers and viewport in place.
- **Gate:** `work-drawer-tween`, `work-dock-scroll`, `work-zoom`, `work-follow-scroll`, `work-lane-resize` and `work-split-drag` show zero region rebuilds; `work-select-click` zero layer-header and viewport rebuilds; `work-playback` shows no chrome layout except when shown digits change; `AssertFresh` at tween end.
- **Demo:** L3, plus a drawer filmstrip (the existing `inspector-drawer-filmstrip.json`) for Peter to look at. **Performer gesture:** open a modulation drawer mid-playback, then alt-scroll to zoom; neither costs a rebuild.
- **Forbidden moves:** keeping a per-frame rebuild for "just the last frame of the tween" · a ruler pool sized by the current zoom (one fixed size, `MAX_RULER_TICKS`).
- **Test scope:** `manifold-ui`, `manifold-app` focused; all flows.

### P5c — The other windows

- **Entry:** P5b landed. `rg -n 'fn rebuild_editor_inspector' crates/manifold-app/src/editor_bridge.rs` hits.
- **Read-back:** D17; `editor_bridge.rs` :13–30 and :1313–1800; `editor_frame.rs`; `composite_main_ui_frame` (`ui_frame.rs` :250); `perform_mode/render.rs`; EDITOR_WINDOW_UNIFICATION_DESIGN.md D5 (cacheless editor) and D6 (redraw keepalive). Restate D17, INV-3.
- **Deliverables:** `EditorRegion`, editor arenas, an editor `UICacheManager`, `plan_editor_frame` and its `apply`; `rebuild_editor_inspector` deleted; canvas redraw keyed on canvas inputs; live node previews as their own pass while visible; `sync_card_values` widened into the editor live push on the live-push inputs (D22) (BUG-36mjb (open graph editor rebuilds the whole inspector every vsync)); `PerformHudInputs` derived from `draw_cue_hud`'s arguments at pixel resolution, section-progress fill included, and draw-list reuse (BUG-1d6sh (perform-mode HUD rebuilds its text and lists every frame)). This phase revises EDITOR_WINDOW_UNIFICATION_DESIGN.md D5 (cacheless editor); that doc gets one pointer here.
- **Gate:** `work-editor-idle` and `work-perform-idle` all counters zero, `FrameAlloc` included; `work-editor-open` zero `EditorTreeBuild` after the first frame; `AssertFresh` on the editor window where the runner drives it. Demo: L3 where the runner reaches, else the live trace by the lead.
- **Performer gesture:** keep the graph editor open on the second screen through a full song; the main window's frame trace does not move when the editor opens.
- **Forbidden moves:** a "small tree" exemption from the planner · rebuilding the card lane on any snapshot.
- **Test scope:** `manifold-app` focused; all flows.

### P6 — Close-out

- **Entry:** P5c landed.
- **Deliverables:** `Command::touches` default removed; every command declares its set (compiler-driven: 204 impls, mostly one line each; re-run the count first); allocation residue removed until `work-idle`, `work-playback`, `work-editor-idle` and `work-perform-idle` show `FrameAlloc` zero on both threads (param-card name clone `param_card/render.rs` :2136, `push_state` churn, `present.rs` scratch, `check_auto_scroll`'s unused `duration_beats()`); final ceilings; one CLAUDE.md line ("UI work is decided by the frame planner; never add an invalidation flag — add an input. Spec: docs/UI_INVALIDATION_DESIGN.md"); supersession sweep (this doc's status, `rg` for `needs_rebuild`, `MutateProjectLive`, `structural()`, `notify_external_change`, `truncate_from` across `docs/` and memory, fix or tombstone each hit); `.claude/skills/content-thread/SKILL.md` updated for the one `MutateProject` and `ProjectEdit`.
- **Gate:** INV-1 complete (no default); the four idle flows all zeros including `FrameAlloc`; a `MANIFOLD_WORK_TRACE=1` live idle run on Liveschool prints nothing for 10 seconds.
- **Demo:** the live idle trace — L2.
- **Forbidden moves:** `ChangeSet::ALL` as a blanket answer for the 204 (each states its parts; `ALL` only for load-scale commands, named in the phase notes).
- **Test scope:** `manifold-editing`, `manifold-app` focused; all flows; nightly workspace sweep covers the rest.

After P6: the structural-sharing design (section 9, BUG-qzea6 (design structural sharing of project parts)).

Phasing completeness: every mechanism in sections 2–5 appears in exactly one phase deliverable above, or in section 9.

---

## 8. Decided — do not reopen

1. Parts and stamps at the gateway; `EditingService` owns content stamps; the UI owns its own. (D1, D5)
2. Commands declare `touches()`; no default after P6; the change audit checks every declaration. (D2, D10)
3. One `MutateProject` with a required change set, through `EditingService::mutate`. Live and Preview variants are gone. (D3)
4. Content reactions run once per drain pass from moved stamps, by one table. (D4)
5. One pure planner per window decides projections, rebuilds, translations, in-place updates and slot repaints. Pull, never push; no subscription registry. (D6)
6. **Revised 2026-10-14:** a region rebuilds alone, in its own arena; translate on move, rebuild on resize; in-place scroll, zoom, selection and motion. (D7) Was: region suffix rebuild in a fixed order.
7. **Revised 2026-10-14 (narrowed):** hover, modifiers, playhead and automation interaction state are not rebuild inputs; `redraw()` never touches the tree. The one exception is the selected point's lane identity, which the inspector shows and which folds into `automation_view`. (D8) Was: no exception.
8. One cached measurer per tree; the renderer measures through it. (D9)
9. Counts, not timings; allocations counted; ceilings move only by explicit record; missing inputs are found by the freshness check, never by adding flags. (D10, D11)
10. Drawers open by reveal: content is laid out once at its final size and revealed as the drawer slides open. (Peter, 2026-10-13)
11. **Revised 2026-10-14 (mechanism only):** an Ableton range follows live mid-drag; each content frame refreshes the cached ranges in place, and the listener set rebuilds only when the mapping set changed. (D20) Was: one listener rebuild per content frame. Peter's call (the range follows live) stands.
12. **Revised 2026-10-14 (reversed):** the split-reset tween translates the header and split handle and resizes layer headers and viewport in place; no other region moves (`layout.rs` :107–202). (D7) Was: every region rebuilds for its ~200 ms because every region's size moves — that premise was false.
13. Work flows run nightly through `scripts/trunk_health.py`, never in the landing gate; landing time is a priority. Each phase runs its own work flows locally as its gate. `content_reaction_budget` is ordinary nextest, scoped normally. (Peter, 2026-10-13)
14. End-game now: known gaps are designed in; a deferral survives only when it needs its own design, and then it names the committed end state. (Peter, 2026-10-14)
15. A drag is a preview; stamps move on commit. (D12)
16. Every project write goes through the gateway on both threads: `ProjectEdit` on content, three helpers and a read-only `ctx.project` on the UI. (D5, D13)
17. Selection splits into identity, visuals and cursor. (D14)
18. Live values and content runtime state carry sequences at the source. (D15, D16)
19. Every window and surface is on the planner: main, editor (regions and atlas), perform HUD. (D17)
20. No drag defers a rebuild; drag targets are `WidgetId`s. (D18)
21. View-state setters bump only on a visible change. (D19)
22. Clock and Ableton gestures update in place; `MappingRanges` is its own part. (D20)
23. Membership and live scalars never share a part; live drags declare Values or Shapes and pay no reaction and no projection. (D21)
24. `push_state` is four pieces with their own inputs: live push, selection push, event drain, chrome push. (D15) The insert cursor has a version. (D14, revised: was unversioned)
25. **Revised round 4:** the publish rule is today's (`data_version`) plus content-originated `mutate`; a UI `MutateProject` does not publish. (D23) Round 3's baseline and mirroring are retracted.
26. The projection table is derived and checked by the input matrix; `AssertFresh` covers immediate-pass inputs and toasts. (D10)
27. Region roots are salted by region id; an arena holds tiered sub-regions; paint and hit order equal today's. (D7)
28. A region move's splits live in the preview; the mid-drag snapshot hold is deleted. (D12)
29. Every dependency is rebuild or in place; live scalars (Values, Shapes, MappingRanges) are shown only through in-place pushes, in every display and both windows. (D22)
30. The design changes only what a finding names; where today's behaviour is correct, it stays. Snapshot clone cost belongs to the structural-sharing design. (Peter, 2026-10-14)
31. Tick writes of BPM, clock authority and in-session tempo points stay where they are, as audited tick writes; every content-side write compares before it mutates. (D24)
32. A live scalar a consumer caches gets an in-place, allocation-free reaction row, and every live scalar is listed with its consumers. (D25)
33. Ghost-tier content lives in an atlas-free arena. (D7)

---

## 9. Deferred

| Item | Why it stays out of this design | Revive when / committed end state |
|---|---|---|
| Structural sharing of project parts (`Arc` per part) to drop snapshot deep clones | Every edit deep-clones the whole project on both threads (`content_thread.rs` `send_state` :1220, `app_render.rs` :287). It needs live values moved out of `Project` first (the content thread writes them every tick, `content_thread.rs` :1208–1210), or every tick copies, and it touches every model access. That is its own design. | **Committed end state:** an `Arc` per `Part`, cloned only for parts whose stamp moved; stamps from this design are its change signal. Its design starts after P6 (BUG-qzea6 (design structural sharing of project parts)). |
| Per-layer or per-effect stamps | With drags as previews (D12), an edit pays one projection on commit, not one per frame. Per-layer stamps would only cut the cost of that one commit on a large project, and would double the stamp bookkeeping in every command. | A commit gesture on Liveschool shows `ProjectSync` or `ClipProjection` as the largest count in its work flow. |
| Projection output equality as a second gate (skip the rebuild when a projection's output equals the last) | Stamps and the selection split (D14) remove the reruns that equality would catch; `selection_identity` is already the one place where output equality pays. A second gate is a second freshness path to keep correct. | A work flow shows region rebuilds whose `AssertFresh` dump is unchanged from the previous frame. |
