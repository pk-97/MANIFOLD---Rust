//! `--script <file.json>` runner (`UI_AUTOMATION_DESIGN.md` section 6, P2 section 9). A
//! JSON array of `AutomationAction`s (`manifold_ui::automation`) executed in
//! order against a scene fixture. Artifacts land in
//! `target/ui-snapshots/<scene>/run-<script-stem>/`: a numbered PNG/dump per
//! `Snapshot`/`Dump` step, plus `result.json`, plus `filmstrip.png` when any
//! `Step` action advanced frames (D9a). Exit 0 only if every step succeeded
//! (D6, D10).
//!
//! Gesture synthesis (D4) follows `interact.rs`'s `select_layer` precedent —
//! `UIRoot::pointer_event` → `ui.process_events()` (the SAME per-frame call
//! the live app makes, `app_render.rs:869`) → real panel dispatch — extended
//! to cover the **drag** vertical path: `process_events()` stashes
//! tracks-area events into `viewport_events` (exactly as it does for the live
//! app, because `UIRoot` can't hold `&mut dyn TimelineEditingHost` — see
//! `ui_root.rs:1417`'s comment) for a caller to route through
//! `InteractionOverlay` + a `TimelineEditingHost`. `app_render.rs` supplies
//! `AppEditingHost` wrapping the live `Application`; this driver builds its
//! OWN `AppEditingHost` wrapping the fixture's `SceneData.project` plus
//! scratch `ContentState`/`CursorManager`/etc. and a `crossbeam_channel`
//! whose receiver it holds and never drains. `ContentCommand::send` only
//! logs on a channel DISCONNECT (`content_command.rs`) — an alive-but-idle
//! channel is silently fine — and the actual clip mutation
//! (`set_clip_start_beat`/`commit_command_batch`) happens directly on
//! `SceneData.project`, which is what the re-dump reads. No live content
//! thread is needed for the real drag path to run headlessly.
//!
//! Generic widget clicks route through the full, REAL `ui_bridge::dispatch`
//! against driver-owned scratch state (see `Runner`'s snapshot fields).
//! Determinism (D7) holds because prefs are `UserPrefs::in_memory()` —
//! empty, host-independent, never the user's file.
//!
//! Every frame the runner advances runs `Runner::live_frame`: the live App's
//! per-frame order, through `crate::ui_frame::apply_frame_plan` (projection,
//! rebuild decision, `push_state`, drag-time re-projection) and, when pixels
//! are read, `composite_main_ui_frame`. The runner holds no projection or
//! rebuild logic of its own; it differs from the App only in what inputs are
//! present (no content thread, no drag polling, no OS window). A flow that
//! clicks a card's graph-editor button opens the editor window through
//! `editor_frame::open_editor_workspace`; every later frame builds its tree
//! through `editor_frame::run_editor_frame`, the functions the App calls.
//! `Dump`, `Assert` and `AssertFresh` read that tree too. A `Menu` step for
//! View > Perform enters perform mode through `perform_mode::frame::enter`;
//! every later frame is a perform frame through the same `perform_mode::frame`
//! functions `tick_perform_mode` calls, drawn into the runner's offscreen, and
//! `AssertFresh` compares what the perform screen shows. After a `Focus` step
//! for the graph editor, pointer steps go into the editor window through
//! `editor_input.rs`, the seam `window_input`'s editor handlers call, and its
//! card lane, canvas edits and mapping drawer drain and dispatch through the
//! same seam `app_render.rs` calls. The editor's canvas draws the watched
//! graph: the runner absorbs the watch commands the App sends and publishes
//! the snapshot into its content state through `graph_snapshot.rs`, as the
//! content thread does.

use std::path::PathBuf;
use std::time::Duration;

use manifold_core::LayerId;
use manifold_editing::command::{CompositeCommand};
use manifold_editing::undo::UndoRedoManager;
use manifold_gpu::GpuDevice;
use manifold_ui_paint::ui_cache_manager::UICacheManager;
use manifold_ui_paint::ui_renderer::UIRenderer;
use manifold_ui::automation::{
    self, AssertCheck, AutomationAction, AutomationTarget, Gesture, MatchInfo,
};
use manifold_ui::automation_hit_tester::AutomationHitTargets;
use manifold_ui::clip_hit_tester::ClipHitTargets;
use manifold_ui::hit_targets::HitTargets;
use manifold_ui::input::{Key, PointerAction, UIEvent};
use manifold_ui::interaction_overlay::InteractionOverlay;
use manifold_ui::node::{Rect, Vec2};
use manifold_ui::panels::PanelAction;

use super::composite_resources::{composite_frame, CompositeResources};
use super::fixtures::SceneData;
use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::editing_host::AppEditingHost;
use crate::ui_root::UIRoot;

/// Fixed per-`Step` delta — the fixture's project runs at 60 fps default
/// (`CLAUDE.md`); the driver's clock only advances on `Step` (D7), never a
/// wall-clock read. A REAL `std::thread::sleep(DT)` (P2, D9a) still happens
/// per stepped frame, because `InspectorCompositePanel::update`'s drawer
/// tween derives its own dt from `Instant::now()`, not from this clock —
/// same honest tradeoff `cache_path_full_render` (P0) already accepted.
const DT: f32 = 1.0 / 60.0;

#[derive(serde::Serialize)]
struct StepResult {
    index: usize,
    action: String,
    status: &'static str,
    detail: String,
    artifact: Option<String>,
}

/// How one flow ended. `Done.code` is the exit code a solo `--script` process
/// gives (0 all steps passed, 1 a step failed, 2 setup error) and `tail` is
/// the last line it wrote to stderr — the two things `run_ui_flows.py`
/// reports, so a batched flow reports exactly what a solo one would.
enum FlowOutcome {
    Done { code: i32, tail: String },
    /// Batch only: this scene would build against a preset registry a fresh
    /// process never has (see [`run_batch`]). Nothing was written.
    Requeue(String),
}

fn setup_error(message: String) -> FlowOutcome {
    eprintln!("{message}");
    let tail = message.lines().last().unwrap_or_default().to_string();
    FlowOutcome::Done { code: 2, tail }
}

/// Run `script_path`'s `AutomationAction` array against `scene`. Exits the
/// process: 0 if every step succeeded, 1 otherwise (D6/D10 — no partial
/// pass), 2 on a setup error.
pub fn run(scene: &str, script_path: &str, record_work: bool) {
    let mut device = None;
    match run_flow(scene, script_path, &mut device, &mut super::fixtures::FixtureCache::default(), None, record_work) {
        FlowOutcome::Done { code: 0, .. } => {}
        FlowOutcome::Done { code, .. } => std::process::exit(code),
        FlowOutcome::Requeue(_) => unreachable!("requeue needs a batch context"),
    }
}

/// Prefix of `run_batch`'s two stdout records per flow (`begin`, then the
/// result); everything else on stdout is the flows' own output.
const BATCH_RECORD: &str = "@@ui-snap-batch@@";

/// `ui-snap batch <scene> <script> [<scene> <script> ...]`: run many flows in
/// one process, sharing only the `GpuDevice` (and with it the one GPU-lock
/// acquisition and process start-up) and the parsed GLBs of imported-glTF
/// scenes ([`super::fixtures::FixtureCache`]). Every flow still builds its own
/// fixture `Project` from scratch, `UIRoot`, `UIRenderer`, cache and offscreen exactly as a solo run
/// does, and writes the same artifacts.
///
/// Each flow prints `@@ui-snap-batch@@ {"begin": index}` on stdout, then one
/// result record carrying the request index and either the solo-equivalent
/// `code`/`tail`/`seconds` or `rerun` with a reason. A flow that panics gets
/// a `rerun` record and ends the batch (exit 101) so nothing runs on top of
/// half-torn state; a `begin` with no result (the process died) is the
/// caller's to rerun solo too.
///
/// The one process-global input a fixture reads is the preset registry.
/// Scenes that install a project preset overlay replace it wholesale, so they
/// see the same registry whatever ran before; every other scene must see the
/// registry a fresh process starts with. So installing scenes run last
/// ([`super::fixtures::installs_preset_overlay`]), and a non-installing scene
/// that finds the catalog generation already moved gets `rerun` instead of
/// running against the wrong registry — a stale list costs speed, never
/// correctness.
pub fn run_batch(flows: &[(String, String)], record_work: bool) {
    let mut order: Vec<usize> = (0..flows.len()).collect();
    order.sort_by_key(|&i| super::fixtures::installs_preset_overlay(&flows[i].0));
    let fresh_generation = manifold_node_engine::load::preset_loader::catalog_generation();
    let mut device = None;
    let mut fixtures = super::fixtures::FixtureCache::default();
    for index in order {
        let (scene, script) = &flows[index];
        println!("{BATCH_RECORD} {}", serde_json::json!({"begin": index}));
        let start = std::time::Instant::now();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_flow(scene, script, &mut device, &mut fixtures, Some(fresh_generation), record_work)
        }));
        let record = match &outcome {
            Ok(FlowOutcome::Done { code, tail }) => serde_json::json!({
                "index": index, "code": code, "tail": tail,
                "seconds": start.elapsed().as_secs_f64(),
            }),
            Ok(FlowOutcome::Requeue(reason)) => serde_json::json!({"index": index, "rerun": reason}),
            Err(_) => serde_json::json!({"index": index, "rerun": "panicked"}),
        };
        println!("{BATCH_RECORD} {record}");
        if outcome.is_err() {
            std::process::exit(101);
        }
    }
}

/// One flow, solo or batched. `device` is created on first use, after the
/// fixture builds (where a solo run always created it); `batch_generation`
/// is the catalog generation a fresh process had, `None` when solo.
fn run_flow(
    scene: &str,
    script_path: &str,
    device: &mut Option<GpuDevice>,
    fixtures: &mut super::fixtures::FixtureCache,
    batch_generation: Option<u64>,
    record_work: bool,
) -> FlowOutcome {
    let generation_before = manifold_node_engine::load::preset_loader::catalog_generation();
    let Some(mut data) = super::fixtures::build_cached(scene, fixtures) else {
        return setup_error(format!(
            "ui-snap --script: unknown scene '{scene}' (known: timeline, states, inspector, dmxcard, \
             paramsteps, scrollshrink, hairlineclips, automation, selectionclips, gltfscene, \
             gltfanimscene, envmod, rtquality)"
        ));
    };
    if let Some(fresh) = batch_generation {
        let installed = manifold_node_engine::load::preset_loader::catalog_generation() != generation_before;
        if !installed && generation_before != fresh {
            return FlowOutcome::Requeue(format!(
                "scene '{scene}' built against a preset registry an earlier flow changed"
            ));
        }
    }
    let script_text = match std::fs::read_to_string(script_path) {
        Ok(t) => t,
        Err(e) => return setup_error(format!("ui-snap --script: can't read '{script_path}': {e}")),
    };
    let actions: Vec<AutomationAction> = match serde_json::from_str(&script_text) {
        Ok(a) => a,
        Err(e) => {
            return setup_error(format!("ui-snap --script: '{script_path}' doesn't parse: {e}"))
        }
    };

    let stem = std::path::Path::new(script_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("script");
    let out_dir = PathBuf::from("target/ui-snapshots").join(scene).join(format!("run-{stem}"));
    std::fs::create_dir_all(&out_dir).expect("create run output dir");

    let zoom_ppb = super::zoom_ppb_for_scene(scene);
    let mut ui = UIRoot::new();
    ui.resize(super::LOGICAL_W, super::LOGICAL_H);
    if scene == "inspector"
        || scene == "audiovisualizers"
        || scene == "modifiergroup"
        || scene == "bug060"
        || scene == "bug060heavy"
        || scene == "paramsteps"
        || scene == "gltfscene"
        || scene == "mushroomscene"
        || scene == "physicsscene"
        || scene == "physicsboxes"
        || scene == "newscene"
        // The Scene tab lives in the inspector (SCENE_SURFACES D16).
        || scene == "fluidauthoring"
        || scene == "scene-setup"
        || scene == "waterdambreak"
        || scene == "waterdambreakmatter"
        || scene == "oceanscene"
        || scene == "gltfanimscene"
        || scene == "bug047"
        || scene == "envmod"
        || scene == "dmxcard"
        // A real show's layer card is what a script on it inspects.
        || scene.starts_with("project:")
    {
        ui.layout.inspector_width = 600.0;
        ui.layout.timeline_split_ratio = 0.6;
    } else {
        ui.layout.inspector_width = 0.0;
        ui.layout.timeline_split_ratio = 0.93;
    }
    // `audiosends` is the Audio Setup dock's flow-testing scene (P4/D8 gain-
    // reset hygiene flow): the panel isn't reachable from a flow's `Click`
    // gesture (the header opens it through the perform-entry menu, which
    // renders outside the `UITree` selector surface) — so, script-mode only,
    // pre-open it the same way `interact.rs`'s `open:audio_setup` verb does,
    // and seed real crossovers/range so the scope isn't "dark". Mirrors the
    // `paramsteps`/`gltfscene` pre-armed-state pattern above (BUG-073: a
    // script harness has no per-frame tick to drive a reveal tween, so state
    // that needs to already be settled is constructed, not clicked into).
    if matches!(scene, "audiosends" | "audiosendsnone" | "audiosendsone" | "audiosendsdefault") {
        ui.audio_setup_panel.open();
        ui.layout.audio_setup_width = manifold_ui::color::DEFAULT_AUDIO_SETUP_WIDTH;
        ui.audio_setup_panel.set_scope_bands(250.0, 2000.0, 10.0, 22_000.0);
    }
    // `rtquality` is the RT Quality panel's flow-testing scene: the panel
    // opens from the MANIFOLD menu / Settings popup, both outside the UITree
    // selector surface — so, script-mode only, pre-open it (same call the
    // live dispatch makes), then the flow's clicks drive the real
    // dropdown → ChangeRtQualityCommand → state-sync path.
    if scene == "rtquality" {
        ui.rt_quality_panel.open();
        ui.mark_overlay_dirty();
    }
    super::sync_build(&mut ui, &data, zoom_ppb);

    // P2 (D3): ONE persistent cache + composited offscreen for the whole
    // script run, seeded with a full self-clearing composite — mirrors
    // `cache_path_full_render`'s own "Frame 0". Every later step updates
    // this SAME state through `Runner`'s seam calls; a `Snapshot` only ever
    // reads it back (see `Runner::write_png`).
    let tex_w = (super::LOGICAL_W * super::SCALE) as u32;
    let tex_h = (super::LOGICAL_H * super::SCALE) as u32;
    let device = device.get_or_insert_with(|| GpuDevice::new_queued("ui-snap script"));
    // The seed composite is deferred like every other one: the first
    // `Snapshot` paints it (the cache starts fully invalidated).
    let mut render = RenderState::new(device, tex_w, tex_h);

    let mut runner = Runner::new();
    runner.work.record = record_work;
    runner.work.flow = stem.to_string();
    // The fixture's values are what the content thread captured last.
    runner.values_capture.capture_into(&data.project);
    // BUG-296: `resolve_effect_target` falls back to `timeline.layers.first()`
    // when `Runner.active_layer` is None, silently mutating the wrong layer's
    // effect chain for structural PanelActions dispatched before any
    // `LayerClicked` gesture. Seed it from the fixture's actual active layer
    // (`data.active`) so a script's first structural dispatch (e.g.
    // `EffectReorder`) targets the layer the display is actually showing.
    runner.active_layer = data
        .active
        .and_then(|i| data.project.timeline.layers.get(i).map(|l| l.layer_id.clone()));
    // `sync_build` already projected this state: the planner starts from it.
    let automation_draw_mode = data.selection.automation_draw_mode;
    let seed = crate::ui_frame::gather_frame_inputs(
        &mut runner.planner,
        &ui,
        &mut data.selection,
        &runner.active_layer,
        &runner.edits,
        &data.content,
        crate::frame_plan::ChromeSource { project: &data.project, is_dirty: false, project_path: None, automation_draw_mode },
    );
    runner.planner.plan(seed);
    // The App's first frame after a load moves every part; `sync_build` is
    // the fixture's build, not that frame, so run it once here.
    runner.edits.stamps.project_replaced();
    runner.live_frame(&mut ui, &mut data, &mut render, false, true);
    let mut results = Vec::with_capacity(actions.len());
    let mut ok = true;

    for (index, action) in actions.iter().enumerate() {
        // The main window is not drawn in perform mode.
        if reads_composite(action) && !runner.perform.active {
            render.flush_composite(&mut ui);
        }
        let outcome = runner.step(&mut ui, &mut data, index, action, &out_dir, &mut render);
        let failed = outcome.status == "fail";
        harness_work(&mut runner.work.excluded, || results.push(outcome));
        if failed {
            ok = false;
            break;
        }
    }

    // D9a: a contact sheet of every frame a `Step` action advanced.
    if !runner.filmstrip.is_empty() {
        let cols = (runner.filmstrip.len() as u32).clamp(1, 4);
        let filmstrip_path = out_dir.join("filmstrip.png");
        super::render::save_filmstrip_png(&runner.filmstrip, tex_w, tex_h, cols, &filmstrip_path);
        println!(
            "ui-snap --script: wrote {} ({} tile(s))",
            filmstrip_path.display(),
            runner.filmstrip.len()
        );
    }

    let result_path = out_dir.join("result.json");
    std::fs::write(&result_path, serde_json::to_string_pretty(&results).expect("serialize result"))
        .expect("write result.json");
    println!("ui-snap --script: wrote {}", result_path.display());
    for r in &results {
        println!("  [{:>2}] {:<5} {} — {}", r.index, r.status, r.action, r.detail);
    }

    if ok {
        FlowOutcome::Done { code: 0, tail: String::new() }
    } else {
        let tail = format!("ui-snap --script: FAILED — see {}", result_path.display());
        eprintln!("{tail}");
        FlowOutcome::Done { code: 1, tail }
    }
}

/// Persistent GPU render state the [`Runner`] drives through the shared seam
/// (P2, D3) — ONE `UICacheManager` + composited offscreen for the whole
/// script run, updated after every dirtying action exactly as the live App's
/// own cache is, not rebuilt from scratch per `Snapshot` (the drift P2
/// exists to kill). Kept OUT of `Runner` itself (rather than a field on it)
/// because `Runner::new()` is also used standalone by [`click_by_text`],
/// which never renders a pixel — building a `GpuDevice`/atlas there would be
/// pure waste. `run_flow` constructs this once per flow and threads it
/// through every `Runner::step` call; only the device outlives the flow.
struct RenderState<'d> {
    device: &'d GpuDevice,
    ui_renderer: UIRenderer,
    cache: UICacheManager,
    composite: CompositeResources,
    tex_w: u32,
    tex_h: u32,
    gate: CompositeGate,
}

/// Composites are deferred until something reads pixels. No flow asserts on
/// pixels and the UI never reads GPU state back, so the only readers are
/// `write_png` (a `Snapshot`) and the filmstrip tile of each `Step` frame. Painting is incremental over dirty panels and
/// the tree's dirty flags accumulate until a composite clears them, so one
/// composite at the `Snapshot` paints the same atlas as compositing after
/// every frame in between.
#[derive(Default)]
struct CompositeGate {
    pending: bool,
    composites: usize,
}

impl CompositeGate {
    fn mark(&mut self) {
        self.pending = true;
    }

    /// True when a composite is owed; counts it.
    fn take(&mut self) -> bool {
        let owed = std::mem::take(&mut self.pending);
        self.composites += usize::from(owed);
        owed
    }

    /// A `Step` frame owes its filmstrip tile a composite (D9a), so it pays
    /// at once. True when the caller must composite.
    fn step_frame(&mut self) -> bool {
        self.mark();
        self.take()
    }
}

/// The only step type whose behaviour depends on a composited frame.
fn reads_composite(action: &AutomationAction) -> bool {
    matches!(action, AutomationAction::Snapshot)
}

impl<'d> RenderState<'d> {
    /// D8: scale factor 1.0 always, at the fixture's logical size (matches
    /// every other headless caller of the seam).
    fn new(device: &'d GpuDevice, tex_w: u32, tex_h: u32) -> Self {
        let ui_renderer = UIRenderer::new(device, manifold_compositor::presentation::UI_FORMAT);
        let mut cache = UICacheManager::new(manifold_compositor::presentation::UI_FORMAT, 1.0);
        cache.set_scale_factor(1.0);
        cache.ensure_atlas(device, tex_w, tex_h);
        cache.invalidate_all();
        let composite = CompositeResources::new(device, tex_w, tex_h);
        let mut gate = CompositeGate::default();
        gate.mark();
        Self { device, ui_renderer, cache, composite, tex_w, tex_h, gate }
    }

    /// Composite now if any frame since the last composite changed the UI.
    fn flush_composite(&mut self, ui: &mut UIRoot) {
        if self.gate.take() {
            self.composite_now(ui);
        }
    }

    fn composite_now(&mut self, ui: &mut UIRoot) {
        composite_frame(self.device, &mut self.ui_renderer, &mut self.cache, ui, &self.composite, 1.0);
    }

    /// Read back the offscreen as last composited — raw linear UI bytes (the
    /// seam's own format). Callers flush first (`flush_composite`).
    fn readback_linear(&self) -> Vec<u8> {
        super::render::readback(self.device, &self.composite.offscreen, self.tex_w, self.tex_h)
    }
}

/// Scratch state the headless driver owns across the whole script run —
/// everything `AppEditingHost`/`InteractionOverlay` need that a live
/// `Application` would otherwise supply. Constructed once; reused per step
/// so overlay drag-mode state (irrelevant between atomic gestures, but kept
/// for parity with the live per-frame object) and the deterministic clock
/// persist naturally.
struct Runner {
    overlay: InteractionOverlay,
    content_tx: crossbeam_channel::Sender<ContentCommand>,
    // Held so the channel stays connected — `ContentCommand::send` only logs
    // on disconnect; never drained (no content thread exists headlessly).
    _content_rx: crossbeam_channel::Receiver<ContentCommand>,
    content_state: ContentState,
    cursor_manager: manifold_ui::cursors::CursorManager,
    active_layer: Option<LayerId>,
    /// A repaint request (UI_INVALIDATION D8), set where the App sets its
    /// `offscreen_dirty`. The runner paints every frame, so nothing reads it.
    redraw: bool,
    /// The frame planner, as the App owns its own (D6).
    planner: crate::frame_plan::FramePlanner,
    /// The content thread's part stamps, modelled: every absorbed command
    /// bumps its parts here, and the next frame folds them into the UI's
    /// stamps as the App folds an accepted snapshot (D5).
    content_stamps: manifold_core::change::PartStamps,
    transport_cache: crate::ui_bridge::TransportDisplayCache,
    /// Commands recorded at the end of a frame: the App sees them as a new
    /// snapshot next frame.
    echo_pending: bool,
    /// `AssertFresh` asked for its reference frame: the next frame runs every
    /// projection (D10). Test oracle only; the App has no such input.
    fresh_reference: bool,
    edits: crate::gesture_preview::UiEdits,
    // BUG-198: the harness had no undo stack at all — commands executed
    // straight against `data.project` via `AppEditingHost`/`ui_bridge` and
    // were never recorded anywhere, so a headless `Key Z` step was a pure
    // no-op that still reported "ok" (silently proving nothing). Real
    // `UndoRedoManager`, same 200-command cap as the content thread's,
    // fed by draining `_content_rx` after every step (see
    // `record_executed_commands`) — every `ContentCommand::Execute`/
    // `ExecuteBatch` sent through the seam this run already applied
    // directly to `data.project` gets `record()`-ed here (NOT re-executed;
    // `AppEditingHost`/`ui_bridge` always mutate the project synchronously
    // before sending, matching the live app's `local_project` pattern —
    // see the module doc). `Key`'s Cmd+Z / Cmd+Shift+Z arm dispatches
    // against this directly.
    undo: UndoRedoManager,
    clock: f32,
    modifiers: manifold_ui::input::Modifiers,
    // Scratch state `ui_bridge::dispatch` threads through the live
    // `Application` — owned here so panel actions run the REAL bridge
    // headlessly (drag snapshots stay None between atomic gestures; prefs
    // are in-memory, never the user's file — D7 determinism holds).
    user_prefs: crate::user_prefs::UserPrefs,
    scrub: crate::ui_bridge::ScrubState,
    // D9a: every composited frame a `Step` action advanced, in order —
    // assembled into one contact-sheet PNG at the end of `run` when
    // non-empty. D9b: the most
    // recent `Pointer` gesture's synthesized
    // point(s) (center, interpolated drag path, drag end) — consumed
    // (drained) by the next `Snapshot`, which stamps a crosshair at each
    // one on the readback COPY only.
    filmstrip: Vec<Vec<u8>>,
    last_gesture_points: Vec<Vec2>,
    // Persistent headless engine: scripted controls use production clip sync.
    modulation_engine: manifold_playback::engine::PlaybackEngine,
    /// The content thread's value capture (D15): after each engine tick it
    /// compares the live values and moves `values_seq` only when one changed,
    /// so the tick's writes reach the widgets through the live push, as the
    /// App's value snapshot does.
    values_capture: crate::content_state::ModulationSnapshot,
    work: WorkWindow,
    /// The headless stand-in for the graph editor's viewport: the layer it
    /// shows and its navigation camera, moved by `ViewportOrbit`.
    viewport: Option<(LayerId, manifold_node_engine::scene::viewport_camera::ViewportCamera)>,
    /// The graph editor window, once a flow opens it.
    editor: Option<RunnerEditor>,
    /// Perform mode, as the App holds it.
    perform: crate::perform_mode::PerformModeState,
    /// The inline text session, as the App holds it (`text_session.rs`).
    text_input: crate::text_input::TextInputState,
    /// Where pointer and key steps go (`AutomationAction::Focus`).
    focus: manifold_ui::automation::FlowWindow,
    /// The graph the editor shows and the catalog default its edits seed
    /// from, as the App's `watched_graph_target` and `watched_catalog_default`.
    watched: Option<manifold_core::GraphTarget>,
    catalog_default: Option<manifold_core::effect_graph_def::EffectGraphDef>,
    /// The App's `editor_graph`: the reset pill's input cache.
    editor_graph: crate::editor_graph_cache::EditorGraphCache,
    /// The card lane's mapping drawer, as the App's `editor_mapping_popover`.
    editor_mapping: crate::mapping_popover::MappingPopover,
    /// What the content thread holds for the canvas: the watched target the
    /// absorbed watch commands set, and its snapshot cache.
    content_watched: Option<manifold_core::GraphTarget>,
    graph_snapshot: Option<crate::graph_snapshot::CachedGraphSnapshot>,
    gizmo: preview_gizmo::RunnerGizmo,
}

mod preview_gizmo;

/// The graph editor window as the App holds it (`graph_editor`,
/// `graph_canvas`, `graph_editor_panel`), opened and built through the
/// `editor_frame.rs` seams. No window exists, so no offscreen, and the window
/// keeps the size a first open gives it.
struct RunnerEditor {
    ws: crate::workspace::Workspace,
    canvas: manifold_ui::graph_canvas::GraphCanvas,
    panel: manifold_ui::panels::graph_editor::GraphEditorPanel,
    logical_w: u32,
    logical_h: u32,
    /// The renderer snapshot's UI translation, as the App's `editor_ui_graph`.
    ui_graph: crate::editor_bridge::EditorUiGraph,
    /// The preview column's smart-preview toggle: its intent registry, the
    /// button the last build drew and the value it holds.
    intents: manifold_ui::intent::IntentRegistry<manifold_ui::GraphEditCommand>,
    smart_preview_toggle: Option<manifold_ui::node::NodeId>,
    node_preview_normalize: bool,
}

impl RunnerEditor {
    /// The tree dump `Dump` and `AssertFresh` read for this window, with the surfaces a
    /// pointer step resolves against (canvas, mapping drawer).
    fn dump(&self, mapping: &crate::mapping_popover::MappingPopover) -> serde_json::Value {
        let layout = crate::editor_frame::EditorLayout::compute(&self.ws.dock, self.logical_w, self.logical_h, None);
        let canvas = manifold_ui::graph_canvas::GraphCanvasTargets { canvas: &self.canvas, viewport: layout.canvas_rect() };
        let surfaces: Vec<&dyn HitTargets> = vec![&canvas, mapping];
        super::dump::dump_tree_ex(&self.ws.ui_root.tree, &surfaces)
    }
}

/// The count gate's state for one flow (UI_INVALIDATION_DESIGN.md D10, D11).
#[derive(Default)]
struct WorkWindow {
    /// Set by the first `ResetWork`. Inside a window a `Step` frame skips its
    /// filmstrip composite, which is harness work the App never does.
    open: bool,
    start: manifold_core::work_count::WorkSnapshot,
    /// Work this thread did that the live App does on the content thread (the
    /// harness's own engine tick), removed from every reading.
    excluded: manifold_core::work_count::WorkSnapshot,
    /// `--record-work`: `AssertWork` writes the observed counts as ceilings.
    record: bool,
    /// The flow's file stem: its key in the ceilings file.
    flow: String,
}

/// Runs the harness's own bookkeeping (step labels, result strings) and
/// removes what it counted from the work window: the App has no steps.
fn harness_work<T>(excluded: &mut manifold_core::work_count::WorkSnapshot, f: impl FnOnce() -> T) -> T {
    let start = manifold_core::work_count::thread_work_snapshot();
    let out = f();
    *excluded = excluded.plus(&manifold_core::work_count::thread_work_snapshot().since(&start));
    out
}

/// Committed ceilings, read and written relative to the repo root (flows run
/// with the repo root as cwd).
pub(crate) const WORK_CEILINGS_PATH: &str = "scripts/ui-flows/work_ceilings.json";

impl Runner {
    fn new() -> Self {
        let (content_tx, _content_rx) = crossbeam_channel::bounded(64);
        let mut modulation_engine = manifold_playback::engine::PlaybackEngine::new(Vec::new());
        modulation_engine.initialize(manifold_core::project::Project::default());
        Self {
            overlay: InteractionOverlay::new(manifold_ui::color::CLIP_VERTICAL_PAD),
            content_tx,
            _content_rx,
            content_state: ContentState::default(),
            cursor_manager: manifold_ui::cursors::CursorManager::default(),
            active_layer: None,
            redraw: false,
            planner: Default::default(),
            content_stamps: Default::default(),
            transport_cache: crate::ui_bridge::TransportDisplayCache::new(),
            echo_pending: false,
            fresh_reference: false,
            edits: Default::default(),
            undo: UndoRedoManager::new(),
            clock: 0.0,
            modifiers: manifold_ui::input::Modifiers::NONE,
            user_prefs: crate::user_prefs::UserPrefs::in_memory(),
            scrub: crate::ui_bridge::ScrubState::default(),
            filmstrip: Vec::new(),
            last_gesture_points: Vec::new(),
            modulation_engine,
            values_capture: crate::content_state::ModulationSnapshot::empty(),
            work: WorkWindow::default(),
            viewport: None,
            editor: None,
            perform: crate::perform_mode::PerformModeState::new(),
            text_input: crate::text_input::TextInputState::new(),
            focus: Default::default(),
            watched: None,
            catalog_default: None,
            editor_graph: Default::default(),
            editor_mapping: crate::mapping_popover::MappingPopover::new(),
            content_watched: None,
            graph_snapshot: None,
            gizmo: Default::default(),
        }
    }

    /// `open_graph_editor` without a window: the same workspace, canvas and
    /// projected inspector column. A second open is a no-op here; live it
    /// only brings the window to the front.
    fn open_editor(&mut self, data: &SceneData) -> bool {
        if self.editor.is_some() {
            return false;
        }
        let active = self.active_layer.as_ref().and_then(|id| data.project.timeline.find_layer_index_by_id(id));
        let (ws, canvas) = crate::editor_frame::open_editor_workspace(
            None,
            &data.project,
            active,
            &data.selection,
            &data.content,
        );
        let (logical_w, logical_h) =
            crate::editor_frame::first_open_logical_size(f64::from(super::LOGICAL_W), f64::from(super::LOGICAL_H));
        self.editor = Some(RunnerEditor {
            ws,
            canvas,
            panel: manifold_ui::panels::graph_editor::GraphEditorPanel::new(),
            logical_w,
            logical_h,
            ui_graph: None,
            intents: manifold_ui::intent::IntentRegistry::new(),
            smart_preview_toggle: None,
            node_preview_normalize: false,
        });
        true
    }

    /// The App's `watch_graph_target`: point the editor at `target` through
    /// the shared watch.
    fn watch(&mut self, data: &SceneData, target: manifold_core::GraphTarget) {
        if crate::editor_bridge::watch_editor_graph(
            &data.project,
            &target,
            &mut self.watched,
            &mut self.catalog_default,
            &mut self.editor_mapping,
            self.editor.as_mut().map(|ed| &mut ed.canvas),
            Some(&self.content_tx),
        ) {
            self.editor_graph.catalog_default_changed();
        }
    }

    /// The watch each open-editor action makes in the App's action loop.
    fn watch_for_open(&mut self, ui: &UIRoot, data: &mut SceneData, action: &manifold_ui::RootAction) {
        use manifold_core::GraphTarget;
        use manifold_ui::RootAction;
        match action {
            RootAction::PreviewSceneModifierObject(layer, modifier_id, _) => self.watch(data, GraphTarget::SceneModifier {
                owner: Box::new(GraphTarget::Generator(layer.clone())),
                modifier_id: modifier_id.clone(),
            }),
            RootAction::OpenGraphTarget(target) => self.watch(data, crate::editing_host::to_graph_target(target)),
            RootAction::SceneSetupOpenGraphEditor(layer) => self.watch(data, GraphTarget::Generator(layer.clone())),
            RootAction::OpenGeneratorGraphEditor => {
                if let Some(layer) = self.active_layer.clone() {
                    self.watch(data, GraphTarget::Generator(layer));
                }
            }
            RootAction::OpenGraphEditor(ei) => {
                let tab = ui.inspector.last_effect_tab();
                match crate::editor_bridge::effect_card_id(tab, &data.project, self.active_layer.as_ref(), &data.selection, *ei) {
                    Some(eid) => self.watch(data, GraphTarget::Effect(eid)),
                    None => {
                        self.watched = None;
                        self.catalog_default = None;
                        self.editor_graph.catalog_default_changed();
                    }
                }
            }
            _ => {}
        }
    }

    /// The App's `open_card_mapping`: resolve the drawer the card row asks
    /// for, watch its graph, open it.
    fn open_card_mapping(
        &mut self,
        data: &SceneData,
        target: &manifold_ui::GraphParamTarget,
        param_id: &str,
        anchor_node_id: manifold_ui::node::NodeId,
    ) {
        let Some(ed) = self.editor.as_ref() else { return };
        let clip = manifold_ui::graph_canvas::Rect::new(0.0, 0.0, ed.logical_w as f32, ed.logical_h as f32);
        let Some(open) = crate::editor_bridge::resolve_card_mapping(
            &ed.ws.ui_root, &data.project, &self.active_layer, &data.selection, self.watched.as_ref(),
            target, param_id, anchor_node_id, clip,
        ) else {
            return;
        };
        if self.watched.as_ref() != Some(&open.owner) {
            self.watch(data, open.owner.clone());
        }
        open.open(&mut self.editor_mapping);
    }

    /// The editor window's frame, after the main window's as in the App
    /// (`present_graph_editor_window` runs after `present_all_windows`). No
    /// content thread publishes a node preview or a pipeline size, so the
    /// node monitor shows no image and the monitors take the default aspect.
    fn editor_frame(&mut self, data: &SceneData, fresh: bool) {
        let Some(ed) = self.editor.as_mut() else { return };
        // The canvas takes the watched graph as the App's tick pushes it; no
        // pipeline publishes an output aspect.
        let graph = crate::editor_bridge::editor_ui_graph(&mut ed.ui_graph, &data.content, self.watched.as_ref(), &data.project);
        if let Some(graph) = graph {
            let has_mod = self.editor_graph.has_graph_mod(
                &data.project,
                self.watched.as_ref(),
                self.catalog_default.as_ref(),
                &self.edits.stamps.parts(),
            );
            crate::editor_frame::feed_canvas(
                &mut ed.canvas,
                &mut ed.ws.editor_frame.canvas,
                &graph,
                None,
                DT * 1000.0,
                &data.content.live_node_params,
                has_mod,
            );
        }
        let layout = crate::editor_frame::EditorLayout::compute(&ed.ws.dock, ed.logical_w, ed.logical_h, None);
        let active = self.active_layer.as_ref().and_then(|id| data.project.timeline.find_layer_index_by_id(id));
        ed.smart_preview_toggle = crate::editor_frame::run_editor_frame(
            &mut ed.ws.ui_root,
            &mut ed.ws.editor_frame,
            &ed.panel,
            Some(&mut ed.canvas),
            &layout,
            crate::editor_frame::EditorFrameInputs {
                project: &data.project,
                content_state: &data.content,
                active_layer: active,
                live: self.planner.last.live(),
                dt: DT,
                show_image: false,
            },
            fresh,
        )
        .smart_preview_toggle;
    }

    #[allow(clippy::too_many_arguments)]
    fn step(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        index: usize,
        action: &AutomationAction,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> StepResult {
        let action_desc = harness_work(&mut self.work.excluded, || format!("{action:?}"));
        // In perform mode window input goes to the perform screen
        // (`perform_mode/input.rs`), never the frozen main window.
        if self.perform.active {
            match action {
                AutomationAction::Pointer { target, gesture } => {
                    return self.perform_pointer(ui, data, index, action_desc, target, gesture, out_dir, render);
                }
                AutomationAction::Key { .. } if crate::perform_mode::frame::takes_keys(&self.perform) => {
                    self.live_frame(ui, data, render, false, true);
                    return StepResult {
                        index,
                        action: action_desc,
                        status: "ok",
                        detail: "dropped: perform mode takes every key".into(),
                        artifact: None,
                    };
                }
                AutomationAction::ScrollTo { .. }
                | AutomationAction::TrackWheel { .. }
                | AutomationAction::ResizeDrag { .. }
                | AutomationAction::Modifiers { .. } => {
                    return self.fail(index, action_desc, ui, out_dir, "perform mode drops this input; only the exit button responds".into());
                }
                _ => {}
            }
        }
        // An active text session takes every key, as `input_keyboard` gives it them.
        if self.text_input.active && matches!(action, AutomationAction::Key { .. } | AutomationAction::Text { .. }) {
            return self.type_into_session(ui, data, index, action_desc, action, out_dir, render);
        }
        // After `Focus` on the graph editor, pointer and key steps go into
        // that window, as the OS routes input to the window under the mouse.
        let editor_outcome = if self.focus == manifold_ui::automation::FlowWindow::GraphEditor {
            self.editor_step(ui, data, index, &action_desc, action, out_dir, render)
        } else {
            None
        };
        let outcome = if let Some(outcome) = editor_outcome { outcome } else { match action {
            AutomationAction::Step { frames } => {
                // Each stepped frame ticks the engine, then runs one live
                // frame. Outside a work window tweens run on the wall clock
                // (a real sleep per frame) and each frame is a filmstrip tile
                // (D9a).
                for _ in 0..*frames {
                    self.clock += DT;
                    std::thread::sleep(Duration::from_secs_f32(DT));
                    let engine_work = manifold_core::work_count::thread_work_snapshot();
                    // Editing owns the fixture between steps. Temporarily lend
                    // that same project to the engine, preserving runtime state
                    // and avoiding a second scheduler in the UI harness.
                    assert!(self.modulation_engine.swap_project(&mut data.project), "initialized engine");
                    self.modulation_engine.locate(manifold_core::Seconds(self.clock as f64));
                    let result = self.modulation_engine.tick(manifold_playback::engine::TickContext {
                        frames: 1,
                        realtime_now: manifold_core::Seconds(self.clock as f64),
                        ..Default::default()
                    });
                    self.modulation_engine.reclaim_tick_result(result);
                    // This runner renders UI only and has no scene consumers.
                    // Consume the frame so Fire controls cannot fill a retained
                    // delivery queue that this harness never renders.
                    self.modulation_engine.with_trigger_pulses(|_, _, _| ());
                    assert!(self.modulation_engine.swap_project(&mut data.project), "initialized engine");
                    if self.values_capture.capture_into(&data.project) {
                        data.content.values_seq += 1;
                    }
                    // The live App ticks the engine on the content thread.
                    let engine_work = manifold_core::work_count::thread_work_snapshot().since(&engine_work);
                    self.work.excluded = self.work.excluded.plus(&engine_work);
                    // Modulated values reach the widgets through `push_state`,
                    // as the App's value snapshot does.
                    let tile = !self.work.open;
                    self.live_frame(ui, data, render, false, !tile);
                    if !tile {
                        continue;
                    }
                    // A perform frame composites itself.
                    if !self.perform.active && render.gate.step_frame() {
                        render.composite_now(ui);
                    }
                    self.filmstrip.push(render.readback_linear());
                }
                let clock = self.clock;
                let detail = harness_work(&mut self.work.excluded, || format!("clock -> {clock:.3}s ({frames} filmstrip tile(s))"));
                StepResult { index, action: action_desc, status: "ok", detail, artifact: None }
            }
            AutomationAction::Dump => {
                let path = out_dir.join(format!("{index:02}.tree.json"));
                self.write_dump(ui, &path);
                StepResult {
                    index,
                    action: action_desc,
                    status: "ok",
                    detail: "dumped".into(),
                    artifact: Some(path.display().to_string()),
                }
            }
            AutomationAction::Snapshot => {
                let path = out_dir.join(format!("{index:02}.png"));
                self.write_png(ui, data, render, &path);
                StepResult {
                    index,
                    action: action_desc,
                    status: "ok",
                    detail: "snapshot written".into(),
                    artifact: Some(path.display().to_string()),
                }
            }
            AutomationAction::SaveReloadProject => {
                let path = out_dir.join("round-trip.manifold");
                let saved = manifold_io::saver::save_project(&mut data.project, &path, None, false);
                let reloaded = saved.map_err(|error| error.to_string()).and_then(|()|
                    manifold_io::loader::load_project(&path).map_err(|error| error.to_string()));
                match reloaded {
                    Ok(project) => {
                        data.project = project;
                        self.undo = UndoRedoManager::new();
                        self.edits.stamps.project_replaced();
                        self.live_frame(ui, data, render, false, true);
                        StepResult { index, action: action_desc, status: "ok",
                            detail: "saved and reloaded through project IO".into(), artifact: Some(path.display().to_string()) }
                    }
                    Err(error) => self.fail(index, action_desc, ui, out_dir, error),
                }
            }
            AutomationAction::Key { key, modifiers } => {
                self.modifiers = *modifiers;
                ui.input.set_modifiers(*modifiers);
                // BUG-198: `UIRoot::key_event` only resolves text-focused
                // widgets — every OTHER modifier-bearing key silently
                // no-op'd through it and still reported "ok", which is the
                // actual bug (a script asserting an undo/redo/shortcut
                // "passed" when nothing happened). Cmd+Z / Cmd+Shift+Z are
                // the one pair with a real headless seam (`self.undo`,
                // above); every other modifier combo fails loudly instead
                // of falling through, mirroring the `Text` arm's fail.
                // Plain unmodified keys keep the existing `key_event` path.
                if modifiers.is_command_only() && *key == Key::Z {
                    // As `EditingService::undo`: what the undo wrote.
                    let undone = self.undo.undo(&mut data.project);
                    if undone {
                        self.content_stamps.bump(self.undo.last_written());
                    }
                    self.live_frame(ui, data, render, false, true);
                    StepResult {
                        index,
                        action: action_desc,
                        status: "ok",
                        detail: format!("undo -> {undone} (undo_count={})", self.undo.undo_count()),
                        artifact: None,
                    }
                } else if modifiers.is_command_shift() && *key == Key::Z {
                    let redone = self.undo.redo(&mut data.project);
                    if redone {
                        self.content_stamps.bump(self.undo.last_written());
                    }
                    self.live_frame(ui, data, render, false, true);
                    StepResult {
                        index,
                        action: action_desc,
                        status: "ok",
                        detail: format!("redo -> {redone} (redo_count={})", self.undo.redo_count()),
                        artifact: None,
                    }
                } else if !modifiers.is_none() {
                    // Real shortcut path: queue the KeyDown through the same
                    // `UIRoot::key_event` the live app forwards to, then drain
                    // through the real dispatch. BUG-198's rule holds via the
                    // action count: a combo nothing handled fails loudly
                    // instead of reporting a vacuous "ok". Needs a focused
                    // widget (click something first), same as the live app.
                    ui.key_event(*key, *modifiers);
                    let handled = self.drain_and_dispatch(ui, data);
                    if handled == 0 {
                        self.fail(
                            index,
                            action_desc,
                            ui,
                            out_dir,
                            format!(
                                "modifier-bearing Key {key:?} ({modifiers:?}) produced no \
                                 panel action — either no widget is focused (click one \
                                 first) or nothing handles this combo; see BUG-198"
                            ),
                        )
                    } else {
                        self.live_frame(ui, data, render, false, true);
                        StepResult {
                            index,
                            action: action_desc,
                            status: "ok",
                            detail: format!("key {key:?} ({modifiers:?}) -> {handled} action(s)"),
                            artifact: None,
                        }
                    }
                } else {
                    ui.key_event(*key, *modifiers);
                    // Was `let _ = ui.process_events()` — key-triggered panel
                    // actions were resolved and then dropped on the floor (the
                    // same dormant seam as the pre-2026-07-07 apply_panel_actions).
                    self.drain_and_dispatch(ui, data);
                    self.live_frame(ui, data, render, false, true);
                    StepResult {
                        index,
                        action: action_desc,
                        status: "ok",
                        detail: format!("key {key:?}"),
                        artifact: None,
                    }
                }
            }
            // An active session took `Text` above; without one nothing types.
            AutomationAction::Text { .. } => {
                self.fail(index, action_desc, ui, out_dir, "no text field is focused: open one first (click a search bar, double-click a value)".into())
            }
            AutomationAction::Pointer { target, gesture } => {
                self.pointer(ui, data, index, action_desc, target, gesture, out_dir, render)
            }
            AutomationAction::Assert { selector, check } => {
                self.assert(ui, index, action_desc, selector, check, out_dir)
            }
            AutomationAction::ScrollTo { target } => {
                self.scroll_to(ui, data, index, action_desc, target, out_dir, render)
            }
            AutomationAction::SetParam { param, value } => {
                self.set_param(ui, data, index, action_desc, param, *value, render)
            }
            AutomationAction::SetSliderRange { param, range } => match flow_value_ref(data, &|ei| crate::editor_bridge::effect_card_id(ui.inspector.last_effect_tab(), &data.project, self.active_layer.as_ref(), &data.selection, ei), param) {
                Ok((manifold_ui::panels::ValueRef::Param(target, id), _)) => {
                    let action = PanelAction::Params(manifold_ui::panels::ParamsAction::SetSliderRange(target, id, *range));
                    self.apply_panel_actions(ui, data, &[action]);
                    self.live_frame(ui, data, render, false, true);
                    StepResult { index, action: action_desc, status: "ok", detail: format!("slider range {range:?} through dispatch"), artifact: None }
                }
                Ok(_) => StepResult { index, action: action_desc, status: "fail", detail: "SetSliderRange needs a card param".into(), artifact: None },
                Err(e) => StepResult { index, action: action_desc, status: "fail", detail: e, artifact: None },
            },
            AutomationAction::ScrubDrag { param, from, to, steps } => {
                self.scrub_drag(ui, data, index, action_desc, param, (*from, *to), *steps, out_dir, render)
            }
            AutomationAction::PlayingStep { frames, bpm, wobble } => {
                self.playing_step(ui, data, *frames, *bpm, *wobble, render);
                // The step's own label is harness work, as `action_desc` is.
                let beat = data.content.current_beat.0;
                let detail = harness_work(&mut self.work.excluded, || {
                    format!("{frames} playing frame(s) at {bpm} bpm, beat -> {beat:.3}")
                });
                StepResult { index, action: action_desc, status: "ok", detail, artifact: None }
            }
            AutomationAction::Modifiers { modifiers } => {
                // `window_input::input_modifiers`: store and forward.
                self.modifiers = *modifiers;
                self.overlay.set_modifiers(*modifiers);
                ui.input.set_modifiers(*modifiers);
                self.live_frame(ui, data, render, false, true);
                StepResult { index, action: action_desc, status: "ok", detail: "modifiers set".into(), artifact: None }
            }
            AutomationAction::ResizeDrag { handle, from, to, steps } => {
                self.resize_drag(ui, data, *handle, (*from, *to), *steps, render);
                StepResult { index, action: action_desc, status: "ok", detail: format!("{steps} resize move(s)"), artifact: None }
            }
            AutomationAction::TrackWheel { at, delta_y, steps } => {
                for _ in 0..*steps {
                    self.track_wheel(ui, *at, *delta_y);
                    self.live_frame(ui, data, render, false, true);
                }
                StepResult { index, action: action_desc, status: "ok", detail: format!("{steps} wheel notch(es)"), artifact: None }
            }
            AutomationAction::ViewportOrbit { dx, dy } => match ui.scene_setup_panel.live_layer_id().cloned() {
                Some(layer) => {
                    let camera = match self.viewport.take() {
                        Some((shown, camera)) if shown == layer => camera,
                        _ => Default::default(),
                    };
                    let mut camera = camera;
                    crate::viewport_input::apply(
                        &mut camera,
                        crate::viewport_input::ViewportGesture::Orbit { dx: *dx, dy: *dy },
                        &crate::viewport_input::ViewportInputSensitivity::default(),
                    );
                    self.viewport = Some((layer, camera));
                    StepResult { index, action: action_desc, status: "ok", detail: format!("viewport yaw {:.3} pitch {:.3}", camera.yaw, camera.pitch), artifact: None }
                }
                None => self.fail(index, action_desc, ui, out_dir, "ViewportOrbit needs the Scene tab open on a scene layer".into()),
            },
            // The App's menu drain turns View > Perform into this action.
            AutomationAction::Menu { item: manifold_ui::automation::MenuItem::Perform } => {
                let action = PanelAction::Project(manifold_ui::panels::ProjectAction::EnterPerformMode);
                self.apply_panel_actions(ui, data, &[action]);
                self.live_frame(ui, data, render, false, true);
                let detail = if self.perform.active { "perform mode entered" } else { "perform mode not entered" };
                StepResult { index, action: action_desc, status: if self.perform.active { "ok" } else { "fail" }, detail: detail.into(), artifact: None }
            }
            AutomationAction::ResetWork => {
                // Settle whatever setup left pending, so the window counts
                // only the gesture that follows.
                self.work.open = true;
                self.live_frame(ui, data, render, false, true);
                let result = StepResult { index, action: action_desc, status: "ok", detail: "work window started".into(), artifact: None };
                self.work.start = manifold_core::work_count::thread_work_snapshot();
                self.work.excluded = manifold_core::work_count::WorkSnapshot::default();
                result
            }
            AutomationAction::AssertWork { window } => self.assert_work(ui, index, action_desc, window, out_dir),
            AutomationAction::AssertFresh => self.assert_fresh(ui, data, render, index, action_desc, out_dir),
            AutomationAction::Focus { window } => {
                if *window == manifold_ui::automation::FlowWindow::GraphEditor && self.editor.is_none() {
                    self.fail(index, action_desc, ui, out_dir, "Focus GraphEditor needs the editor open first".into())
                } else {
                    self.focus = *window;
                    StepResult { index, action: action_desc, status: "ok", detail: format!("input goes to {window:?}"), artifact: None }
                }
            }
            AutomationAction::ExternalEdit => {
                use manifold_core::change::{ChangeSet, Part};
                self.content_stamps.bump(ChangeSet::of(&[Part::Layers, Part::Effects]));
                self.live_frame(ui, data, render, false, true);
                StepResult { index, action: action_desc, status: "ok", detail: "layers and effects stamps moved".into(), artifact: None }
            }
        } };
        // BUG-198: whatever this step just sent over `content_tx` (via
        // `AppEditingHost`/`ui_bridge::dispatch`) has already been applied
        // directly to `data.project` — record it into the harness's own
        // undo stack so `Key`'s Cmd+Z/Cmd+Shift+Z arm has something real to
        // act on. Every step drains unconditionally (most steps sent
        // nothing, so this is a no-op `try_recv` miss).
        if self.absorb_commands(ui, data) || std::mem::take(&mut self.echo_pending) {
            self.live_frame(ui, data, render, false, true);
        }
        outcome
    }

    /// The overlay half of a cursor move (`window_input` cursor-moved): every
    /// move, pressed or not, runs `InteractionOverlay::on_pointer_move` with the
    /// App's own repaint request, unless a modal blocks background input.
    fn overlay_hover(&mut self, ui: &mut UIRoot, data: &mut SceneData, pos: Vec2) {
        if ui.background_input_blocked() {
            if data.selection.automation_feedback.take().is_some() {
                self.redraw = true;
            }
            return;
        }
        let mut host = AppEditingHost::new(
            crate::ui_bridge::ProjectRef::new(&mut data.project, &mut self.edits, &self.content_tx),
            &self.content_tx,
            &self.content_state,
            &mut self.cursor_manager,
            &mut self.active_layer,
            &mut self.redraw,
        );
        self.overlay.on_pointer_move(pos, &mut host, &mut data.selection, &ui.viewport);
    }

    /// Drain and record what the step sent (see `record_executed_commands`);
    /// when that changed the project, apply the selection update; the commands
    /// bumped `content_stamps`, which the next frame folds. True when it did.
    fn absorb_commands(&mut self, ui: &mut UIRoot, data: &mut SceneData) -> bool {
        if !self.record_executed_commands(data) {
            return false;
        }
        crate::edit_selection::apply_update(ui, &data.project, data.content.edit_selection_update.as_deref(), &mut data.selection, &mut self.active_layer);
        data.active = self.active_layer.as_ref().and_then(|id| data.project.timeline.find_layer_index_by_id(id));
        true
    }

    /// One UI frame in the live App's order (`app_render.rs` `tick_and_render`):
    /// fold the content echo, plan, then `ui_frame::apply_frame_plan`
    /// (the same function the App calls), then the tween polls and the CPU half
    /// of the grid and overview repaints. No forced zoom. With `settle_tweens`
    /// a tween settles in the frame after it starts, as under reduced motion,
    /// so counts and asserts do not depend on machine speed; a `Step` frame
    /// outside a work window lets tweens run on the wall clock instead.
    ///
    /// A structural tween armed by this frame's own rebuild (a new card's spawn
    /// pop) gets the one settling frame here: the next step may run no frame
    /// (Snapshot, Assert, an in-band ScrollTo), and its resolves and hit tests
    /// would read the half-scaled card.
    fn live_frame(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        render: &mut RenderState,
        scrolled_in_place: bool,
        settle_tweens: bool,
    ) {
        self.live_frame_once(ui, data, render, scrolled_in_place, settle_tweens);
        if settle_tweens && !self.perform.active {
            let editor_structural = self.editor.as_ref().is_some_and(|ed| ed.ws.ui_root.inspector.structural_motion());
            if ui.inspector.structural_motion() || ui.scene_setup_panel.cards_structural_motion() || editor_structural {
                self.live_frame_once(ui, data, render, false, true);
            }
        }
    }

    fn live_frame_once(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        render: &mut RenderState,
        scrolled_in_place: bool,
        settle_tweens: bool,
    ) {
        // Perform mode replaces the whole UI frame, as `tick_and_render`
        // hands off to `tick_perform_mode`: the main and editor trees stay
        // frozen and nothing projects.
        if self.perform.active {
            self.perform_frame(ui, data, render);
            return;
        }
        crate::text_session::pump_main_window(&mut self.text_input, ui);
        // The watched graph's snapshot, as the content thread publishes it in
        // every state push (`graph_snapshot.rs`). Content-thread work, so it
        // stays out of the UI's counts.
        let before = manifold_core::work_count::thread_work_snapshot();
        let (target, snapshot) = crate::graph_snapshot::active_graph(
            &mut self.graph_snapshot,
            Some(&data.project),
            self.content_watched.as_ref(),
            self.content_stamps.get(manifold_core::change::Part::EmbeddedPresets),
        );
        data.content.active_graph_target = target;
        data.content.active_graph_snapshot = snapshot;
        self.gizmo.report(ui, data);
        let content_work = manifold_core::work_count::thread_work_snapshot().since(&before);
        self.work.excluded = self.work.excluded.plus(&content_work);
        self.gizmo.present(ui, data, &self.edits.stamps.parts());
        // The snapshot echo of what the last frame sent, folded as the App
        // folds an accepted snapshot.
        self.edits.stamps.fold_snapshot(&self.content_stamps);
        // Settled drawers move in place; a settled structural tween (collapse,
        // spawn, delete) moves its panel's view version, so this frame's plan
        // rebuilds the settled layout.
        if settle_tweens {
            ui.inspector.skip_to_settled(&mut ui.tree);
            ui.scene_setup_panel.skip_cards_to_settled(&mut ui.tree);
            if let Some(ed) = self.editor.as_mut() {
                let ed_ui = &mut ed.ws.ui_root;
                ed_ui.inspector.skip_to_settled(&mut ed_ui.tree);
            }
        }
        crate::ui_bridge::intake_overlay_state(ui, &data.content);
        crate::ui_bridge::check_auto_scroll(ui, &data.content);
        let automation_draw_mode = data.selection.automation_draw_mode;
        let inputs = crate::frame_plan::FrameInputs {
            gizmo_view: self.gizmo.host.version(),
            ..crate::ui_frame::gather_frame_inputs(
                &mut self.planner,
                ui,
                &mut data.selection,
                &self.active_layer,
                &self.edits,
                &data.content,
                crate::frame_plan::ChromeSource {
                    project: &data.project,
                    is_dirty: false,
                    project_path: None,
                    automation_draw_mode,
                },
            )
        };
        let mut plan = self.planner.plan(inputs);
        let fresh = std::mem::take(&mut self.fresh_reference);
        if fresh {
            plan = plan.with_every_projection();
        }
        let changes = crate::ui_frame::ProjectionChanges { scrolled_in_place };
        let overlay = &self.overlay;
        crate::ui_frame::apply_frame_plan(
            ui,
            self.editor.as_mut().map(|ed| &mut ed.ws.ui_root),
            Some(&mut render.cache),
            &mut data.project,
            &mut data.selection,
            &mut self.active_layer,
            &mut self.redraw,
            &plan,
            changes,
            crate::ui_frame::PushInputs {
                content_state: &data.content,
                is_dirty: false,
                project_path: None,
                transport_cache: &mut self.transport_cache,
            },
            &mut self.edits,
            |_, _, _, _, _, _| {
                use manifold_ui::interaction_overlay::DragMode;
                matches!(overlay.drag_mode(), DragMode::Move | DragMode::TrimLeft | DragMode::TrimRight)
            },
        );
        data.active = self.active_layer.as_ref().and_then(|id| data.project.timeline.find_layer_index_by_id(id));
        ui.update();
        ui.viewport.repaint_overview();
        self.editor_frame(data, fresh);
        render.gate.mark();
        // What this frame sent reaches the App's next frame as a content
        // snapshot; record it now (a long gesture sends a command per move
        // and the harness channel is bounded) and re-project next frame.
        self.echo_pending |= self.absorb_commands(ui, data);
    }

    /// `tick_perform_mode` without a window: the same intake, HUD derivation
    /// and composite, at the fixture's logical size into the runner's
    /// offscreen. No output window to poll, no preview bridge, no drawable.
    fn perform_frame(&mut self, ui: &mut UIRoot, data: &SceneData, render: &mut RenderState) {
        use crate::perform_mode::frame;
        // A fresh reference redraws the HUD from its inputs.
        let force = std::mem::take(&mut self.fresh_reference);
        let layout = frame::begin_perform_frame(
            &mut self.perform,
            ui,
            &data.content,
            super::LOGICAL_W as u32,
            super::LOGICAL_H as u32,
        );
        let inputs = frame::PerformHudInputs::derive(
            &self.perform,
            &data.content,
            ui.ableton_session.as_deref(),
            self.edits.stamps.parts().key(frame::HUD_PROJECT_PARTS),
            &layout,
            frame::preview_aspect(None),
        );
        if !frame::hud_moved(&self.perform, &inputs, force) {
            return;
        }
        let session = ui.ableton_session.clone();
        let hud = frame::PerformHud::derive(&data.content, session.as_deref(), &data.project);
        let mut encoder = render.device.create_encoder("Perform Frame");
        frame::composite_perform_frame(
            render.device,
            &mut encoder,
            Some(&mut render.ui_renderer),
            ui.tree.measurer(),
            &layout,
            &hud,
            self.perform.exit_button_hover,
            &render.composite.offscreen,
            f64::from(super::SCALE),
        );
        encoder.commit();
        self.perform.shown = Some(inputs);
    }

    /// Window input in perform mode, as `perform_mode/input.rs` takes it: the
    /// exit button follows hover and a release over it asks to leave; every
    /// other press is dropped. Then the exit `about_to_wait` applies, and one
    /// frame.
    #[allow(clippy::too_many_arguments)]
    fn perform_pointer(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        index: usize,
        action_desc: String,
        target: &AutomationTarget,
        gesture: &Gesture,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> StepResult {
        use crate::perform_mode::frame;
        let rect = match self.resolve(ui, target) {
            Ok(r) => r,
            Err(e) => return self.fail(index, action_desc, ui, out_dir, e),
        };
        let center = Vec2::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
        self.last_gesture_points.clear();
        self.last_gesture_points.push(center);
        match gesture {
            Gesture::Hover | Gesture::Press { .. } => {
                frame::pointer_moved(&mut self.perform, center);
            }
            Gesture::Release => frame::pointer_released(&mut self.perform, center),
            Gesture::Click { .. } => {
                frame::pointer_moved(&mut self.perform, center);
                frame::pointer_released(&mut self.perform, center);
            }
            _ => {
                return self.fail(index, action_desc, ui, out_dir, "perform mode takes hover, press, release and click".into());
            }
        }
        let exited = frame::take_pending_exit(&mut self.perform, ui, Some(&mut render.cache));
        if exited {
            self.redraw = true;
        }
        self.live_frame(ui, data, render, false, true);
        let detail = if exited {
            "perform mode exited".to_string()
        } else {
            format!("perform input at ({:.1},{:.1}), exit hover {}", center.x, center.y, self.perform.exit_button_hover)
        };
        StepResult { index, action: action_desc, status: "ok", detail, artifact: None }
    }

    /// `Key` and `Text` while a session is up, through the main window's text
    /// key handler. `Text` types each character as its own key with no
    /// modifiers held, as the live door sends it.
    #[allow(clippy::too_many_arguments)]
    fn type_into_session(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        index: usize,
        action_desc: String,
        action: &AutomationAction,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> StepResult {
        use crate::text_session::KeyOutcome;
        let keys = step_keys(action);
        let mut committed = None;
        for (chars, key, mods) in &keys {
            self.modifiers = *mods;
            ui.input.set_modifiers(*mods);
            let text_key = text_key(chars, *key);
            match crate::text_session::main_window_key(&mut self.text_input, ui, text_key, *mods, true) {
                KeyOutcome::NotTyping => {
                    return self.fail(index, action_desc, ui, out_dir, "the text session ended before every key was typed".into());
                }
                KeyOutcome::Consumed | KeyOutcome::Filtered => {}
                KeyOutcome::Committed(field, text) => {
                    self.commit_text(ui, data, field, &text);
                    committed = Some((field, text));
                }
            }
        }
        // A keyboard pick from the browser dispatches next frame, through
        // `process_events`.
        if !ui.pending_keyboard_actions.is_empty() {
            self.drain_and_dispatch(ui, data);
        }
        self.live_frame(ui, data, render, false, true);
        let detail = match committed {
            Some((field, text)) => format!("committed {field:?} {text:?}"),
            None if self.text_input.active => format!("{:?} reads {:?}", self.text_input.field, self.text_input.text()),
            None => "text session closed".into(),
        };
        StepResult { index, action: action_desc, status: "ok", detail, artifact: None }
    }

    /// Commits a session through the shared seam with the runner's dispatch
    /// context. The runner has no watched graph or GPU, so graph-editor and
    /// preset-save commits do nothing here.
    fn commit_text(&mut self, ui: &mut UIRoot, data: &mut SceneData, field: crate::text_input::TextInputField, text: &str) {
        let mut dctx = crate::ui_bridge::DispatchCtx {
            project: crate::ui_bridge::ProjectRef::new(&mut data.project, &mut self.edits, &self.content_tx),
            content_tx: &self.content_tx,
            content_state: &self.content_state,
            ui: &mut *ui,
            selection: &mut data.selection,
            active_layer: &mut self.active_layer,
            user_prefs: &mut self.user_prefs,
            editor_target: None,
            scrub: &mut self.scrub,
            dialogs: Default::default(),
        };
        crate::text_session::commit(&mut self.text_input, field, text, &mut dctx, &crate::text_session::EditorCommitCtx::none());
        data.active = self.active_layer.as_ref().and_then(|lid| data.project.timeline.find_layer_index_by_id(lid));
        println!("ui-snap --script: commit {field:?} {text:?}");
    }

    /// One step in the graph editor window. `None` hands the step to the
    /// main window's handling: undo and redo, which reach the content thread
    /// from either window, and every non-input step.
    #[allow(clippy::too_many_arguments)]
    fn editor_step(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        index: usize,
        action_desc: &str,
        action: &AutomationAction,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> Option<StepResult> {
        let desc = action_desc.to_string();
        match action {
            AutomationAction::Pointer { target, gesture } => {
                Some(self.editor_pointer(ui, data, index, desc, target, gesture, out_dir, render))
            }
            AutomationAction::Key { .. } | AutomationAction::Text { .. } if self.text_input.active => None,
            AutomationAction::Key { .. } | AutomationAction::Text { .. } => {
                let typing = self.editor_mapping.is_editing()
                    || self.editor.as_ref().is_some_and(|ed| ed.canvas.popover_is_editing());
                if !typing {
                    if let AutomationAction::Key { key: Key::Z, modifiers } = action
                        && (modifiers.is_command_only() || modifiers.is_command_shift())
                    {
                        return None;
                    }
                    return Some(self.fail(index, desc, ui, out_dir,
                        "this editor-window key stays on Application (editor_keyboard_input); only a mapping field being typed into and undo are reached".into()));
                }
                for (chars, key, modifiers) in step_keys(action) {
                    self.modifiers = modifiers;
                    let canvas = self.editor.as_mut().map(|ed| &mut ed.canvas);
                    if !crate::editor_input::popover_key(&mut self.editor_mapping, canvas, text_key(&chars, key), modifiers) {
                        return Some(self.fail(index, desc, ui, out_dir, "the mapping field closed before every key was typed".into()));
                    }
                }
                if let Err(e) = self.editor_drain_and_dispatch(ui, data) {
                    return Some(self.fail(index, desc, ui, out_dir, e));
                }
                self.live_frame(ui, data, render, false, true);
                let detail = if self.editor_mapping.is_editing() { "typed into the mapping field" } else { "mapping field closed" };
                Some(StepResult { index, action: desc, status: "ok", detail: detail.into(), artifact: None })
            }
            AutomationAction::ScrollTo { .. } | AutomationAction::TrackWheel { .. } | AutomationAction::ResizeDrag { .. } => {
                Some(self.fail(index, desc, ui, out_dir, "a main-window input: Focus Main first".into()))
            }
            _ => None,
        }
    }

    /// The editor-input seam's borrow of this runner for one editor event, as
    /// the App's `editor_pointer` borrows the App.
    fn editor_cx<'s>(&'s mut self, data: &'s mut SceneData) -> Option<crate::editor_input::EditorPointer<'s>> {
        let ed = self.editor.as_mut()?;
        Some(crate::editor_input::EditorPointer {
            area: manifold_ui::Rect::new(0.0, 0.0, ed.logical_w as f32, ed.logical_h as f32),
            ws: &mut ed.ws,
            canvas: Some(&mut ed.canvas),
            mapping: &mut self.editor_mapping,
            modifiers: self.modifiers,
            time: self.clock,
            project: &data.project,
            content_tx: Some(&self.content_tx),
            is_playing: data.content.is_playing,
            watched: self.watched.as_ref(),
            graph: data.content.active_graph_snapshot.as_deref(),
            active_layer: &mut self.active_layer,
            selection: &mut data.selection,
        })
    }

    /// A cursor move into the editor window: an armed text drag takes it,
    /// else the seam (`window_input`'s cursor-moved order).
    fn editor_move(&mut self, ui: &UIRoot, data: &mut SceneData, pos: Vec2) {
        self.last_gesture_points.push(pos);
        if crate::text_session::pointer_move(&mut self.text_input, Some(&ui.tree), pos) {
            return;
        }
        if let Some(mut cx) = self.editor_cx(data) {
            // No viewport session opens here, so no gizmo drag arms.
            let _ = crate::editor_input::cursor_moved(&mut cx, pos);
        }
    }

    /// A button press at the cursor: the text gate first for the left
    /// button, then the seam.
    fn editor_press(&mut self, ui: &mut UIRoot, data: &mut SceneData, button: winit::event::MouseButton) -> Result<(), String> {
        let pos = self.last_gesture_points.last().copied().unwrap_or(Vec2::ZERO);
        if button == winit::event::MouseButton::Left && self.text_press(ui, data, pos, self.modifiers.shift) {
            return Ok(());
        }
        let Some(mut cx) = self.editor_cx(data) else { return Ok(()) };
        let out = crate::editor_input::button(&mut cx, button, winit::event::ElementState::Pressed);
        if out.viewport_press.is_some() {
            return Err("a 3D viewport press needs the App's scene projection; the runner's editor has no viewport".into());
        }
        Ok(())
    }

    fn editor_release(&mut self, data: &mut SceneData, button: winit::event::MouseButton) {
        if button == winit::event::MouseButton::Left {
            crate::text_session::pointer_up(&mut self.text_input);
        }
        if let Some(mut cx) = self.editor_cx(data) {
            crate::editor_input::button(&mut cx, button, winit::event::ElementState::Released);
        }
    }

    /// The editor window's per-frame drain, in the App's order: the
    /// popovers' mapping actions join the main queue, the card lane
    /// dispatches against the editor's tree, then the canvas and sidebar
    /// edits. A graph edit only `app_render`'s loop dispatches fails loudly.
    fn editor_drain_and_dispatch(&mut self, ui: &mut UIRoot, data: &mut SceneData) -> Result<usize, String> {
        use manifold_ui::GraphEditCommand;
        let Some(ed) = self.editor.as_mut() else { return Ok(0) };
        let drain = crate::editor_input::drain(
            Some(&mut ed.ws),
            Some(&mut ed.canvas),
            &mut self.editor_mapping,
            &mut self.text_input,
            crate::editor_input::SmartPreviewToggle {
                intents: &mut ed.intents,
                id: ed.smart_preview_toggle,
                normalize: ed.node_preview_normalize,
            },
        );
        let handled = drain.actions.len() + drain.card_actions.len() + drain.graph_edits.len();
        self.apply_panel_actions(ui, data, &drain.actions);
        if !drain.card_actions.is_empty() {
            let mappings = match self.editor.as_mut() {
                Some(ed) => crate::editor_input::card_lane_entries(
                    &mut self.text_input, &mut ed.ws, &data.project, &data.selection, &self.active_layer, &drain.card_actions,
                ),
                None => Vec::new(),
            };
            for (target, param_id, anchor_node_id) in mappings {
                self.open_card_mapping(data, target, param_id, anchor_node_id);
            }
            let Some(ed) = self.editor.as_mut() else { return Ok(handled) };
            let outcome = crate::editor_input::dispatch_card_lane(
                crate::editor_input::CardLane {
                    ws: &mut ed.ws,
                    project: crate::ui_bridge::ProjectRef::new(&mut data.project, &mut self.edits, &self.content_tx),
                    content_tx: &self.content_tx,
                    content_state: &self.content_state,
                    selection: &mut data.selection,
                    active_layer: &mut self.active_layer,
                    user_prefs: &mut self.user_prefs,
                    scrub: &mut self.scrub,
                    dialogs: Default::default(),
                },
                &drain.card_actions,
            );
            println!("ui-snap --script: editor card lane dispatched {:?}", drain.card_actions);
            data.active = self.active_layer.as_ref().and_then(|lid| data.project.timeline.find_layer_index_by_id(lid));
            if outcome.save_preset.is_some() {
                return Err("Save preset opens its name prompt on Application, not reached headless".into());
            }
            let tab = ui.inspector.last_effect_tab();
            if let Some(ei) = outcome.retarget_effect {
                if let Some(eid) = crate::editor_bridge::effect_card_id(tab, &data.project, self.active_layer.as_ref(), &data.selection, ei) {
                    self.watch(data, manifold_core::GraphTarget::Effect(eid));
                }
            } else if outcome.retarget_generator
                && let Some(layer) = self.active_layer.clone()
            {
                self.watch(data, manifold_core::GraphTarget::Generator(layer));
            }
        }
        for edit in drain.graph_edits {
            match edit {
                // Preview-only display preference: the UI mirror flips; the
                // content side's node-preview blit has no headless consumer.
                GraphEditCommand::SetNodePreviewNormalize(on) => {
                    if let Some(ed) = self.editor.as_mut() {
                        ed.node_preview_normalize = on;
                    }
                }
                other => {
                    return Err(format!("{other:?} dispatches in app_render's graph-edit loop on Application, not reached headless"));
                }
            }
        }
        Ok(handled)
    }

    /// A pointer step into the editor window. Targets resolve against the
    /// editor's tree, its canvas and its mapping drawer. Every gesture starts
    /// with a move, since winit gives button and wheel events no position and
    /// the editor reads the cursor its canvas tracked.
    #[allow(clippy::too_many_arguments)]
    fn editor_pointer(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        index: usize,
        action_desc: String,
        target: &AutomationTarget,
        gesture: &Gesture,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> StepResult {
        use winit::event::MouseButton;
        let center_of = |r: Rect| Vec2::new(r.x + r.width * 0.5, r.y + r.height * 0.5);
        let center = match self.resolve_editor(target) {
            Ok(r) => center_of(r),
            Err(e) => return self.fail(index, action_desc, ui, out_dir, e),
        };
        self.last_gesture_points.clear();
        let mut pressed = Ok(());
        match gesture {
            Gesture::Press { modifiers } => {
                self.modifiers = *modifiers;
                self.editor_move(ui, data, center);
                pressed = self.editor_press(ui, data, MouseButton::Left);
            }
            Gesture::Release => {
                self.editor_move(ui, data, center);
                self.editor_release(data, MouseButton::Left);
            }
            Gesture::Click { modifiers } => {
                self.modifiers = *modifiers;
                self.editor_move(ui, data, center);
                pressed = self.editor_press(ui, data, MouseButton::Left);
                self.editor_release(data, MouseButton::Left);
            }
            Gesture::DoubleClick => {
                self.editor_move(ui, data, center);
                for _ in 0..2 {
                    pressed = pressed.and(self.editor_press(ui, data, MouseButton::Left));
                    self.editor_release(data, MouseButton::Left);
                }
            }
            Gesture::RightClick => {
                self.editor_move(ui, data, center);
                pressed = self.editor_press(ui, data, MouseButton::Right);
                self.editor_release(data, MouseButton::Right);
            }
            Gesture::Hover => self.editor_move(ui, data, center),
            Gesture::Scroll { delta } => {
                self.editor_move(ui, data, center);
                let delta = winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                    f64::from(delta.x),
                    f64::from(delta.y),
                ));
                if let Some(mut cx) = self.editor_cx(data) {
                    crate::editor_input::wheel(&mut cx, delta);
                }
            }
            Gesture::Drag { to, steps } => {
                let to_center = match self.resolve_editor(to) {
                    Ok(r) => center_of(r),
                    Err(e) => return self.fail(index, action_desc, ui, out_dir, e),
                };
                self.editor_move(ui, data, center);
                pressed = self.editor_press(ui, data, MouseButton::Left);
                for pt in automation::interpolate_drag(center, to_center, *steps) {
                    self.editor_move(ui, data, pt);
                }
                self.editor_move(ui, data, to_center);
                self.editor_release(data, MouseButton::Left);
            }
            Gesture::DragFrames { to, steps } | Gesture::Sweep { to, steps } => {
                let to_center = match self.resolve_editor(to) {
                    Ok(r) => center_of(r),
                    Err(e) => return self.fail(index, action_desc, ui, out_dir, e),
                };
                let held = matches!(gesture, Gesture::DragFrames { .. });
                self.editor_move(ui, data, center);
                if held {
                    pressed = self.editor_press(ui, data, MouseButton::Left);
                }
                let mut frame = |runner: &mut Self, ui: &mut UIRoot, data: &mut SceneData| -> Result<(), String> {
                    runner.editor_drain_and_dispatch(ui, data)?;
                    runner.live_frame(ui, data, render, false, true);
                    Ok(())
                };
                let mut moved = frame(self, ui, data);
                for pt in automation::interpolate_drag(center, to_center, *steps) {
                    self.clock += DT;
                    self.editor_move(ui, data, pt);
                    moved = moved.and(frame(self, ui, data));
                }
                self.clock += DT;
                self.editor_move(ui, data, to_center);
                if held {
                    self.editor_release(data, MouseButton::Left);
                }
                if let Err(e) = moved {
                    return self.fail(index, action_desc, ui, out_dir, e);
                }
            }
        }
        if let Err(e) = pressed.and(self.editor_drain_and_dispatch(ui, data).map(|_| ())) {
            return self.fail(index, action_desc, ui, out_dir, e);
        }
        self.live_frame(ui, data, render, false, true);
        StepResult {
            index,
            action: action_desc,
            status: "ok",
            detail: format!("editor window: acted at ({:.1},{:.1})", center.x, center.y),
            artifact: None,
        }
    }

    /// The editor window's hit-test surfaces: its canvas at this frame's
    /// viewport and its mapping drawer.
    fn editor_matches(&self, target: &AutomationTarget) -> Option<(Vec<MatchInfo>, String)> {
        let ed = self.editor.as_ref()?;
        let layout = crate::editor_frame::EditorLayout::compute(&ed.ws.dock, ed.logical_w, ed.logical_h, None);
        let canvas = manifold_ui::graph_canvas::GraphCanvasTargets { canvas: &ed.canvas, viewport: layout.canvas_rect() };
        let surfaces: Vec<&dyn HitTargets> = vec![&canvas, &self.editor_mapping];
        Some(automation::resolve_all(&ed.ws.ui_root.tree, &surfaces, target))
    }

    fn resolve_editor(&self, target: &AutomationTarget) -> Result<Rect, String> {
        let ed = self.editor.as_ref().ok_or("the graph editor is not open")?;
        let layout = crate::editor_frame::EditorLayout::compute(&ed.ws.dock, ed.logical_w, ed.logical_h, None);
        let canvas = manifold_ui::graph_canvas::GraphCanvasTargets { canvas: &ed.canvas, viewport: layout.canvas_rect() };
        let surfaces: Vec<&dyn HitTargets> = vec![&canvas, &self.editor_mapping];
        automation::resolve(&ed.ws.ui_root.tree, &surfaces, target).map(|r| r.rect).map_err(|e| e.to_string())
    }

    /// The App's text gate ahead of a left press (`window_input`'s
    /// `input_mouse_input`): true when the field took the press. A press
    /// outside commits first and still reaches its target.
    fn text_press(&mut self, ui: &mut UIRoot, data: &mut SceneData, pos: Vec2, shift: bool) -> bool {
        use crate::text_session::PointerDown;
        match crate::text_session::pointer_down(&mut self.text_input, Some(&ui.tree), pos, self.clock, shift) {
            PointerDown::Idle => false,
            PointerDown::Inside => true,
            PointerDown::Committed(field, text) => {
                self.commit_text(ui, data, field, &text);
                false
            }
        }
    }

    /// A left press through the text gate, then the tree, as the App routes it.
    fn press(&mut self, ui: &mut UIRoot, data: &mut SceneData, pos: Vec2) {
        let extend = self.modifiers.shift;
        if !self.text_press(ui, data, pos, extend) && !self.preview_gizmo(ui, data, |host, ctx, fit| host.press(ctx, fit, pos, extend)) {
            ui.pointer_event(pos, PointerAction::Down, self.clock);
        }
    }

    /// A left release: it ends a text drag, clears the selection after a
    /// click on empty preview space, then ends a preview gizmo drag
    /// (`window_input`'s release order), else reaches the tree.
    fn release(&mut self, ui: &mut UIRoot, data: &mut SceneData, pos: Vec2) {
        crate::text_session::pointer_up(&mut self.text_input);
        self.preview_gizmo(ui, data, |host, ctx, _| {
            host.release_click(ctx, pos);
            false
        });
        if !self.preview_gizmo(ui, data, |host, ctx, _| host.release(ctx)) {
            ui.pointer_event(pos, PointerAction::Up, self.clock);
        }
    }

    /// Pointer motion: an armed text drag takes it, then a preview gizmo
    /// drag, else the tree.
    fn move_to(&mut self, ui: &mut UIRoot, data: &mut SceneData, pos: Vec2) {
        if !crate::text_session::pointer_move(&mut self.text_input, Some(&ui.tree), pos)
            && !self.preview_gizmo(ui, data, |host, ctx, fit| host.drag_to(ctx, fit, pos))
        {
            ui.pointer_event(pos, PointerAction::Move, self.clock);
        }
    }

    /// What a frame shows that the tree dump cannot see: the inputs of the
    /// immediate passes (playhead, insert cursor, selection band, markers,
    /// the clip draw list, automation lanes, overview strip), the pending
    /// toast and, in perform mode, everything the perform screen shows.
    fn immediate_inputs(&self, ui: &UIRoot, data: &SceneData) -> Vec<String> {
        let insert_layer = data
            .selection
            .insert_cursor_layer_id()
            .as_ref()
            .and_then(|id| data.project.timeline.find_layer_index_by_id(id));
        let mut markers = Vec::new();
        let overlays = ui.viewport.timeline_overlays(insert_layer, data.selection.has_insert_cursor(), &mut markers);
        let (clip_rects, automation_lanes) = self.surfaces_owned(ui);
        let mut out = vec![
            format!("playhead {:?}", ui.viewport.playhead_pixel()),
            format!("overlays {overlays:?}"),
            format!("markers {markers:?}"),
            format!("overview {:?}", ui.viewport.overview_rect()),
            format!("toast {:?}", ui.toast.pending()),
        ];
        out.extend(clip_rects.iter().map(|c| format!("clip {c:?}")));
        out.extend(automation_lanes.iter().map(|l| format!("lane {l:?}")));
        if self.perform.active {
            let session = ui.ableton_session.clone();
            let hud = crate::perform_mode::frame::PerformHud::derive(&data.content, session.as_deref(), &data.project);
            out.push(format!("perform {hud:?}"));
            // What the HUD on screen was drawn from: a missed input leaves it
            // behind the inputs a fresh frame derives.
            out.push(format!("perform shown {:?}", self.perform.shown));
            out.push(format!(
                "perform exit {:?} hover {}",
                self.perform.exit_button_rect, self.perform.exit_button_hover
            ));
        }
        out
    }

    /// `AssertFresh` (UI_INVALIDATION_DESIGN.md D10, INV-4): the incremental
    /// frame must equal a full projection and build from the same inputs.
    fn assert_fresh(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        render: &mut RenderState,
        index: usize,
        action_desc: String,
        out_dir: &std::path::Path,
    ) -> StepResult {
        let surfaces_dump = |runner: &Self, ui: &UIRoot| {
            let (clip_rects, automation_lanes) = runner.surfaces_owned(ui);
            let clip_targets = ClipHitTargets(&clip_rects);
            let automation_targets = AutomationHitTargets(&automation_lanes);
            let surfaces: Vec<&dyn HitTargets> = vec![&clip_targets, &automation_targets];
            super::dump::dump_tree_ex(&ui.tree, &surfaces)
        };
        // A tween the last frame started (the value-change flash after an
        // undo) settles in the next frame; compare settled frames only.
        let editor_settled = self.editor.as_mut().is_some_and(|ed| {
            let ed_ui = &mut ed.ws.ui_root;
            ed_ui.inspector.skip_to_settled(&mut ed_ui.tree)
        });
        if ui.inspector.skip_to_settled(&mut ui.tree) | ui.scene_setup_panel.skip_cards_to_settled(&mut ui.tree) | editor_settled {
            self.live_frame(ui, data, render, false, true);
        }
        let tree_before = surfaces_dump(self, ui);
        let immediate_before = self.immediate_inputs(ui, data);
        let editor_before = self.editor.as_ref().map(|ed| ed.dump(&self.editor_mapping));

        self.fresh_reference = true;
        self.live_frame(ui, data, render, false, true);

        let tree_after = surfaces_dump(self, ui);
        let immediate_after = self.immediate_inputs(ui, data);
        let editor_after = self.editor.as_ref().map(|ed| ed.dump(&self.editor_mapping));

        let (mut tree_lines, mut tree_differs) = super::readback::diff_trees(&tree_before, &tree_after);
        // The editor window's tree must equal its own full build too.
        if let (Some(before), Some(after)) = (&editor_before, &editor_after) {
            let (lines, differs) = super::readback::diff_trees(before, after);
            if differs {
                tree_differs = true;
                tree_lines.extend(lines.into_iter().map(|line| format!("editor: {line}")));
            }
        }
        let immediate_diff: Vec<String> = immediate_before
            .iter()
            .zip(immediate_after.iter())
            .filter(|(a, b)| a != b)
            .map(|(a, b)| format!("immediate: {a} -> {b}"))
            .chain(
                (immediate_before.len() != immediate_after.len())
                    .then(|| format!("immediate: {} -> {} entries", immediate_before.len(), immediate_after.len())),
            )
            .collect();
        if !tree_differs && immediate_diff.is_empty() {
            let detail = match (self.perform.active, editor_before.is_some()) {
                (true, _) => "fresh (perform screen, frozen main tree)",
                (false, true) => "fresh (main and editor windows)",
                (false, false) => "fresh",
            };
            return StepResult { index, action: action_desc, status: "ok", detail: detail.into(), artifact: None };
        }
        let path = out_dir.join(format!("{index:02}.stale.txt"));
        let report: Vec<String> = immediate_diff.iter().cloned().chain(tree_lines).collect();
        let _ = std::fs::write(&path, report.join("\n"));
        StepResult {
            index,
            action: action_desc,
            status: "fail",
            detail: format!(
                "incremental frame differs from a full rebuild: {}",
                report.iter().take(6).map(String::as_str).collect::<Vec<_>>().join(" | ")
            ),
            artifact: Some(path.display().to_string()),
        }
    }

    fn assert_work(
        &mut self,
        ui: &UIRoot,
        index: usize,
        action_desc: String,
        window: &str,
        out_dir: &std::path::Path,
    ) -> StepResult {
        use manifold_core::work_count::{thread_work_snapshot, WorkKind};
        let counts = thread_work_snapshot().since(&self.work.start).minus(&self.work.excluded);
        let observed: Vec<(WorkKind, u64)> = counts.nonzero().collect();
        let mut ceilings = read_work_ceilings();
        let observed_text = || {
            observed.iter().map(|(k, n)| format!("{}={n}", k.name())).collect::<Vec<_>>().join(" ")
        };
        if self.work.record {
            let entry = ceilings
                .as_object_mut()
                .expect("ceilings file is an object")
                .entry("flows")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .expect("flows is an object")
                .entry(self.work.flow.clone())
                .or_insert_with(|| serde_json::json!({}));
            let mut window_counts = serde_json::Map::new();
            for (kind, n) in &observed {
                window_counts.insert(kind.name().to_string(), serde_json::json!(n));
            }
            entry
                .as_object_mut()
                .expect("flow entry is an object")
                .insert(window.to_string(), serde_json::Value::Object(window_counts));
            write_work_ceilings(&ceilings);
            println!("ui-snap --script: recorded work ceilings {}/{window}: {}", self.work.flow, observed_text());
            return StepResult { index, action: action_desc, status: "ok", detail: format!("recorded {}", observed_text()), artifact: None };
        }
        let ceiling = |kind: WorkKind| {
            ceilings
                .pointer(&format!("/flows/{}/{window}/{}", self.work.flow, kind.name()))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        };
        let over: Vec<String> = observed
            .iter()
            .filter(|&&(k, n)| n > ceiling(k))
            .map(|&(k, n)| format!("{}={n} > {}", k.name(), ceiling(k)))
            .collect();
        if over.is_empty() {
            StepResult { index, action: action_desc, status: "ok", detail: format!("within ceilings: {}", observed_text()), artifact: None }
        } else {
            self.fail(index, action_desc, ui, out_dir, format!(
                "work over ceiling in {}/{window}: {} (re-record only by `scripts/run_ui_flows.py --record-work`)",
                self.work.flow,
                over.join(", "),
            ))
        }
    }

    /// `PlayingStep`: the content clock as the UI sees it. Each frame moves
    /// `ContentState`'s beat at `bpm` and alternates its BPM by `wobble`.
    #[allow(clippy::too_many_arguments)]
    fn playing_step(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        frames: u32,
        bpm: f32,
        wobble: f32,
        render: &mut RenderState,
    ) {
        data.content.is_playing = true;
        for frame in 0..frames {
            let sign = if frame % 2 == 0 { 1.0 } else { -1.0 };
            data.content.bpm = (bpm + sign * wobble) as f64;
            data.content.current_beat =
                manifold_core::Beats(data.content.current_beat.0 + bpm as f64 / 60.0 * DT as f64);
            self.clock += DT;
            self.live_frame(ui, data, render, false, true);
        }
    }

    /// One wheel notch over the tracks: `window_input::primary_mouse_wheel`'s
    /// tracks branch and `zoom_timeline_at_cursor`, with the held modifiers.
    fn track_wheel(&mut self, ui: &mut UIRoot, pos: Vec2, dy: f32) {
        if !ui.layout.timeline_tracks().contains(pos) {
            return;
        }
        let viewport = &mut ui.viewport;
        if self.modifiers.alt {
            let factor = manifold_ui::color::ZOOM_WHEEL_STEP_PER_NOTCH.powf(dy / crate::window_input::LINE_DELTA_PX);
            let beat_at_cursor = viewport.pixel_to_beat(pos.x).as_f32();
            let new_ppb = viewport.zoom_continuous(factor);
            viewport.zoom_to(new_ppb, beat_at_cursor, pos.x);
        } else if self.modifiers.shift {
            let beat_delta = dy * manifold_ui::color::SCROLL_SENSITIVITY / viewport.pixels_per_beat();
            let new_x = (viewport.scroll_x_beats().as_f32() - beat_delta).max(0.0);
            viewport.set_scroll(new_x, viewport.scroll_y_px());
            viewport.note_user_scroll_x();
        } else {
            let new_y = (viewport.scroll_y_px() - dy).max(0.0);
            viewport.set_scroll(viewport.scroll_x_beats().as_f32(), new_y);
        }
    }

    /// `ResizeDrag`: `window_input`'s raw split and inspector-width drags.
    #[allow(clippy::too_many_arguments)]
    fn resize_drag(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        handle: manifold_ui::automation::ResizeHandle,
        (from, to): (f32, f32),
        steps: u32,
        render: &mut RenderState,
    ) {
        use manifold_ui::automation::ResizeHandle;
        let steps = steps.max(1);
        if matches!(handle, ResizeHandle::InspectorWidth) {
            ui.begin_inspector_resize(from);
        }
        for i in 1..=steps {
            let pos = from + (to - from) * i as f32 / steps as f32;
            match handle {
                ResizeHandle::TimelineSplit => {
                    ui.layout.update_split_from_drag(pos);
                }
                ResizeHandle::InspectorWidth => {
                    ui.update_inspector_resize(pos);
                }
            }
            self.live_frame(ui, data, render, false, true);
        }
        ui.end_inspector_resize();
    }

    /// Drains every `ContentCommand` this step's `AppEditingHost`/
    /// `ui_bridge::dispatch` calls sent over `content_tx` (the receiver is
    /// otherwise held only to keep the channel alive — see `Runner::new`)
    /// and folds the editing ones into the harness's `undo` stack. NEVER
    /// re-executes ordinary Execute messages: `AppEditingHost`/`ui_bridge` mutate
    /// `data.project` synchronously before sending (mirroring the live
    /// app's `local_project` pattern, module doc), so by the time this runs
    /// the command has already taken effect — `record()`, not `execute()`.
    /// SceneModifier messages are content-owned requests, so this harness
    /// executes their shared authoring/admission path exactly once before recording.
    /// Non-editing variants (`SeekTo`, `Undo`, …) are inert here; the
    /// harness's headless `Key` arm drives `self.undo` directly instead of
    /// routing through this channel.
    fn record_executed_commands(&mut self, data: &mut SceneData) -> bool {
        let mut changed = false;
        while let Ok(cmd) = self._content_rx.try_recv() {
            // The content thread watches what the editor asks for; its next
            // state push carries the snapshot.
            if let Some(target) = crate::graph_snapshot::watch_command_target(&cmd) {
                self.content_watched = target;
                changed = true;
                continue;
            }
            let cmd = match cmd {
                ContentCommand::FireParameter { target, param_id } => {
                    ContentCommand::ExecuteOnContent(Box::new(
                        manifold_editing::commands::effects::FireGraphParamCommand::new(target, param_id),
                    ))
                }
                ContentCommand::ChangeGeneratorType { layer_id, new_type } => {
                    match crate::generator_change::build_change(&data.project, layer_id, new_type) {
                        Ok(command) => ContentCommand::ExecuteOnContent(command),
                        Err(message) => ContentCommand::GraphEditRejected(message),
                    }
                }
                ContentCommand::ObjectModifier(action) => {
                    match crate::object_modifier_transfer::build_action(&data.project, action) {
                        Ok(command) => ContentCommand::ExecuteOnContent(command),
                        Err(message) => ContentCommand::GraphEditRejected(message),
                    }
                }
                ContentCommand::SceneCameraSetup(layer_id) => {
                    match crate::scene_camera_edit::build_action(&data.project, layer_id) {
                        Ok(command) => ContentCommand::ExecuteOnContent(command),
                        Err(message) => ContentCommand::GraphEditRejected(message),
                    }
                }
                ContentCommand::SceneItem(action) => {
                    let selection = action.selection_request();
                    match crate::scene_item_transfer::build_action(&data.project, action) {
                        Ok(command) => match selection {
                            Some(request) => ContentCommand::ExecuteSelecting(command, request),
                            None => ContentCommand::ExecuteOnContent(command),
                        },
                        Err(message) => ContentCommand::GraphEditRejected(message),
                    }
                }
                other => other,
            };
            match cmd {
                ContentCommand::ExecuteSelecting(command, request) => {
                    let pending = request.capture(&data.project);
                    let applied = self.undo.execute(crate::scene_modifier_edit::with_admission(command), &mut data.project);
                    if applied {
                        self.content_stamps.bump(self.undo.last_written());
                    }
                    changed |= applied;
                    if let Some(message) = self.undo.take_rejection() {
                        let sequence = data.content.graph_edit_diagnostic.as_ref().map_or(1, |old| old.sequence.wrapping_add(1));
                        data.content.graph_edit_diagnostic = Some(std::sync::Arc::new(crate::content_state::GraphEditDiagnostic { sequence, message }));
                    } else if applied && let Some(selection) = pending.resolve(&data.project) {
                        let sequence = data.content.edit_selection_update.as_ref().map_or(1, |old| old.sequence.wrapping_add(1));
                        data.content.edit_selection_update = Some(std::sync::Arc::new(crate::edit_selection::EditSelectionUpdate { sequence, selection }));
                    }
                }
                ContentCommand::SceneModifier(action) => {
                    let error = match crate::scene_modifier_edit::build_action(&data.project, action) {
                        Ok(command) => {
                            let applied = self.undo.execute(crate::scene_modifier_edit::with_admission(command), &mut data.project);
                            if applied {
                                self.content_stamps.bump(self.undo.last_written());
                            }
                            changed |= applied;
                            self.undo.take_rejection()
                        }
                        Err(error) => Some(error),
                    };
                    if let Some(message) = error {
                        let sequence = data.content.graph_edit_diagnostic.as_ref().map_or(1, |old| old.sequence.wrapping_add(1));
                        data.content.graph_edit_diagnostic = Some(std::sync::Arc::new(crate::content_state::GraphEditDiagnostic { sequence, message }));
                        changed = true;
                    }
                }
                ContentCommand::GraphEditRejected(message) => {
                    let sequence = data.content.graph_edit_diagnostic.as_ref().map_or(1, |old| old.sequence.wrapping_add(1));
                    data.content.graph_edit_diagnostic = Some(std::sync::Arc::new(crate::content_state::GraphEditDiagnostic { sequence, message }));
                    changed = true;
                }
                // The App gets these back as a snapshot with a new data
                // version, which re-projects; this is that echo.
                ContentCommand::Execute(cmd) => {
                    self.content_stamps.bump(cmd.touches());
                    self.undo.record(cmd);
                    changed = true;
                }
                ContentCommand::ExecuteOnContent(cmd) => {
                    let applied = self.undo.execute(crate::scene_modifier_edit::with_admission(cmd), &mut data.project);
                    if applied {
                        // After execute, as EditingService reads it.
                        self.content_stamps.bump(self.undo.last_written());
                    }
                    changed |= applied;
                    if let Some(message) = self.undo.take_rejection() {
                        let sequence = data.content.graph_edit_diagnostic.as_ref().map_or(1, |old| old.sequence.wrapping_add(1));
                        data.content.graph_edit_diagnostic = Some(std::sync::Arc::new(crate::content_state::GraphEditDiagnostic { sequence, message }));
                        changed = true;
                    }
                }
                ContentCommand::ExecuteBatch(cmds, desc) => {
                    self.content_stamps.bump(crate::gesture_preview::touches_of(&cmds));
                    self.undo.record(Box::new(CompositeCommand::new(cmds, desc)));
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    fn surfaces_owned(&self, ui: &UIRoot) -> (Vec<manifold_ui::panels::viewport::ClipScreenRect>, Vec<manifold_ui::panels::viewport::AutomationLaneScreen>) {
        let mut clip_rects = Vec::new();
        ui.viewport.visible_clip_rects(&mut clip_rects);
        let automation_lanes = ui.viewport.automation_lane_screens().to_vec();
        (clip_rects, automation_lanes)
    }

    fn resolve(
        &self,
        ui: &UIRoot,
        target: &AutomationTarget,
    ) -> Result<Rect, String> {
        let (clip_rects, automation_lanes) = self.surfaces_owned(ui);
        let clip_targets = ClipHitTargets(&clip_rects);
        let automation_targets = AutomationHitTargets(&automation_lanes);
        // The perform screen's exit button is a surface only while it shows.
        let perform_targets = crate::perform_mode::frame::PerformHitTargets(self.perform.exit_button_rect);
        // The preview gizmo's handles are a surface while Show gizmos draws them.
        let gizmo_targets = crate::physics_overlay::GizmoHitTargets(&self.gizmo.grips);
        let mut surfaces: Vec<&dyn HitTargets> = vec![&clip_targets, &automation_targets];
        if self.perform.active {
            surfaces.push(&perform_targets);
        }
        if !self.gizmo.grips.is_empty() {
            surfaces.push(&gizmo_targets);
        }
        automation::resolve(&ui.tree, &surfaces, target)
            .map(|r| r.rect)
            .map_err(|e| e.to_string())
    }

    fn resolve_all(
        &self,
        ui: &UIRoot,
        target: &AutomationTarget,
    ) -> (Vec<MatchInfo>, String) {
        let (clip_rects, automation_lanes) = self.surfaces_owned(ui);
        let clip_targets = ClipHitTargets(&clip_rects);
        let automation_targets = AutomationHitTargets(&automation_lanes);
        // The perform screen's exit button is a surface only while it shows.
        let perform_targets = crate::perform_mode::frame::PerformHitTargets(self.perform.exit_button_rect);
        // The preview gizmo's handles are a surface while Show gizmos draws them.
        let gizmo_targets = crate::physics_overlay::GizmoHitTargets(&self.gizmo.grips);
        let mut surfaces: Vec<&dyn HitTargets> = vec![&clip_targets, &automation_targets];
        if self.perform.active {
            surfaces.push(&perform_targets);
        }
        if !self.gizmo.grips.is_empty() {
            surfaces.push(&gizmo_targets);
        }
        // The focused window answers first; a selector nothing there matches
        // is looked up in the other open window, so a flow can assert on
        // what either window shows.
        let main = || automation::resolve_all(&ui.tree, &surfaces, target);
        if self.focus == manifold_ui::automation::FlowWindow::GraphEditor
            && let Some((matches, query)) = self.editor_matches(target)
            && !matches.is_empty()
        {
            return (matches, query);
        }
        let (matches, query) = main();
        match self.editor_matches(target) {
            Some((editor_matches, _)) if matches.is_empty() => (editor_matches, query),
            _ => (matches, query),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pointer(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        index: usize,
        action_desc: String,
        target: &AutomationTarget,
        gesture: &Gesture,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> StepResult {
        let rect = match self.resolve(ui, target) {
            Ok(r) => r,
            Err(e) => return self.fail(index, action_desc, ui, out_dir, e),
        };
        let center = Vec2::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);

        // D9b: this gesture's point(s) replace whatever the last one left —
        // consumed (drained) by the next Snapshot only.
        self.last_gesture_points.clear();
        let mut scrolled_in_place = false;

        match gesture {
            Gesture::Press { modifiers } => {
                self.modifiers = *modifiers;
                ui.input.set_modifiers(*modifiers);
                self.last_gesture_points.push(center);
                self.press(ui, data, center);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::Release => {
                self.last_gesture_points.push(center);
                self.release(ui, data, center);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::Click { modifiers } => {
                self.modifiers = *modifiers;
                ui.input.set_modifiers(*modifiers);
                self.last_gesture_points.push(center);
                self.press(ui, data, center);
                self.release(ui, data, center);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::DoubleClick => {
                ui.input.set_modifiers(self.modifiers);
                self.last_gesture_points.push(center);
                self.press(ui, data, center);
                self.release(ui, data, center);
                self.press(ui, data, center);
                self.release(ui, data, center);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::RightClick => {
                self.last_gesture_points.push(center);
                ui.input.process_right_click(&ui.tree, center);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::Hover => {
                self.last_gesture_points.push(center);
                self.move_to(ui, data, center);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::Scroll { delta } => {
                scrolled_in_place = self.scroll_once(ui, data, center, *delta);
            }
            Gesture::Drag { to, steps } => {
                let to_rect = match self.resolve(ui, to) {
                    Ok(r) => r,
                    Err(e) => return self.fail(index, action_desc, ui, out_dir, e),
                };
                let to_center = Vec2::new(to_rect.x + to_rect.width * 0.5, to_rect.y + to_rect.height * 0.5);
                ui.input.set_modifiers(self.modifiers);
                self.last_gesture_points.push(center);
                self.press(ui, data, center);
                for pt in automation::interpolate_drag(center, to_center, *steps) {
                    self.last_gesture_points.push(pt);
                    self.move_to(ui, data, pt);
                }
                self.last_gesture_points.push(to_center);
                self.release(ui, data, to_center);
                self.drain_and_dispatch(ui, data);
            }
            Gesture::DragFrames { to, steps } | Gesture::Sweep { to, steps } => {
                let to_rect = match self.resolve(ui, to) {
                    Ok(r) => r,
                    Err(e) => return self.fail(index, action_desc, ui, out_dir, e),
                };
                let to_center = Vec2::new(to_rect.x + to_rect.width * 0.5, to_rect.y + to_rect.height * 0.5);
                let pressed = matches!(gesture, Gesture::DragFrames { .. });
                ui.input.set_modifiers(self.modifiers);
                self.last_gesture_points.push(center);
                if pressed {
                    self.press(ui, data, center);
                } else {
                    self.move_to(ui, data, center);
                    self.overlay_hover(ui, data, center);
                }
                self.drain_and_dispatch(ui, data);
                self.live_frame(ui, data, render, false, true);
                for pt in automation::interpolate_drag(center, to_center, *steps) {
                    self.clock += DT;
                    self.last_gesture_points.push(pt);
                    self.move_to(ui, data, pt);
                    self.overlay_hover(ui, data, pt);
                    self.drain_and_dispatch(ui, data);
                    self.live_frame(ui, data, render, false, true);
                }
                self.clock += DT;
                self.last_gesture_points.push(to_center);
                if pressed {
                    self.release(ui, data, to_center);
                } else {
                    self.move_to(ui, data, to_center);
                    self.overlay_hover(ui, data, to_center);
                }
                self.drain_and_dispatch(ui, data);
            }
        }

        self.live_frame(ui, data, render, scrolled_in_place, true);
        StepResult {
            index,
            action: action_desc,
            status: "ok",
            detail: format!("acted at ({:.1},{:.1})", center.x, center.y),
            artifact: None,
        }
    }

    /// One scroll step through the PRODUCTION path — the shared core of the
    /// `Scroll` gesture and the `ScrollTo` action's loop, so both drive the same
    /// dispatch the live mouse wheel does (`window_input.rs`). `center` picks the
    /// container exactly as the real handler does: the inspector scrolls in place
    /// (synchronous node-offset, returns whether it handled it so the caller can
    /// set `scrolled_in_place`); the Scene and Audio Setup docks route the
    /// generic pipeline and ask for the rebuild that bakes the offset in
    /// (BUG-294); anything else routes the generic pipeline. Returns whether the inspector handled the scroll in place.
    fn scroll_once(&mut self, ui: &mut UIRoot, data: &mut SceneData, center: Vec2, delta: Vec2) -> bool {
        // The Scene tab sits inside the inspector rect, so it is tested first,
        // in the same order as `window_input::primary_mouse_wheel`.
        if ui.inspector.scene_content_rect().contains(center) || ui.layout.audio_setup().contains(center) {
            // `window_input::primary_mouse_wheel`'s dock branch: the generic
            // scroll pipeline; the moved offset is a planner input.
            ui.input.process_scroll(center, delta);
            self.drain_and_dispatch(ui, data);
        } else if ui.layout.inspector().contains(center) {
            if ui.try_inspector_scroll(delta.y, center.x) {
                return ui.inspector.take_scrolled_in_place();
            }
            // Nothing built yet — the real handler's same fallback.
            ui.inspector.handle_scroll_at(delta.y, center.x);
        } else {
            ui.input.process_scroll(center, delta);
            self.drain_and_dispatch(ui, data);
        }
        false
    }

    /// Scroll `target`'s enclosing container until it sits inside the visible
    /// band, so a following `Pointer` step can act on a row that laid out far
    /// below the fold (`WIDGET_TREE_DESIGN.md` section 5/section 6 converged card rows —
    /// `Angle` at y≈7789 in the gltfscene fixture). Loops [`Self::scroll_once`]
    /// and re-resolves each step, converging by OBSERVATION (it watches the
    /// target's rect move) rather than a fixture-fragile fixed delta. Direction
    /// is auto-detected on the first productive step, so it is correct for either
    /// container regardless of that container's wheel-sign convention.
    #[allow(clippy::too_many_arguments)]
    fn scroll_to(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        index: usize,
        action_desc: String,
        target: &AutomationTarget,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> StepResult {
        // Start with a chunky wheel notch so deep rows reach the viewport within
        // the cap. Halve it when a step crosses the whole viewport; wheel speed
        // changes must not make this driver oscillate past a reachable row.
        // Success is judged on the target's CENTER (that is where
        // a following gesture synthesizes its pointer / drag origin), not full
        // containment — the last row of a maxed-out scroll can never clear a
        // both-sides margin, yet its center is perfectly clickable. `MARGIN` just
        // keeps the center off the very clipped edge.
        const STEP: f32 = 2400.0;
        const MAX_ITERS: usize = 40;
        const MARGIN: f32 = 8.0;

        // Resolve once to locate the target's COLUMN. Pick the container by X
        // (scroll-invariant) — the target's Y is exactly what is off-screen, so a
        // `contains(center)` test on it would mis-route.
        let rect0 = match self.resolve(ui, target) {
            Ok(r) => r,
            Err(e) => return self.fail(index, action_desc, ui, out_dir, e),
        };
        let cx = rect0.x + rect0.width * 0.5;
        let insp = ui.layout.inspector();
        let scene = ui.inspector.scene_content_rect();
        let in_x = |r: Rect| r.width > 0.0 && cx >= r.x && cx < r.x + r.width;
        let container_is_inspector = if ui.scene_setup_panel.is_open() && in_x(scene) {
            false
        } else if in_x(insp) {
            true
        } else {
            return self.fail(
                index,
                action_desc,
                ui,
                out_dir,
                format!("ScrollTo: target column x={cx:.0} is in no open scroll container (inspector {insp:?}, scene {scene:?})"),
            );
        };
        // The Scene tab has two scroll panes; the target's own one scrolls.
        let node = self.resolve_all(ui, target).0.first().and_then(|m| m.node);
        let scene_pane = ui.scene_setup_panel.scroll_viewport_of(&ui.tree, node);
        let container_rect = |ui: &UIRoot| if container_is_inspector { ui.layout.inspector() } else { scene_pane };

        // +1: a negative delta reveals lower content (target's Y decreases toward
        // the band). Both containers share this convention today; detecting it on
        // the first productive step keeps ScrollTo correct if one ever flips.
        let mut sign = 1.0_f32;
        let mut sign_locked = false;
        let mut step = STEP;

        for _ in 0..MAX_ITERS {
            let rect = match self.resolve(ui, target) {
                Ok(r) => r,
                Err(e) => return self.fail(index, action_desc.clone(), ui, out_dir, e),
            };
            let vis = container_rect(ui);
            let top = vis.y + MARGIN;
            let bot = vis.y + vis.height - MARGIN;
            let cy_target = rect.y + rect.height * 0.5;
            if cy_target >= top && cy_target <= bot {
                return StepResult {
                    index,
                    action: action_desc,
                    status: "ok",
                    detail: format!("scrolled target center to y={cy_target:.0}"),
                    artifact: None,
                };
            }
            // Below the band → want the row's Y to DECREASE (move up into view);
            // above → INCREASE. `dir` encodes that; `sign` corrects the container's
            // actual polarity.
            let want_y_decrease = cy_target > bot;
            let dir = if want_y_decrease { -1.0 } else { 1.0 };
            let sc = Vec2::new(cx.clamp(vis.x + 1.0, vis.x + vis.width - 1.0), vis.y + vis.height * 0.5);
            let prev_y = rect.y;
            let sip = self.scroll_once(ui, data, sc, Vec2::new(0.0, dir * sign * step));
            self.live_frame(ui, data, render, sip, true);
            let moved = match self.resolve(ui, target) {
                Ok(r) => r.y - prev_y,
                Err(_) => 0.0,
            };
            if moved.abs() <= 0.5 {
                // Clamped at an edge and still not in the band — cannot reach it.
                break;
            }
            if !sign_locked {
                // If the row moved the WRONG way, our polarity guess was inverted.
                let y_decreased = moved < 0.0;
                if want_y_decrease != y_decreased {
                    sign = -sign;
                }
                sign_locked = true;
            }
            let next_y = cy_target + moved;
            if (cy_target > bot && next_y < top) || (cy_target < top && next_y > bot) {
                step *= 0.5;
            }
        }

        match self.resolve(ui, target) {
            Ok(rect) => {
                let vis = container_rect(ui);
                let cy_target = rect.y + rect.height * 0.5;
                if cy_target >= vis.y + MARGIN && cy_target <= vis.y + vis.height - MARGIN {
                    StepResult {
                        index,
                        action: action_desc,
                        status: "ok",
                        detail: format!("scrolled target center to y={cy_target:.0}"),
                        artifact: None,
                    }
                } else {
                    self.fail(
                        index,
                        action_desc,
                        ui,
                        out_dir,
                        format!(
                            "ScrollTo: exhausted {MAX_ITERS} steps, target center y={cy_target:.0} still outside band [{:.0},{:.0}]",
                            vis.y + MARGIN,
                            vis.y + vis.height - MARGIN
                        ),
                    )
                }
            }
            Err(e) => self.fail(index, action_desc, ui, out_dir, e),
        }
    }

    /// Drain events queued by the pointer/key synthesis above through the
    /// SAME per-frame call the live app makes (`app_render.rs:869`), then
    /// route any tracks-area events (clip click/drag) through the overlay —
    /// the exact real path `app_render.rs`'s viewport-events block uses,
    /// minus the live `Application` (see the module doc for why that's
    /// still the real path, not a parallel one).
    /// Returns the number of panel actions the UI produced this drain, so
    /// key steps can distinguish a handled shortcut from a silent no-op
    /// (BUG-198: "ok" with nothing happened is the bug, not the fallthrough).
    fn drain_and_dispatch(&mut self, ui: &mut UIRoot, data: &mut SceneData) -> usize {
        let actions = ui.process_events();
        let n_actions = actions.len();
        self.apply_panel_actions(ui, data, &actions);

        let viewport_events = ui.drain_viewport_events();
        if viewport_events.is_empty() {
            return n_actions;
        }
        self.overlay.set_modifiers(self.modifiers);
        if viewport_events.iter().any(|event| matches!(event, UIEvent::PointerDown { .. })) {
            crate::ui_bridge::sync_automation_lane_order(&data.project, &mut data.selection);
        }
        let mut host = AppEditingHost::new(
            crate::ui_bridge::ProjectRef::new(&mut data.project, &mut self.edits, &self.content_tx),
            &self.content_tx,
            &self.content_state,
            &mut self.cursor_manager,
            &mut self.active_layer,
            &mut self.redraw,
        );
        for event in &viewport_events {
            if let UIEvent::PointerDown { modifiers, .. }
                | UIEvent::Click { modifiers, .. }
                | UIEvent::DoubleClick { modifiers, .. }
                | UIEvent::RightClick { modifiers, .. }
                | UIEvent::DragBegin { modifiers, .. }
                | UIEvent::Drag { modifiers, .. } = event
            {
                self.overlay.set_modifiers(*modifiers);
            }
            match event {
                UIEvent::PointerDown { pos, .. } => {
                    self.overlay.on_pointer_down(
                        *pos, &mut host, &mut data.selection, &ui.viewport,
                    );
                }
                UIEvent::PointerUp { .. } => {
                    self.overlay.on_pointer_up(&mut host, &mut data.selection);
                }
                UIEvent::Click { pos, modifiers, .. } => {
                    self.overlay.on_pointer_click(
                        *pos,
                        modifiers.shift,
                        modifiers.ctrl || modifiers.command,
                        1,
                        false,
                        &mut host,
                        &mut data.selection,
                        &ui.viewport,
                    );
                }
                UIEvent::DoubleClick { pos, modifiers, .. } => {
                    self.overlay.on_pointer_click(
                        *pos,
                        modifiers.shift,
                        modifiers.ctrl || modifiers.command,
                        2,
                        false,
                        &mut host,
                        &mut data.selection,
                        &ui.viewport,
                    );
                }
                UIEvent::RightClick { pos, .. } => {
                    self.overlay.on_pointer_click(
                        *pos,
                        false,
                        false,
                        1,
                        true,
                        &mut host,
                        &mut data.selection,
                        &ui.viewport,
                    );
                }
                UIEvent::DragBegin { origin, pos, .. } => {
                    self.overlay.on_begin_drag(*origin, &mut host, &mut data.selection, &ui.viewport);
                    if self.overlay.is_automation_drag() {
                        self.overlay.on_drag(*pos, &mut host, &mut data.selection, &mut ui.viewport);
                    }
                }
                UIEvent::Drag { pos, .. } => {
                    self.overlay.on_drag(*pos, &mut host, &mut data.selection, &mut ui.viewport);
                }
                UIEvent::DragEnd { pos, .. } => {
                    if self.overlay.is_automation_drag() {
                        self.overlay.on_drag(*pos, &mut host, &mut data.selection, &mut ui.viewport);
                    }
                    self.overlay.on_end_drag(&mut host);
                }
                _ => {}
            }
        }
        // BUG-293: context-menu / overlay-originated actions land in
        // `host.pending_actions` (e.g. `AppEditingHost::push_context_menu`)
        // and the live app drains them the same way (`app_render.rs:1471`,
        // `actions.append(&mut host.pending_actions)`); this driver used to
        // just discard them. Take them before `host`'s borrow of
        // `data.project` ends, then dispatch through the same real bridge
        // every other `PanelAction` here goes through.
        let mut pending = std::mem::take(&mut host.pending_actions);
        drop(host);
        // Match the live application pass for overlay-generated context menus.
        ui.intercept_overlay_actions(&mut pending);
        self.apply_panel_actions(ui, data, &pending);
        n_actions + pending.len()
    }

    /// Route every `PanelAction` through the REAL `ui_bridge::dispatch` —
    /// the same bridge the live app's action loop calls (`app_render.rs`'s
    /// dispatch site), against driver-owned scratch state. The original
    /// driver mirrored a single `LayerClicked` arm here and logged the rest
    /// unapplied; that made the driver blind to every transport/inspector
    /// action (found 2026-07-07 chasing the "dead LANES button" — the click
    /// resolved, then evaporated in this exact function). The stated reason
    /// (dispatch needs `UserPrefs::load()` disk I/O, breaking D7 determinism)
    /// dissolves with `UserPrefs::in_memory()`: empty prefs, deterministic on
    /// any host, `save()` diverted to the temp dir.
    fn apply_panel_actions(&mut self, ui: &mut UIRoot, data: &mut SceneData, actions: &[PanelAction]) {
        for action in actions {
            // The live action loop answers these by watching the graph and
            // opening the editor window (`open_graph_editor`).
            if let PanelAction::Root(
                root @ (manifold_ui::RootAction::OpenGraphEditor(_)
                | manifold_ui::RootAction::OpenGeneratorGraphEditor
                | manifold_ui::RootAction::OpenGraphTarget(_)
                | manifold_ui::RootAction::SceneSetupOpenGraphEditor(_)
                | manifold_ui::RootAction::PreviewSceneModifierObject(..)),
            ) = action
            {
                self.watch_for_open(ui, data, root);
                let opened = self.open_editor(data);
                println!("ui-snap --script: {action:?} -> graph editor {}", if opened { "opened" } else { "already open" });
                continue;
            }
            // The live action loop defers this to `handle_perform_mode_pending`,
            // which enters once an output window exists. The runner has no
            // output window to open, so it enters at once.
            if let PanelAction::Project(manifold_ui::panels::ProjectAction::EnterPerformMode) = action {
                if !self.perform.active {
                    crate::perform_mode::frame::enter(&mut self.perform, ui, &mut self.text_input, None);
                }
                continue;
            }
            // The App's action loop opens text sessions before the bridge.
            if crate::text_session::begin_inspector_entry(
                &mut self.text_input,
                action,
                false,
                ui.inspector.last_effect_tab(),
                &self.active_layer,
                &data.selection,
                &data.project,
            ) || crate::text_session::begin_for_action(&mut self.text_input, ui, &data.project, &data.selection, action)
            {
                println!("ui-snap --script: {action:?} -> text session {:?}", self.text_input.field);
                continue;
            }
            // Mapping-drawer actions carry their owner; the App's action loop
            // hands them to the shared mapping handler before the bridge.
            if let PanelAction::Root(root) = action {
                let outcome = crate::editor_bridge::MappingHost {
                    project: crate::ui_bridge::ProjectRef::new(&mut data.project, &mut self.edits, &self.content_tx),
                    scrub: &mut self.scrub,
                }
                .dispatch(root);
                match outcome {
                    crate::editor_bridge::MappingDispatch::NotMapping => {}
                    crate::editor_bridge::MappingDispatch::Done => {
                        println!("ui-snap --script: dispatched mapping {action:?}");
                        continue;
                    }
                    crate::editor_bridge::MappingDispatch::GotoNode(target, binding_id) => {
                        if let Some(ed) = self.editor.as_mut() {
                            let graph = crate::editor_bridge::editor_ui_graph(&mut ed.ui_graph, &data.content, self.watched.as_ref(), &data.project);
                            crate::editor_bridge::goto_mapped_node(self.watched.as_ref(), graph.as_ref(), Some(&mut ed.canvas), &target, &binding_id);
                        }
                        continue;
                    }
                }
            }
            // A main-window card click retargets an open editor to that card's
            // graph, then still dispatches for the card's own selection.
            if self.editor.is_some() {
                match action {
                    PanelAction::Params(manifold_ui::ParamsAction::EffectCardClicked(ei)) => {
                        let tab = ui.inspector.last_effect_tab();
                        if let Some(eid) = crate::editor_bridge::effect_card_id(tab, &data.project, self.active_layer.as_ref(), &data.selection, *ei) {
                            self.watch(data, manifold_core::GraphTarget::Effect(eid));
                        }
                    }
                    PanelAction::Params(manifold_ui::ParamsAction::GenCardClicked) => {
                        if let Some(layer) = self.active_layer.clone() {
                            self.watch(data, manifold_core::GraphTarget::Generator(layer));
                        }
                    }
                    _ => {}
                }
            }
            // The live app resolves this in `app_render.rs` from its graph
            // editor's viewport; here the runner's viewport stands in.
            if let PanelAction::Root(manifold_ui::RootAction::SceneSetupCopyViewportCamera(layer)) = action {
                let view = self.viewport.as_ref().filter(|(shown, _)| shown == layer).map(|(_, camera)| *camera);
                crate::scene_view_copy::run(&mut crate::ui_bridge::ProjectRef::new(&mut data.project, &mut self.edits, &self.content_tx), ui, layer, view.as_ref());
                continue;
            }
            if let PanelAction::Params(manifold_ui::ParamsAction::EditCards(edit)) = action {
                // Use the live input host: treating this app-intercepted action
                // as a bridge no-op loses clipboard state while reporting success.
                let mut scratch = [false; 2];
                let mut project_io = crate::project_io::ProjectIOService::new(&self.user_prefs);
                let mut import_failures_rx = None;
                #[cfg(target_os = "macos")]
                let mut clipboard_change_count = None;
                let mut host = crate::input_host::AppInputHost::detached(
                    &mut data.project,
                    &self.content_tx,
                    &self.content_state,
                    &mut *ui,
                    &mut data.selection,
                    &mut self.active_layer,
                    &mut self.redraw,
                    &mut self.edits,
                    &mut scratch,
                    &mut project_io,
                    &mut import_failures_rx,
                    #[cfg(target_os = "macos")]
                    &mut clipboard_change_count,
                );
                host.edit_cards(*edit);
                let pending = std::mem::take(&mut ui.pending_keyboard_actions);
                self.apply_panel_actions(ui, data, &pending);
                continue;
            }
            // The live app intercepts generator string dropdowns in
            // `app_render.rs` before they reach the bridge. The headless
            // runner owns that action loop, so mirror the audio-send branch
            // here; otherwise the click dispatches successfully but no
            // dropdown overlay is minted for the next scripted pointer.
            if let PanelAction::Params(manifold_ui::ParamsAction::GenStringParamDropdownClicked(sp_idx)) = action
                && let Some((sp, rect)) = ui.inspector.gen_params().and_then(|gp| {
                    let sp = gp.string_param(*sp_idx).cloned()?;
                    (sp.key == "audioSend")
                        .then(|| gp.string_param_rect(&ui.tree, *sp_idx).map(|rect| (sp, rect)))
                        .flatten()
                })
            {
                let items = sp
                    .dropdown_choices
                    .iter()
                    .map(|choice| {
                        if choice.disabled {
                            manifold_ui::panels::dropdown::DropdownItem::disabled(&choice.label)
                        } else {
                            manifold_ui::panels::dropdown::DropdownItem::new(&choice.label)
                                .with_action(PanelAction::Params(
                                    manifold_ui::ParamsAction::GenStringParamSelected(
                                        *sp_idx,
                                        choice.value.clone(),
                                    ),
                                ))
                        }
                    })
                    .collect();
                ui.open_dropdown_typed(
                    items,
                    manifold_ui::node::Rect::new(rect.x, rect.y, rect.width, rect.height),
                );
                println!("ui-snap --script: opened generator audio-send dropdown");
                continue;
            }
            let mut dctx = crate::ui_bridge::DispatchCtx {
                project: crate::ui_bridge::ProjectRef::new(&mut data.project, &mut self.edits, &self.content_tx),
                content_tx: &self.content_tx,
                content_state: &self.content_state,
                ui: &mut *ui,
                selection: &mut data.selection,
                active_layer: &mut self.active_layer,
                user_prefs: &mut self.user_prefs,
                editor_target: None,
                scrub: &mut self.scrub,
                dialogs: Default::default(),
            };
            crate::ui_bridge::dispatch(action, &mut dctx);
            println!("ui-snap --script: dispatched {action:?}");
            // The fixture's active-layer INDEX feeds `sync_build`'s inspector
            // sync; derive it from the id the real bridge maintains (the old
            // mirrored arm set it directly).
            data.active = self.active_layer.as_ref()
                .and_then(|lid| data.project.timeline.find_layer_index_by_id(lid));
        }
    }

    /// D8 (UI_FUNNEL_DECOMPOSITION): direct-set a scrubable param by dispatching
    /// the three scrub phases (`Begin → Move → Commit`) through the SAME
    /// `ui_bridge::dispatch` a live drag reaches (`apply_panel_actions`), then
    /// advancing a frame so the result renders. Undo-correct by construction —
    /// one command per gesture — with no pixel geometry, so a flow can set up a
    /// param value in one line instead of a fragile pointer-drag.
    #[allow(clippy::too_many_arguments)]
    fn set_param(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        index: usize,
        action_desc: String,
        param: &manifold_ui::automation::FlowScrubTarget,
        value: f32,
        render: &mut RenderState,
    ) -> StepResult {
        use manifold_ui::panels::ScrubPhase;
        let (value_ref, range) = match flow_value_ref(data, &|ei| crate::editor_bridge::effect_card_id(ui.inspector.last_effect_tab(), &data.project, self.active_layer.as_ref(), &data.selection, ei), param) {
            Ok(v) => v,
            Err(e) => return StepResult { index, action: action_desc, status: "fail", detail: e, artifact: None },
        };
        let actions = vec![
            PanelAction::Scrub(value_ref.clone(), ScrubPhase::Begin),
            PanelAction::Scrub(value_ref.clone(), ScrubPhase::Move(scrub_value(range, value))),
            PanelAction::Scrub(value_ref, ScrubPhase::Commit),
        ];
        self.apply_panel_actions(ui, data, &actions);
        self.live_frame(ui, data, render, false, true);
        StepResult {
            index,
            action: action_desc,
            status: "ok",
            detail: format!("set {param:?} = {value} (Begin/Move/Commit through dispatch)"),
            artifact: None,
        }
    }

    /// `ScrubDrag`: Begin, `steps` Moves with a frame each, Commit — the scrub a
    /// held mouse produces, through the same dispatch as `set_param`.
    #[allow(clippy::too_many_arguments)]
    fn scrub_drag(
        &mut self,
        ui: &mut UIRoot,
        data: &mut SceneData,
        index: usize,
        action_desc: String,
        param: &manifold_ui::automation::FlowScrubTarget,
        (from, to): (f32, f32),
        steps: u32,
        out_dir: &std::path::Path,
        render: &mut RenderState,
    ) -> StepResult {
        use manifold_ui::panels::ScrubPhase;
        let (value_ref, range) = match flow_value_ref(data, &|ei| crate::editor_bridge::effect_card_id(ui.inspector.last_effect_tab(), &data.project, self.active_layer.as_ref(), &data.selection, ei), param) {
            Ok(v) => v,
            Err(e) => return self.fail(index, action_desc, ui, out_dir, e),
        };
        let steps = steps.max(1);
        self.apply_panel_actions(ui, data, &[PanelAction::Scrub(value_ref.clone(), ScrubPhase::Begin)]);
        self.live_frame(ui, data, render, false, true);
        for i in 1..=steps {
            let value = from + (to - from) * i as f32 / steps as f32;
            self.apply_panel_actions(
                ui,
                data,
                &[PanelAction::Scrub(value_ref.clone(), ScrubPhase::Move(scrub_value(range, value)))],
            );
            self.live_frame(ui, data, render, false, true);
        }
        self.apply_panel_actions(ui, data, &[PanelAction::Scrub(value_ref, ScrubPhase::Commit)]);
        self.live_frame(ui, data, render, false, true);
        StepResult {
            index,
            action: action_desc,
            status: "ok",
            detail: format!("{steps} scrub move(s) {from} -> {to} through dispatch"),
            artifact: None,
        }
    }

    fn assert(
        &mut self,
        ui: &mut UIRoot,
        index: usize,
        action_desc: String,
        selector: &AutomationTarget,
        check: &AssertCheck,
        out_dir: &std::path::Path,
    ) -> StepResult {
        let (matches, query) = self.resolve_all(ui, selector);
        let result = match check {
            AssertCheck::Exists => {
                if matches.is_empty() {
                    Err(format!("expected a match for {query}, found none"))
                } else {
                    Ok(format!("{} match(es) for {query}", matches.len()))
                }
            }
            AssertCheck::Count(n) => {
                if matches.len() as u32 == *n {
                    Ok(format!("count({n}) held for {query}"))
                } else {
                    Err(format!("expected count {n} for {query}, found {}", matches.len()))
                }
            }
            AssertCheck::TextEquals(expected) => match matches.len() {
                1 => {
                    let text = matches[0].text.as_deref();
                    if text == Some(expected.as_str()) {
                        Ok(format!("text == {expected:?} for {query}"))
                    } else {
                        Err(format!("expected text {expected:?} for {query}, found {text:?}"))
                    }
                }
                0 => Err(format!("no match for {query} (want text {expected:?})")),
                n => Err(format!("{n} matches for {query} — TextEquals needs exactly one")),
            },
            AssertCheck::RectWithin(expected) => match matches.len() {
                1 => {
                    let r = matches[0].rect;
                    let tol = 2.0_f32; // sub-pixel jitter tolerance, not a design gap
                    let within = (r.x - expected.x).abs() <= tol
                        && (r.y - expected.y).abs() <= tol
                        && (r.width - expected.width).abs() <= tol
                        && (r.height - expected.height).abs() <= tol;
                    if within {
                        Ok(format!(
                            "rect ({:.1},{:.1} {:.1}x{:.1}) within tolerance of expected for {query}",
                            r.x, r.y, r.width, r.height
                        ))
                    } else {
                        Err(format!(
                            "rect ({:.1},{:.1} {:.1}x{:.1}) NOT within tolerance of expected \
                             ({:.1},{:.1} {:.1}x{:.1}) for {query}",
                            r.x, r.y, r.width, r.height, expected.x, expected.y, expected.width, expected.height
                        ))
                    }
                }
                0 => Err(format!("no match for {query} (want rect_within)")),
                n => Err(format!("{n} matches for {query} — RectWithin needs exactly one")),
            },
            AssertCheck::Above(other) => {
                let other_target = AutomationTarget::Query(other.clone());
                let (other_matches, other_query) = self.resolve_all(ui, &other_target);
                match matches.len() {
                    0 => Err(format!("no match for {query} (want Above {other_query})")),
                    1 => match other_matches.len() {
                        0 => Err(format!("no match for {other_query} (Above needs exactly one)")),
                        1 => {
                            let source = matches[0].rect;
                            let destination = other_matches[0].rect;
                            if source.y_max() <= destination.y {
                                Ok(format!("{query} is above {other_query}"))
                            } else {
                                Err(format!(
                                    "incorrect order: {query} is not above {other_query}: bottom {:.1} > top {:.1}",
                                    source.y_max(), destination.y
                                ))
                            }
                        }
                        n => Err(format!(
                            "{n} matches for {other_query} — Above needs exactly one destination"
                        )),
                    },
                    n => Err(format!("{n} matches for {query} — Above needs exactly one source")),
                }
            }
        };

        match result {
            Ok(detail) => StepResult { index, action: action_desc, status: "ok", detail, artifact: None },
            Err(detail) => self.fail(index, action_desc, ui, out_dir, detail),
        }
    }

    /// D6: a failure carries the dump as evidence — write it now regardless
    /// of whether the failing step was itself a `Dump`.
    fn fail(
        &mut self,
        index: usize,
        action_desc: String,
        ui: &UIRoot,
        out_dir: &std::path::Path,
        detail: String,
    ) -> StepResult {
        let path = out_dir.join(format!("{index:02}.fail.tree.json"));
        self.write_dump(ui, &path);
        StepResult {
            index,
            action: action_desc,
            status: "fail",
            detail,
            artifact: Some(path.display().to_string()),
        }
    }

    fn write_dump(&self, ui: &UIRoot, path: &std::path::Path) {
        let (clip_rects, automation_lanes) = self.surfaces_owned(ui);
        let clip_targets = ClipHitTargets(&clip_rects);
        let automation_targets = AutomationHitTargets(&automation_lanes);
        let surfaces: Vec<&dyn HitTargets> = vec![&clip_targets, &automation_targets];
        let json = super::dump::dump_tree_ex(&ui.tree, &surfaces);
        std::fs::write(path, serde_json::to_string_pretty(&json).expect("serialize dump"))
            .expect("write dump json");
        // An open editor window's tree lands beside it: `NN.tree.json` ->
        // `NN.editor.tree.json`.
        if let Some(ed) = self.editor.as_ref() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("tree.json");
            let editor_path = path.with_file_name(name.replacen("tree.json", "editor.tree.json", 1));
            std::fs::write(&editor_path, serde_json::to_string_pretty(&ed.dump(&self.editor_mapping)).expect("serialize dump"))
                .expect("write editor dump json");
        }
    }

    /// P2 (D3): reads back the offscreen `live_frame`/the `Step` loop
    /// already composited through the shared seam — no fresh device/cache is
    /// built here (that was `render_ui_to_png`'s old, always-full-clear
    /// shape; see `render.rs`'s module doc). Draws the SAME
    /// `crate::ui_frame::render_main_ui_passes` the live app and
    /// `render_ui_to_png` call (P2, `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md`
    /// section 4 step 2 — no more parallel `draw_immediate_passes`) on top, then
    /// stamps D9b's pointer crosshair(s) CPU-side on the readback COPY —
    /// never into the atlas/offscreen, which would poison it for any later
    /// frame or the P0 differential shelf tool. No thumbnails here (this
    /// driver never opted into `--thumbs`, matching the old call's `false`).
    fn write_png(&mut self, ui: &mut UIRoot, data: &mut SceneData, render: &mut RenderState, path: &std::path::Path) {
        // The perform screen replaces the main window's passes, as in the
        // App; its last frame is already in the offscreen.
        if !self.perform.active {
            self.paint_main_passes(ui, data, render);
        }
        let (tex_w, tex_h) = (render.tex_w, render.tex_h);
        let mut linear = render.readback_linear();
        for pt in self.last_gesture_points.drain(..) {
            stamp_crosshair(&mut linear, tex_w, tex_h, pt);
        }
        super::render::save_ui_png(&linear, tex_w, tex_h, path);
    }

    fn paint_main_passes(&mut self, ui: &mut UIRoot, data: &mut SceneData, render: &mut RenderState) {
        let (tex_w, tex_h) = (render.tex_w, render.tex_h);
        let mut clip_rects = Vec::new();
        ui.viewport.visible_clip_rects(&mut clip_rects);
        let hovered_clip = ui.viewport.hovered_clip_id();
        let clip_bodies: Vec<manifold_ui_paint::clip_draw::ClipBody> = clip_rects
            .iter()
            .map(|cr| manifold_ui_paint::clip_draw::ClipBody {
                rect: cr.rect,
                base_color: cr.base_color,
                selected: data.selection.is_selected(&cr.clip_id),
                hovered: hovered_clip == Some(cr.clip_id.as_str()),
                muted: cr.is_muted,
                locked: cr.is_locked,
                generator: cr.is_generator,
                alpha: 1.0,
            })
            .collect();
        // Use the live point-feedback resolver so snapshots exercise the same
        // nearby readout and point highlight as frame/present.rs.
        self.overlay.set_modifiers(self.modifiers);
        if ui.background_input_blocked() {
            data.selection.automation_feedback = None;
        } else if let Some(&position) = self.last_gesture_points.last() {
            self.overlay.refresh_automation_feedback(position, ui.viewport.automation_lane_screens(),
                &mut data.selection, &ui.viewport);
        }
        let frame_timer = crate::frame_timer::FrameTimer::new(60.0);
        // LED composite preview band (LED_STRIPS_DESIGN MVP-P4): a real
        // panel-bitmap instance so the band's quad paints in flow snapshots
        // too — same helper the base render path uses.
        let mut bitmap_gpu = super::render::make_panel_bitmap_gpu(render.device, ui);
        crate::ui_frame::render_main_ui_passes(
            render.device,
            &mut render.ui_renderer,
            ui,
            &render.composite.offscreen,
            tex_w,
            tex_h,
            f64::from(super::SCALE),
            crate::ui_frame::MainUiPassInputs {
                panel_bitmap_gpu: Some(&mut bitmap_gpu),
                clip_bodies: &clip_bodies,
                clip_rects: &clip_rects,
                clip_content_gpu: None,
                thumb: None,
                timeline_overlays: manifold_ui::panels::viewport::TimelineOverlays::default(),
                markers: &[],
                landing_flash: None,
                automation_latched: &data.content.automation_latched_params,
                automation_selection: Some(&data.selection),
                cursor_pos: manifold_ui::node::Vec2::ZERO,
                text_input: &self.text_input,
                frame_timer: &frame_timer,
                vqt: None,
                blit_pipeline: &render.composite.blit_pipeline,
                blit_sampler: &render.composite.blit_sampler,
                gpu_sink: None,
            },
        );
    }
}

/// The keys a `Key` or `Text` step types: each character of `Text` as its own
/// key with no modifiers held, as the live door sends it; a letter key
/// carries its character, as winit reports it.
fn step_keys(action: &AutomationAction) -> Vec<(String, Option<Key>, manifold_ui::input::Modifiers)> {
    match action {
        AutomationAction::Text { text } => {
            text.chars().map(|c| (c.to_string(), None, manifold_ui::input::Modifiers::NONE)).collect()
        }
        AutomationAction::Key { key, modifiers } => vec![(format!("{key:?}").to_lowercase(), Some(*key), *modifiers)],
        _ => unreachable!("only Key and Text type"),
    }
}

/// One step key as a text field reads it.
fn text_key(chars: &str, key: Option<Key>) -> crate::text_session::TextKey<'_> {
    use crate::text_session::{Arrow, TextKey};
    match key {
        None => TextKey::Chars(chars),
        Some(Key::Escape) => TextKey::Escape,
        Some(Key::Enter) => TextKey::Enter,
        Some(Key::Backspace) => TextKey::Backspace,
        Some(Key::Delete) => TextKey::Delete,
        Some(Key::Left) => TextKey::Arrow(Arrow::Left),
        Some(Key::Right) => TextKey::Arrow(Arrow::Right),
        Some(Key::Up) => TextKey::Arrow(Arrow::Up),
        Some(Key::Down) => TextKey::Arrow(Arrow::Down),
        Some(Key::Space) => TextKey::Space,
        Some(_) if chars.len() == 1 => TextKey::Chars(chars),
        Some(_) => TextKey::Other,
    }
}

/// The real `ValueRef` a flow's scrub address names, and whether its Moves
/// carry a `(min, max)` range rather than a scalar.
fn flow_value_ref(
    data: &SceneData,
    effect_at: &dyn Fn(usize) -> Option<manifold_core::EffectId>,
    param: &manifold_ui::automation::FlowScrubTarget,
) -> Result<(manifold_ui::panels::ValueRef, bool), String> {
    use manifold_ui::automation::FlowScrubTarget as F;
    use manifold_ui::panels::{GraphParamTarget, ValueRef};
    let fx = |i: usize| effect_at(i).ok_or_else(|| format!("scrub address names effect {i}; the active chain has no such effect"));
    let target = |effect: &Option<usize>| match effect {
        Some(i) => fx(*i).map(GraphParamTarget::Effect),
        None => Ok(GraphParamTarget::Generator),
    };
    let pid = |p: &String| std::borrow::Cow::Owned(p.clone());
    let layer = |i: usize| {
        data.project.timeline.layers.get(i).map(|l| l.layer_id.clone())
            .ok_or_else(|| format!("scrub address names layer {i}; the fixture has {}", data.project.timeline.layers.len()))
    };
    Ok(match param {
        F::MasterOpacity => (ValueRef::MasterOpacity, false),
        F::LedBrightness => (ValueRef::LedBrightness, false),
        F::LayerOpacity => (ValueRef::LayerOpacity, false),
        F::Macro(idx) => (ValueRef::Macro(*idx), false),
        F::EffectParam { effect, param } => (ValueRef::Param(GraphParamTarget::Effect(fx(*effect)?), pid(param)), false),
        F::GeneratorParam { param } => (ValueRef::Param(GraphParamTarget::Generator, pid(param)), false),
        F::Trim { kind, effect, param } => (ValueRef::Trim(*kind, target(effect)?, pid(param)), true),
        F::EnvelopeTarget { effect, param } => (ValueRef::EnvelopeTarget(target(effect)?, pid(param)), false),
        F::AudioModShape { effect, param, shape } => (ValueRef::AudioModShape(target(effect)?, pid(param), *shape), false),
        F::AudioTriggerShape { layer: l, index, shape } => (ValueRef::AudioTriggerShape(layer(*l)?, *index, *shape), false),
        F::LayerAudioGain { layer: l } => (ValueRef::LayerAudioGain(layer(*l)?), false),
        F::AudioSendGain { send } => {
            let id = data.project.audio_setup.sends.get(*send).map(|s| s.id.clone())
                .ok_or_else(|| format!("scrub address names send {send}; the fixture has {}", data.project.audio_setup.sends.len()))?;
            (ValueRef::AudioSendGain(id), false)
        }
        F::Crossover(divider) => (ValueRef::AudioCrossover(*divider), false),
        F::AbletonMacroTrim(slot) => (ValueRef::AbletonMacroTrim(*slot), true),
    })
}

fn scrub_value(range: bool, value: f32) -> manifold_ui::panels::ScrubValue {
    use manifold_ui::panels::ScrubValue;
    if range { ScrubValue::Range(0.0, value) } else { ScrubValue::Scalar(value) }
}

/// The committed ceilings file, or an empty one when it does not exist yet.
fn read_work_ceilings() -> serde_json::Value {
    std::fs::read_to_string(WORK_CEILINGS_PATH)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({ "flows": {} }))
}

fn write_work_ceilings(ceilings: &serde_json::Value) {
    let text = serde_json::to_string_pretty(ceilings).expect("serialize work ceilings") + "\n";
    std::fs::write(WORK_CEILINGS_PATH, text).expect("write work ceilings");
}

/// D9b: draw a small crosshair (~11px, opaque red) centered at `pt` (logical
/// == texel here — the harness is always scale factor 1.0, D8) directly into
/// linear Rgba16Float bytes of stride `tex_w * 8`. CPU-side only; never called
/// on a texture, only on a readback `Vec<u8>` already destined for a PNG.
fn stamp_crosshair(linear: &mut [u8], tex_w: u32, tex_h: u32, pt: Vec2) {
    const RADIUS: i32 = 5;
    let color = [
        half::f16::from_f32(1.0).to_bits().to_le_bytes(),
        half::f16::from_f32(0.0).to_bits().to_le_bytes(),
        half::f16::from_f32(0.0).to_bits().to_le_bytes(),
        half::f16::from_f32(1.0).to_bits().to_le_bytes(),
    ];
    let (cx, cy) = (pt.x.round() as i32, pt.y.round() as i32);
    let mut plot = |x: i32, y: i32| {
        if x < 0 || y < 0 || x as u32 >= tex_w || y as u32 >= tex_h {
            return;
        }
        let off = ((y as u32 * tex_w + x as u32) * 8) as usize;
        for (channel, bytes) in color.iter().enumerate() {
            linear[off + channel * 2..off + channel * 2 + 2].copy_from_slice(bytes);
        }
    };
    for d in -RADIUS..=RADIUS {
        plot(cx + d, cy);
        plot(cx, cy + d);
    }
}

/// One-shot click dispatch for `interact.rs`'s `select:` sugar (section 6 — the
/// existing `select:`/`open:` verbs become one-step scripts compiled to the
/// section 4 core). Resolves a layer header by its display `text` and fires a real
/// `Click` through the exact same core the `--script` runner uses
/// (`Runner::resolve` + `drain_and_dispatch`) — no bespoke reimplementation.
/// `Err` on a miss (D6): the caller surfaces it loudly instead of guessing at
/// a fallback index; no fallback path exists here. Deliberately RenderState-
/// free (see that struct's doc) — this never renders a pixel.
pub(super) fn click_by_text(ui: &mut UIRoot, data: &mut SceneData, text: &str) -> Result<(), String> {
    let mut runner = Runner::new();
    let target = AutomationTarget::Query(automation::SelectorQuery {
        text: Some(text.to_string()),
        ..Default::default()
    });
    let rect = runner.resolve(ui, &target)?;
    let center = Vec2::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
    ui.input.set_modifiers(manifold_ui::input::Modifiers::NONE);
    ui.pointer_event(center, PointerAction::Down, 0.0);
    ui.pointer_event(center, PointerAction::Up, 0.0);
    runner.drain_and_dispatch(ui, data);
    Ok(())
}

/// A dump written as failure evidence for a non-script (`--interact`) miss —
/// same D6 contract as `Runner::fail`, exposed for `mod.rs`'s `--interact`
/// branch (no `Runner`/script run in progress there).
pub(super) fn write_fail_dump(ui: &UIRoot, path: &std::path::Path) {
    Runner::new().write_dump(ui, path);
}

#[cfg(test)]
mod composite_gate_tests;
